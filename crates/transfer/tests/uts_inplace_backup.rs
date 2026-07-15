//! `--inplace --backup` must COPY the original aside, not rename it.
//!
//! # Background
//!
//! Normally `--backup` renames the existing destination to its backup name and
//! a fresh temp file becomes the new destination (a new inode). Under
//! `--inplace` the destination is rewritten in place - same inode, no
//! temp+rename - so a plain rename-to-backup would move the very file rsync is
//! about to overwrite. Upstream instead makes the backup a COPY of the
//! pre-transfer contents BEFORE the inplace rewrite, leaving the original inode
//! in place to be updated.
//!
//! Upstream condition (both from `generator.c`):
//!   `inplace && make_backups > 0 && fnamecmp_type == FNAMECMP_FNAME`
//! -> `copy_file(fname, backupptr, ...)` (`generator.c:1862` for the
//! whole-file/read-batch case, `generator.c:1898` for the delta case), then the
//! `INFO_GTE(BACKUP, 1)` "backed up X to Y" line at `generator.c:1990-1992`.
//!
//! # Why this matters
//!
//! Two invariants ride on this and both silently corrupt data if broken:
//!   1. The backup must hold the ORIGINAL pre-transfer bytes, not the rewritten
//!      ones. If oc renamed the already-overwritten dest, the "backup" would be
//!      a copy of the NEW content - the prior version is lost.
//!   2. The destination inode must be PRESERVED. `--inplace` exists precisely to
//!      update the same inode (hardlinks, mmaps, reflinks stay valid). A backup
//!      that renames the dest away would give the updated file a new inode,
//!      defeating `--inplace`.
//!
//! The control test asserts the opposite for plain `--backup` (no `--inplace`):
//! there the dest inode SHOULD change, because temp+rename is the correct
//! mechanism and the rename-to-backup path is what must stay in force.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use filetime::{FileTime, set_file_mtime};
use tempfile::TempDir;
use test_support::{OcRsyncCliRunner, require_binary};

/// Backdate `path` well before the freshly written source so rsync's
/// quick-check (matching size + mtime) never short-circuits the transfer.
fn backdate(path: &Path) {
    set_file_mtime(path, FileTime::from_unix_time(946_684_800, 0)).expect("backdate mtime");
}

/// Seed `dest` with `old` content and backdate it, forcing an overwrite (and
/// thus a backup) on the next transfer.
fn seed_dest(dest: &Path, old: &str) {
    fs::write(dest, old).expect("seed dest");
    backdate(dest);
}

/// `st_ino` of `path`, for the preserve-inode assertions.
fn inode(path: &Path) -> u64 {
    fs::metadata(path).expect("stat").ino()
}

/// Trailing-slash form so rsync copies a directory's contents.
fn slash(path: &Path) -> std::ffi::OsString {
    let mut s = path.as_os_str().to_os_string();
    s.push("/");
    s
}

/// Delta path (`--no-whole-file`): `--inplace --backup` copies the original to
/// `name~`, writes the new content into the SAME inode, and emits the upstream
/// backup line.
#[test]
fn inplace_backup_copies_original_and_preserves_dest_inode() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root: TempDir = tempfile::tempdir().expect("tempdir");
    let base = root.path();
    let from = base.join("from");
    let to = base.join("to");
    fs::create_dir_all(&from).expect("mkdir from");
    fs::create_dir_all(&to).expect("mkdir to");

    // Different sizes so the quick-check cannot skip even if mtimes collided.
    fs::write(from.join("name1"), "brand-new-inplace-content\n").expect("write source");
    let dest = to.join("name1");
    seed_dest(&dest, "old\n");
    let ino_before = inode(&dest);

    let out = OcRsyncCliRunner::new()
        .args([
            "-ai",
            "--info=backup",
            "--no-whole-file",
            "--inplace",
            "--backup",
        ])
        .arg(slash(&from))
        .arg(slash(&to))
        .run()
        .expect("run oc-rsync");
    out.assert_success();

    // (a) backup holds the ORIGINAL pre-transfer bytes.
    assert_eq!(
        fs::read_to_string(to.join("name1~")).unwrap(),
        "old\n",
        "backup must contain the original pre-transfer content, not the rewrite"
    );
    // (b) dest holds the NEW content.
    assert_eq!(
        fs::read_to_string(&dest).unwrap(),
        "brand-new-inplace-content\n",
        "destination must hold the new content"
    );
    // (c) dest inode is UNCHANGED - the whole point of --inplace.
    assert_eq!(
        inode(&dest),
        ino_before,
        "--inplace must preserve the destination inode (copy-backup, not rename)"
    );
    assert!(
        out.stdout_contains("backed up name1 to name1~"),
        "missing upstream inplace-backup message; stdout was:\n{}",
        out.stdout_str()
    );
}

