//! Backup-before-delete on the network receiver's delete pass.
//!
//! WHY this matters: over SSH/daemon the receiver drives `--delete` locally.
//! Upstream backs up each extraneous FILE before unlinking it whenever
//! `--backup`/`--backup-dir` is set, rather than losing the data outright
//! (`delete.c:165-174`). These tests pin that behaviour at every file-victim
//! removal site the receiver uses: the immediate parallel pass
//! (`delete_extraneous_files`), the deferred `--delete-delay` executor
//! (`execute_delayed_deletions`), and the capped serial executor
//! (`--max-delete`). Directories are never backed up here, matching upstream.
//!
//! Deterministic-order note: sibling `~` backups are created mid-pass, so the
//! default-suffix cases are exercised on the two paths that iterate a fixed
//! snapshot (the delayed victim list and the capped candidate list), never the
//! streaming `read_dir` fallback. The `--backup-dir` case writes outside the
//! destination and so is safe on the parallel path.

use std::ffi::OsString;

use protocol::flist::FileEntry;

use super::super::super::ReceiverContext;
use super::super::support::{TestDeletionWriter, test_config, test_handshake};

/// Builds a receiver whose file list keeps `keep.txt` (so any other
/// destination entry is an extraneous deletion candidate), with the supplied
/// backup settings applied. `late_delete` selects the deferred `--delete-delay`
/// scheduling; `max_delete` routes the immediate pass through the capped serial
/// executor.
fn build_receiver(
    dest: &std::path::Path,
    backup: bool,
    backup_dir: Option<&str>,
    late_delete: bool,
    max_delete: Option<u64>,
) -> ReceiverContext {
    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.delete = true;
    config.flags.backup = backup;
    config.deletion.delete_after = false;
    config.deletion.late_delete = late_delete;
    config.deletion.max_delete = max_delete;
    config.backup_dir = backup_dir.map(str::to_owned);
    config.args = vec![OsString::from(dest.to_str().unwrap())];

    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list
        .push(FileEntry::new_file("keep.txt".into(), 6, 0o644));
    ctx
}

/// Default `~` suffix, exercised on the deferred `--delete-delay` executor
/// (a fixed victim list, so the sibling backup is never re-scanned). The
/// extraneous file must land at `<name>~` and its original be gone; the listed
/// file must survive.
#[test]
fn delayed_delete_backs_up_victim_with_default_suffix() {
    let dir = tempfile::TempDir::new().unwrap();
    let dest = dir.path();
    std::fs::write(dest.join("stale.txt"), b"extraneous").unwrap();
    std::fs::write(dest.join("keep.txt"), b"listed").unwrap();

    let ctx = build_receiver(dest, true, None, true, None);
    let mut writer = TestDeletionWriter;

    let (victims, _io) = ctx
        .collect_delayed_deletions(dest, None, &mut writer)
        .unwrap();
    let (stats, _io) = ctx
        .execute_delayed_deletions(dest, None, &victims, &mut writer)
        .unwrap();

    assert!(
        !dest.join("stale.txt").exists(),
        "the extraneous original must be gone after the delete pass"
    );
    assert!(
        dest.join("stale.txt~").exists(),
        "the victim must be preserved at its default `~` backup path"
    );
    assert_eq!(
        std::fs::read(dest.join("stale.txt~")).unwrap(),
        b"extraneous",
        "the backup must carry the victim's original bytes"
    );
    assert_eq!(stats.files, 1, "exactly one extraneous file deleted");
    assert!(dest.join("keep.txt").exists(), "listed file must survive");
}

