use super::common::*;
use super::*;

#[cfg(unix)]
#[test]
fn transfer_request_with_sparse_preserves_holes() {
    use std::os::unix::fs::MetadataExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.bin");
    let mut source_file = std::fs::File::create(&source).expect("create source");
    source_file.write_all(&[0x10]).expect("write leading byte");
    source_file
        .seek(SeekFrom::Start(1024 * 1024))
        .expect("seek to hole");
    source_file.write_all(&[0x20]).expect("write trailing byte");
    source_file.set_len(3 * 1024 * 1024).expect("extend source");

    let dense_dest = tmp.path().join("dense.bin");
    let sparse_dest = tmp.path().join("sparse.bin");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        source.clone().into_os_string(),
        dense_dest.clone().into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--sparse"),
        source.into_os_string(),
        sparse_dest.clone().into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let dense_meta = std::fs::metadata(&dense_dest).expect("dense metadata");
    let sparse_meta = std::fs::metadata(&sparse_dest).expect("sparse metadata");

    let dense_blocks = dense_meta.blocks();
    let sparse_blocks = sparse_meta.blocks();

    // On filesystems with compression or automatic hole punching (APFS, btrfs, ZFS, etc.)
    // a "dense" write of zeros can already be stored efficiently. In that case the sparse
    // copy may use the same number of blocks as the dense copy. The portable guarantee
    // we care about is that a sparse copy never uses *more* blocks than a dense copy of
    // the same contents.
    assert!(
        sparse_blocks <= dense_blocks,
        "sparse copy must not use more blocks than dense copy (sparse={sparse_blocks}, dense={dense_blocks})",
    );
}

#[cfg(unix)]
#[test]
fn transfer_request_with_sparse_copies_all_zero_source_without_extra_blocks() {
    use std::os::unix::fs::MetadataExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("zeros.bin");
    let mut source_file = std::fs::File::create(&source).expect("create source");
    let payload = vec![0u8; 2 * 1024 * 1024];
    source_file.write_all(&payload).expect("write zero payload");

    let dense_dest = tmp.path().join("dense.bin");
    let sparse_dest = tmp.path().join("sparse.bin");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        source.clone().into_os_string(),
        dense_dest.clone().into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--sparse"),
        source.into_os_string(),
        sparse_dest.clone().into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let dense_meta = std::fs::metadata(&dense_dest).expect("dense metadata");
    let sparse_meta = std::fs::metadata(&sparse_dest).expect("sparse metadata");

    assert_eq!(dense_meta.len(), sparse_meta.len());

    let dense_blocks = dense_meta.blocks();
    let sparse_blocks = sparse_meta.blocks();

    assert!(
        sparse_blocks <= dense_blocks,
        "sparse copy must not use more blocks than dense copy (sparse={sparse_blocks}, dense={dense_blocks})",
    );
}

/// `--sparse --preallocate` reserves the destination extent, then punches the
/// source's zero run back out. Upstream keeps `sparse_files > 0` under
/// `--preallocate` and `do_fallocate()` reports the preallocated length so
/// `write_sparse()` punches the interior hole (rather than leaving the reserved
/// blocks allocated).
// upstream: fileio.c:95 write_sparse() -> do_punch_hole within preallocated_len
#[cfg(target_os = "linux")]
#[test]
fn transfer_request_with_sparse_and_preallocate_punches_hole() {
    use std::os::unix::fs::MetadataExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.bin");
    let mut source_file = std::fs::File::create(&source).expect("create source");
    source_file.write_all(&[0x10; 4096]).expect("write head");
    source_file
        .write_all(&vec![0u8; 2 * 1024 * 1024])
        .expect("write hole");
    source_file.write_all(&[0x20; 4096]).expect("write tail");
    let apparent = source_file.metadata().expect("meta").len();
    drop(source_file);

    let prealloc_dest = tmp.path().join("sparse-prealloc.bin");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--sparse"),
        OsString::from("--preallocate"),
        source.into_os_string(),
        prealloc_dest.clone().into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let prealloc_meta = std::fs::metadata(&prealloc_dest).expect("prealloc metadata");
    assert_eq!(prealloc_meta.len(), apparent, "content length preserved");
    assert!(
        prealloc_meta.blocks() * 512 < apparent,
        "preallocated zero run must be punched (allocated {}, size {apparent})",
        prealloc_meta.blocks() * 512,
    );
}

#[cfg(unix)]
#[test]
fn transfer_request_with_sparse_and_append_uses_dense_allocation() {
    use std::fs::{self, File, OpenOptions};
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let base = tmp.path().join("base.bin");
    let mut base_file = File::create(&base).expect("create base");
    base_file
        .write_all(&vec![0x55; 1024])
        .expect("write base prefix");
    base_file.flush().expect("flush base");
    drop(base_file);

    let dense_dest = tmp.path().join("append-dense.bin");
    let sparse_dest = tmp.path().join("append-sparse.bin");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        base.as_os_str().to_os_string(),
        dense_dest.as_os_str().to_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        base.as_os_str().to_os_string(),
        sparse_dest.as_os_str().to_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let appended_source = tmp.path().join("append-source.bin");
    fs::copy(&base, &appended_source).expect("copy base to appended source");
    let mut appended_file = OpenOptions::new()
        .append(true)
        .open(&appended_source)
        .expect("open appended source");
    appended_file
        .write_all(&vec![0u8; 1_048_576])
        .expect("write zero run");
    appended_file
        .write_all(&[0x7f])
        .expect("write trailing byte");
    appended_file.flush().expect("flush appended source");
    drop(appended_file);

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--append"),
        appended_source.as_os_str().to_os_string(),
        dense_dest.as_os_str().to_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--sparse"),
        OsString::from("--append"),
        appended_source.as_os_str().to_os_string(),
        sparse_dest.as_os_str().to_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let dense_meta = fs::metadata(&dense_dest).expect("dense metadata");
    let sparse_meta = fs::metadata(&sparse_dest).expect("sparse metadata");

    assert_eq!(dense_meta.len(), sparse_meta.len());
    assert_eq!(sparse_meta.blocks(), dense_meta.blocks());
}

