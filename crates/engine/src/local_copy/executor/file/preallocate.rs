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
            Err(Errno::OPNOTSUPP | Errno::NOSYS | Errno::INVAL) => {
                file.set_len(total_len).map_err(|error| {
                    LocalCopyError::io("preallocate destination file", path.to_path_buf(), error)
                })
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
            return Ok(());
        }

        file.set_len(total_len).map_err(|error| {
            LocalCopyError::io("preallocate destination file", path.to_path_buf(), error)
        })
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
}
