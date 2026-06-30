//! Unit tests for the disk commit process submodules.

use super::*;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Verifies the io_uring RENAMEAT2 fallback renames a file regardless of
/// whether io_uring handles it or `std::fs::rename` does. Same-device
/// rename returns `false` (no cross-device copy).
#[test]
fn rename_with_io_uring_fallback_moves_file() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("rename_src.txt");
    let dst = dir.path().join("rename_dst.txt");

    fs::write(&src, b"io_uring rename data").unwrap();

    let was_copy = rename_with_io_uring_fallback(&src, &dst).unwrap();

    assert!(!was_copy);
    assert!(!src.exists());
    assert!(dst.exists());
    assert_eq!(fs::read(&dst).unwrap(), b"io_uring rename data");
}

/// Verifies the rename replaces an existing destination file.
#[test]
fn rename_with_io_uring_fallback_replaces_existing() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("rename_replace_src.txt");
    let dst = dir.path().join("rename_replace_dst.txt");

    fs::write(&src, b"new data").unwrap();
    fs::write(&dst, b"old data").unwrap();

    let was_copy = rename_with_io_uring_fallback(&src, &dst).unwrap();

    assert!(!was_copy);
    assert!(!src.exists());
    assert_eq!(fs::read(&dst).unwrap(), b"new data");
}

/// Verifies the rename fails with an error when the source does not exist.
#[test]
fn rename_with_io_uring_fallback_fails_for_missing_source() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("missing_src.txt");
    let dst = dir.path().join("rename_fail_dst.txt");

    let result = rename_with_io_uring_fallback(&src, &dst);
    assert!(result.is_err());
}

/// Verifies `is_cross_device` correctly identifies EXDEV errors.
#[test]
fn is_cross_device_detects_exdev() {
    #[cfg(unix)]
    {
        let exdev = io::Error::from_raw_os_error(libc::EXDEV);
        assert!(is_cross_device(&exdev));
    }
    let not_found = io::Error::new(io::ErrorKind::NotFound, "not found");
    assert!(!is_cross_device(&not_found));

    let perm = io::Error::from_raw_os_error(1); // EPERM
    assert!(!is_cross_device(&perm));
}

/// Verifies `make_writer` selects [`Writer::Macos`] when sparse mode is
/// disabled and `append_offset` is zero, so the `F_NOCACHE` + `writev`
/// optimization is engaged on the common write path.
#[cfg(target_os = "macos")]
#[test]
fn make_writer_selects_macos_for_non_sparse_zero_offset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("macos_writer_select.bin");
    let file = fs::File::create(&path).unwrap();
    let mut write_buf = Vec::with_capacity(256 * 1024);

    let writer = make_writer(
        file,
        &mut write_buf,
        None,
        None,
        /* use_sparse */ false,
        /* append_offset */ 0,
        /* size_hint */ 0,
    )
    .unwrap();

    assert!(
        matches!(writer, Writer::Macos(_)),
        "macOS non-sparse zero-offset writes must select Writer::Macos"
    );
}

/// Verifies `make_writer` falls back to [`Writer::Buffered`] when sparse
/// mode or append mode is active on macOS, preserving `Seek` semantics.
#[cfg(target_os = "macos")]
#[test]
fn make_writer_falls_back_to_buffered_when_seek_required() {
    let dir = tempfile::tempdir().unwrap();

    // Sparse mode forces buffered.
    let sparse_path = dir.path().join("sparse.bin");
    let sparse_file = fs::File::create(&sparse_path).unwrap();
    let mut sparse_buf = Vec::with_capacity(256 * 1024);
    let sparse_writer = make_writer(
        sparse_file,
        &mut sparse_buf,
        None,
        None,
        /* use_sparse */ true,
        /* append_offset */ 0,
        /* size_hint */ 0,
    )
    .unwrap();
    assert!(
        matches!(sparse_writer, Writer::Buffered(_)),
        "sparse mode must select Writer::Buffered"
    );

    // Append mode forces buffered.
    let append_path = dir.path().join("append.bin");
    let append_file = fs::File::create(&append_path).unwrap();
    let mut append_buf = Vec::with_capacity(256 * 1024);
    let append_writer = make_writer(
        append_file,
        &mut append_buf,
        None,
        None,
        /* use_sparse */ false,
        /* append_offset */ 4096,
        /* size_hint */ 0,
    )
    .unwrap();
    assert!(
        matches!(append_writer, Writer::Buffered(_)),
        "append mode must select Writer::Buffered"
    );
}