#[cfg(unix)]
#[test]
fn transfer_request_with_sparse_and_append_verify_uses_dense_allocation() {
    use std::fs::{self, File, OpenOptions};
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let base = tmp.path().join("base.bin");
    let mut base_file = File::create(&base).expect("create base");
    base_file
        .write_all(&vec![0x42; 1024])
        .expect("write base prefix");
    base_file.flush().expect("flush base");
    drop(base_file);

    let dense_dest = tmp.path().join("append-verify-dense.bin");
    let sparse_dest = tmp.path().join("append-verify-sparse.bin");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        base.as_os_str().to_os_string(),
        dense_dest.as_os_str().to_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        base.as_os_str().to_os_string(),
        sparse_dest.as_os_str().to_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let appended_source = tmp.path().join("append-verify-source.bin");
    fs::copy(&base, &appended_source).expect("copy base to appended source");
    let mut appended_file = OpenOptions::new()
        .append(true)
        .open(&appended_source)
        .expect("open appended source");
    appended_file
        .write_all(&vec![0u8; 1_048_576])
        .expect("write zero run");
    appended_file
        .write_all(&[0x99])
        .expect("write trailing byte");
    appended_file.flush().expect("flush appended source");
    drop(appended_file);

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--append-verify"),
        appended_source.as_os_str().to_os_string(),
        dense_dest.as_os_str().to_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--sparse"),
        OsString::from("--append-verify"),
        appended_source.as_os_str().to_os_string(),
        sparse_dest.as_os_str().to_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let dense_meta = fs::metadata(&dense_dest).expect("dense metadata");
    let sparse_meta = fs::metadata(&sparse_dest).expect("sparse metadata");

    assert_eq!(dense_meta.len(), sparse_meta.len());
    assert_eq!(sparse_meta.blocks(), dense_meta.blocks());
}

/// `--sparse --inplace` writes the source's zero run as a hole rather than
/// materialising it. Upstream keeps `sparse_files > 0` in inplace mode, so an
/// inplace update with a large zero run allocates far fewer blocks than a dense
/// (`--inplace` only) update of the same content.
// upstream: fileio.c:write_sparse() stays active with inplace (preallocated_len = size_r)
#[cfg(target_os = "linux")]
#[test]
fn transfer_request_with_sparse_and_inplace_punches_hole() {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let base = tmp.path().join("inplace-base.bin");
    let mut base_file = fs::File::create(&base).expect("create base");
    base_file
        .write_all(&vec![0x24; 2048])
        .expect("write base prefix");
    base_file.flush().expect("flush base");
    drop(base_file);

    let dense_dest = tmp.path().join("inplace-dense.bin");
    let sparse_dest = tmp.path().join("inplace-sparse.bin");

    fs::copy(&base, &dense_dest).expect("seed dense destination");
    fs::copy(&base, &sparse_dest).expect("seed sparse destination");

    let updated_source = tmp.path().join("inplace-source.bin");
    fs::copy(&base, &updated_source).expect("copy base to updated source");
    let mut updated_file = OpenOptions::new()
        .append(true)
        .open(&updated_source)
        .expect("open updated source");
    updated_file
        .write_all(&vec![0u8; 1_048_576])
        .expect("write zero run");
    updated_file
        .write_all(&[0x7a])
        .expect("write trailing byte");
    updated_file.flush().expect("flush updated source");
    drop(updated_file);

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--inplace"),
        updated_source.as_os_str().to_os_string(),
        dense_dest.as_os_str().to_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--sparse"),
        OsString::from("--inplace"),
        updated_source.into_os_string(),
        sparse_dest.clone().into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let dense_meta = fs::metadata(&dense_dest).expect("dense metadata");
    let sparse_meta = fs::metadata(&sparse_dest).expect("sparse metadata");

    // Content length matches; the sparse update must allocate strictly fewer
    // blocks than the dense one because the 1 MiB zero run became a hole.
    assert_eq!(dense_meta.len(), sparse_meta.len());
    assert!(
        sparse_meta.blocks() < dense_meta.blocks(),
        "inplace sparse update must punch the zero run (sparse={} blocks, dense={} blocks)",
        sparse_meta.blocks(),
        dense_meta.blocks(),
    );
}

/// upstream: options.c:2413-2419 - `--write-devices` forces the global inplace
/// flag on. An inplace update rewrites the destination file in place (same
/// inode), whereas the default atomic mode writes a temp file and renames it
/// (new inode). Comparing the destination inode before and after a
/// `--write-devices` update proves the inplace behaviour was enabled.
#[cfg(unix)]
#[test]
fn transfer_request_with_write_devices_updates_in_place() {
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("wd-source.bin");
    let dest = tmp.path().join("wd-dest.bin");

    // Different sizes guarantee a transfer regardless of quick-check.
    fs::write(&source, vec![0x41; 4096]).expect("write source");
    fs::write(&dest, vec![0x42; 2048]).expect("write dest");
    let ino_before = fs::metadata(&dest).expect("dest metadata").ino();

    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--write-devices"),
        source.into_os_string(),
        dest.clone().into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let after_meta = fs::metadata(&dest).expect("dest metadata after");
    assert_eq!(
        after_meta.ino(),
        ino_before,
        "--write-devices must rewrite the destination in place (inode preserved)"
    );
    assert_eq!(after_meta.len(), 4096, "content must be updated");
}
