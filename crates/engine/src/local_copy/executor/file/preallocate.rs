//! File preallocation for reducing fragmentation during writes.
//!
//! Uses `fallocate(2)` on Linux to reserve contiguous disk space before
//! writing file data, falling back to a no-op on other platforms.
//!
//! upstream: receiver.c - preallocate support via --preallocate

use std::fs;
#[cfg(unix)]
use std::io;
use std::path::Path;

#[cfg(unix)]
use rustix::{
    fd::AsFd,
    fs::{FallocateFlags, fallocate},
    io::Errno,
};

use crate::local_copy::LocalCopyError;

/// Preallocates disk space for the destination file when enabled and needed.
///
/// Skips preallocation when disabled, when `total_len` is zero, or when the
/// file already has at least `total_len` bytes allocated.
///
/// Returns the value upstream `do_fallocate()` feeds into `preallocated_len`,
/// which the sparse writer compares against to punch versus seek interior zero
/// runs: `0` when preallocation was skipped or reserved with `FALLOC_FL_KEEP_SIZE`
/// (the `--preallocate`/`--inplace` path, so runs are seeked and the reserved
/// blocks stay dense), and `st_blocks * 512` only on the `opts == 0` fallback
/// path where no `KEEP_SIZE` flag was available and the run is punched instead.
// upstream: syscall.c:1528 do_fallocate() - returns 0 when opts != 0 (KEEP_SIZE),
// else st_blocks * S_BLKSIZE; receiver.c:323 preallocated_len = do_fallocate(...)
pub(crate) fn maybe_preallocate_destination(
    file: &mut fs::File,
    path: &Path,
    total_len: u64,
    existing_bytes: u64,
    enabled: bool,
) -> Result<u64, LocalCopyError> {
    if !enabled || total_len == 0 || total_len <= existing_bytes {
        return Ok(0);
    }

    preallocate_destination_file(file, path, total_len)
}

fn preallocate_destination_file(
    file: &mut fs::File,
    path: &Path,
    total_len: u64,
) -> Result<u64, LocalCopyError> {
    #[cfg(unix)]
    {
        if total_len == 0 {
            return Ok(0);
        }

        if total_len > i64::MAX as u64 {
            return Err(LocalCopyError::io(
                "preallocate destination file",
                path.to_path_buf(),
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "preallocation size exceeds platform limit",
                ),
            ));
        }

        let fd = file.as_fd();
        // upstream: syscall.c:1523 DO_FALLOC_OPTIONS = FALLOC_FL_KEEP_SIZE. The
        // receiver's do_fallocate() reserves blocks with KEEP_SIZE so the file's
        // apparent size (st_size) is NOT extended to total_len - it grows only as
        // data is actually written, preserving the sparse-until-written
        // appearance observable mid-transfer via stat / du --apparent-size. The
        // reserved allocation (st_blocks * S_BLKSIZE) still lets the sparse
        // writer punch holes within the extent rather than seeking over (and
        // leaving) it. KEEP_SIZE is Linux-only; other unix platforms (where
        // upstream compiles the preallocation path out entirely) fall back to a
        // size-extending reservation.
        #[cfg(target_os = "linux")]
        let flags = FallocateFlags::KEEP_SIZE;
        #[cfg(not(target_os = "linux"))]
        let flags = FallocateFlags::empty();
        match fallocate(fd, flags, 0, total_len) {
            // upstream: syscall.c:1554-1556 do_fallocate() returns 0 on the KEEP_SIZE
            // path (opts != 0), so preallocated_len == 0 and write_sparse() seeks over
            // interior zero runs, leaving the reserved blocks allocated (dense). This is
            // deterministic - unlike reading back st_blocks, it does not depend on whether
            // the filesystem eagerly allocates blocks for a KEEP_SIZE reservation.
            #[cfg(target_os = "linux")]
            Ok(()) => Ok(0),
            // Non-Linux uses no KEEP_SIZE flag (opts == 0), so mirror do_fallocate's
            // `return st.st_blocks * S_BLKSIZE` for that path.
            #[cfg(not(target_os = "linux"))]
            Ok(()) => Ok(allocated_bytes(file).unwrap_or(total_len)),
            // KEEP_SIZE unavailable at runtime: fall back to a size-extending
            // reservation (equivalent to upstream's opts == 0 path) and report the
            // resulting allocation so the sparse writer punches within it.
            Err(Errno::OPNOTSUPP | Errno::NOSYS | Errno::INVAL) => {
                file.set_len(total_len).map_err(|error| {
                    LocalCopyError::io("preallocate destination file", path, error)
                })?;
                Ok(allocated_bytes(file).unwrap_or(total_len))
            }
            Err(errno) => Err(LocalCopyError::io(
                "preallocate destination file",
                path.to_path_buf(),
                io::Error::from_raw_os_error(errno.raw_os_error()),
            )),
        }
    }

    #[cfg(not(unix))]
    {
        if total_len == 0 {
            return Ok(0);
        }

        file.set_len(total_len)
            .map_err(|error| LocalCopyError::io("preallocate destination file", path, error))?;
        Ok(total_len)
    }
}

