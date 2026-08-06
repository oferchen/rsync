//! Regression coverage for overlapping `-R` (`--relative`) source operands.
//!
//! When two `--relative` operands overlap - one an ancestor of the other, e.g.
//! `-R a/b a/b/c` - upstream rsync merges every source arg into one shared
//! flist and then collapses the overlap in `flist_sort_and_clean()`
//! (flist.c:3016), so each directory and file in the shared subtree appears
//! exactly once. oc's streaming local-copy executor emits each operand's rows
//! as it walks, so before this fix the descendant operand re-listed the whole
//! overlapping subtree (its implied parents, itself, and its recursive
//! contents), producing duplicate itemize rows and inflated tallies.
//!
//! These tests pin the deduplicated result: no relative path is emitted twice,
//! and the destination tree is still complete. The behaviour is asserted at the
//! `LocalCopyPlan` level (the same path the CLI drives), matching upstream
//! `rsync -R -r -i a/b a/b/c` which prints each entry once.
//!
//! # Upstream Reference
//!
//! - `flist.c:3016 flist_sort_and_clean()` - collapses duplicate/overlapping
//!   entries in the shared flist so each name appears once.

#![cfg(unix)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use engine::local_copy::{
    LocalCopyAction, LocalCopyExecution, LocalCopyOptions, LocalCopyPlan, LocalCopyRecord,
};
use tempfile::tempdir;

/// Builds a source tree `from/a/b/f1` + `from/a/b/c/f2` and returns a
/// `/./`-anchored `-R` operand for each of `subpaths`, followed by the
/// destination. The dot anchor sits right after `from`, so each operand's
/// relative chain is exactly `subpath` (e.g. `a/b`, `a/b/c`).
fn overlapping_tree(subpaths: &[&str]) -> (Vec<OsString>, PathBuf, tempfile::TempDir) {
    let temp = tempdir().expect("tempdir");
    let from = temp.path().join("from");
    let to = temp.path().join("to");
    fs::create_dir_all(from.join("a/b/c")).expect("create source tree");
    fs::create_dir_all(&to).expect("create destination root");
    fs::write(from.join("a/b/f1"), b"one").expect("write f1");
    fs::write(from.join("a/b/c/f2"), b"two").expect("write f2");

    let mut operands: Vec<OsString> = Vec::new();
    for sub in subpaths {
        let mut operand = from.clone();
        operand.push("."); // `/./` anchor: relative chain starts after `from`.
        for comp in sub.split('/') {
            operand.push(comp);
        }
        operands.push(operand.into_os_string());
    }
    operands.push(to.clone().into_os_string());
    (operands, to, temp)
}

/// Collects the relative path of every record that materialises an entry
/// (a created directory or a copied file), which is exactly the set upstream
/// itemizes for a fresh transfer.
fn materialised_paths(records: &[LocalCopyRecord]) -> Vec<PathBuf> {
    records
        .iter()
        .filter(|r| {
            matches!(r.action(), LocalCopyAction::DataCopied)
                || (r.was_created() && matches!(r.action(), LocalCopyAction::DirectoryCreated))
        })
        .map(|r| r.relative_path().to_path_buf())
        .collect()
}

fn assert_no_duplicates(paths: &[PathBuf]) {
    let mut counts: HashMap<&PathBuf, usize> = HashMap::new();
    for p in paths {
        *counts.entry(p).or_default() += 1;
    }
    let dups: Vec<_> = counts
        .iter()
        .filter(|&(_, n)| *n > 1)
        .map(|(p, n)| format!("{} x{n}", p.display()))
        .collect();
    assert!(
        dups.is_empty(),
        "overlapping -R operands must dedup like flist_sort_and_clean; duplicated: {dups:?}; all paths: {paths:?}"
    );
}

/// `-R a/b a/b/c`: the descendant `a/b/c` is wholly covered by the ancestor
/// `a/b`, so it must contribute nothing new. Every directory and file in the
/// shared subtree is itemized exactly once, matching upstream
/// `rsync -R -r -i a/b a/b/c`.
#[test]
fn ancestor_then_descendant_dir_dedups() {
    let (operands, to, _temp) = overlapping_tree(&["a/b", "a/b/c"]);
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .relative_paths(true)
        .collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("overlapping relative copy succeeds");

    // Destination tree is complete.
    assert!(to.join("a/b/f1").is_file());
    assert!(to.join("a/b/c/f2").is_file());

    let paths = materialised_paths(report.records());
    assert_no_duplicates(&paths);

    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        vec![
            PathBuf::from("a"),
            PathBuf::from("a/b"),
            PathBuf::from("a/b/c"),
            PathBuf::from("a/b/c/f2"),
            PathBuf::from("a/b/f1"),
        ],
        "each entry of the shared subtree appears exactly once"
    );
}

