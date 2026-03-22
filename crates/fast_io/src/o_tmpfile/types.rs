//! Higher-level wrappers around the low-level `O_TMPFILE` syscalls.
//!
//! [`AnonymousTempFile`] owns an anonymous file descriptor and provides a safe
//! `link_to` method for atomic materialization. [`open_temp_file`] is the
//! recommended entry point - it probes once and returns a [`TempFileResult`]
//! that callers can match on without error handling.

use std::fs::File;
use std::io;
use std::path::Path;

use super::low_level;

/// Result of probing `O_TMPFILE` support on a filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OTmpfileSupport {
    /// `O_TMPFILE` is supported on this filesystem.
    Available,
    /// `O_TMPFILE` is not supported (non-Linux, old kernel, or unsupported fs).
    Unavailable,
}

/// Probes whether `O_TMPFILE` is available on the filesystem containing `dir`.
///
/// This is a thin wrapper around [`low_level::o_tmpfile_available`] that returns
/// a typed enum instead of a boolean.
#[must_use]
pub fn o_tmpfile_probe(dir: &Path) -> OTmpfileSupport {
    if low_level::o_tmpfile_available(dir) {
        OTmpfileSupport::Available
    } else {
        OTmpfileSupport::Unavailable
    }
}

/// An anonymous temporary file opened with `O_TMPFILE`.
///
/// The file has no directory entry until [`link_to`](Self::link_to) is called.
/// If dropped without linking, the kernel reclaims the inode automatically -
/// no cleanup is needed.
///
/// # Platform support
///
/// On Linux 3.11+ with a supporting filesystem (ext4, xfs, btrfs, tmpfs),
/// [`open`](Self::open) succeeds. On all other platforms it returns
/// `io::ErrorKind::Unsupported`.
pub struct AnonymousTempFile {
    file: File,
}

impl AnonymousTempFile {
    /// Opens an anonymous temporary file in `dir`.
    ///
    /// The file is created with mode `0o644`. Write data to it via
    /// [`file_mut`](Self::file_mut), then call [`link_to`](Self::link_to)
    /// to atomically materialize it at the destination path.
    ///
    /// # Errors
    ///
    /// Returns an error if `O_TMPFILE` is not supported or the directory is
    /// not writable.
    pub fn open(dir: &Path) -> io::Result<Self> {
        let file = low_level::open_anonymous_tmpfile(dir, 0o644)?;
        Ok(Self { file })
    }

    /// Returns a mutable reference to the underlying file for writing.
    pub fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    /// Consumes the guard and returns the underlying [`File`].
    ///
    /// After calling this, the caller owns the raw fd. If the file is dropped
    /// without being linked via [`low_level::link_anonymous_tmpfile`], the
    /// inode is reclaimed by the kernel.
    pub fn into_file(self) -> File {
        self.file
    }

    /// Atomically materializes the anonymous file at `dest` using `linkat(2)`.
    ///
    /// The destination path must not already exist. To replace an existing
    /// file, remove it first (see [`remove_existing_destination`] in the
    /// engine crate).
    ///
    /// This method consumes `self` because the anonymous fd is no longer
    /// useful after linking - the file now has a directory entry.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `dest` already exists (`EEXIST`)
    /// - `/proc` is not mounted
    /// - The parent directory of `dest` does not exist
    pub fn link_to(self, dest: &Path) -> io::Result<()> {
        low_level::link_anonymous_tmpfile(&self.file, dest)
    }
}

/// Result of attempting to open an anonymous or named temporary file.
///
/// Use [`open_temp_file`] to obtain this. Match on the variant to determine
/// whether the anonymous path is available or the caller should fall back to
/// a named temp file.
pub enum TempFileResult {
    /// An anonymous file was opened successfully via `O_TMPFILE`.
    Anonymous(AnonymousTempFile),
    /// `O_TMPFILE` is not available; caller should use a named temp file.
    Unavailable,
}

/// Attempts to open an anonymous temp file, returning [`TempFileResult::Unavailable`]
/// if `O_TMPFILE` is not supported.
///
/// This is the recommended entry point for the write strategy: probe once,
/// then branch on the result without error handling.
pub fn open_temp_file(dir: &Path) -> TempFileResult {
    match AnonymousTempFile::open(dir) {
        Ok(atf) => TempFileResult::Anonymous(atf),
        Err(_) => TempFileResult::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn probe_returns_valid_enum() {
        let dir = tempdir().expect("tempdir");
        let result = o_tmpfile_probe(dir.path());
        assert!(result == OTmpfileSupport::Available || result == OTmpfileSupport::Unavailable);
    }

    #[test]
    fn probe_nonexistent_dir_is_unavailable() {
        let result = o_tmpfile_probe(Path::new("/nonexistent_o_tmpfile_test_path"));
        assert_eq!(result, OTmpfileSupport::Unavailable);
    }

    #[test]
    fn open_temp_file_returns_result() {
        let dir = tempdir().expect("tempdir");
        let result = open_temp_file(dir.path());
        match result {
            TempFileResult::Anonymous(_) => {}
            TempFileResult::Unavailable => {}
        }
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use super::*;
        use std::io::Write;

        #[test]
        fn anonymous_temp_file_write_and_link() {
            let dir = tempdir().expect("tempdir");
            let mut atf = match AnonymousTempFile::open(dir.path()) {
                Ok(a) => a,
                Err(_) => return, // O_TMPFILE not supported
            };

            atf.file_mut().write_all(b"test data").expect("write");
            let dest = dir.path().join("linked.txt");
            atf.link_to(&dest).expect("link");

            let content = std::fs::read_to_string(&dest).expect("read");
            assert_eq!(content, "test data");
        }

        #[test]
        fn anonymous_temp_file_into_file() {
            let dir = tempdir().expect("tempdir");
            let atf = match AnonymousTempFile::open(dir.path()) {
                Ok(a) => a,
                Err(_) => return,
            };
            let mut file = atf.into_file();
            file.write_all(b"via into_file").expect("write");
        }
    }

    #[cfg(not(target_os = "linux"))]
    mod non_linux {
        use super::*;

        #[test]
        fn open_returns_unsupported() {
            let dir = tempdir().expect("tempdir");
            let err = AnonymousTempFile::open(dir.path()).expect_err("should fail");
            assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        }

        #[test]
        fn open_temp_file_returns_unavailable() {
            let dir = tempdir().expect("tempdir");
            assert!(matches!(
                open_temp_file(dir.path()),
                TempFileResult::Unavailable
            ));
        }
    }
}
