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

/// `--inplace --backup` delta path: when the generator already created the
/// backup and selected it as the delta basis (`FNAMECMP_BACKUP`, upstream
/// generator.c:2328-2356; carried here as `xattr_basis`), the disk thread must
/// NOT copy again - re-copying would `O_TRUNC` the very file the network
/// thread is resolving matched blocks from mid-transfer.
#[test]
fn make_inplace_backup_skips_when_the_delta_basis_is_the_backup() {
    use std::ffi::OsString;

    use super::super::super::config::BackupConfig;
    use super::make_inplace_backup;

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("file.bin");
    fs::write(&dest, b"pre-image").unwrap();
    let backup_path = dir.path().join("file.bin~");

    let config = DiskCommitConfig {
        backup: Some(BackupConfig {
            dest_dir: dir.path().to_path_buf(),
            backup_dir: None,
            suffix: OsString::from("~"),
        }),
        ..DiskCommitConfig::default()
    };
    let mut begin = inplace_begin(&dest);
    begin.xattr_basis = Some(backup_path.clone());

    // The generator's copy, standing in for find_basis_file_with_config. The
    // sentinel content makes a wrongful re-copy observable: it would clobber
    // this with the destination's current bytes.
    fs::write(&backup_path, b"sentinel: do not truncate").unwrap();

    let notice = make_inplace_backup(&begin, &config).expect("gate must not fail");
    assert!(
        notice.is_none(),
        "no second notice for the generator's backup"
    );
    assert_eq!(
        fs::read(&backup_path).unwrap(),
        b"sentinel: do not truncate",
        "the delta basis must never be rewritten by the disk thread"
    );
}

/// Sibling control: with no delta basis carried (whole-file transfer, or a
/// basis that IS the destination), the disk thread still owns the pre-image
/// copy - upstream's whole-file/read-batch branch (generator.c:2280-2301).
#[test]
fn make_inplace_backup_still_copies_when_no_backup_basis_is_carried() {
    use std::ffi::OsString;

    use super::super::super::config::BackupConfig;
    use super::make_inplace_backup;

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("file.bin");
    fs::write(&dest, b"pre-image").unwrap();

    let config = DiskCommitConfig {
        backup: Some(BackupConfig {
            dest_dir: dir.path().to_path_buf(),
            backup_dir: None,
            suffix: OsString::from("~"),
        }),
        ..DiskCommitConfig::default()
    };
    let begin = inplace_begin(&dest);

    let notice = make_inplace_backup(&begin, &config)
        .expect("copy must succeed")
        .expect("a notice is produced");
    assert_eq!(notice.original, PathBuf::from("file.bin"));
    assert_eq!(
        fs::read(dir.path().join("file.bin~")).unwrap(),
        b"pre-image",
        "the whole-file inplace backup still comes from the disk thread"
    );
}
