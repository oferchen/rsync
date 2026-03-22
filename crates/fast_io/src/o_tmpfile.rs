//! Anonymous temporary file creation using `O_TMPFILE` with automatic fallback.
//!
//! This module provides anonymous temporary file creation using Linux's `O_TMPFILE`
//! flag (Linux 3.11+, ext4/XFS/Btrfs), with automatic fallback to named temporary
//! files on other platforms or unsupported filesystems.
//!
//! # How It Works
//!
//! `O_TMPFILE` creates a file with no directory entry. The file exists only as an
//! open file descriptor in `/proc/self/fd/<n>`. To persist it, the caller links it
//! to a final path via `linkat(AT_SYMLINK_FOLLOW)` on the proc symlink. If the
//! process crashes before linking, the kernel reclaims the inode automatically -
//! no orphaned temp files.
//!
//! # Platform Support
//!
//! - **Linux 3.11+**: Uses `O_TMPFILE` + `linkat` for anonymous temp files
//! - **Other platforms**: Returns `Unsupported` error; caller falls back to named temp files
//!
//! # Upstream Reference
//!
//! upstream rsync does not currently use `O_TMPFILE`, but this optimization eliminates
//! the visible `.~tmp~` files that can confuse backup tools and directory watchers.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// Result of probing `O_TMPFILE` support on a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OTmpfileSupport {
    /// `O_TMPFILE` is available on this filesystem.
    Available,
    /// `O_TMPFILE` is not supported (wrong platform, old kernel, or unsupported filesystem).
    Unavailable,
}

/// Probes whether `O_TMPFILE` is supported on the filesystem containing `dir`.
///
/// This opens an anonymous file in `dir` with `O_TMPFILE | O_WRONLY` and immediately
/// closes it. The probe result can be cached per mount point.
///
/// # Arguments
///
/// * `dir` - Directory to probe. Must exist and be writable.
///
/// # Returns
///
/// `OTmpfileSupport::Available` if `O_TMPFILE` works, `Unavailable` otherwise.
///
/// # Example
///
/// ```no_run
/// use fast_io::o_tmpfile::{o_tmpfile_probe, OTmpfileSupport};
/// use std::path::Path;
///
/// let support = o_tmpfile_probe(Path::new("/tmp"));
/// if support == OTmpfileSupport::Available {
///     println!("O_TMPFILE is supported");
/// }
/// ```
#[cfg(target_os = "linux")]
pub fn o_tmpfile_probe(dir: &Path) -> OTmpfileSupport {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    // O_TMPFILE = 0o20000000 | O_DIRECTORY = 0o200000
    // Combined: 0o20200000
    const O_TMPFILE: libc::c_int = 0o20_200_000;

    let c_path = match CString::new(dir.as_os_str().as_bytes()) {
        Ok(p) => p,
        Err(_) => return OTmpfileSupport::Unavailable,
    };

    // SAFETY: open() with O_TMPFILE creates an anonymous file. We close it immediately.
    let fd = unsafe { libc::open(c_path.as_ptr(), O_TMPFILE | libc::O_WRONLY, 0o600) };

    if fd >= 0 {
        // SAFETY: fd is valid, we just opened it.
        unsafe {
            libc::close(fd);
        }
        OTmpfileSupport::Available
    } else {
        OTmpfileSupport::Unavailable
    }
}

/// Stub for non-Linux platforms. Always returns `Unavailable`.
#[cfg(not(target_os = "linux"))]
pub fn o_tmpfile_probe(_dir: &Path) -> OTmpfileSupport {
    OTmpfileSupport::Unavailable
}

/// An anonymous temporary file created via `O_TMPFILE`.
///
/// The file has no directory entry and exists only as an open file descriptor.
/// To persist the file, call [`link_to`](Self::link_to) which uses
/// `linkat(AT_SYMLINK_FOLLOW)` on `/proc/self/fd/<n>` to atomically create
/// a directory entry.
///
/// If the `AnonymousTempFile` is dropped without linking, the kernel reclaims
/// the inode automatically - no cleanup needed.
///
/// # Linux Only
///
/// This type is only constructible on Linux 3.11+ with a filesystem that supports
/// `O_TMPFILE` (ext4, XFS, Btrfs, tmpfs). Use [`o_tmpfile_probe`] to check first.
pub struct AnonymousTempFile {
    file: File,
    dir: PathBuf,
}

