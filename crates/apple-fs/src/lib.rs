#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(clippy::undocumented_unsafe_blocks)]
#![doc = include_str!("../README.md")]

use std::io;
use std::path::Path;

#[cfg(not(unix))]
type ModeType = libc::c_uint;
#[cfg(not(unix))]
type DeviceType = libc::c_uint;

#[cfg(unix)]
mod unix {
    use super::{Path, io};
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    fn path_to_c(path: &Path) -> io::Result<CString> {
        CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains interior NUL"))
    }

    pub(super) fn mkfifo(path: &Path, mode: libc::mode_t) -> io::Result<()> {
        let c_path = path_to_c(path)?;
        // SAFETY: `c_path` is a valid, NUL-terminated representation of `path`.
        let result = unsafe { libc::mkfifo(c_path.as_ptr(), mode) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub(super) fn mknod(path: &Path, mode: libc::mode_t, device: libc::dev_t) -> io::Result<()> {
        let c_path = path_to_c(path)?;
        // SAFETY: `c_path` is a valid, NUL-terminated representation of `path`.
        let result = unsafe { libc::mknod(c_path.as_ptr(), mode, device) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(unix)]
#[cfg_attr(docsrs, doc(cfg(unix)))]
/// Creates a FIFO special file at `path` using the requested `mode`.
///
/// The helper mirrors the behaviour of `mkfifo(3)` and is only available on
/// Unix platforms. The function returns an error when the path cannot be
/// represented as a C string or when the underlying syscall fails.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] if the provided path contains an
/// interior NUL byte. Other error kinds bubble up from the `mkfifo(3)` call,
/// such as [`io::ErrorKind::AlreadyExists`] or [`io::ErrorKind::PermissionDenied`].
///
/// # Examples
///
/// ```rust
/// # #[cfg(unix)] {
/// use std::env;
/// use std::fs;
/// use std::os::unix::fs::FileTypeExt;
/// use std::time::{SystemTime, UNIX_EPOCH};
///
/// let unique = SystemTime::now()
///     .duration_since(UNIX_EPOCH)
///     .unwrap()
///     .as_nanos();
/// let path = env::temp_dir().join(format!("rsync_fifo_{unique}"));
/// # let _ = fs::remove_file(&path);
/// apple_fs::mkfifo(&path, 0o600).unwrap();
/// let metadata = fs::metadata(&path).unwrap();
/// assert!(metadata.file_type().is_fifo());
/// fs::remove_file(&path).unwrap();
/// # }
/// ```
pub fn mkfifo(path: &Path, mode: libc::mode_t) -> io::Result<()> {
    unix::mkfifo(path, mode)
}

#[cfg(unix)]
#[cfg_attr(docsrs, doc(cfg(unix)))]
/// Creates a filesystem node at `path` with the supplied `mode` and `device`.
///
/// This wrapper exposes the subset of `mknod(2)` used by the rsync
/// implementation. Passing `libc::S_IFIFO` as the mode creates a named pipe.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] if the path cannot be converted to a
/// C string. Other failures surface directly from the `mknod(2)` syscall.
///
/// # Examples
///
/// ```rust
/// # #[cfg(unix)] {
/// use std::env;
/// use std::fs;
/// use std::os::unix::fs::FileTypeExt;
/// use std::time::{SystemTime, UNIX_EPOCH};
///
/// let unique = SystemTime::now()
///     .duration_since(UNIX_EPOCH)
///     .unwrap()
///     .as_nanos();
/// let path = env::temp_dir().join(format!("rsync_mknod_{unique}"));
/// # let _ = fs::remove_file(&path);
/// apple_fs::mknod(&path, libc::S_IFIFO | 0o600, 0).unwrap();
/// let metadata = fs::metadata(&path).unwrap();
/// assert!(metadata.file_type().is_fifo());
/// fs::remove_file(&path).unwrap();
/// # }
/// ```
pub fn mknod(path: &Path, mode: libc::mode_t, device: libc::dev_t) -> io::Result<()> {
    unix::mknod(path, mode, device)
}

#[cfg(not(unix))]
/// Stub implementation that reports the lack of FIFO support on non-Unix
/// platforms.
///
/// # Errors
///
/// Always returns an [`io::ErrorKind::Unsupported`] error to mirror the
/// behaviour of upstream rsync on unsupported targets.
pub fn mkfifo(_path: &Path, _mode: ModeType) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "mkfifo is only implemented on Unix platforms",
    ))
}

#[cfg(not(unix))]
/// Stub implementation that reports the lack of `mknod` support on non-Unix
/// platforms.
///
/// # Errors
///
/// Always returns an [`io::ErrorKind::Unsupported`] error.
pub fn mknod(_path: &Path, _mode: ModeType, _device: DeviceType) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "mknod is only implemented on Unix platforms",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    fn unique_path(prefix: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        env::temp_dir().join(format!("{prefix}_{unique}"))
    }

    #[cfg(unix)]
    #[test]
    fn mkfifo_creates_named_pipe() -> io::Result<()> {
        use std::os::unix::fs::FileTypeExt;

        let path = unique_path("rsync_fifo");
        mkfifo(&path, 0o600)?;
        let metadata = fs::metadata(&path)?;
        assert!(metadata.file_type().is_fifo());
        fs::remove_file(&path)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn mknod_creates_fifo_when_requested() -> io::Result<()> {
        use std::os::unix::fs::FileTypeExt;

        let path = unique_path("rsync_mknod");
        mknod(&path, libc::S_IFIFO | 0o600, 0)?;
        let metadata = fs::metadata(&path)?;
        assert!(metadata.file_type().is_fifo());
        fs::remove_file(&path)?;
        Ok(())
    }

    #[cfg(not(unix))]
    #[test]
    fn non_unix_platforms_report_unsupported_operations() {
        let path = Path::new("nonexistent");
        assert_eq!(
            mkfifo(path, 0).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            mknod(path, 0, 0).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }
}
