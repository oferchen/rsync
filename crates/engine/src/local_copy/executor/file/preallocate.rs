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

pub(crate) fn maybe_preallocate_destination(
    file: &mut fs::File,
    path: &Path,
    total_len: u64,
    existing_bytes: u64,
    enabled: bool,
) -> Result<(), LocalCopyError> {
    if !enabled || total_len == 0 || total_len <= existing_bytes {
        return Ok(());
    }

    preallocate_destination_file(file, path, total_len)
}

fn preallocate_destination_file(
    file: &mut fs::File,
    path: &Path,
    total_len: u64,
) -> Result<(), LocalCopyError> {
    #[cfg(unix)]
    {
        if total_len == 0 {
            return Ok(());
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
        match fallocate(fd, FallocateFlags::empty(), 0, total_len) {
            Ok(()) => Ok(()),
            Err(Errno::OPNOTSUPP | Errno::NOSYS | Errno::INVAL) => file
                .set_len(total_len)
                .map_err(|error| LocalCopyError::io("preallocate destination file", path, error)),
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
            return Ok(());
        }

        file.set_len(total_len)
            .map_err(|error| LocalCopyError::io("preallocate destination file", path, error))
    }
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

        // File should be preallocated to the requested size
        let metadata = fs::metadata(&path).expect("metadata");
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
        assert_eq!(metadata.len(), one_mib);

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
        assert_eq!(metadata.len(), 4096);

        // Write some content to the preallocated space
        file.write_all(b"hello preallocated world").expect("write");
        file.flush().expect("flush");

        // File should still show the preallocated size, not the written content size
        let metadata = fs::metadata(&path).expect("metadata after write");
        assert_eq!(metadata.len(), 4096);
    }

    /// Verify that preallocating a file that already has some content extends
    /// it to the requested size (the append offset scenario).
    #[test]
    fn preallocate_extends_partially_written_file() {
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
        assert_eq!(metadata.len(), 1);
    }
}