/// Opens an anonymous temporary file in `dir`.
///
/// The file is created with mode 0o600 and no directory entry.
///
/// # Arguments
///
/// * `dir` - Directory on whose filesystem the anonymous file is created.
///   Must exist, be writable, and reside on a filesystem supporting `O_TMPFILE`.
///
/// # Errors
///
/// Returns an error if:
/// - `O_TMPFILE` is not supported on this filesystem
/// - The directory does not exist or is not writable
/// - The filesystem is full
impl AnonymousTempFile {
    /// Opens an anonymous temporary file in `dir`.
    #[cfg(target_os = "linux")]
    pub fn open(dir: &Path) -> io::Result<Self> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::io::FromRawFd;

        const O_TMPFILE: libc::c_int = 0o20_200_000;

        let c_path = CString::new(dir.as_os_str().as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        // SAFETY: open() with O_TMPFILE | O_RDWR creates an anonymous file.
        let fd = unsafe { libc::open(c_path.as_ptr(), O_TMPFILE | libc::O_RDWR, 0o600) };

        if fd < 0 {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: fd is a valid, newly opened file descriptor.
        let file = unsafe { File::from_raw_fd(fd) };

        Ok(Self {
            file,
            dir: dir.to_path_buf(),
        })
    }

    /// Stub for non-Linux platforms. Always returns an error.
    #[cfg(not(target_os = "linux"))]
    pub fn open(_dir: &Path) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "O_TMPFILE is only available on Linux",
        ))
    }

    /// Returns a reference to the underlying file for reading.
    pub fn file(&self) -> &File {
        &self.file
    }

    /// Returns a mutable reference to the underlying file for writing.
    pub fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    /// Consumes self and returns the underlying `File`.
    pub fn into_file(self) -> File {
        self.file
    }

    /// Returns the directory this anonymous file resides on.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Links the anonymous file to `dest`, making it visible in the filesystem.
    ///
    /// This uses `linkat(AT_FDCWD, "/proc/self/fd/<n>", AT_FDCWD, dest, AT_SYMLINK_FOLLOW)`
    /// to atomically create a directory entry pointing to the anonymous inode.
    /// The `/proc/self/fd/<n>` path is a symlink to the anonymous inode, so
    /// `AT_SYMLINK_FOLLOW` resolves it to the actual file.
    ///
    /// The caller should ensure `dest` does not already exist, or remove it first.
    /// After linking, this `AnonymousTempFile` still holds the file descriptor;
    /// dropping it simply closes the fd (the directory entry persists).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `dest` already exists (`AlreadyExists`)
    /// - The destination directory does not exist
    /// - Cross-device link (anonymous file and dest on different filesystems)
    #[cfg(target_os = "linux")]
    pub fn link_to(&self, dest: &Path) -> io::Result<()> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::io::AsRawFd;

        let fd = self.file.as_raw_fd();
        let proc_path = format!("/proc/self/fd/{fd}");
        let c_proc = CString::new(proc_path.as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let c_dest = CString::new(dest.as_os_str().as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        // AT_SYMLINK_FOLLOW resolves the /proc/self/fd/<n> symlink to the
        // anonymous inode. This works without CAP_DAC_READ_SEARCH, unlike
        // AT_EMPTY_PATH which requires it.
        const AT_SYMLINK_FOLLOW: libc::c_int = 0x400;

        // SAFETY: linkat with valid C-string paths and AT_FDCWD for both dir fds.
        let ret = unsafe {
            libc::linkat(
                libc::AT_FDCWD,
                c_proc.as_ptr(),
                libc::AT_FDCWD,
                c_dest.as_ptr(),
                AT_SYMLINK_FOLLOW,
            )
        };

        if ret == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Stub for non-Linux platforms. Always returns an error.
    #[cfg(not(target_os = "linux"))]
    pub fn link_to(&self, _dest: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "linkat for anonymous temp files is only available on Linux",
        ))
    }
}

/// Creates a file at `dest` using `O_TMPFILE` if available, otherwise indicates
/// that the caller should fall back to a named temporary file.
///
/// This is the main entry point for anonymous temp file support. On success,
/// the caller receives an anonymous file. On failure, the caller should use
/// `DestinationWriteGuard` for the traditional named temp file approach.
///
/// # Arguments
///
/// * `dir` - Directory to create the anonymous file in (should be same filesystem as dest)
///
/// # Returns
///
/// A `TempFileResult` indicating which strategy was used.
pub fn open_temp_file(dir: &Path) -> TempFileResult {
    if o_tmpfile_probe(dir) == OTmpfileSupport::Available {
        match AnonymousTempFile::open(dir) {
            Ok(atf) => return TempFileResult::Anonymous(atf),
            Err(_) => {}
        }
    }
    TempFileResult::Unavailable
}

/// Result of attempting to open an anonymous temp file.
#[derive(Debug)]
pub enum TempFileResult {
    /// Successfully created an anonymous temp file via `O_TMPFILE`.
    Anonymous(AnonymousTempFile),
    /// `O_TMPFILE` is not available; caller should fall back to named temp file.
    Unavailable,
}

