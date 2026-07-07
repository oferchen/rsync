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
///
/// Gated off when `macos-gcd` is enabled: that feature routes the same
/// non-sparse/zero-offset path to [`Writer::MacosGcd`] instead (covered by
/// `make_writer_selects_macos_gcd_when_feature_enabled`).
#[cfg(all(target_os = "macos", not(feature = "macos-gcd")))]
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

/// Verifies `make_writer` selects [`Writer::MacosGcd`] for the common
/// non-sparse, zero-offset path when the `macos-gcd` feature is enabled, so
/// the GCD (`dispatch_io`) writer replaces the F_NOCACHE + writev writer.
#[cfg(all(target_os = "macos", feature = "macos-gcd"))]
#[test]
fn make_writer_selects_macos_gcd_when_feature_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("macos_gcd_select.bin");
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
        matches!(writer, Writer::MacosGcd(_)),
        "macOS non-sparse zero-offset writes must select Writer::MacosGcd \
         when the macos-gcd feature is on"
    );
}

/// Verifies the GCD writer falls back to [`Writer::Buffered`] under sparse or
/// append mode, which both require `Seek` that the channel cannot provide.
#[cfg(all(target_os = "macos", feature = "macos-gcd"))]
#[test]
fn make_writer_macos_gcd_falls_back_to_buffered_for_seek() {
    let dir = tempfile::tempdir().unwrap();

    let sparse_path = dir.path().join("gcd_sparse.bin");
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
        "sparse mode must fall back to Writer::Buffered even with macos-gcd on"
    );

    let append_path = dir.path().join("gcd_append.bin");
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
        "append mode must fall back to Writer::Buffered even with macos-gcd on"
    );
}

/// Verifies a full write-then-commit through [`Writer::MacosGcd`] produces
/// byte-for-byte the same on-disk file as the buffered writer, exercising the
/// real `write_chunk` -> `flush_and_sync` -> `finish` disk-commit lifecycle.
#[cfg(all(target_os = "macos", feature = "macos-gcd"))]
#[test]
fn macos_gcd_writer_matches_buffered_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();

    // Representative payload: several chunks spanning the direct-write
    // threshold so both the small-buffered and large-direct paths are hit on
    // the buffered side, and the same bytes flow through the GCD channel.
    let chunks: Vec<Vec<u8>> = vec![
        (0..1024u32).map(|i| (i % 251) as u8).collect(),
        (0..64 * 1024u32).map(|i| (i % 239) as u8).collect(),
        (0..37u32).map(|i| (i % 211) as u8).collect(),
        (0..256 * 1024u32).map(|i| (i % 193) as u8).collect(),
    ];

    let write_all = |mut w: Writer<'_>, path: &Path| {
        for c in &chunks {
            w.write_chunk(c).unwrap();
        }
        w.flush_and_sync(/* do_fsync */ true, path).unwrap();
        w.finish(/* do_fsync */ true, path).unwrap();
    };

    // Obtain a `Writer::Buffered` through `make_writer` by requesting sparse
    // mode, which always falls back to the buffered variant. The stored writer
    // carries no sparse state, so `write_chunk` writes the bytes verbatim.
    let buffered_path = dir.path().join("parity_buffered.bin");
    let buffered_file = fs::File::create(&buffered_path).unwrap();
    let mut buffered_buf = Vec::with_capacity(256 * 1024);
    let buffered_writer = make_writer(
        buffered_file,
        &mut buffered_buf,
        None,
        None,
        /* use_sparse */ true,
        /* append_offset */ 0,
        /* size_hint */ 0,
    )
    .unwrap();
    assert!(matches!(buffered_writer, Writer::Buffered(_)));
    write_all(buffered_writer, &buffered_path);

    let gcd_path = dir.path().join("parity_gcd.bin");
    let gcd_file = fs::File::create(&gcd_path).unwrap();
    let gcd_writer = Writer::MacosGcd(fast_io::GcdWriter::from_file(gcd_file).unwrap());
    write_all(gcd_writer, &gcd_path);

    assert_eq!(
        fs::read(&buffered_path).unwrap(),
        fs::read(&gcd_path).unwrap(),
        "GCD writer output must be byte-identical to the buffered writer"
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
    let notice = make_backup(&file_path, &config, &DiskCommitConfig::default())
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
    let notice = make_backup(&file_path, &config, &DiskCommitConfig::default())
        .expect("make_backup succeeds");
    assert!(notice.is_none(), "no notice when nothing was backed up");
}

