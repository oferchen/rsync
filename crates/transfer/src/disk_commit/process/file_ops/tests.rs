//! Tests for the in-place output open chain, in particular upstream's read-only
//! recovery arm (`receiver.c:1236-1241`).

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
        log_name: None,
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
/// upstream: receiver.c:1236-1241.
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
/// upstream: receiver.c:213-219.
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
            dir_cache: Default::default(),
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
            dir_cache: Default::default(),
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

/// Opens the sandbox on `dest/`, then swaps `dest/a` for a symlink to
/// `outside/`, which holds a bait file `f`. Returns the config, `dest/a/f`
/// and the bait path.
fn sandboxed_config_then_swap(root: &Path) -> (DiskCommitConfig, PathBuf, PathBuf) {
    let dest = root.join("dest");
    let outside = root.join("outside");
    fs::create_dir_all(dest.join("a")).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("f"), b"bait").unwrap();
    let sandbox = std::sync::Arc::new(fast_io::DirSandbox::open_root(&dest).unwrap());
    fs::remove_dir(dest.join("a")).unwrap();
    std::os::unix::fs::symlink(&outside, dest.join("a")).unwrap();
    let config = DiskCommitConfig {
        dest_dir: Some(dest.clone()),
        sandbox: Some(sandbox),
        ..DiskCommitConfig::default()
    };
    (config, dest.join("a/f"), outside.join("f"))
}

/// upstream: receiver.c:1212-1235 - the in-place output opens beneath the
/// destination the receiver entered, so a parent swapped for a symlink after
/// entry cannot carry the write outside the tree.
#[test]
fn inplace_output_under_a_swapped_parent_is_not_opened() {
    let tmp = tempfile::tempdir().unwrap();
    let (config, target, bait) = sandboxed_config_then_swap(tmp.path());
    fs::remove_file(&bait).unwrap();
    assert!(open_output_file(&inplace_begin(&target), &config).is_err());
    assert!(
        fs::symlink_metadata(&bait).is_err(),
        "the in-place open created a file through the swapped parent"
    );
}

/// A `--write-devices` target opens beneath the destination the same way.
#[test]
fn device_target_under_a_swapped_parent_is_not_opened() {
    let tmp = tempfile::tempdir().unwrap();
    let (config, target, _bait) = sandboxed_config_then_swap(tmp.path());
    let begin = BeginMessage {
        is_device_target: true,
        ..inplace_begin(&target)
    };
    assert!(
        open_output_file(&begin, &config).is_err(),
        "the device open followed the swapped parent"
    );
}

/// upstream: receiver.c:353 set_file_length(fd) - the post-transfer truncate
/// and hole punch reopen the destination beneath the sandbox.
#[test]
fn destination_reopen_under_a_swapped_parent_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (config, target, bait) = sandboxed_config_then_swap(tmp.path());
    assert!(super::super::commit::open_dest_for_write(config.backup_env(), &target).is_err());
    assert_eq!(fs::read(bait).unwrap(), b"bait");
}

/// Non-vacuity: with the parent intact, every routed open reaches the
/// in-tree destination.
#[test]
fn routed_destination_opens_reach_an_intact_nested_target() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("dest");
    fs::create_dir_all(dest.join("a")).unwrap();
    fs::write(dest.join("a/f"), b"live").unwrap();
    let config = DiskCommitConfig {
        dest_dir: Some(dest.clone()),
        sandbox: Some(std::sync::Arc::new(
            fast_io::DirSandbox::open_root(&dest).unwrap(),
        )),
        ..DiskCommitConfig::default()
    };
    let target = dest.join("a/f");
    assert!(open_output_file(&inplace_begin(&target), &config).is_ok());
    assert!(super::super::commit::open_dest_for_write(config.backup_env(), &target).is_ok());
}