impl std::fmt::Debug for AnonymousTempFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnonymousTempFile")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn o_tmpfile_probe_returns_result_for_tmp() {
        let dir = tempdir().expect("tempdir");
        let result = o_tmpfile_probe(dir.path());
        // On Linux with ext4/XFS/Btrfs/tmpfs this is Available.
        // On other platforms or unsupported fs, Unavailable.
        assert!(
            result == OTmpfileSupport::Available || result == OTmpfileSupport::Unavailable,
            "probe must return a valid variant"
        );
    }

    #[test]
    fn o_tmpfile_probe_unavailable_for_nonexistent_dir() {
        let result = o_tmpfile_probe(Path::new("/nonexistent_dir_for_probe_test"));
        assert_eq!(result, OTmpfileSupport::Unavailable);
    }

    #[cfg(target_os = "linux")]
    mod linux_tests {
        use super::*;

        #[test]
        fn anonymous_temp_file_open_and_write() {
            let dir = tempdir().expect("tempdir");
            if o_tmpfile_probe(dir.path()) != OTmpfileSupport::Available {
                return;
            }

            let mut atf = AnonymousTempFile::open(dir.path()).expect("open anonymous temp file");
            atf.file_mut()
                .write_all(b"anonymous content")
                .expect("write to anonymous file");
            assert_eq!(atf.dir(), dir.path());
        }

        #[test]
        fn anonymous_temp_file_is_invisible() {
            let dir = tempdir().expect("tempdir");
            if o_tmpfile_probe(dir.path()) != OTmpfileSupport::Available {
                return;
            }

            let _atf = AnonymousTempFile::open(dir.path()).expect("open anonymous temp file");

            let entries: Vec<_> = std::fs::read_dir(dir.path()).expect("read dir").collect();
            assert!(
                entries.is_empty(),
                "anonymous temp file should not appear in directory listing, found {} entries",
                entries.len()
            );
        }

        #[test]
        fn anonymous_temp_file_link_to_destination() {
            let dir = tempdir().expect("tempdir");
            if o_tmpfile_probe(dir.path()) != OTmpfileSupport::Available {
                return;
            }

            let mut atf = AnonymousTempFile::open(dir.path()).expect("open");
            atf.file_mut().write_all(b"linked content").expect("write");

            let dest = dir.path().join("final.txt");
            atf.link_to(&dest).expect("link_to");

            let content = std::fs::read_to_string(&dest).expect("read linked file");
            assert_eq!(content, "linked content");
        }

        #[test]
        fn anonymous_temp_file_link_to_preserves_content_after_large_write() {
            let dir = tempdir().expect("tempdir");
            if o_tmpfile_probe(dir.path()) != OTmpfileSupport::Available {
                return;
            }

            let mut atf = AnonymousTempFile::open(dir.path()).expect("open");

            // Write 1 MB of patterned data to verify integrity.
            let pattern: Vec<u8> = (0..=255u8).cycle().take(1024 * 1024).collect();
            atf.file_mut().write_all(&pattern).expect("write large");

            let dest = dir.path().join("large_file.bin");
            atf.link_to(&dest).expect("link_to");

            let read_back = std::fs::read(&dest).expect("read back");
            assert_eq!(read_back.len(), pattern.len());
            assert_eq!(read_back, pattern);
        }

        #[test]
        fn anonymous_temp_file_drop_without_link_leaves_no_orphan() {
            let dir = tempdir().expect("tempdir");
            if o_tmpfile_probe(dir.path()) != OTmpfileSupport::Available {
                return;
            }

            {
                let mut atf = AnonymousTempFile::open(dir.path()).expect("open");
                atf.file_mut().write_all(b"orphan test").expect("write");
            }

            let entries: Vec<_> = std::fs::read_dir(dir.path()).expect("read dir").collect();
            assert!(
                entries.is_empty(),
                "dropping anonymous temp file without link must leave no orphan, found {} entries",
                entries.len()
            );
        }

        #[test]
        fn anonymous_temp_file_link_fails_if_dest_exists() {
            let dir = tempdir().expect("tempdir");
            if o_tmpfile_probe(dir.path()) != OTmpfileSupport::Available {
                return;
            }

            let dest = dir.path().join("existing.txt");
            std::fs::write(&dest, b"existing").expect("create existing");

            let atf = AnonymousTempFile::open(dir.path()).expect("open");
            let result = atf.link_to(&dest);
            assert!(result.is_err(), "link_to should fail when dest exists");
        }

        #[test]
        fn anonymous_temp_file_link_fails_for_nonexistent_parent() {
            let dir = tempdir().expect("tempdir");
            if o_tmpfile_probe(dir.path()) != OTmpfileSupport::Available {
                return;
            }

            let atf = AnonymousTempFile::open(dir.path()).expect("open");
            let bad_dest = dir.path().join("no_such_dir").join("file.txt");
            let result = atf.link_to(&bad_dest);
            assert!(
                result.is_err(),
                "link_to should fail for nonexistent parent dir"
            );
        }

        #[test]
        fn anonymous_temp_file_into_file_returns_writable_fd() {
            let dir = tempdir().expect("tempdir");
            if o_tmpfile_probe(dir.path()) != OTmpfileSupport::Available {
                return;
            }

            let atf = AnonymousTempFile::open(dir.path()).expect("open");
            let mut file = atf.into_file();
            file.write_all(b"via into_file")
                .expect("write via into_file");
        }

        #[test]
        fn open_temp_file_returns_anonymous_when_supported() {
            let dir = tempdir().expect("tempdir");
            if o_tmpfile_probe(dir.path()) != OTmpfileSupport::Available {
                return;
            }

            let result = open_temp_file(dir.path());
            assert!(
                matches!(result, TempFileResult::Anonymous(_)),
                "open_temp_file should return Anonymous on supported filesystem"
            );
        }

        #[test]
        fn open_temp_file_anonymous_write_and_link() {
            let dir = tempdir().expect("tempdir");
            if o_tmpfile_probe(dir.path()) != OTmpfileSupport::Available {
                return;
            }

            let result = open_temp_file(dir.path());
            if let TempFileResult::Anonymous(mut atf) = result {
                atf.file_mut()
                    .write_all(b"via open_temp_file")
                    .expect("write");
                let dest = dir.path().join("output.txt");
                atf.link_to(&dest).expect("link");
                let content = std::fs::read_to_string(&dest).expect("read");
                assert_eq!(content, "via open_temp_file");
            } else {
                panic!("expected Anonymous variant");
            }
        }

        #[test]
        fn multiple_anonymous_files_in_same_directory() {
            let dir = tempdir().expect("tempdir");
            if o_tmpfile_probe(dir.path()) != OTmpfileSupport::Available {
                return;
            }

            let mut files = Vec::new();
            for i in 0..5 {
                let mut atf = AnonymousTempFile::open(dir.path()).expect("open");
                atf.file_mut()
                    .write_all(format!("file_{i}").as_bytes())
                    .expect("write");
                files.push(atf);
            }

            let count = std::fs::read_dir(dir.path()).expect("read_dir").count();
            assert_eq!(count, 0, "all files should be invisible");

            for (i, atf) in files.into_iter().enumerate() {
                let dest = dir.path().join(format!("file_{i}.txt"));
                atf.link_to(&dest).expect("link_to");
            }

            for i in 0..5 {
                let dest = dir.path().join(format!("file_{i}.txt"));
                let content = std::fs::read_to_string(&dest).expect("read");
                assert_eq!(content, format!("file_{i}"));
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    mod non_linux_tests {
        use super::*;

        #[test]
        fn o_tmpfile_probe_always_unavailable() {
            let dir = tempdir().expect("tempdir");
            assert_eq!(o_tmpfile_probe(dir.path()), OTmpfileSupport::Unavailable);
        }

        #[test]
        fn anonymous_temp_file_open_returns_error() {
            let dir = tempdir().expect("tempdir");
            let result = AnonymousTempFile::open(dir.path());
            assert!(result.is_err());
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
        }

        #[test]
        fn open_temp_file_returns_unavailable() {
            let dir = tempdir().expect("tempdir");
            let result = open_temp_file(dir.path());
            assert!(
                matches!(result, TempFileResult::Unavailable),
                "open_temp_file should return Unavailable on non-Linux"
            );
        }
    }

    #[test]
    fn o_tmpfile_support_debug_display() {
        let available = OTmpfileSupport::Available;
        let unavailable = OTmpfileSupport::Unavailable;
        assert_eq!(format!("{available:?}"), "Available");
        assert_eq!(format!("{unavailable:?}"), "Unavailable");
    }

    #[test]
    fn o_tmpfile_support_clone_and_eq() {
        let a = OTmpfileSupport::Available;
        let b = a;
        assert_eq!(a, b);

        let c = OTmpfileSupport::Unavailable;
        assert_ne!(a, c);
    }

    #[test]
    fn anonymous_temp_file_debug_format() {
        #[cfg(target_os = "linux")]
        {
            let dir = tempdir().expect("tempdir");
            if o_tmpfile_probe(dir.path()) == OTmpfileSupport::Available {
                let atf = AnonymousTempFile::open(dir.path()).expect("open");
                let debug = format!("{atf:?}");
                assert!(debug.contains("AnonymousTempFile"));
            }
        }
    }
}