/// #507 (TOCTOU regression): the dirfd anchor on the commit rename refuses to
/// land the final file through a destination parent that was swapped for a
/// symlink pointing outside the tree.
///
/// Proven deterministically by program order (no threads, no sleeps): the
/// `DirSandbox` opens the destination root's dirfd before the swap, so a later
/// symlink swap on the `dest_dir` path cannot redirect the anchored
/// `renameat`. The out-of-tree location must stay empty; the final file must
/// land in the real (now-moved-aside) destination directory, never through the
/// symlink. WHY it matters: without the anchor, `fs::rename(temp, dest/name)`
/// would follow the swapped symlink and write the received file outside the
/// destination tree.
#[cfg(unix)]
#[test]
fn commit_rename_refuses_parent_symlink_escape() {
    use std::sync::Arc;

    let tmp = tempfile::tempdir().expect("tempdir");
    let staging = std::fs::canonicalize(tmp.path()).expect("canon");

    // Destination root holds the temp file and the final name.
    let dest_dir = staging.join("dest");
    fs::create_dir(&dest_dir).unwrap();
    let temp_path = dest_dir.join(".tmp.payload");
    fs::write(&temp_path, b"received content").unwrap();
    let final_path = dest_dir.join("payload.bin");

    // Out-of-tree location a redirected rename would land the file in. Must
    // stay empty.
    let outside = staging.join("outside");
    fs::create_dir(&outside).unwrap();
    let dest_aside = staging.join("dest.aside");

    // Open the sandbox at the real dest root BEFORE any swap; the dirfd pins
    // the real inode.
    let sandbox = Arc::new(fast_io::DirSandbox::open_root(&dest_dir).expect("open sandbox"));
    let config = DiskCommitConfig {
        sandbox: Some(sandbox),
        dest_dir: Some(dest_dir.clone()),
        ..DiskCommitConfig::default()
    };

    // Move the real dest aside and plant a symlink at the original path
    // pointing at `outside`. Any path-based resolver now routes
    // `dest/payload.bin` to `outside/payload.bin`; the anchored renameat is
    // immune because its dirfd still names the moved-aside real directory.
    fs::rename(&dest_dir, &dest_aside).expect("move dest aside");
    std::os::unix::fs::symlink(&outside, &dest_dir).expect("plant dest symlink");

    // Pass the ORIGINAL path strings (temp_path, final_path), exactly as
    // production does: the receiver holds `dest_dir/<leaf>` paths that predate
    // the swap. The gate resolves both leaves against the pinned dirfd, so the
    // stale path strings do not follow the symlink.
    let was_copy = rename_config_sandboxed(&config, &temp_path, &final_path)
        .expect("anchored commit rename succeeds");
    assert!(!was_copy, "same-parent rename is never a cross-device copy");

    // The out-of-tree location must be empty: the anchor refused the redirect.
    assert!(
        !outside.join("payload.bin").exists(),
        "final file must not land outside the tree through the swapped symlink",
    );
    // The final file landed in the real (moved-aside) destination directory.
    assert!(
        dest_aside.join("payload.bin").exists(),
        "anchored rename must place the final file inside the real destination",
    );
    assert_eq!(
        fs::read(dest_aside.join("payload.bin")).unwrap(),
        b"received content",
    );
}

/// Fallback parity: with no sandbox attached, `rename_config_sandboxed` uses
/// the path-based [`rename_with_io_uring_fallback`], moving the temp file to
/// its final destination exactly as before. Proves the sandbox wiring never
/// regresses a working rename on the common (no-sandbox) path.
#[cfg(unix)]
#[test]
fn commit_rename_without_sandbox_uses_path_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let temp_path = dir.path().join(".tmp.payload");
    let final_path = dir.path().join("payload.bin");
    fs::write(&temp_path, b"fallback content").unwrap();

    let config = DiskCommitConfig::default();
    assert!(config.sandbox.is_none());

    let was_copy = rename_config_sandboxed(&config, &temp_path, &final_path)
        .expect("path-based rename succeeds");
    assert!(!was_copy);
    assert!(!temp_path.exists(), "temp file renamed away");
    assert!(final_path.exists());
    assert_eq!(fs::read(&final_path).unwrap(), b"fallback content");
}
