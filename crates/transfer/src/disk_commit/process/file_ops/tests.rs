//! Tests for the in-place output open chain, in particular upstream's read-only
//! recovery arm (`receiver.c:1219-1224`).

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::{BeginMessage, DiskCommitConfig, open_output_file};

const READ_ONLY: u32 = 0o444;

fn mode_of(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn inplace_begin(path: &Path) -> BeginMessage {
    BeginMessage {
        file_path: path.to_path_buf(),
        target_size: 0,
        file_entry_index: 0,
        checksum_verifier: None,
        is_device_target: false,
        is_inplace: true,
        append_offset: 0,
        xattr_list: None,
        xattr_basis: None,
        file_entry: None,
    }
}

fn plant_readonly(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(READ_ONLY)).unwrap();
    path
}

/// A read-only in-place destination must still be updated, as upstream 3.5.0
/// does through the third arm of its open chain. Without that arm oc failed the
/// whole transfer with `Permission denied (13)` and left the file untouched.
/// upstream: receiver.c:1219-1224.
#[test]
fn a_read_only_inplace_destination_is_opened_for_writing() {
    let dir = tempfile::tempdir().unwrap();
    let path = plant_readonly(dir.path(), "basis.bin", b"old");

    let (mut file, _guard, is_temp) =
        open_output_file(&inplace_begin(&path), &DiskCommitConfig::default())
            .expect("a read-only in-place destination is recoverable");

    assert!(!is_temp, "in-place writes the destination itself");
    file.write_all(b"new").unwrap();
    drop(file);

    assert_eq!(fs::read(&path).unwrap(), b"new");
    assert_eq!(
        mode_of(&path),
        READ_ONLY,
        "the update must not alter the mode"
    );
}

/// The prior mode is restored *before* the descriptor is handed back, so an abort
/// part-way through the transfer - peer EOF, checksum failure, a signal - cannot
/// strand the file owner-writable. That is what makes a cleanup path unnecessary,
/// and why the restore must never be deferred to commit time.
/// upstream: receiver.c:200-206.
#[test]
fn the_prior_mode_is_restored_before_the_descriptor_is_returned() {
    let dir = tempfile::tempdir().unwrap();
    let path = plant_readonly(dir.path(), "basis.bin", b"old");

    let (file, _guard, _) =
        open_output_file(&inplace_begin(&path), &DiskCommitConfig::default()).unwrap();

    assert_eq!(
        mode_of(&path),
        READ_ONLY,
        "restored while the writable fd is still open"
    );

    // The abort: the transfer loop never writes a byte.
    drop(file);

    assert_eq!(mode_of(&path), READ_ONLY);
    assert_eq!(fs::read(&path).unwrap(), b"old");
}
