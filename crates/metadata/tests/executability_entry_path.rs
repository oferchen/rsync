//! Regression test for the `--executability` (`-E`) wire path that flows
//! through `apply_metadata_from_file_entry` rather than the source-`Metadata`
//! variant.
//!
//! upstream: rsync.c:457-465 `dest_mode()` - when `-E` is on without `-p`,
//! the receiver transfers only the executability bits from source to
//! destination: if source has no exec bits, clear them on dest; else if dest
//! has no exec bits, grant exec to everyone who can already read
//! (`new_mode & 0444 >> 2`). Exec bits already present on dest are kept.
//!
//! Pre-fix, the entry-based code path returned early without applying the
//! exec-bit transfer when `--executability` was set without `--perms`,
//! producing the upstream testsuite `executability` failure where a file
//! ended up `rw-r--r--` instead of `rwx------`.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;

use metadata::{MetadataOptions, apply_metadata_from_file_entry};
use protocol::flist::FileEntry;
use tempfile::tempdir;

fn mode_of(path: &std::path::Path) -> u32 {
    fs::metadata(path).expect("stat dest").permissions().mode() & 0o7777
}

/// Source has exec bits, dest does not: receiver must grant exec on dest
/// for every class that can already read (`(dest & 0444) >> 2`).
#[test]
fn executability_entry_path_grants_exec_when_source_is_executable() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("dest.bin");
    fs::write(&dest, b"data").expect("write dest");
    // Start dest at rw------- so only the owner can read; after `-E` the
    // owner should additionally gain x (rwx------).
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o600)).expect("seed dest mode");

    // Source mode includes x bits; entry permissions are the source mode.
    let entry = FileEntry::new_file("dest.bin".into(), 4, 0o755);

    let opts = MetadataOptions::new()
        .preserve_permissions(false)
        .preserve_executability(true)
        .preserve_times(false);
    apply_metadata_from_file_entry(&dest, &entry, &opts).expect("apply -E from entry");

    assert_eq!(
        mode_of(&dest),
        0o700,
        "source-with-x must grant x on dest for every class that can already read"
    );
}

/// Source has no exec bits, dest does: receiver must clear all exec bits.
#[test]
fn executability_entry_path_clears_exec_when_source_is_not_executable() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("dest.txt");
    fs::write(&dest, b"data").expect("write dest");
    // Dest starts with x for every class; source has no x at all.
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).expect("seed dest mode");

    let entry = FileEntry::new_file("dest.txt".into(), 4, 0o644);

    let opts = MetadataOptions::new()
        .preserve_permissions(false)
        .preserve_executability(true)
        .preserve_times(false);
    apply_metadata_from_file_entry(&dest, &entry, &opts).expect("apply -E from entry");

    assert_eq!(
        mode_of(&dest) & 0o111,
        0,
        "source-without-x must clear all x bits on dest"
    );
    assert_eq!(
        mode_of(&dest) & 0o666,
        0o644,
        "non-exec permission bits on dest must be preserved"
    );
}