/// `--backup-dir` to an external directory, exercised on the immediate parallel
/// pass. The victim must be relocated under the backup directory (suffix is
/// empty when `--backup-dir` is set) with its original removed.
#[test]
fn immediate_delete_backs_up_victim_into_backup_dir() {
    let dir = tempfile::TempDir::new().unwrap();
    let dest = dir.path();
    let backup_root = tempfile::TempDir::new().unwrap();
    std::fs::write(dest.join("stale.txt"), b"extraneous").unwrap();
    std::fs::write(dest.join("keep.txt"), b"listed").unwrap();

    let ctx = build_receiver(
        dest,
        true,
        Some(backup_root.path().to_str().unwrap()),
        false,
        None,
    );
    let mut writer = TestDeletionWriter;

    let (stats, _limit, _io) = ctx
        .delete_extraneous_files(dest, None, &mut writer)
        .unwrap();

    assert!(
        !dest.join("stale.txt").exists(),
        "the extraneous original must be gone after the delete pass"
    );
    assert!(
        backup_root.path().join("stale.txt").exists(),
        "the victim must be relocated into the --backup-dir"
    );
    assert!(
        !dest.join("stale.txt~").exists(),
        "with --backup-dir the suffix is empty; no sibling `~` file is made"
    );
    assert_eq!(stats.files, 1, "exactly one extraneous file deleted");
    assert!(dest.join("keep.txt").exists(), "listed file must survive");
}

/// A victim already named like a backup (`foo~`) with NO `--backup-dir` must be
/// unlinked directly, never re-backed-up to `foo~~`
/// (upstream `is_backup_file` guard, `delete.c:165`).
#[test]
fn already_suffixed_victim_is_not_rebacked_up() {
    let dir = tempfile::TempDir::new().unwrap();
    let dest = dir.path();
    std::fs::write(dest.join("foo~"), b"old backup").unwrap();
    std::fs::write(dest.join("keep.txt"), b"listed").unwrap();

    let ctx = build_receiver(dest, true, None, false, None);
    let mut writer = TestDeletionWriter;

    let (stats, _limit, _io) = ctx
        .delete_extraneous_files(dest, None, &mut writer)
        .unwrap();

    assert!(
        !dest.join("foo~").exists(),
        "an already-suffixed victim must be unlinked directly"
    );
    assert!(
        !dest.join("foo~~").exists(),
        "it must NOT be re-backed-up to `foo~~` (is_backup_file guard)"
    );
    assert_eq!(stats.files, 1, "the victim is still counted as deleted");
    assert!(dest.join("keep.txt").exists(), "listed file must survive");
}

/// With `--backup` disabled, an extraneous file is unlinked outright and no
/// backup is ever created.
#[test]
fn delete_without_backup_leaves_no_backup() {
    let dir = tempfile::TempDir::new().unwrap();
    let dest = dir.path();
    std::fs::write(dest.join("stale.txt"), b"extraneous").unwrap();
    std::fs::write(dest.join("keep.txt"), b"listed").unwrap();

    let ctx = build_receiver(dest, false, None, false, None);
    let mut writer = TestDeletionWriter;

    let (stats, _limit, _io) = ctx
        .delete_extraneous_files(dest, None, &mut writer)
        .unwrap();

    assert!(
        !dest.join("stale.txt").exists(),
        "the extraneous file is still deleted with backup off"
    );
    assert!(
        !dest.join("stale.txt~").exists(),
        "no backup must be created when --backup is off"
    );
    assert_eq!(stats.files, 1, "exactly one extraneous file deleted");
    assert!(dest.join("keep.txt").exists(), "listed file must survive");
}

/// The capped serial executor (`--max-delete`) must also back up file victims
/// before unlinking them. Its candidate list is a fixed snapshot, so the
/// default-suffix sibling backup is safe here too.
#[test]
fn capped_delete_backs_up_victim_with_default_suffix() {
    let dir = tempfile::TempDir::new().unwrap();
    let dest = dir.path();
    std::fs::write(dest.join("stale.txt"), b"extraneous").unwrap();
    std::fs::write(dest.join("keep.txt"), b"listed").unwrap();

    let ctx = build_receiver(dest, true, None, false, Some(100));
    let mut writer = TestDeletionWriter;

    let (stats, _limit, _io) = ctx
        .delete_extraneous_files(dest, None, &mut writer)
        .unwrap();

    assert!(
        !dest.join("stale.txt").exists(),
        "the capped executor must remove the extraneous original"
    );
    assert!(
        dest.join("stale.txt~").exists(),
        "the capped executor must preserve the victim at `<name>~`"
    );
    assert_eq!(stats.files, 1, "exactly one extraneous file deleted");
    assert!(dest.join("keep.txt").exists(), "listed file must survive");
}