/// Returns the number of bytes currently allocated on disk for `file`
/// (`st_blocks * 512`), mirroring upstream `st.st_blocks * S_BLKSIZE`.
#[cfg(unix)]
fn allocated_bytes(file: &fs::File) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    file.metadata().ok().map(|meta| meta.blocks() * 512)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn maybe_preallocate_disabled_does_nothing() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        let mut file = fs::File::create(&path).expect("create file");

        // When disabled, should succeed without preallocating
        let result = maybe_preallocate_destination(&mut file, &path, 1000, 0, false);
        assert!(result.is_ok());

        // File should remain empty
        let metadata = fs::metadata(&path).expect("metadata");
        assert_eq!(metadata.len(), 0);
    }

    #[test]
    fn maybe_preallocate_zero_length_does_nothing() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        let mut file = fs::File::create(&path).expect("create file");

        // When total_len is 0, should succeed without preallocating
        let result = maybe_preallocate_destination(&mut file, &path, 0, 0, true);
        assert!(result.is_ok());
    }

    #[test]
    fn maybe_preallocate_already_large_enough_does_nothing() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        let mut file = fs::File::create(&path).expect("create file");
        file.write_all(b"existing content").expect("write");
        file.flush().expect("flush");

        let existing_bytes = 16; // Length of "existing content"
        // When total_len <= existing_bytes, should succeed without preallocating
        let result = maybe_preallocate_destination(&mut file, &path, 10, existing_bytes, true);
        assert!(result.is_ok());
    }

    #[test]
    fn maybe_preallocate_enabled_preallocates_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        let mut file = fs::File::create(&path).expect("create file");

        // When enabled and total_len > existing_bytes, should preallocate
        let result = maybe_preallocate_destination(&mut file, &path, 1000, 0, true);
        assert!(result.is_ok());

        let metadata = fs::metadata(&path).expect("metadata");
        // Linux reserves blocks with FALLOC_FL_KEEP_SIZE, leaving the apparent
        // size untouched; other platforms extend the file to the requested size.
        #[cfg(target_os = "linux")]
        assert_eq!(metadata.len(), 0, "KEEP_SIZE must not extend apparent size");
        #[cfg(not(target_os = "linux"))]
        assert_eq!(metadata.len(), 1000);
    }

    #[test]
    fn preallocate_destination_file_sets_length() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        let mut file = fs::File::create(&path).expect("create file");

        let result = preallocate_destination_file(&mut file, &path, 2048);
        assert!(result.is_ok());

        let metadata = fs::metadata(&path).expect("metadata");
        // KEEP_SIZE (Linux) reserves blocks without extending the apparent size.
        #[cfg(target_os = "linux")]
        assert_eq!(metadata.len(), 0, "KEEP_SIZE must not extend apparent size");
        #[cfg(not(target_os = "linux"))]
        assert_eq!(metadata.len(), 2048);
    }

    #[test]
    fn preallocate_destination_file_zero_length_succeeds() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        let mut file = fs::File::create(&path).expect("create file");

        let result = preallocate_destination_file(&mut file, &path, 0);
        assert!(result.is_ok());

        let metadata = fs::metadata(&path).expect("metadata");
        assert_eq!(metadata.len(), 0);
    }

    /// Verify that preallocation rejects sizes exceeding the i64::MAX platform
    /// limit on Unix.  The `fallocate()` offset parameter is a signed 64-bit
    /// integer, so values above `i64::MAX` must be rejected before the syscall.
    #[cfg(unix)]
    #[test]
    fn preallocate_rejects_size_exceeding_i64_max() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("overflow.bin");
        let mut file = fs::File::create(&path).expect("create file");

        let oversized = (i64::MAX as u64) + 1;
        let result = preallocate_destination_file(&mut file, &path, oversized);
        assert!(result.is_err(), "expected error for size > i64::MAX");

        let error = result.unwrap_err();
        let msg = format!("{error}");
        assert!(
            msg.contains("platform limit"),
            "error should mention platform limit, got: {msg}"
        );
    }

    /// Verify that the boundary value `i64::MAX` itself is not rejected
    /// (the syscall may still fail for other reasons, but the size check
    /// should pass).
    #[cfg(unix)]
    #[test]
    fn preallocate_accepts_i64_max_boundary() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("boundary.bin");
        let mut file = fs::File::create(&path).expect("create file");

        // i64::MAX is a valid argument to fallocate(), though the OS will
        // likely reject it due to disk space.  We only verify that our
        // size guard does not reject it prematurely.
        let boundary = i64::MAX as u64;
        let result = preallocate_destination_file(&mut file, &path, boundary);
        // The result may be Err (ENOSPC or similar) but not our "platform limit" error
        if let Err(ref error) = result {
            let msg = format!("{error}");
            assert!(
                !msg.contains("platform limit"),
                "i64::MAX should pass the size guard, got: {msg}"
            );
        }
    }

    /// Verify that preallocation of a large file (1 MiB) actually allocates
    /// disk blocks on Linux.
    #[cfg(target_os = "linux")]
    #[test]
    fn preallocate_large_file_allocates_blocks() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("large.bin");
        let mut file = fs::File::create(&path).expect("create file");

        let one_mib = 1024 * 1024;
        let result = preallocate_destination_file(&mut file, &path, one_mib);
        assert!(result.is_ok());

        let metadata = fs::metadata(&path).expect("metadata");
        // FALLOC_FL_KEEP_SIZE reserves the blocks but leaves st_size at 0.
        assert_eq!(metadata.len(), 0, "KEEP_SIZE must not extend apparent size");

        // On Linux, fallocate() should reserve disk blocks.  The 512-byte
        // block count should be at least file_size / 512.  Some filesystems
        // may allocate slightly more due to alignment, but never less.
        let expected_min_blocks: u64 = one_mib / 512;
        assert!(
            metadata.blocks() >= expected_min_blocks,
            "expected at least {} blocks, got {}",
            expected_min_blocks,
            metadata.blocks()
        );
    }

    /// Verify `maybe_preallocate_destination` mirrors upstream `do_fallocate()`:
    /// the Linux `FALLOC_FL_KEEP_SIZE` path returns 0, so `preallocated_len` stays
    /// 0 and the sparse writer seeks over interior zero runs (leaving the reserved
    /// blocks dense) rather than punching them. Returning a deterministic 0 - not a
    /// read-back `st_blocks` - avoids depending on whether the filesystem eagerly
    /// allocates blocks for a KEEP_SIZE reservation.
    #[cfg(target_os = "linux")]
    #[test]
    fn maybe_preallocate_returns_keep_size_zero() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("prealloc_len.bin");
        let mut file = fs::File::create(&path).expect("create file");

        let one_mib = 1024 * 1024;
        let prealloc =
            maybe_preallocate_destination(&mut file, &path, one_mib, 0, true).expect("preallocate");
        // upstream: syscall.c:1554 do_fallocate() returns 0 for the KEEP_SIZE path.
        assert_eq!(
            prealloc, 0,
            "KEEP_SIZE preallocation must report 0 (seek, not punch), got {prealloc}"
        );

        // Disabled preallocation reports zero (no reserved extent to punch).
        let mut skip = fs::File::create(temp.path().join("skip.bin")).expect("create file");
        let skipped = maybe_preallocate_destination(
            &mut skip,
            &temp.path().join("skip.bin"),
            one_mib,
            0,
            false,
        )
        .expect("skip");
        assert_eq!(skipped, 0, "disabled preallocation should report 0 length");
    }

    /// Verify that disabled preallocation does not allocate extra blocks.
    #[cfg(target_os = "linux")]
    #[test]
    fn disabled_preallocate_does_not_reserve_blocks() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("no_prealloc.bin");
        let mut file = fs::File::create(&path).expect("create file");

        let result = maybe_preallocate_destination(&mut file, &path, 1024 * 1024, 0, false);
        assert!(result.is_ok());

        let metadata = fs::metadata(&path).expect("metadata");
        assert_eq!(metadata.len(), 0);
        assert_eq!(
            metadata.blocks(),
            0,
            "disabled preallocate should not reserve any blocks"
        );
    }

    /// Verify that `maybe_preallocate_destination` handles the exact boundary
    /// where `total_len == existing_bytes` by skipping preallocation.
    #[test]
    fn maybe_preallocate_exact_boundary_does_nothing() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("boundary.txt");
        let mut file = fs::File::create(&path).expect("create file");
        file.write_all(b"12345").expect("write");
        file.flush().expect("flush");

        // total_len == existing_bytes: should skip
        let result = maybe_preallocate_destination(&mut file, &path, 5, 5, true);
        assert!(result.is_ok());
    }

    /// Verify preallocation works when writing to an already opened file
    /// (simulating the inplace write pattern).
    #[test]
    fn preallocate_works_with_writable_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("inplace.bin");

        // Open in read-write mode (like --inplace)
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .expect("open file");

        let result = maybe_preallocate_destination(&mut file, &path, 4096, 0, true);
        assert!(result.is_ok());

        let metadata = fs::metadata(&path).expect("metadata");
        // KEEP_SIZE (Linux) leaves the apparent size at 0; writes then grow it as
        // data lands, exactly as upstream's receiver observes it mid-transfer.
        #[cfg(target_os = "linux")]
        assert_eq!(metadata.len(), 0, "KEEP_SIZE must not extend apparent size");
        #[cfg(not(target_os = "linux"))]
        assert_eq!(metadata.len(), 4096);

        // Write some content to the preallocated space
        file.write_all(b"hello preallocated world").expect("write");
        file.flush().expect("flush");

        let metadata = fs::metadata(&path).expect("metadata after write");
        // On Linux the size now reflects the 24 bytes written; elsewhere the
        // earlier size-extending reservation still governs the length.
        #[cfg(target_os = "linux")]
        assert_eq!(metadata.len(), 24, "size grows only as data is written");
        #[cfg(not(target_os = "linux"))]
        assert_eq!(metadata.len(), 4096);
    }

    /// Verify that preallocating a file that already has some content reserves
    /// the requested extent (the append offset scenario). On Linux KEEP_SIZE
    /// leaves the apparent size at what was already written; other platforms
    /// extend it to the requested size.
    #[test]
    fn preallocate_reserves_for_partially_written_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("partial.bin");
        let mut file = fs::File::create(&path).expect("create file");
        file.write_all(&[0xAA; 100]).expect("write initial");
        file.flush().expect("flush");

        // Preallocate to 4096 even though 100 bytes are written.
        // existing_bytes=100 < total_len=4096, so preallocation should happen.
        let result = maybe_preallocate_destination(&mut file, &path, 4096, 100, true);
        assert!(result.is_ok());

        let metadata = fs::metadata(&path).expect("metadata");
        #[cfg(target_os = "linux")]
        assert_eq!(
            metadata.len(),
            100,
            "KEEP_SIZE preserves the written length"
        );
        #[cfg(not(target_os = "linux"))]
        assert_eq!(metadata.len(), 4096);
    }

    /// Verify preallocation with a variety of sizes including small files
    /// that might not be worth preallocating in practice, but should still
    /// succeed when the feature is enabled.
    #[test]
    fn preallocate_small_file_succeeds() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("tiny.bin");
        let mut file = fs::File::create(&path).expect("create file");

        // Even a 1-byte preallocation should succeed
        let result = maybe_preallocate_destination(&mut file, &path, 1, 0, true);
        assert!(result.is_ok());

        let metadata = fs::metadata(&path).expect("metadata");
        #[cfg(target_os = "linux")]
        assert_eq!(metadata.len(), 0, "KEEP_SIZE must not extend apparent size");
        #[cfg(not(target_os = "linux"))]
        assert_eq!(metadata.len(), 1);
    }

    /// Regression guard for the KEEP_SIZE behavior-fidelity fix. Upstream's
    /// do_fallocate() reserves blocks with FALLOC_FL_KEEP_SIZE, so the apparent
    /// size (st_size) must NOT jump to total_len while the transfer is still
    /// writing - it grows only as data lands. Before the fix, a plain fallocate
    /// (or the set_len fallback) extended st_size to total_len immediately,
    /// observable via stat / du --apparent-size mid-transfer.
    // upstream: syscall.c:1523 DO_FALLOC_OPTIONS = FALLOC_FL_KEEP_SIZE
    #[cfg(target_os = "linux")]
    #[test]
    fn preallocate_keep_size_does_not_extend_apparent_size() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("keep_size.bin");
        let mut file = fs::File::create(&path).expect("create file");

        let total_len: u64 = 1024 * 1024;
        let reserved =
            maybe_preallocate_destination(&mut file, &path, total_len, 0, true).expect("prealloc");

        let metadata = fs::metadata(&path).expect("metadata");
        // The apparent size must stay at 0: KEEP_SIZE reserves blocks without
        // extending st_size to the eventual length.
        assert_eq!(
            metadata.len(),
            0,
            "apparent size must not be prematurely extended to total_len"
        );
        // Yet the blocks are reserved (unless the filesystem lacks fallocate, in
        // which case the fallback set_len would have reported total_len as the
        // length above - which it did not).
        assert!(
            reserved >= total_len || metadata.blocks() * 512 >= total_len,
            "blocks should be reserved for the eventual length (reserved={reserved}, blocks*512={})",
            metadata.blocks() * 512
        );
    }
}