/// Whole-file path (`--whole-file`): the copy-before-write branch in
/// `process_whole_file` must equally preserve the inode and back up the
/// original.
#[test]
fn inplace_whole_file_backup_preserves_dest_inode() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root: TempDir = tempfile::tempdir().expect("tempdir");
    let base = root.path();
    let from = base.join("from");
    let to = base.join("to");
    fs::create_dir_all(&from).expect("mkdir from");
    fs::create_dir_all(&to).expect("mkdir to");

    fs::write(from.join("name1"), "whole-file-new\n").expect("write source");
    let dest = to.join("name1");
    seed_dest(&dest, "whole-file-old-longer\n");
    let ino_before = inode(&dest);

    let out = OcRsyncCliRunner::new()
        .args([
            "-ai",
            "--info=backup",
            "--whole-file",
            "--inplace",
            "--backup",
        ])
        .arg(slash(&from))
        .arg(slash(&to))
        .run()
        .expect("run oc-rsync");
    out.assert_success();

    assert_eq!(
        fs::read_to_string(to.join("name1~")).unwrap(),
        "whole-file-old-longer\n",
        "whole-file inplace backup must hold the original content"
    );
    assert_eq!(
        fs::read_to_string(&dest).unwrap(),
        "whole-file-new\n",
        "destination must hold the new content"
    );
    assert_eq!(
        inode(&dest),
        ino_before,
        "--inplace --whole-file must preserve the destination inode"
    );
    assert!(
        out.stdout_contains("backed up name1 to name1~"),
        "missing inplace whole-file backup message; stdout was:\n{}",
        out.stdout_str()
    );
}

/// `--inplace --backup --backup-dir=DIR`: the pre-image copy lands under the
/// backup dir at the same relative path (no `~`), and the dest inode is still
/// preserved.
#[test]
fn inplace_backup_dir_relocates_copy_and_preserves_inode() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root: TempDir = tempfile::tempdir().expect("tempdir");
    let base = root.path();
    let from = base.join("from");
    let to = base.join("to");
    let bak = base.join("bak");
    fs::create_dir_all(&from).expect("mkdir from");
    fs::create_dir_all(&to).expect("mkdir to");

    fs::write(from.join("name1"), "dir-new-content\n").expect("write source");
    let dest = to.join("name1");
    seed_dest(&dest, "dir-old\n");
    let ino_before = inode(&dest);

    let mut backup_dir_arg = std::ffi::OsString::from("--backup-dir=");
    backup_dir_arg.push(&bak);
    let out = OcRsyncCliRunner::new()
        .args([
            std::ffi::OsString::from("-ai"),
            std::ffi::OsString::from("--info=backup"),
            std::ffi::OsString::from("--no-whole-file"),
            std::ffi::OsString::from("--inplace"),
            std::ffi::OsString::from("--backup"),
            backup_dir_arg,
        ])
        .arg(slash(&from))
        .arg(slash(&to))
        .run()
        .expect("run oc-rsync");
    out.assert_success();

    assert_eq!(
        fs::read_to_string(bak.join("name1")).unwrap(),
        "dir-old\n",
        "backup-dir copy must hold the original content at the relative path"
    );
    assert!(
        !to.join("name1~").exists(),
        "backup-dir mode must not also create an in-place ~ backup"
    );
    assert_eq!(fs::read_to_string(&dest).unwrap(), "dir-new-content\n");
    assert_eq!(
        inode(&dest),
        ino_before,
        "--inplace --backup-dir must preserve the destination inode"
    );
}

/// Control: plain `--backup` WITHOUT `--inplace` still renames via temp+rename,
/// so the destination gets a NEW inode. This pins the contrast - the copy-aside
/// behaviour is specific to `--inplace` and must not leak into the default path.
#[test]
fn backup_without_inplace_changes_dest_inode() {
    if !require_binary("oc-rsync") {
        return;
    }
    let root: TempDir = tempfile::tempdir().expect("tempdir");
    let base = root.path();
    let from = base.join("from");
    let to = base.join("to");
    fs::create_dir_all(&from).expect("mkdir from");
    fs::create_dir_all(&to).expect("mkdir to");

    fs::write(from.join("name1"), "replaced-content\n").expect("write source");
    let dest = to.join("name1");
    seed_dest(&dest, "original\n");
    let ino_before = inode(&dest);

    let out = OcRsyncCliRunner::new()
        .args(["-ai", "--info=backup", "--no-whole-file", "--backup"])
        .arg(slash(&from))
        .arg(slash(&to))
        .run()
        .expect("run oc-rsync");
    out.assert_success();

    // Backup holds the original, dest holds the new content - same end state.
    assert_eq!(
        fs::read_to_string(to.join("name1~")).unwrap(),
        "original\n",
        "non-inplace backup must still hold the original content"
    );
    assert_eq!(fs::read_to_string(&dest).unwrap(), "replaced-content\n");
    // But the inode changes: temp+rename installs a fresh inode over the dest.
    assert_ne!(
        inode(&dest),
        ino_before,
        "non-inplace --backup uses temp+rename, so the dest inode must change"
    );
}
