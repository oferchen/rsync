//! Higher-level wrappers around the low-level delete-on-close syscalls.
//!
//! [`WindowsTempFile`] owns a delete-on-close file handle and provides a
//! safe `commit_to` method for atomic materialization. [`open_win_temp_file`]
//! is the recommended entry point - it probes once and returns a
//! [`WinTempFileResult`] that callers can match on without error handling.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use super::low_level;

/// Result of probing `FILE_FLAG_DELETE_ON_CLOSE` support on a filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WinDeleteOnCloseSupport {
    /// Delete-on-close temp files are supported.
    Available,
    /// Delete-on-close is not supported (non-Windows or unsupported fs).
    Unavailable,
}

/// Probes whether delete-on-close temp files are available in `dir`.
///
/// This is a thin wrapper around [`low_level::delete_on_close_available`]
/// that returns a typed enum instead of a boolean.
#[must_use]
pub fn win_tmpfile_probe(dir: &Path) -> WinDeleteOnCloseSupport {
    if low_level::delete_on_close_available(dir) {
        WinDeleteOnCloseSupport::Available
    } else {
        WinDeleteOnCloseSupport::Unavailable
    }
}

/// A temporary file opened with `FILE_FLAG_DELETE_ON_CLOSE`.
///
/// The file is automatically deleted when dropped unless
/// [`commit_to`](Self::commit_to) is called first, which clears the
/// delete-on-close flag and renames the file to the destination.
///
/// # Platform support
///
/// Windows Vista+ with any NTFS, ReFS, or FAT32 filesystem.
/// On all other platforms, [`open`](Self::open) returns
/// `io::ErrorKind::Unsupported`.
pub struct WindowsTempFile {
    file: File,
    temp_path: PathBuf,
}

impl WindowsTempFile {
    /// Opens a delete-on-close temporary file in `dir`.
    ///
    /// The file is created with a unique name and `FILE_FLAG_DELETE_ON_CLOSE`.
    /// Write data to it via [`file_mut`](Self::file_mut), then call
    /// [`commit_to`](Self::commit_to) to atomically materialize it at the
    /// destination path.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory does not exist, is not writable,
    /// or if temp file creation fails.
    pub fn open(dir: &Path) -> io::Result<Self> {
        let (file, temp_path) = low_level::open_delete_on_close_tmpfile(dir)?;
        Ok(Self { file, temp_path })
    }

    /// Returns a mutable reference to the underlying file for writing.
    pub fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    /// Returns the on-disk path of the temporary file.
    ///
    /// Unlike `O_TMPFILE` anonymous inodes, delete-on-close files have a
    /// visible directory entry. This path is useful for diagnostics.
    #[must_use]
    pub fn temp_path(&self) -> &Path {
        &self.temp_path
    }

    /// Consumes the guard and returns the underlying [`File`] and path.
    ///
    /// After calling this, the caller owns the raw handle. If the file is
    /// dropped without clearing the delete-on-close disposition, the kernel
    /// will delete it.
    pub fn into_parts(self) -> (File, PathBuf) {
        (self.file, self.temp_path)
    }

    /// Atomically materializes the temp file at `dest`.
    ///
    /// This method:
    /// 1. Flushes and syncs the file to disk.
    /// 2. Clears the `FILE_FLAG_DELETE_ON_CLOSE` disposition.
    /// 3. Closes the handle.
    /// 4. Renames the temp file to `dest`, replacing any existing file.
    ///
    /// If any step fails, the temp file retains its delete-on-close flag
    /// and will be cleaned up when the handle is closed.
    ///
    /// # Errors
    ///
    /// Returns an error if flushing, clearing the disposition, or renaming
    /// fails.
    pub fn commit_to(self, dest: &Path) -> io::Result<()> {
        let temp_path = self.temp_path.clone();
        low_level::commit_delete_on_close(self.file, &temp_path, dest)
    }
}

/// Result of attempting to open a delete-on-close or named temporary file.
///
/// Use [`open_win_temp_file`] to obtain this. Match on the variant to
/// determine whether the delete-on-close path is available or the caller
/// should fall back to a named temp file.
pub enum WinTempFileResult {
    /// A delete-on-close file was opened successfully.
    DeleteOnClose(WindowsTempFile),
    /// Delete-on-close is not available; caller should use a named temp file.
    Unavailable,
}

