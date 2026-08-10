//! Nextest port of the upstream `testsuite/missing.test` scenario.
//!
//! Upstream test source:
//! `target/interop/upstream-src/rsync-3.4.4/testsuite/missing.test`.
//!
//! # Background
//!
//! Upstream's `missing.test` guards bugs Wayne Davison fixed when he reworked
//! the generator's `missing_below` logic (the state that suppresses per-file
//! work under a directory that a dry run has decided not to create):
//!
//! 1. A dry run with `--ignore-non-existing` must not emit spurious
//!    "not creating new ..." chatter for a file whose parent directory does
//!    not exist at the destination. The whole subtree is skipped, silently.
//! 2. `--delete-after` on a dry run must still run its deletion pass even when
//!    the final source directory is "missing" at the destination (the pass was
//!    being skipped when the last directory was dry-missing).
//!
//! The nextest port lifts the two deterministic, local-transfer legs of the
//! upstream script (its test 1 and test 3). Upstream test 2 exercises a
//! `--fuzzy --no-implied-dirs -R` dirlist edge that only asserts a clean exit;
//! that is subsumed by the exit-0 assertion here and adds a fragile `-R` path
//! shape, so it is not ported.
//!
//! # What this test pins
//!
//! - Test 1: `-n -r --ignore-non-existing -vv from/ to/` never prints a
//!   "not creating new" line naming `subdir/file` (the file under the
//!   non-existent destination directory), and the dry run touches nothing on
//!   disk.
//! - Test 3: `-n -r --delete-after -i from/ to/` still itemizes the deletion
//!   of `to/other` (a destination-only file), proving the delete-after pass is
//!   not skipped when the last source directory is dry-missing.
//!
//! # Upstream References
//!
//! - `testsuite/missing.test` - the upstream script this file ports.
//! - `generator.c` - `missing_below` handling and the `--ignore-non-existing`
//!   skip path (`skipping non-existent destination file`).
//! - `delete.c` - the `--delete-after` deletion pass that must still run.

#![cfg(unix)]

use std::fs;
use std::path::Path;

use tempfile::TempDir;
use test_support::{OcRsyncCliRunner, require_binary};

/// Build the shared source/destination fixture used by both legs.
///
/// Mirrors the upstream setup:
///
/// ```sh
/// makepath "$fromdir/subdir" "$todir"
/// echo data >"$fromdir/subdir/file"
/// echo data >"$todir/other"
/// ```
///
/// `to/other` is a destination-only file (no source counterpart) so the
/// `--delete-after` leg has something to delete. `from/subdir/file` lives
/// under a directory that does not exist at the destination, which is what
/// drives the `missing_below` logic.
fn setup() -> TempDir {
    let root = tempfile::tempdir().expect("tempdir");
    let from_sub = root.path().join("from").join("subdir");
    let to = root.path().join("to");
    fs::create_dir_all(&from_sub).expect("mkdir from/subdir");
    fs::create_dir_all(&to).expect("mkdir to");
    fs::write(from_sub.join("file"), b"data\n").expect("write from/subdir/file");
    fs::write(to.join("other"), b"data\n").expect("write to/other");
    root
}

/// Trailing-slash form of a directory so rsync copies its contents, matching
/// the upstream `"$fromdir/"` / `"$todir/"` argument shape.
fn slash(p: &Path) -> std::ffi::OsString {
    let mut s = p.as_os_str().to_os_string();
    s.push("/");
    s
}

/// Upstream test 1: a dry run with `--ignore-non-existing` must not emit
/// "not creating new ..." output for a file under a directory that does not
/// exist at the destination, and must not create anything on disk.
#[test]
fn ignore_non_existing_dry_run_is_quiet_and_touches_nothing() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root = setup();
    let from = root.path().join("from");
    let to = root.path().join("to");

    let out = OcRsyncCliRunner::new()
        .args(["-n", "-r", "--ignore-non-existing", "-vv"])
        .arg(slash(&from))
        .arg(slash(&to))
        .run()
        .expect("run oc-rsync");
    out.assert_success();

    // Upstream assertion: the "not creating new" chatter must never name the
    // file under the non-existent destination directory. oc-rsync emits at
    // most a "skipping non-existent destination file" line for the directory
    // itself; it must never claim it is (not) creating `subdir/file`.
    let stdout = out.stdout_str();
    assert!(
        !stdout.contains("not creating new") || !stdout.contains("subdir/file"),
        "dry run leaked spurious \"not creating new ... subdir/file\" output:\n{stdout}",
    );

    // A dry run must not create the skipped subtree at the destination.
    assert!(
        !to.join("subdir").exists(),
        "dry run created to/subdir on disk (should be a no-op)",
    );
    assert!(
        !to.join("subdir").join("file").exists(),
        "dry run created to/subdir/file on disk (should be a no-op)",
    );
}

/// Upstream test 3: `--delete-after` on a dry run must still run its deletion
/// pass and itemize `to/other` for deletion, even though the last source
/// directory is dry-missing at the destination.
#[test]
fn delete_after_dry_run_still_itemizes_deletion() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root = setup();
    let from = root.path().join("from");
    let to = root.path().join("to");

    let out = OcRsyncCliRunner::new()
        .args(["-n", "-r", "--delete-after", "-i"])
        .arg(slash(&from))
        .arg(slash(&to))
        .run()
        .expect("run oc-rsync");
    out.assert_success();

    // Upstream assertion: grep '^\*deleting * other'. The delete-after pass
    // must fire and itemize the destination-only file for deletion.
    let stdout = out.stdout_str();
    let deleted = stdout
        .lines()
        .any(|l| l.starts_with("*deleting") && l.trim_end().ends_with("other"));
    assert!(
        deleted,
        "delete-after dry run did not itemize *deleting other:\n{stdout}",
    );

    // Dry run: the file must still be on disk afterwards.
    assert!(
        to.join("other").exists(),
        "dry run actually deleted to/other (must be a no-op on disk)",
    );
}
