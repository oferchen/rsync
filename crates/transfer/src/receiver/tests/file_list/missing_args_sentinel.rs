//! UTS-19.f: receiver-side mode-0 sentinel handler for
//! `--delete-missing-args`.
//!
//! Exercises [`crate::receiver::ReceiverContext::process_missing_args_sentinels`]
//! end-to-end against a real on-disk destination. The sender writes a mode-0
//! sentinel entry into the wire flist for a vanished top-level source; the
//! receiver must consume the entry, delete the named destination path, and
//! perform no further filesystem creation for that entry.
//!
//! # Upstream Reference
//!
//! - `generator.c:1348-1354` - `missing_args == 2 && file->mode == 0`
//!   branch that calls `delete_item()` when the destination exists and
//!   falls through (no creation) otherwise.

use std::ffi::OsString;
use std::io::Cursor;

use protocol::flist::{FileEntry, FileListWriter};
use tempfile::TempDir;

use super::super::super::ReceiverContext;
use super::super::support::{test_config, test_handshake};

/// Builds a wire-encoded file list containing the supplied entries.
fn encode_flist(entries: &[FileEntry]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let handshake = test_handshake();
    let mut writer = FileListWriter::new(handshake.protocol);
    for entry in entries {
        writer.write_entry(&mut bytes, entry).unwrap();
    }
    writer.write_end(&mut bytes, None).unwrap();
    bytes
}

/// Constructs a mode-0 sentinel entry whose name matches a top-level
/// destination path (`flist.c:2254-2258`, `make_file()` + `file->mode = 0`).
fn sentinel_entry(name: &str) -> FileEntry {
    let mut entry = FileEntry::new_file(name.into(), 0, 0);
    entry.set_mode(0);
    entry
}

/// Receiver consumes a mode-0 sentinel entry and deletes the named
/// destination file when `--delete-missing-args` is in effect.
///
/// Why this matters: without the receiver-side branch, the sentinel is
/// silently dropped (mode 0 is neither file/dir/symlink/special), so the
/// transfer exits cleanly but the destination retains the stale file -
/// observable as "missing-arg file was not deleted" in upstream interop.
#[test]
fn receiver_consumes_mode_zero_sentinel_and_deletes_destination() {
    let temp_dir = TempDir::new().unwrap();
    let dest = temp_dir.path();

    // Pre-existing destination file that the sentinel must remove.
    std::fs::write(dest.join("ghost.txt"), b"stale").unwrap();
    // Sibling file that is NOT a sentinel target - must survive.
    std::fs::write(dest.join("keep.txt"), b"keep me").unwrap();

    // Receive the wire flist: a root directory plus the mode-0 sentinel.
    let mut data = Vec::new();
    data.extend_from_slice(&encode_flist(&[
        FileEntry::new_directory(".".into(), 0o755),
        sentinel_entry("ghost.txt"),
    ]));

    let handshake = test_handshake();
    let mut config = test_config();
    config.file_selection.delete_missing_args = true;
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    let mut cursor = Cursor::new(&data[..]);
    let count = ctx.receive_file_list(&mut cursor).unwrap();
    assert_eq!(count, 2);

    // Drive the sentinel handler against the real destination.
    ctx.process_missing_args_sentinels(
        dest,
        #[cfg(unix)]
        None,
    )
    .unwrap();

    assert!(
        !dest.join("ghost.txt").exists(),
        "sentinel must delete the named destination file",
    );
    assert!(
        dest.join("keep.txt").exists(),
        "non-sentinel sibling must be preserved",
    );
}

/// When `--delete-missing-args` is off, sentinel entries (if any) must NOT
/// trigger deletion. Upstream guards the `delete_item()` call on
/// `missing_args == 2`; the receiver mirrors that guard via the config flag.
#[test]
fn receiver_ignores_sentinel_when_delete_missing_args_off() {
    let temp_dir = TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("ghost.txt"), b"stale").unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    // delete_missing_args left default (false).
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list.push(sentinel_entry("ghost.txt"));

    ctx.process_missing_args_sentinels(
        dest,
        #[cfg(unix)]
        None,
    )
    .unwrap();

    assert!(
        dest.join("ghost.txt").exists(),
        "sentinel must not trigger deletion without --delete-missing-args",
    );
}

/// Missing destination path is a no-op (mirrors upstream's `statret == 0`
/// guard at `generator.c:1351`).
#[test]
fn receiver_sentinel_for_missing_destination_is_noop() {
    let temp_dir = TempDir::new().unwrap();
    let dest = temp_dir.path();

    let handshake = test_handshake();
    let mut config = test_config();
    config.file_selection.delete_missing_args = true;
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list.push(sentinel_entry("never-existed.txt"));

    // Should not error even though the destination path does not exist.
    ctx.process_missing_args_sentinels(
        dest,
        #[cfg(unix)]
        None,
    )
    .unwrap();
}

/// `--dry-run` short-circuits all filesystem mutations (mirrors upstream's
/// receiver.c:693).
#[test]
fn receiver_sentinel_dry_run_skips_deletion() {
    let temp_dir = TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("ghost.txt"), b"stale").unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.file_selection.delete_missing_args = true;
    config.flags.dry_run = true;
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list.push(sentinel_entry("ghost.txt"));

    ctx.process_missing_args_sentinels(
        dest,
        #[cfg(unix)]
        None,
    )
    .unwrap();

    assert!(
        dest.join("ghost.txt").exists(),
        "dry_run must not perform any deletion",
    );
}

/// Mode-0 sentinel naming a directory removes it recursively, mirroring
/// upstream's `delete_item()` dispatch on the destination's `sx.st.st_mode`.
#[test]
fn receiver_sentinel_removes_directory_recursively() {
    let temp_dir = TempDir::new().unwrap();
    let dest = temp_dir.path();

    let dir = dest.join("ghost-dir");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("inner.txt"), b"inner").unwrap();

    let handshake = test_handshake();
    let mut config = test_config();
    config.file_selection.delete_missing_args = true;
    config.args = vec![OsString::from(dest.to_str().unwrap())];
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    ctx.file_list
        .push(FileEntry::new_directory(".".into(), 0o755));
    ctx.file_list.push(sentinel_entry("ghost-dir"));

    ctx.process_missing_args_sentinels(
        dest,
        #[cfg(unix)]
        None,
    )
    .unwrap();

    assert!(
        !dir.exists(),
        "sentinel must remove the named directory and its contents",
    );
}
