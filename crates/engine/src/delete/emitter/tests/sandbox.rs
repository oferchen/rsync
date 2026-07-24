//! SEC-1.q sandbox-anchored dispatch tests for the delete emitter.
//!
//! Each test exercises a real on-disk layout under a [`tempfile::TempDir`]
//! so the dirfd-anchored `*_at` trait methods route through the live
//! `unlinkat(2)` / SEC-1.s `recursive_unlinkat_via_sandbox_or_fallback`
//! helpers from `fast_io::dir_sandbox::at_syscalls`.

use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::Arc;

use fast_io::DirSandbox;

use super::super::super::{DeleteEntryKind, DeletePlanMap, DirTraversalCursor};
use super::super::{DeleteEmitter, RealDeleteFs};
use super::{entry, plan};

/// Open a sandbox at `root` so the emitter's `*_at` dispatch can anchor
/// per-plan `openat` calls against it.
fn sandbox_for(root: &std::path::Path) -> Arc<DirSandbox> {
    Arc::new(DirSandbox::open_root(root).expect("open sandbox root"))
}

#[test]
fn unlink_file_at_removes_regular_file_under_sandbox() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("d");
    std::fs::create_dir(&dir).expect("mkdir d");
    let file = dir.join("victim");
    std::fs::write(&file, b"x").expect("write victim");
    assert!(file.exists());

    let plans = DeletePlanMap::new();
    plans.insert(plan("d", vec![entry("victim", DeleteEntryKind::File)]));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter =
        DeleteEmitter::new(RealDeleteFs, plans, cursor).with_sandbox(sandbox_for(tmp.path()));
    emitter.emit_all().expect("drain succeeds");

    assert!(!file.exists(), "sandbox unlinkat removed the file");
    assert_eq!(emitter.stats().files, 1);
    assert!(emitter.sandbox().is_some());
}

#[test]
fn rmdir_at_removes_empty_directory_under_sandbox() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("d");
    std::fs::create_dir(&dir).expect("mkdir d");
    let inner = dir.join("empty-child");
    std::fs::create_dir(&inner).expect("mkdir empty-child");

    let plans = DeletePlanMap::new();
    plans.insert(plan("d", vec![entry("empty-child", DeleteEntryKind::Dir)]));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter =
        DeleteEmitter::new(RealDeleteFs, plans, cursor).with_sandbox(sandbox_for(tmp.path()));
    emitter.emit_all().expect("drain succeeds");

    assert!(!inner.exists(), "sandbox rmdir removed the directory");
    assert_eq!(emitter.stats().dirs, 1);
}

#[test]
fn unlink_symlink_at_removes_symlink_without_following_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("d");
    std::fs::create_dir(&dir).expect("mkdir d");
    let target = tmp.path().join("outside");
    std::fs::write(&target, b"do-not-touch").expect("write outside target");
    let link = dir.join("link");
    symlink(&target, &link).expect("symlink");

    let plans = DeletePlanMap::new();
    plans.insert(plan("d", vec![entry("link", DeleteEntryKind::Symlink)]));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter =
        DeleteEmitter::new(RealDeleteFs, plans, cursor).with_sandbox(sandbox_for(tmp.path()));
    emitter.emit_all().expect("drain succeeds");

    assert!(!link.exists(), "sandbox unlinkat removed the symlink");
    assert!(
        target.exists(),
        "symlink unlink must never follow the link to the outside target",
    );
    assert_eq!(emitter.stats().symlinks, 1);
}

#[test]
fn remove_dir_all_at_peels_nested_tree_without_following_symlinks() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("d");
    std::fs::create_dir(&dir).expect("mkdir d");
    let tree = dir.join("tree");
    std::fs::create_dir_all(tree.join("level-1/level-2")).expect("nest dirs");
    std::fs::write(tree.join("level-1/file"), b"x").expect("write file");
    let outside_target = tmp.path().join("outside-tree");
    std::fs::write(&outside_target, b"do-not-touch").expect("write outside");
    symlink(&outside_target, tree.join("level-1/escape-link")).expect("symlink");

    // No nested plan for "d/tree"; ENOTEMPTY fallback routes through
    // remove_dir_all_at -> SEC-1.s recursive helper.
    let plans = DeletePlanMap::new();
    plans.insert(plan("d", vec![entry("tree", DeleteEntryKind::Dir)]));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter =
        DeleteEmitter::new(RealDeleteFs, plans, cursor).with_sandbox(sandbox_for(tmp.path()));
    emitter.emit_all().expect("drain succeeds");

    assert!(!tree.exists(), "recursive peel removed the tree");
    assert!(
        outside_target.exists(),
        "recursive peel must never follow the escape symlink to the outside target",
    );
    assert_eq!(emitter.stats().dirs, 1);
}

#[test]
fn sandbox_off_falls_back_to_path_based_methods() {
    // No sandbox attached: dispatch must use the path-based methods so
    // every existing caller stays on the legacy fallback. This is the
    // baseline contract for SEC-1.q's "additive" trait extension.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("d");
    std::fs::create_dir(&dir).expect("mkdir d");
    let file = dir.join("victim");
    std::fs::write(&file, b"x").expect("write");

    let plans = DeletePlanMap::new();
    plans.insert(plan(
        dir.to_str().expect("path utf-8"),
        vec![entry("victim", DeleteEntryKind::File)],
    ));
    let mut cursor = DirTraversalCursor::new(dir.clone());
    cursor.observe_segment(dir.clone(), &[]);

    let mut emitter = DeleteEmitter::new(RealDeleteFs, plans, cursor);
    assert!(emitter.sandbox().is_none());
    emitter.emit_all().expect("drain succeeds");

    assert!(!file.exists(), "path-based fallback removed the file");
    assert_eq!(emitter.stats().files, 1);
}

