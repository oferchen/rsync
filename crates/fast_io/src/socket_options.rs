//! Safe wrappers around platform `setsockopt` for integer-valued options.
//!
//! `socket2` exposes typed setters for the common options (`set_keepalive`,
//! `set_send_buffer_size`, etc.), but a handful of options consumed by
//! `--sockopts` parsing have no typed equivalent (`SO_SNDLOWAT`,
//! `SO_RCVLOWAT`, and the `SO_SNDTIMEO`/`SO_RCVTIMEO` quirk that upstream
//! rsync writes as a plain `int`). This module exposes a single safe entry
//! point that consumer crates can call without breaking their
//! `#![deny(unsafe_code)]` discipline.
//
// upstream: socket.c:set_socket_options() - the OPT_BOOL, OPT_INT, and OPT_ON
// branches all call `setsockopt(fd, level, option, &value, sizeof(int))`, so
// this helper preserves the exact wire semantics for the small number of
// options that have no typed `socket2` setter.

use std::io;
use std::net::TcpStream;

/// Sets an integer-valued socket option on a connected `TcpStream`.
///
/// Mirrors upstream rsync's `setsockopt(fd, level, option, &value, sizeof(int))`
/// call from `socket.c:set_socket_options()`. The `level` and `option`
/// arguments are platform `c_int` values (e.g. `SOL_SOCKET`, `SO_SNDLOWAT`).
///
/// Prefer the typed setters on `socket2::SockRef` (e.g. `set_keepalive`,
/// `set_send_buffer_size`, `set_tcp_nodelay`, `set_tos_v4`) when one exists
/// for the option being set. This helper exists for the small number of
/// options that have no typed equivalent in `socket2`.
///
/// # Errors
///
/// Returns the OS error reported by `setsockopt(2)` (Unix) or `setsockopt`
/// from Winsock (Windows) if the call fails.
pub fn set_socket_int_option(
    stream: &TcpStream,
    level: i32,
    option: i32,
    value: i32,
) -> io::Result<()> {
    set_socket_int_option_impl(stream, level, option, value)
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn set_socket_int_option_impl(
    stream: &TcpStream,
    level: i32,
    option: i32,
    value: i32,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let raw = stream.as_raw_fd();
    // SAFETY: `raw` is a valid file descriptor borrowed from the live
    // `TcpStream` for the duration of this synchronous call. `&value` points
    // to a stack `c_int` that outlives the syscall, and the size argument
    // matches its actual size. `setsockopt` only reads `optval`; it performs
    // no aliasing or allocation.
    let ret = unsafe {
        libc::setsockopt(
            raw,
            level,
            option,
            std::ptr::from_ref(&value).cast::<libc::c_void>(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };

    if ret == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn set_socket_int_option_impl(
    stream: &TcpStream,
    level: i32,
    option: i32,
    value: i32,
) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;

    use windows_sys::Win32::Networking::WinSock::{SOCKET, SOCKET_ERROR, setsockopt};

    let raw = stream.as_raw_socket() as SOCKET;
    let value_bytes = std::ptr::from_ref(&value).cast::<u8>();
    // SAFETY: `raw` is a valid socket handle borrowed from the live
    // `TcpStream` for the duration of this synchronous call. `value_bytes`
    // points to a stack `i32` that outlives the syscall and is read-only for
    // the kernel. The length argument matches its actual size.
    let ret = unsafe {
        setsockopt(
            raw,
            level,
            option,
            value_bytes,
            std::mem::size_of::<i32>() as i32,
        )
    };

    if ret == SOCKET_ERROR {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    fn connected_stream() -> (TcpStream, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let addr = listener.local_addr().expect("addr");
        let handle = std::thread::spawn(move || {
            let _ = listener.accept();
        });
        let stream = TcpStream::connect(addr).expect("connect");
        (stream, handle)
    }

    #[test]
    fn set_socket_int_option_sets_send_buffer_size() {
        let (stream, handle) = connected_stream();

        #[cfg(unix)]
        let (level, option) = (libc::SOL_SOCKET, libc::SO_SNDBUF);
        #[cfg(windows)]
        let (level, option) = (0xFFFF_i32, 0x1001_i32);

        set_socket_int_option(&stream, level, option, 32_768).expect("setsockopt succeeds");

        drop(stream);
        handle.join().expect("accept thread completes");
    }

    #[test]
    fn set_socket_int_option_reports_errors_for_invalid_option() {
        let (stream, handle) = connected_stream();

        let result = set_socket_int_option(&stream, 0xFFFF, -1, 0);
        assert!(result.is_err(), "expected error for invalid option");

        drop(stream);
        handle.join().expect("accept thread completes");
    }
}
