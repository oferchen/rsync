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

    pub fn mkfifo(path: &Path, mode: libc::mode_t) -> io::Result<()> {
        let c_path = path_to_c(path)?;
        let result = unsafe { libc::mkfifo(c_path.as_ptr(), mode) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn mknod(path: &Path, mode: libc::mode_t, device: libc::dev_t) -> io::Result<()> {
        let c_path = path_to_c(path)?;
        let result = unsafe { libc::mknod(c_path.as_ptr(), mode, device) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(unix)]
pub use unix::{mkfifo, mknod};

#[cfg(not(unix))]
pub fn mkfifo(_path: &Path, _mode: ModeType) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "mkfifo is only implemented on Unix platforms",
    ))
}

#[cfg(not(unix))]
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
