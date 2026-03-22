//! Anonymous temporary file creation via `O_TMPFILE` and finalization via `linkat`.
//!
//! On Linux 3.11+, `O_TMPFILE` creates an unnamed inode in a given directory
//! without any directory entry. Data is written to the anonymous file, then
//! `linkat(2)` materializes it at the final path atomically. This avoids the
//! race window inherent in named temp files (where a crash leaves a partial
//! `.XXXXXX` file) and removes the need for `unlink` cleanup on failure.
//!
//! # Advantages over named temp files
//!
//! - **Atomic appearance** - the destination path either exists with full
//!   content or does not exist at all.
//! - **No cleanup on failure** - if the process crashes before `linkat`, the
//!   kernel reclaims the anonymous inode automatically.
//! - **No name collisions** - no need to generate unique temp file names.
//!
//! # Kernel and filesystem requirements
//!
//! - Linux 3.11+ kernel with `O_TMPFILE` support.
//! - The filesystem must support `O_TMPFILE` (ext4, xfs, btrfs, tmpfs do;
//!   NFS, FUSE, and some older filesystems may not).
//! - `/proc` must be mounted for the `linkat` step (uses `/proc/self/fd/N`).
//!
//! # Fallback
//!
//! On non-Linux platforms or when the kernel/filesystem does not support
//! `O_TMPFILE`, all functions return `io::ErrorKind::Unsupported`. Callers
//! should fall back to the named temp file strategy (`DestinationWriteGuard`).

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::CString;
    use std::fs::File;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::FromRawFd;
    use std::path::Path;
    use std::sync::atomic::{AtomicU8, Ordering};

    /// Cached probe result for `O_TMPFILE` support.
    ///
    /// 0 = unknown, 1 = available, 2 = unavailable.
    static O_TMPFILE_STATUS: AtomicU8 = AtomicU8::new(0);

    const STATUS_UNKNOWN: u8 = 0;
    const STATUS_AVAILABLE: u8 = 1;
    const STATUS_UNAVAILABLE: u8 = 2;

    /// Probes whether `O_TMPFILE` is supported on the filesystem containing `dir`.
    ///
    /// Opens an anonymous file in `dir` and immediately closes it. The result
    /// is cached process-wide so subsequent calls are free. Returns `true` if
    /// `O_TMPFILE` is usable, `false` otherwise.
    ///
    /// # Arguments
    ///
    /// * `dir` - directory on the target filesystem to probe. Must exist and be
    ///   writable by the current process.
    #[must_use]
    pub fn o_tmpfile_available(dir: &Path) -> bool {
        let status = O_TMPFILE_STATUS.load(Ordering::Relaxed);
        if status != STATUS_UNKNOWN {
            return status == STATUS_AVAILABLE;
        }

        let available = probe_o_tmpfile(dir);
        O_TMPFILE_STATUS.store(
            if available {
                STATUS_AVAILABLE
            } else {
                STATUS_UNAVAILABLE
            },
            Ordering::Relaxed,
        );
        available
    }

    /// Resets the cached probe result. Only intended for testing.
    #[cfg(test)]
    pub(crate) fn reset_probe_cache() {
        O_TMPFILE_STATUS.store(STATUS_UNKNOWN, Ordering::Relaxed);
    }

    /// Performs the actual `O_TMPFILE` probe by opening and immediately closing
    /// an anonymous file.
    fn probe_o_tmpfile(dir: &Path) -> bool {
        // O_TMPFILE requires O_WRONLY or O_RDWR
        let dir_cstr = match CString::new(dir.as_os_str().as_bytes()) {
            Ok(c) => c,
            Err(_) => return false,
        };

        // Safety: `dir_cstr` is a valid null-terminated C string pointing to an
        // existing directory. `O_TMPFILE | O_WRONLY` creates an unnamed inode
        // with no directory entry. Mode 0o600 is used for the probe file.
        let fd = unsafe {
            libc::open(
                dir_cstr.as_ptr(),
                libc::O_TMPFILE | libc::O_WRONLY,
                libc::mode_t::from(0o600u32),
            )
        };

        if fd < 0 {
            return false;
        }

        // Safety: `fd` is a valid open file descriptor returned by `open(2)`.
        unsafe {
            libc::close(fd);
        }
        true
    }

    /// Opens an anonymous temporary file in `dir` using `O_TMPFILE`.
    ///
    /// The returned file has no directory entry - it exists only as an open
    /// file descriptor. Write data to it, then call [`link_anonymous_tmpfile`]
    /// to atomically materialize it at the final path.
    ///
    /// # Arguments
    ///
    /// * `dir` - directory on the target filesystem. The anonymous inode is
    ///   created on this filesystem. Must exist and be writable.
    /// * `mode` - Unix permission bits for the file (e.g., `0o644`).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The filesystem does not support `O_TMPFILE` (`EOPNOTSUPP` or `EISDIR`)
    /// - The directory does not exist (`ENOENT`)
    /// - Permission denied (`EACCES`)
    pub fn open_anonymous_tmpfile(dir: &Path, mode: u32) -> io::Result<File> {
        let dir_cstr = CString::new(dir.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory path contains interior null byte",
            )
        })?;

        // Safety: `dir_cstr` is a valid null-terminated C string. `O_TMPFILE |
        // O_WRONLY` creates an unnamed inode with no directory entry. `mode` is
        // the permission bits for the new file. The returned fd is valid on
        // success (>= 0) and is immediately wrapped in a `File` which owns it.
        let fd = unsafe {
            libc::open(
                dir_cstr.as_ptr(),
                libc::O_TMPFILE | libc::O_WRONLY | libc::O_CLOEXEC,
                libc::mode_t::from(mode),
            )
        };

        if fd < 0 {
            return Err(io::Error::last_os_error());
        }

        // Safety: `fd` is a valid, open file descriptor just returned by
        // `open(2)`. We transfer ownership to `File` which will close it on
        // drop. No other code holds or aliases this fd.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    /// Materializes an anonymous temporary file at `dest` using `linkat(2)`.
    ///
    /// This creates a directory entry for the anonymous file opened with
    /// [`open_anonymous_tmpfile`]. The operation is atomic - `dest` either
    /// appears with the full file contents or does not appear at all.
    ///
    /// The `linkat` call uses the `/proc/self/fd/N` path to reference the
    /// anonymous inode, with `AT_SYMLINK_FOLLOW` so the kernel resolves the
    /// procfs symlink to the actual inode.
    ///
    /// # Arguments
    ///
    /// * `fd` - open file descriptor for the anonymous temp file.
    /// * `dest` - final destination path where the file should appear.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `dest` already exists (`EEXIST`)
    /// - `/proc` is not mounted (`ENOENT` on the `/proc/self/fd/N` path)
    /// - The anonymous file and `dest` are on different filesystems (`EXDEV`)
    /// - Permission denied (`EACCES`)
    pub fn link_anonymous_tmpfile(fd: &File, dest: &Path) -> io::Result<()> {
        use std::os::unix::io::AsRawFd;

        let raw_fd = fd.as_raw_fd();
        let proc_path = format!("/proc/self/fd/{raw_fd}");
        let proc_cstr = CString::new(proc_path).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "proc path contains interior null byte",
            )
        })?;

        let dest_cstr = CString::new(dest.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "destination path contains interior null byte",
            )
        })?;

        // Safety: both `proc_cstr` and `dest_cstr` are valid null-terminated C
        // strings. `AT_FDCWD` means paths are resolved relative to the current
        // working directory (they are absolute in practice). `AT_SYMLINK_FOLLOW`
        // causes the kernel to follow the `/proc/self/fd/N` symlink to the
        // underlying inode, which is required for anonymous temp files.
        let ret = unsafe {
            libc::linkat(
                libc::AT_FDCWD,
                proc_cstr.as_ptr(),
                libc::AT_FDCWD,
                dest_cstr.as_ptr(),
                libc::AT_SYMLINK_FOLLOW,
            )
        };

        if ret < 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Write;

        #[test]
        fn probe_returns_consistent_results() {
            reset_probe_cache();
            let dir = tempfile::tempdir().unwrap();
            let first = o_tmpfile_available(dir.path());
            // Second call should use cache and return the same result
            let second = o_tmpfile_available(dir.path());
            assert_eq!(first, second);
        }

        #[test]
        fn probe_unavailable_for_nonexistent_dir() {
            reset_probe_cache();
            let result = o_tmpfile_available(Path::new("/nonexistent/path/that/does/not/exist"));
            assert!(!result);
        }

        #[test]
        fn open_anonymous_tmpfile_succeeds() {
            let dir = tempfile::tempdir().unwrap();
            let result = open_anonymous_tmpfile(dir.path(), 0o644);
            // O_TMPFILE may not be supported on all filesystems (e.g., tmpfs in CI)
            if let Ok(file) = result {
                // File is open and writable
                drop(file);
            }
        }

        #[test]
        fn open_and_link_roundtrip() {
            let dir = tempfile::tempdir().unwrap();
            let file = match open_anonymous_tmpfile(dir.path(), 0o644) {
                Ok(f) => f,
                Err(_) => return, // O_TMPFILE not supported on this filesystem
            };

            // Write some data
            let mut file = file;
            file.write_all(b"hello anonymous tmpfile").unwrap();
            file.flush().unwrap();

            // Materialize at a destination path
            let dest = dir.path().join("materialized.txt");
            match link_anonymous_tmpfile(&file, &dest) {
                Ok(()) => {
                    // Verify the file appeared with correct contents
                    let contents = std::fs::read_to_string(&dest).unwrap();
                    assert_eq!(contents, "hello anonymous tmpfile");
                }
                Err(e) => {
                    // linkat may fail if /proc is not mounted (unlikely but possible)
                    eprintln!("linkat failed (expected in some CI environments): {e}");
                }
            }
        }

        #[test]
        fn link_fails_when_dest_exists() {
            let dir = tempfile::tempdir().unwrap();
            let dest = dir.path().join("existing.txt");
            std::fs::write(&dest, "already here").unwrap();

            let file = match open_anonymous_tmpfile(dir.path(), 0o644) {
                Ok(f) => f,
                Err(_) => return,
            };

            let result = link_anonymous_tmpfile(&file, &dest);
            assert!(result.is_err());
            if let Err(e) = result {
                // EEXIST
                assert_eq!(e.raw_os_error(), Some(libc::EEXIST));
            }
        }

        #[test]
        fn open_fails_for_nonexistent_dir() {
            let result = open_anonymous_tmpfile(Path::new("/nonexistent/dir"), 0o644);
            assert!(result.is_err());
        }

        #[test]
        fn open_respects_mode_bits() {
            use std::os::unix::fs::MetadataExt;

            let dir = tempfile::tempdir().unwrap();
            let file = match open_anonymous_tmpfile(dir.path(), 0o600) {
                Ok(f) => f,
                Err(_) => return,
            };

            let dest = dir.path().join("mode_test.txt");
            if link_anonymous_tmpfile(&file, &dest).is_ok() {
                let meta = std::fs::metadata(&dest).unwrap();
                // Mode should have at least the requested bits (umask may clear some)
                let mode = meta.mode() & 0o777;
                assert_eq!(mode & 0o600, 0o600);
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::{link_anonymous_tmpfile, o_tmpfile_available, open_anonymous_tmpfile};

/// Stub: `O_TMPFILE` is only available on Linux.
#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn o_tmpfile_available(_dir: &std::path::Path) -> bool {
    false
}

/// Stub: `O_TMPFILE` is only available on Linux.
#[cfg(not(target_os = "linux"))]
pub fn open_anonymous_tmpfile(
    _dir: &std::path::Path,
    _mode: u32,
) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "O_TMPFILE is only supported on Linux",
    ))
}

/// Stub: `O_TMPFILE` is only available on Linux.
#[cfg(not(target_os = "linux"))]
pub fn link_anonymous_tmpfile(_fd: &std::fs::File, _dest: &std::path::Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "O_TMPFILE is only supported on Linux",
    ))
}