/// `-R a/b a/b/c/f2`: a descendant *file* operand under an already-walked
/// directory operand is also fully redundant and must not re-list its implied
/// parents or itself.
#[test]
fn ancestor_dir_then_descendant_file_dedups() {
    let (operands, to, _temp) = overlapping_tree(&["a/b", "a/b/c/f2"]);
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .relative_paths(true)
        .collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("overlapping relative copy succeeds");

    assert!(to.join("a/b/f1").is_file());
    assert!(to.join("a/b/c/f2").is_file());

    let paths = materialised_paths(report.records());
    assert_no_duplicates(&paths);
    assert!(
        paths
            .iter()
            .filter(|p| p.as_path() == std::path::Path::new("a/b/c/f2"))
            .count()
            == 1,
        "the overlapping leaf file is itemized once, not once per operand"
    );
}

/// A non-overlapping `-R` pair that merely shares an implied ancestor
/// (`a/x/f1` and `a/y/f2`) must be UNAFFECTED by the dedup: both distinct
/// subtrees are emitted, and the shared parent `a` still appears once (as
/// upstream's `send_implied_dirs` lastpath cache already guarantees).
#[test]
fn shared_ancestor_non_overlapping_pair_is_untouched() {
    let temp = tempdir().expect("tempdir");
    let from = temp.path().join("from");
    let to = temp.path().join("to");
    fs::create_dir_all(from.join("a/x")).expect("mk x");
    fs::create_dir_all(from.join("a/y")).expect("mk y");
    fs::create_dir_all(&to).expect("mk to");
    fs::write(from.join("a/x/f1"), b"1").expect("f1");
    fs::write(from.join("a/y/f2"), b"2").expect("f2");

    let mut op1 = from.clone();
    op1.push(".");
    op1.push("a");
    op1.push("x");
    op1.push("f1");
    let mut op2 = from.clone();
    op2.push(".");
    op2.push("a");
    op2.push("y");
    op2.push("f2");
    let operands = vec![
        op1.into_os_string(),
        op2.into_os_string(),
        to.clone().into_os_string(),
    ];

    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .relative_paths(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert!(to.join("a/x/f1").is_file());
    assert!(to.join("a/y/f2").is_file());

    let paths = materialised_paths(report.records());
    assert_no_duplicates(&paths);
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        vec![
            PathBuf::from("a"),
            PathBuf::from("a/x"),
            PathBuf::from("a/x/f1"),
            PathBuf::from("a/y"),
            PathBuf::from("a/y/f2"),
        ],
        "distinct subtrees kept; the shared implied ancestor `a` appears once"
    );
}

/// Regression for upstream `testsuite/relative.test`: a later `--relative`
/// operand whose SOURCE lives in a different directory but whose relative root
/// descends from an earlier recursively-walked operand must STILL transfer.
///
/// Upstream runs `rsync -aiR down/3/deep extra/./down/3/deep/extra.added.value`:
/// the second source (`extra/.../extra.added.value`) is not inside the first
/// operand's walked directory, so `flist_sort_and_clean()` keeps it (no earlier
/// flist entry produced that name). Deduping on the destination-relative root
/// would wrongly skip it, dropping the file. Keying the covering set on the
/// source path fixes that while preserving the same-source overlap dedup above.
#[test]
fn distinct_source_sharing_relative_suffix_is_transferred() {
    let temp = tempdir().expect("tempdir");
    let from = temp.path().join("from");
    let extra = temp.path().join("extra");
    let to = temp.path().join("to");
    fs::create_dir_all(from.join("down/3/deep")).expect("mk from tree");
    fs::create_dir_all(extra.join("down/3/deep")).expect("mk extra tree");
    fs::create_dir_all(&to).expect("mk to");
    fs::write(from.join("down/3/deep/text"), b"body").expect("write text");
    fs::write(extra.join("down/3/deep/extra.added.value"), b"wowza\n").expect("write extra");

    // Operand 1: the recursively-walked directory `down/3/deep` from `from`.
    let mut op_dir = from.clone();
    op_dir.push(".");
    for comp in ["down", "3", "deep"] {
        op_dir.push(comp);
    }
    // Operand 2: a single file from `extra`, relative-rooted at the SAME
    // `down/3/deep/...` suffix but living under a different source directory.
    let mut op_file = extra.clone();
    op_file.push(".");
    for comp in ["down", "3", "deep", "extra.added.value"] {
        op_file.push(comp);
    }
    let operands = vec![
        op_dir.into_os_string(),
        op_file.into_os_string(),
        to.clone().into_os_string(),
    ];

    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .relative_paths(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // The distinct file from `extra` must land at its relative destination.
    assert!(
        to.join("down/3/deep/extra.added.value").is_file(),
        "the distinct-source file sharing the relative suffix must transfer"
    );
    assert!(to.join("down/3/deep/text").is_file());

    let paths = materialised_paths(report.records());
    assert_no_duplicates(&paths);
    assert!(
        paths
            .iter()
            .any(|p| p.as_path() == std::path::Path::new("down/3/deep/extra.added.value")),
        "extra.added.value is itemized as a materialised entry"
    );
}
