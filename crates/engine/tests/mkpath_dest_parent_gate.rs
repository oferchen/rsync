//! Regression coverage for the `--mkpath` gate on missing destination parents.
//!
//! WHY: Upstream rsync gates exactly ONE thing on `--mkpath` - creating the
//! missing path of the DESTINATION ARGUMENT itself. See `main.c:735-749`:
//! `if (mkpath_dest_arg && statret < 0 && (cp || file_total > 1))
//! make_path(dest_arg, ...)`. So a plain file-to-file copy into a missing deep
//! destination parent chain must FAIL and create nothing unless `--mkpath` is
//! given.
//!
//! Creating IN-TREE parents during the transfer (parents of transferred files,
//! strictly under an already-existing destination root) is separate and
//! unconditional for recursive/relative transfers - it is just tree
//! reconstruction, not "mkpath". A normal `-a src/ dst/` recursive copy and
//! `-R` implied dirs both rely on it and must keep working without `--mkpath`.
//!
//! # Upstream Reference
//!
//! - `main.c:735-749` - `mkpath_dest_arg && statret < 0` -> `make_path()` for
//!   the destination argument's own missing path, only under `--mkpath`.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;

use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use tempfile::tempdir;

/// A file-to-file copy into a MISSING deep destination parent chain must FAIL
/// and create nothing when `--mkpath` is absent, even though `implied_dirs`
/// defaults on. Mirrors upstream refusing to `make_path()` without
/// `mkpath_dest_arg` (`main.c:736`).
#[test]
fn file_to_file_missing_deep_parent_without_mkpath_fails_and_creates_nothing() {
    let temp = tempdir().expect("tempdir");
    let src = temp.path().join("src.txt");
    fs::write(&src, b"payload").expect("write source");

    // Destination sits under a parent chain that does not exist yet.
    let missing_root = temp.path().join("a");
    let dest = missing_root.join("b").join("c").join("dst.txt");

    let operands = vec![src.clone().into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Default options: implied_dirs on, mkpath off, relative off.
    let options = LocalCopyOptions::default();

    let result = plan.execute_with_options(LocalCopyExecution::Apply, options);

    assert!(
        result.is_err(),
        "copy into a missing deep parent without --mkpath must fail (upstream main.c:736)"
    );
    assert!(
        !missing_root.exists(),
        "no part of the missing parent chain may be created without --mkpath"
    );
    assert!(!dest.exists(), "destination file must not be created");
}

/// With `--mkpath`, the same copy must succeed and create the full chain.
/// Mirrors upstream `mkpath_dest_arg` -> `make_path()` (`main.c:738`).
#[test]
fn file_to_file_missing_deep_parent_with_mkpath_succeeds() {
    let temp = tempdir().expect("tempdir");
    let src = temp.path().join("src.txt");
    fs::write(&src, b"payload").expect("write source");

    let dest = temp.path().join("a").join("b").join("c").join("dst.txt");

    let operands = vec![src.clone().into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().mkpath(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy with --mkpath must succeed");

    assert!(dest.is_file(), "destination file must exist");
    assert_eq!(
        fs::read(&dest).expect("read dest"),
        b"payload",
        "copied contents must match"
    );
}

/// Guard against regression: `-R` (`--relative`) must still create the implied
/// intermediate directory chain WITHOUT `--mkpath`. These parents are strictly
/// under the existing destination root, so the `--mkpath` gate never applies to
/// them and `implied_dirs` (default on) creates them (`flist.c:2468`
/// send_implied_dirs).
#[test]
fn relative_still_creates_implied_dirs_without_mkpath() {
    let temp = tempdir().expect("tempdir");
    let from = temp.path().join("from");
    let to = temp.path().join("to");

    let deep = from.join("down").join("3").join("deep");
    fs::create_dir_all(&deep).expect("create source tree");
    fs::create_dir_all(&to).expect("create destination root");
    fs::write(deep.join("payload"), b"relative payload").expect("write payload");

    // `<from>/./down/3/deep` anchors the relative chain at `down/3/deep`.
    let mut operand = from.clone();
    operand.push(".");
    operand.push("down");
    operand.push("3");
    operand.push("deep");

    let operands: Vec<OsString> = vec![operand.into_os_string(), to.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Relative on, recursive on, mkpath OFF.
    let options = LocalCopyOptions::default()
        .recursive(true)
        .relative_paths(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("relative copy must succeed and create implied dirs");

    assert!(
        to.join("down")
            .join("3")
            .join("deep")
            .join("payload")
            .is_file(),
        "-R must create implied dirs and copy the payload without --mkpath"
    );
}

/// Guard against the regression the `--mkpath` gate must NOT cause: a normal
/// recursive `-a src/ dst/` copy (no `-R`, `relative` OFF) into an existing
/// destination root must still create the in-tree subdirectory parents for
/// nested files (`dst/a/b/` for `src/a/b/file`). These parents are strictly
/// under the existing `dst/`, so they are tree reconstruction, not "mkpath",
/// and must be created without `--mkpath`. Gating in-tree parents on relative
/// mode broke `itemize`/`delete`/`batch-mode` (all recursive `-a` transfers).
#[test]
fn recursive_creates_in_tree_parents_without_mkpath() {
    let temp = tempdir().expect("tempdir");
    let from = temp.path().join("from");
    let to = temp.path().join("to");

    // Source tree: from/a/b/file, from/a/c/other - two nested subdir levels.
    let deep_b = from.join("a").join("b");
    let deep_c = from.join("a").join("c");
    fs::create_dir_all(&deep_b).expect("create from/a/b");
    fs::create_dir_all(&deep_c).expect("create from/a/c");
    fs::write(deep_b.join("file"), b"one").expect("write file");
    fs::write(deep_c.join("other"), b"two").expect("write other");

    // Destination root exists; its in-tree subdirs do not.
    fs::create_dir_all(&to).expect("create destination root");

    // Contents copy: `from/` -> `to/` (trailing slash on source).
    let mut source = from.clone().into_os_string();
    source.push("/");
    let operands: Vec<OsString> = vec![source, to.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Recursive on, relative OFF, mkpath OFF - the common `-a src/ dst/` case.
    let options = LocalCopyOptions::default().recursive(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("recursive copy must recreate the in-tree subdir parents");

    assert!(
        to.join("a").join("b").join("file").is_file(),
        "recursive -a must create in-tree parent dst/a/b/ without --mkpath"
    );
    assert!(
        to.join("a").join("c").join("other").is_file(),
        "recursive -a must create in-tree parent dst/a/c/ without --mkpath"
    );
}