/// Verifies consistent io_uring availability for RENAMEAT2 across calls.
#[test]
fn rename_io_uring_availability_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("avail_src.txt");
    let dst1 = dir.path().join("avail_dst1.txt");
    let dst2 = dir.path().join("avail_dst2.txt");
    fs::write(&src, b"data").unwrap();

    let first = fast_io::try_rename_via_io_uring(&src, &dst1).is_some();
    // If first call consumed the file, recreate it.
    if first {
        fs::write(&src, b"data").unwrap();
        let _ = fs::remove_file(&dst1);
    }
    let second = fast_io::try_rename_via_io_uring(&src, &dst2).is_some();
    assert_eq!(
        first, second,
        "io_uring RENAMEAT2 availability must be consistent"
    );
}

/// Verifies `partial_dir_path` constructs `.~tmp~/<basename>` under the
/// file's parent directory, matching upstream `options.c:tmp_partialdir`.
#[test]
fn partial_dir_path_constructs_staging_path() {
    let path = Path::new("/dest/subdir/file.txt");
    let staging = partial_dir_path(path);
    assert_eq!(staging, PathBuf::from("/dest/subdir/.~tmp~/file.txt"));
}

/// Verifies `partial_dir_path` handles files directly in the root dest dir.
#[test]
fn partial_dir_path_root_level_file() {
    let path = Path::new("/dest/file.txt");
    let staging = partial_dir_path(path);
    assert_eq!(staging, PathBuf::from("/dest/.~tmp~/file.txt"));
}

/// Verifies `make_backup` returns the upstream-format backup notice with
/// destination-relative paths so the main thread can surface upstream's
/// `INFO_GTE(BACKUP, 1)` line during wire transfers.
///
/// upstream: backup.c:352 - `rprintf(FINFO, "backed up %s to %s\n",
/// fname, buf)` fires on the `success:` label for every backup written.
/// We propagate the notice via [`CommitResult::backup_notice`] because
/// the disk thread's `VerbosityConfig` is not seeded with the user's
/// `--info=backup` selection.
#[test]
fn make_backup_returns_destination_relative_notice() {
    use std::ffi::OsString;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("payload.bin");
    fs::write(&file_path, b"existing content").unwrap();

    let config = BackupConfig {
        dest_dir: dir.path().to_path_buf(),
        backup_dir: None,
        suffix: OsString::from("~"),
    };
    let notice = make_backup(&file_path, &config)
        .expect("make_backup succeeds")
        .expect("notice produced when an existing file is backed up");

    let backup_path = file_path.with_extension("bin~");
    assert!(backup_path.exists(), "backup file must exist after rename");
    assert!(!file_path.exists(), "original file must be renamed away");

    assert_eq!(notice.original, PathBuf::from("payload.bin"));
    assert_eq!(notice.backup, PathBuf::from("payload.bin~"));
}

/// Verifies `make_backup` is a no-op (and returns `None`) when the file
/// does not exist, mirroring upstream `backup.c:make_backup()` which
/// short-circuits when `stat(fname, &st) != 0`.
#[test]
fn make_backup_missing_file_is_noop() {
    use std::ffi::OsString;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("absent.bin");

    let config = BackupConfig {
        dest_dir: dir.path().to_path_buf(),
        backup_dir: None,
        suffix: OsString::from("~"),
    };
    let notice = make_backup(&file_path, &config).expect("make_backup succeeds");
    assert!(notice.is_none(), "no notice when nothing was backed up");
}
