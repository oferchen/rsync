//! Regression coverage: a LOCAL multi-source copy visits its operands in
//! upstream `f_name_cmp` order, not command-line order, so the itemize /
//! `--list-only` / `-v` stdout stream matches a real upstream 3.4.4 transfer.
//!
//! # Why this matters (observable stdout-fidelity contract)
//!
//! Upstream `send_file_list()` (flist.c:2227) accumulates every source operand
//! into ONE file list and sorts it globally with `f_name_cmp`
//! (`flist_sort_and_clean`, flist.c:2544) before the generator itemizes it, so
//! top-level operands are emitted in name order regardless of the order they
//! were given on the command line. oc's local-copy executor walks each
//! operand's subtree already in `f_name_cmp` order but, before this fix, visited
//! the operands themselves in argument order - so `oc-rsync -ai f_mango
//! f_cherry f_apple f_banana dst/` itemized `mango, cherry, apple, banana`
//! where upstream prints `apple, banana, cherry, mango`. The files landed
//! correctly either way (a copy is a copy); only the observable order diverged,
//! which the project's output-fidelity contract forbids.
//!
//! The expected orders below are the ones a real upstream rsync 3.4.4 prints
//! for the identical command (verified against the reference binary). These
//! tests FAIL before the operand-sort fix (they would see argument order).
//!
//! Scope (matches the fix): the reorder applies to every NAMED operand - both
//! the plain case (each operand contributes its destination basename) and the
//! `--relative` (`-R`) case (each operand keeps its full relative path), keyed
//! on the exact name upstream's global flist sort would give the operand's row.
//! Trailing-slash / copy-contents operands merge their CONTENTS into the
//! destination, which upstream interleaves across operands by content name -
//! unreproducible by operand ordering - so they are left in argument order and
//! covered separately (#212).

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use engine::local_copy::{
    LocalCopyAction, LocalCopyExecution, LocalCopyOptions, LocalCopyPlan, LocalCopyRecord,
};
use tempfile::{TempDir, tempdir};

/// Relative paths of the records that represent an emitted flist entry
/// (a created directory or a copied regular file), in stream order.
fn emitted_entry_order(records: &[LocalCopyRecord]) -> Vec<String> {
    records
        .iter()
        .filter(|record| {
            matches!(
                record.action(),
                LocalCopyAction::DataCopied | LocalCopyAction::DirectoryCreated
            )
        })
        .map(|record| record.relative_path().to_string_lossy().into_owned())
        .collect()
}

