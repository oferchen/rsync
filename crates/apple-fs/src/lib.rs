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