/// Attempts to open a delete-on-close temp file, returning
/// [`WinTempFileResult::Unavailable`] if not supported.
///
/// This is the recommended entry point for the write strategy on Windows:
/// probe once, then branch on the result without error handling.
pub fn open_win_temp_file(dir: &Path) -> WinTempFileResult {
    match WindowsTempFile::open(dir) {
        Ok(wtf) => WinTempFileResult::DeleteOnClose(wtf),
        Err(_) => WinTempFileResult::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn probe_returns_valid_enum() {
        let dir = tempdir().expect("tempdir");
        let result = win_tmpfile_probe(dir.path());
        assert!(
            result == WinDeleteOnCloseSupport::Available
                || result == WinDeleteOnCloseSupport::Unavailable
        );
    }

    #[test]
    fn probe_nonexistent_dir_is_unavailable() {
        let result = win_tmpfile_probe(Path::new(
            "/nonexistent_win_tmpfile_test_path_that_does_not_exist",
        ));
        assert_eq!(result, WinDeleteOnCloseSupport::Unavailable);
    }

    #[test]
    fn open_win_temp_file_returns_result() {
        let dir = tempdir().expect("tempdir");
        let result = open_win_temp_file(dir.path());
        match result {
            WinTempFileResult::DeleteOnClose(_) => {}
            WinTempFileResult::Unavailable => {}
        }
    }

    #[cfg(target_os = "windows")]
    mod windows {
        use super::*;
        use std::io::Write;

        #[test]
        fn temp_file_write_and_commit() {
            let dir = tempdir().expect("tempdir");
            let mut wtf = WindowsTempFile::open(dir.path()).expect("open");

            wtf.file_mut().write_all(b"test data").expect("write");
            let dest = dir.path().join("committed.txt");
            wtf.commit_to(&dest).expect("commit");

            let content = std::fs::read_to_string(&dest).expect("read");
            assert_eq!(content, "test data");
        }

        #[test]
        fn temp_file_drop_deletes_file() {
            let dir = tempdir().expect("tempdir");
            let wtf = WindowsTempFile::open(dir.path()).expect("open");
            let path = wtf.temp_path().to_path_buf();
            assert!(path.exists());
            drop(wtf);
            assert!(!path.exists(), "file must be deleted on drop");
        }

        #[test]
        fn temp_file_into_parts() {
            let dir = tempdir().expect("tempdir");
            let wtf = WindowsTempFile::open(dir.path()).expect("open");
            let (file, path) = wtf.into_parts();
            assert!(path.exists());
            drop(file);
            assert!(
                !path.exists(),
                "file must be deleted when parts are dropped"
            );
        }

        #[test]
        fn temp_file_visible_in_directory() {
            let dir = tempdir().expect("tempdir");
            let wtf = WindowsTempFile::open(dir.path()).expect("open");
            // Unlike O_TMPFILE, delete-on-close files have a directory entry.
            let count = std::fs::read_dir(dir.path()).expect("read_dir").count();
            assert!(count >= 1, "temp file must be visible in directory");
            drop(wtf);
        }

        #[test]
        fn commit_replaces_existing_file() {
            let dir = tempdir().expect("tempdir");
            let dest = dir.path().join("existing.txt");
            std::fs::write(&dest, b"old content").expect("create existing");

            let mut wtf = WindowsTempFile::open(dir.path()).expect("open");
            wtf.file_mut().write_all(b"new content").expect("write");
            wtf.commit_to(&dest).expect("commit");

            assert_eq!(std::fs::read_to_string(&dest).expect("read"), "new content");
        }

        #[test]
        fn large_write_integrity() {
            let dir = tempdir().expect("tempdir");
            let size = 2 * 1024 * 1024;
            let pattern: Vec<u8> = (0..=255u8).cycle().take(size).collect();

            let mut wtf = WindowsTempFile::open(dir.path()).expect("open");
            wtf.file_mut().write_all(&pattern).expect("write");

            let dest = dir.path().join("large.bin");
            wtf.commit_to(&dest).expect("commit");

            let data = std::fs::read(&dest).expect("read");
            assert_eq!(data.len(), size);
            assert_eq!(data, pattern);
        }
    }

    #[cfg(not(target_os = "windows"))]
    mod non_windows {
        use super::*;

        #[test]
        fn open_returns_unsupported() {
            let dir = tempdir().expect("tempdir");
            match WindowsTempFile::open(dir.path()) {
                Err(err) => assert_eq!(err.kind(), std::io::ErrorKind::Unsupported),
                Ok(_) => panic!("should fail on non-Windows"),
            }
        }

        #[test]
        fn open_win_temp_file_returns_unavailable() {
            let dir = tempdir().expect("tempdir");
            assert!(matches!(
                open_win_temp_file(dir.path()),
                WinTempFileResult::Unavailable
            ));
        }
    }
}