fn apply(operands: &[OsString], options: LocalCopyOptions) -> engine::local_copy::LocalCopyReport {
    let plan = LocalCopyPlan::from_operands(operands).expect("plan builds");
    plan.execute_with_report(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds")
}

/// Builds a `--relative` operand that reroots at `tail` via a `/./` marker, so
/// the recorded relative path is exactly `tail` (independent of the temp dir's
/// absolute prefix). Mirrors how `rsync -R ./tail` anchors the relative root.
fn dot_rerooted(root: &Path, tail: &str) -> OsString {
    let mut path = root.to_path_buf();
    path.push(".");
    path.push(tail);
    path.into_os_string()
}

/// Four single-file operands supplied in an order that is deliberately NOT
/// their sorted order. Upstream itemizes them sorted (`apple, banana, cherry,
/// mango`); before the fix oc emitted argument order (`mango, cherry, apple,
/// banana`).
#[test]
fn multi_file_operands_are_itemized_in_sorted_order() {
    let temp = tempdir().expect("tempdir");
    let dst = temp.path().join("dst");
    fs::create_dir_all(&dst).expect("create dst");

    // Command-line order: mango, cherry, apple, banana.
    let names = ["mango", "cherry", "apple", "banana"];
    let mut operands: Vec<OsString> = Vec::new();
    for name in names {
        let path = temp.path().join(name);
        fs::write(&path, name.as_bytes()).expect("write source file");
        operands.push(path.into_os_string());
    }
    operands.push(dst.clone().into_os_string());

    let report = apply(&operands, LocalCopyOptions::default().collect_events(true));

    assert_eq!(
        emitted_entry_order(report.records()),
        vec!["apple", "banana", "cherry", "mango"],
        "multi-file operands must itemize in f_name_cmp order (upstream 3.4.4), \
         not command-line order",
    );

    // Regression: every file still lands with correct content.
    for name in names {
        assert_eq!(
            fs::read(dst.join(name)).expect("dst file exists"),
            name.as_bytes(),
            "content for {name} must be copied intact",
        );
    }
}

/// Builds two NAMED directory operands whose sorted order is the reverse of the
/// command-line order, with contents chosen so a naive "sort by content" would
/// give a different answer than "sort the operands": `zdir` holds the
/// low-sorting file, `adir` holds the high-sorting file. Upstream orders the
/// operands (`adir` before `zdir`) and keeps each subtree contiguous.
fn two_named_dirs() -> (Vec<OsString>, PathBuf, TempDir) {
    let temp = tempdir().expect("tempdir");
    let dst = temp.path().join("dst");
    fs::create_dir_all(&dst).expect("create dst");
    for (dir, file) in [("zdir", "aaa"), ("adir", "zzz")] {
        let dpath = temp.path().join(dir);
        fs::create_dir_all(&dpath).expect("create source dir");
        fs::write(dpath.join(file), file.as_bytes()).expect("write file");
    }
    // Command-line order: zdir then adir.
    let operands = vec![
        temp.path().join("zdir").into_os_string(),
        temp.path().join("adir").into_os_string(),
        dst.clone().into_os_string(),
    ];
    (operands, dst, temp)
}

/// Named directory operands are ordered by `f_name_cmp`, and each operand's
/// subtree stays contiguous with it - exactly upstream's `adir/, adir/zzz,
/// zdir/, zdir/aaa`. Proves the reorder keys on the OPERAND name (not on the
/// contents): `adir` sorts first even though it holds the higher-sorting file.
#[test]
fn multi_dir_operands_order_by_operand_name_with_contiguous_subtrees() {
    let (operands, dst, _temp) = two_named_dirs();

    let report = apply(
        &operands,
        LocalCopyOptions::default()
            .recursive(true)
            .collect_events(true),
    );

    assert_eq!(
        emitted_entry_order(report.records()),
        vec!["adir", "adir/zzz", "zdir", "zdir/aaa"],
        "named directory operands must sort by operand name with each subtree \
         contiguous, matching upstream's global flist sort",
    );

    // Regression: both trees land intact.
    assert_eq!(fs::read(dst.join("adir/zzz")).unwrap(), b"zzz");
    assert_eq!(fs::read(dst.join("zdir/aaa")).unwrap(), b"aaa");
}

/// A single operand is never reordered (nothing to sort) and its own subtree
/// stays in `f_name_cmp` order - guarding against the sort touching the common
/// single-source path.
#[test]
fn single_directory_source_subtree_stays_sorted() {
    let temp = tempdir().expect("tempdir");
    let src = temp.path().join("src");
    let dst = temp.path().join("dst");
    fs::create_dir_all(&src).expect("create src");
    fs::create_dir_all(&dst).expect("create dst");
    for name in ["zebra", "mango", "apple", "delta"] {
        fs::write(src.join(name), name.as_bytes()).expect("write");
    }
    // Trailing slash: copy the directory's CONTENTS into dst.
    let operands = vec![
        format!("{}/", src.display()).into(),
        dst.clone().into_os_string(),
    ];

    let report = apply(
        &operands,
        LocalCopyOptions::default()
            .recursive(true)
            .collect_events(true),
    );

    assert_eq!(
        emitted_entry_order(report.records()),
        vec!["apple", "delta", "mango", "zebra"],
        "a single source's contents stay in f_name_cmp order",
    );
}

/// Under `--relative` (`-R`) the operand keeps its FULL relative path in the
/// flist, so upstream's global `f_name_cmp` sort orders the operands by that
/// whole path. `two_named_dirs` supplies `zdir` then `adir` (absolute operands,
/// so each records its path-minus-root); `adir` sorts before `zdir`, and each
/// subtree stays contiguous. This is the `-R` counterpart of the named-operand
/// sort above and pins that the fix now reorders `-R` too.
#[test]
fn relative_operands_are_ordered_by_full_relative_path() {
    let (operands, _dst, _temp) = two_named_dirs();

    let report = apply(
        &operands,
        LocalCopyOptions::default()
            .recursive(true)
            .relative_paths(true)
            .collect_events(true),
    );

    let order = emitted_entry_order(report.records());
    let adir = order.iter().position(|p| p.ends_with("adir"));
    let zdir = order.iter().position(|p| p.ends_with("zdir"));
    assert!(
        matches!((adir, zdir), (Some(a), Some(z)) if a < z),
        "under --relative the operands sort by full relative path (adir before \
         zdir), matching upstream's global flist sort. Order was {order:?}",
    );
}

/// `-R` multi-source operands with DISTINCT relative prefixes, supplied in
/// reverse-sorted command-line order (`b/x` before `a/y`), must itemize in
/// upstream's global `f_name_cmp` order with each implied-parent chain and leaf
/// contiguous. The `/./` marker reroots each operand so the recorded relative
/// paths are exactly `a/y`, `b/x`. Expected order is the one upstream rsync
/// 3.4.4 prints for `rsync -aiR ./b/x ./a/y dst/` (verified against the
/// reference binary): `a, a/y, a/y/f3, b, b/x, b/x/f1`.
#[test]
fn relative_multi_source_distinct_prefixes_match_upstream_order() {
    let temp = tempdir().expect("tempdir");
    let dst = temp.path().join("dst");
    fs::create_dir_all(&dst).expect("create dst");
    for (dir, file) in [("b/x", "f1"), ("a/y", "f3")] {
        let dpath = temp.path().join(dir);
        fs::create_dir_all(&dpath).expect("create source dir");
        fs::write(dpath.join(file), file.as_bytes()).expect("write file");
    }
    // Command-line order b/x then a/y; `/./` reroots to the bare relative path.
    let operands = vec![
        dot_rerooted(temp.path(), "b/x"),
        dot_rerooted(temp.path(), "a/y"),
        dst.clone().into_os_string(),
    ];

    let report = apply(
        &operands,
        LocalCopyOptions::default()
            .recursive(true)
            .relative_paths(true)
            .collect_events(true),
    );

    assert_eq!(
        emitted_entry_order(report.records()),
        vec!["a", "a/y", "a/y/f3", "b", "b/x", "b/x/f1"],
        "-R operands with distinct prefixes must itemize in upstream's global \
         f_name_cmp order, not command-line order",
    );
}

/// `-R` multi-source operands SHARING a prefix, supplied reversed (`b/x` before
/// `b/w`), must still sort by the full relative path so `b/w` precedes `b/x`
/// under the shared `b/` parent - matching `rsync -aiR ./b/x ./b/w dst/`
/// (upstream 3.4.4): `b, b/w, b/w/f2, b/x, b/x/f1`.
#[test]
fn relative_multi_source_shared_prefix_match_upstream_order() {
    let temp = tempdir().expect("tempdir");
    let dst = temp.path().join("dst");
    fs::create_dir_all(&dst).expect("create dst");
    for (dir, file) in [("b/x", "f1"), ("b/w", "f2")] {
        let dpath = temp.path().join(dir);
        fs::create_dir_all(&dpath).expect("create source dir");
        fs::write(dpath.join(file), file.as_bytes()).expect("write file");
    }
    let operands = vec![
        dot_rerooted(temp.path(), "b/x"),
        dot_rerooted(temp.path(), "b/w"),
        dst.clone().into_os_string(),
    ];

    let report = apply(
        &operands,
        LocalCopyOptions::default()
            .recursive(true)
            .relative_paths(true)
            .collect_events(true),
    );

    assert_eq!(
        emitted_entry_order(report.records()),
        vec!["b", "b/w", "b/w/f2", "b/x", "b/x/f1"],
        "-R operands sharing a prefix must sort by the full relative path \
         (b/w before b/x), matching upstream's global flist sort",
    );
}

/// The operand sort is a pure output-ordering change: reordering the operands
/// must never change WHICH files land on disk. A copy of the same two dirs in
/// either command-line order produces byte-identical destinations.
#[test]
fn reordering_does_not_change_destination_contents() {
    let (operands, dst, _temp) = two_named_dirs();
    apply(&operands, LocalCopyOptions::default().recursive(true));

    fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).expect("read_dir") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let rel = path.strip_prefix(root).unwrap().to_path_buf();
                    out.push((rel, fs::read(&path).expect("read")));
                }
            }
        }
        out.sort();
        out
    }

    assert_eq!(
        snapshot(&dst),
        vec![
            (PathBuf::from("adir/zzz"), b"zzz".to_vec()),
            (PathBuf::from("zdir/aaa"), b"aaa".to_vec()),
        ],
        "destination contents are independent of operand visit order",
    );
}

