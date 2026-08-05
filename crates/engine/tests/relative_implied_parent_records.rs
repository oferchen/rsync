//! Regression coverage for `-R` implied parent directories in the local-copy
//! itemize / `--stats` stream.
//!
//! Upstream `flist.c::send_implied_dirs()` injects one flagged directory entry
//! per ancestor of a `--relative` operand into the shared flist, *before* the
//! operand's own entry. The receiver then itemizes each as `cd+++++++++ <dir>/`
//! and counts it under `stats.num_dirs` / `stats.created_dirs`. A local copy
//! must surface the same ancestor rows and tallies; before this fix oc's
//! local-copy executor materialised the directories on disk but never recorded
//! them, so `-R a/b/c.txt` reported only the file.
//!
//! `--no-implied-dirs` clears `implied_dirs`, which suppresses the receiver's
//! attribute application (hence the itemize row and the created-dir bump) but,
//! at protocol >= 30 (which a local transfer always is), still ships the
//! ancestors in the flist - so they keep counting toward "Number of files".
//!
//! # Upstream Reference
//!
//! - `flist.c:1937 send_implied_dirs()` - one `FLAG_IMPLIED_DIR` entry per ancestor.
//! - `flist.c:2258` - `protocol_version >= 30` forces `implied_dirs = 1` for the flist.
//! - `main.c:387-411 output_itemized_counts()` - the `dir:` breakdown.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use engine::local_copy::{
    LocalCopyAction, LocalCopyExecution, LocalCopyOptions, LocalCopyPlan, LocalCopyRecord,
};
use tempfile::tempdir;

/// Builds `<from>/./a/b/c.txt` (a `/./`-anchored `-R` operand rooted at
/// `<from>`) and returns `(operands, from, to)`. The dot anchor sits after
/// `from`, so the relative chain is `a/b/c.txt` and the implied ancestors are
/// `a` and `a/b` - with no synthetic `.` transfer-root entry.
fn relative_tree() -> (Vec<OsString>, PathBuf, PathBuf, tempfile::TempDir) {
    let temp = tempdir().expect("tempdir");
    let from = temp.path().join("from");
    let to = temp.path().join("to");
    let leaf_dir = from.join("a").join("b");
    fs::create_dir_all(&leaf_dir).expect("create source tree");
    fs::create_dir_all(&to).expect("create destination root");
    fs::write(leaf_dir.join("c.txt"), b"payload").expect("write leaf file");

    let mut operand = from.clone();
    operand.push(".");
    operand.push("a");
    operand.push("b");
    operand.push("c.txt");

    let operands = vec![operand.into_os_string(), to.clone().into_os_string()];
    (operands, from, to, temp)
}

fn is_created_dir(record: &LocalCopyRecord) -> bool {
    record.was_created() && matches!(record.action(), LocalCopyAction::DirectoryCreated)
}

/// `-R a/b/c.txt` surfaces the implied ancestors `a` and `a/b` as created
/// directory records, ordered ahead of the file, and counts both in the
/// directory tallies - matching `send_implied_dirs()` + upstream `--stats`.
#[test]
fn relative_implied_parents_are_itemized_and_counted() {
    let (operands, _from, to, _temp) = relative_tree();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .relative_paths(true)
        .collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("relative copy succeeds");

    // The physical tree is still correct.
    assert!(to.join("a").join("b").join("c.txt").is_file());

    let records = report.records();
    let dir_paths: Vec<PathBuf> = records
        .iter()
        .filter(|r| is_created_dir(r))
        .map(|r| r.relative_path().to_path_buf())
        .collect();
    assert_eq!(
        dir_paths,
        vec![PathBuf::from("a"), PathBuf::from("a/b")],
        "both implied ancestors must be itemized as created dirs, shallowest first"
    );

    // Ancestors precede the leaf file in the record stream (send_implied_dirs
    // runs before send_file_name).
    let first_file = records
        .iter()
        .position(|r| matches!(r.action(), LocalCopyAction::DataCopied))
        .expect("leaf file record present");
    let last_dir = records
        .iter()
        .rposition(is_created_dir)
        .expect("implied dir records present");
    assert!(
        last_dir < first_file,
        "implied parent rows must appear before the transferred file row"
    );

    // `--stats` counts both ancestors (created and total); the leaf is the only
    // regular file, and there is no synthetic `.` root for a mid-path dot.
    let summary = report.summary();
    assert_eq!(summary.directories_created(), 2, "created dirs: a, a/b");
    assert_eq!(summary.directories_total(), 2, "total dirs: a, a/b");
}

/// `--no-implied-dirs` drops the ancestor itemize rows and the created-dir
/// bump, but the ancestors still ship in the flist (protocol >= 30), so they
/// keep counting toward "Number of files" (`directories_total`).
#[test]
fn no_implied_dirs_suppresses_rows_but_still_counts_ancestors() {
    let (operands, _from, to, _temp) = relative_tree();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .relative_paths(true)
        .implied_dirs(false)
        .collect_events(true);

    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("relative copy succeeds");

    assert!(to.join("a").join("b").join("c.txt").is_file());

    let records = report.records();
    assert!(
        !records.iter().any(is_created_dir),
        "--no-implied-dirs must not itemize the implied ancestors"
    );

    let summary = report.summary();
    assert_eq!(
        summary.directories_created(),
        0,
        "--no-implied-dirs applies no source attrs, so no created-dir bump"
    );
    assert_eq!(
        summary.directories_total(),
        2,
        "ancestors still ship in the flist at protocol >= 30, so they count"
    );
}
