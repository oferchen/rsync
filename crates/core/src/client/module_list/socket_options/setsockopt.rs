//! Platform-specific `setsockopt` wrappers.

use std::io;
use std::net::TcpStream;

/// Sets an integer-valued socket option (Unix).
///
/// # Safety
/// Calls `libc::setsockopt` with a valid fd from the `TcpStream`.
#[cfg(unix)]
#[allow(unsafe_code)]
pub(super) fn set_socket_option_int(
    stream: &TcpStream,
    level: libc::c_int,
    option: libc::c_int,
    value: libc::c_int,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let raw = stream.as_raw_fd();
    // SAFETY: `raw` is a valid file descriptor from the TcpStream.
    // `value` is a local variable with a valid address for the duration of the call.
    // The size parameter matches the actual size of the value.
    let ret = unsafe {
        libc::setsockopt(
            raw,
            level,
            option,
            &value as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };

    if ret == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Sets an integer-valued socket option (Windows).
///
/// # Safety
/// Calls `libc::setsockopt` with a valid socket handle from the `TcpStream`.
#[cfg(windows)]
#[allow(unsafe_code)]
pub(super) fn set_socket_option_int(
    stream: &TcpStream,
    level: libc::c_int,
    option: libc::c_int,
    value: libc::c_int,
) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;

    use super::consts::SOCKET_ERROR;

    let raw = stream.as_raw_socket();
    // SAFETY: `raw` is a valid socket handle from the TcpStream.
    // `value` is a local variable with a valid address for the duration of the call.
    // The size parameter matches the actual size of the value.
    let ret = unsafe {
        libc::setsockopt(
            raw as libc::SOCKET,
            level,
            option,
            &value as *const libc::c_int as *const libc::c_char,
            std::mem::size_of::<libc::c_int>() as libc::c_int,
        )
    };

    if ret == SOCKET_ERROR {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