#[test]
fn sandbox_dispatch_handles_device_and_special_leaves() {
    // Devices and FIFOs both route through unlinkat with UnlinkFlags::File;
    // the kinds are distinguished only for the stats counters and the
    // dispatch matrix in unit tests. Using a regular file as the on-disk
    // backing entry keeps the test portable across platforms that gate
    // mknod behind elevated capabilities.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("d");
    std::fs::create_dir(&dir).expect("mkdir d");
    let dev = dir.join("dev-entry");
    let fifo = dir.join("fifo-entry");
    std::fs::write(&dev, b"x").expect("write dev");
    std::fs::write(&fifo, b"y").expect("write fifo");

    let plans = DeletePlanMap::new();
    plans.insert(plan(
        "d",
        vec![
            entry("dev-entry", DeleteEntryKind::Device),
            entry("fifo-entry", DeleteEntryKind::Special),
        ],
    ));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter =
        DeleteEmitter::new(RealDeleteFs, plans, cursor).with_sandbox(sandbox_for(tmp.path()));
    emitter.emit_all().expect("drain succeeds");

    assert!(!dev.exists() && !fifo.exists());
    assert_eq!(emitter.stats().devices, 1);
    assert_eq!(emitter.stats().specials, 1);
}

#[test]
fn sandbox_with_missing_plan_directory_falls_back_to_path_dispatch() {
    // The plan references a directory that no longer exists on disk
    // (a concurrent removal raced the emitter). The sandbox dirfd open
    // fails; the dispatcher must still attempt the path-based methods
    // so the standard NotFound-vs-IO error policy applies uniformly.
    let tmp = tempfile::tempdir().expect("tempdir");

    let plans = DeletePlanMap::new();
    plans.insert(plan("absent", vec![entry("victim", DeleteEntryKind::File)]));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("absent"));
    cursor.observe_segment(PathBuf::from("absent"), &[]);

    let mut emitter =
        DeleteEmitter::new(RealDeleteFs, plans, cursor).with_sandbox(sandbox_for(tmp.path()));
    emitter
        .emit_all()
        .expect("drain returns ok under default policy");

    // The directory does not exist, so the path-based fallback's
    // remove_file returns NotFound which the policy records as a
    // vanished race rather than a fatal abort.
    assert_eq!(emitter.stats().files, 0);
    assert_ne!(emitter.io_error(), 0);
}

#[test]
fn peel_stepped_over_child_error_sets_io_error_exit_23() {
    // A directory whose recursive peel stepped over a genuine child error
    // (an EACCES the walker logged and skipped, upstream FERROR_XFER) must
    // set io_error and map to RERR_PARTIAL (23), even though the pass kept
    // going. The scripted residue models the walker's report: the root is
    // left non-empty because the un-removable child survived.
    use super::super::EMITTER_PARTIAL_EXIT_CODE;
    use super::ScriptedDeleteFs;
    use fast_io::UnlinkResidue;

    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join("d")).expect("mkdir d");

    // rmdir_at yields ENOTEMPTY (no nested plan), so the emitter peels via
    // remove_dir_all_at, which returns a residue reporting a stepped-over
    // child error plus a surviving (non-empty) root.
    let fs = ScriptedDeleteFs::new()
        .fail("locked", std::io::ErrorKind::DirectoryNotEmpty)
        .peel(
            "locked",
            UnlinkResidue {
                not_empty: true,
                had_errors: true,
            },
        );
    let plans = DeletePlanMap::new();
    plans.insert(plan("d", vec![entry("locked", DeleteEntryKind::Dir)]));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter = DeleteEmitter::new(fs, plans, cursor).with_sandbox(sandbox_for(tmp.path()));
    emitter.emit_all().expect("stepped-over error is non-fatal");

    assert_ne!(
        emitter.io_error(),
        0,
        "a genuine swallowed error must set io_error"
    );
    assert_eq!(emitter.exit_code(), EMITTER_PARTIAL_EXIT_CODE);
    // The surviving directory is not counted as deleted.
    assert_eq!(emitter.stats().dirs, 0);
}

#[test]
fn peel_non_empty_without_error_stays_exit_0() {
    // A directory left non-empty with NO stepped-over child error - the
    // legitimate case where content was moved to --backup, filtered, or
    // protected - is upstream DR_NOT_EMPTY: the notice prints but the run
    // stays exit 0. The residue reports not_empty without had_errors.
    use super::ScriptedDeleteFs;
    use fast_io::UnlinkResidue;

    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join("d")).expect("mkdir d");

    let fs = ScriptedDeleteFs::new()
        .fail("kept", std::io::ErrorKind::DirectoryNotEmpty)
        .peel(
            "kept",
            UnlinkResidue {
                not_empty: true,
                had_errors: false,
            },
        );
    let plans = DeletePlanMap::new();
    plans.insert(plan("d", vec![entry("kept", DeleteEntryKind::Dir)]));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter = DeleteEmitter::new(fs, plans, cursor).with_sandbox(sandbox_for(tmp.path()));
    emitter.emit_all().expect("non-empty dir is benign");

    assert_eq!(
        emitter.io_error(),
        0,
        "a backup-emptied / filtered non-empty dir must not set io_error"
    );
    assert_eq!(emitter.exit_code(), 0);
    assert_eq!(emitter.stats().dirs, 0);
}