// ---------------------------------------------------------------------------
// Known divergences NOT addressed by the operand sort, pinned as ignored specs
// so they stay tracked rather than silently masked. Each encodes upstream's
// exact behaviour and is ready to un-ignore when its dedicated follow-up lands.
// ---------------------------------------------------------------------------

/// KNOWN DIVERGENCE (tracked, ignored): copy-contents (trailing-slash) sources
/// merging into one destination. Upstream flattens every operand's CONTENTS
/// into one flist and sorts globally, interleaving files across operands by
/// content name (`aaa, mmm, zzz` below); oc processes each operand's contents
/// contiguously (`aaa, zzz, mmm`). Ordering the operands cannot reproduce a
/// cross-operand interleave, so this needs a local-copy flist-collect refactor
/// and is deliberately out of scope for the named-operand sort. Un-ignore when
/// that refactor lands.
#[test]
#[ignore = "known divergence: copy-contents multi-source content interleave \
            needs a local-copy flist-collect refactor (separate follow-up)"]
fn copy_contents_multi_source_interleave_matches_upstream() {
    let temp = tempdir().expect("tempdir");
    let dst = temp.path().join("dst");
    fs::create_dir_all(&dst).expect("create dst");
    fs::create_dir_all(temp.path().join("asrc")).unwrap();
    fs::create_dir_all(temp.path().join("zsrc")).unwrap();
    for f in ["aaa", "zzz"] {
        fs::write(temp.path().join("asrc").join(f), f.as_bytes()).unwrap();
    }
    fs::write(temp.path().join("zsrc").join("mmm"), b"mmm").unwrap();

    // Both sources are copy-contents (trailing slash), given asrc then zsrc.
    let operands = vec![
        format!("{}/", temp.path().join("asrc").display()).into(),
        format!("{}/", temp.path().join("zsrc").display()).into(),
        dst.clone().into_os_string(),
    ];

    let report = apply(
        &operands,
        LocalCopyOptions::default()
            .recursive(true)
            .collect_events(true),
    );

    // Upstream 3.4.4 interleaves by content name across the two operands.
    assert_eq!(
        emitted_entry_order(report.records()),
        vec!["aaa", "mmm", "zzz"],
        "copy-contents sources must interleave globally by content name",
    );
}

// KNOWN DIVERGENCE (tracked, not tested here): multi-source `--delete` timing.
// With copy-contents directory sources under `--delete`, upstream removes an
// extraneous destination entry BEFORE it transfers the sources (the `*deleting`
// row precedes the `>f` rows), whereas oc runs its delete pass at the end so the
// deletion trails the transfers. The operand sort deliberately does not move the
// delete pass (the deletion keep-set is order-independent, so results are
// unchanged). This timing is a separate investigate-and-fix follow-up; it is
// noted here rather than tested because it only arises with copy-contents
// sources, which are themselves out of scope above.
