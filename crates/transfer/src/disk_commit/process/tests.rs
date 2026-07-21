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
        /* is_inplace */ false,
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
        /* is_inplace */ false,
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
        /* is_inplace */ false,
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
        /* is_inplace */ false,
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
        /* is_inplace */ false,
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
        /* is_inplace */ false,
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
        /* is_inplace */ false,
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

/// Verifies `make_backup` succeeds when the backup path is on a different
/// filesystem than the destination: the bare rename fails cross-device
/// (`EXDEV`) and upstream's copy tier moves the pre-image by copying its bytes
/// to the backup and unlinking the original.
///
/// Before the copy fallback existed, `make_backup` did a bare rename with no
/// `EXDEV` handling, so a `--backup-dir` on another mount propagated the error
/// and failed the commit. The cross-device condition is injected at the rename
/// boundary via [`ForceExdev`] so the test is deterministic on a single-
/// filesystem CI runner. If the copy tier were removed the rename error would
/// surface and this test would fail.
///
/// upstream: backup.c:226 make_backup() - `copy_file()` + unlink when
/// `do_rename_at` cannot cross the mount.
#[test]
fn make_backup_cross_device_uses_copy_fallback() {
    use std::ffi::OsString;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("payload.bin");
    fs::write(&file_path, b"pre-transfer content").unwrap();

    let config = BackupConfig {
        dest_dir: dir.path().to_path_buf(),
        backup_dir: None,
        suffix: OsString::from("~"),
    };

    let notice = {
        let _force = ForceExdev::new();
        make_backup(&file_path, &config, &DiskCommitConfig::default())
            .expect("cross-device backup must succeed via the copy fallback")
            .expect("notice produced when an existing file is backed up")
    };

    let backup_path = file_path.with_extension("bin~");
    assert!(
        backup_path.exists(),
        "backup must exist after the copy fallback"
    );
    assert!(
        !file_path.exists(),
        "original must be unlinked once the copy completes"
    );
    assert_eq!(
        fs::read(&backup_path).unwrap(),
        b"pre-transfer content",
        "backup must hold the original pre-transfer bytes"
    );

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

/// Verifies `make_backup_copy` (the `--inplace --backup` path) DUPLICATES the
/// original to the backup and LEAVES the original in place, unlike `make_backup`
/// which renames it away. Preserving the original is the whole point: under
/// `--inplace` that file is the destination inode about to be rewritten in
/// place, so it must survive the backup step.
///
/// upstream: generator.c:1862 - `copy_file(fname, backupptr, ...)` copies the
/// pre-image aside while the destination stays put for the inplace rewrite.
#[test]
fn make_backup_copy_duplicates_and_keeps_original() {
    use std::ffi::OsString;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("payload.bin");
    fs::write(&file_path, b"original bytes").unwrap();

    let config = BackupConfig {
        dest_dir: dir.path().to_path_buf(),
        backup_dir: None,
        suffix: OsString::from("~"),
    };
    let notice = make_backup_copy(&file_path, &config, &DiskCommitConfig::default())
        .expect("make_backup_copy succeeds")
        .expect("notice produced when an existing file is copied");

    let backup_path = file_path.with_extension("bin~");
    assert!(backup_path.exists(), "backup copy must exist");
    assert!(
        file_path.exists(),
        "original must REMAIN in place for the inplace rewrite (copy, not rename)"
    );
    assert_eq!(
        fs::read(&backup_path).unwrap(),
        b"original bytes",
        "backup must hold the original pre-transfer bytes"
    );
    assert_eq!(
        fs::read(&file_path).unwrap(),
        b"original bytes",
        "original content is untouched by the copy step"
    );

    assert_eq!(notice.original, PathBuf::from("payload.bin"));
    assert_eq!(notice.backup, PathBuf::from("payload.bin~"));
}

/// Verifies `make_backup_copy` no-ops (returns `None`, writes nothing) when the
/// destination does not yet exist, mirroring upstream's `x_lstat` guard: a
/// first-time inplace create has no pre-image to preserve.
#[test]
fn make_backup_copy_missing_file_is_noop() {
    use std::ffi::OsString;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("absent.bin");

    let config = BackupConfig {
        dest_dir: dir.path().to_path_buf(),
        backup_dir: None,
        suffix: OsString::from("~"),
    };
    let notice = make_backup_copy(&file_path, &config, &DiskCommitConfig::default())
        .expect("make_backup_copy succeeds");
    assert!(notice.is_none(), "no notice when nothing was backed up");
    assert!(
        !file_path.with_extension("bin~").exists(),
        "no backup file created when the destination is absent"
    );
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

/// CVE-2026-29518 secondary residual: the pipelined disk-commit rename must
/// anchor a file committed into a destination *subdirectory*, not just a file
/// directly under the sandbox root. Before the fix the subdir case fell through
/// to a path-based `std::fs::rename`, letting a swapped interior directory
/// redirect the committed file out of the module on a privileged daemon.
///
/// Staging (deterministic, program order - no threads): the temp source is
/// planted inside the out-of-tree location and the interior directory `sub`
/// under the pinned root is a symlink to that location, modelling an attacker
/// who swapped `dest/sub` for a symlink between temp-create and commit. A
/// path-based `rename(dest/sub/.tmp, dest/sub/payload.bin)` would resolve both
/// through the symlink and land the file in `outside/payload.bin` (the escape).
/// The `openat2(RESOLVE_BENEATH)` parent anchor instead refuses to open `sub`
/// (its symlink target escapes beneath the root), so the rename fails safe and
/// nothing lands outside the module.
///
/// upstream: `syscall.c:910` `do_rename_at()` opens each slashed path's parent
/// via `secure_relative_open()` before `renameat()`.
///
/// Linux + openat2 only: `RESOLVE_BENEATH` is the confinement primitive and has
/// no portable equivalent; other targets keep the path-based fallback, matching
/// upstream's `am_daemon && !am_chrooted` Linux gate.
#[cfg(target_os = "linux")]
#[test]
fn commit_rename_subdir_refuses_interior_symlink_escape() {
    use std::sync::Arc;

    if !fast_io::openat2_supported() {
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let staging = std::fs::canonicalize(tmp.path()).expect("canon");

    // Pinned destination root (sandbox anchor) with a real interior subdir.
    let dest_dir = staging.join("dest");
    fs::create_dir(&dest_dir).unwrap();

    // Out-of-tree location the escape would target; the temp source lives here
    // so a symlink-followed rename would find a valid source and succeed.
    let outside = staging.join("outside");
    fs::create_dir(&outside).unwrap();
    let temp_path = dest_dir.join("sub").join(".tmp.payload");
    let final_path = dest_dir.join("sub").join("payload.bin");
    fs::write(outside.join(".tmp.payload"), b"received content").unwrap();

    // Open the sandbox at the real dest root BEFORE the swap; the dirfd pins the
    // real root inode.
    let sandbox = Arc::new(fast_io::DirSandbox::open_root(&dest_dir).expect("open sandbox"));
    let config = DiskCommitConfig {
        sandbox: Some(sandbox),
        dest_dir: Some(dest_dir.clone()),
        ..DiskCommitConfig::default()
    };

    // Plant the interior directory as a symlink escaping beneath the root. A
    // path-based resolver routes `dest/sub/...` to `outside/...`.
    std::os::unix::fs::symlink(&outside, dest_dir.join("sub")).expect("plant sub symlink");

    // The anchored rename must refuse: opening `sub` under RESOLVE_BENEATH sees
    // a symlink whose target escapes the root and fails (EXDEV), so the commit
    // cannot follow the swap.
    let result = rename_config_sandboxed(&config, &temp_path, &final_path);
    assert!(
        result.is_err(),
        "anchored subdir rename must fail rather than follow the interior symlink swap",
    );

    // The escape must not have happened: no committed file outside the module,
    // and the temp source is left untouched.
    assert!(
        !outside.join("payload.bin").exists(),
        "committed file must not land outside the module through the swapped interior dir",
    );
    assert!(
        outside.join(".tmp.payload").exists(),
        "temp source is left in place when the anchored rename refuses",
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
