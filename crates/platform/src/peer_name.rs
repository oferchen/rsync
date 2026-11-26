//! The numeric host text the C library gives for a socket's peer.
//!
//! A remote-shell daemon whose environment names no usable client address
//! asks fd 0 for its peer and renders it with `getnameinfo(NI_NUMERICHOST)`
//! (upstream: clientname.c:55-84). The text depends on the C library for a
//! peer that is not an IP socket - glibc renders an `AF_UNIX` peer as
//! `localhost`, other libraries may refuse the family - so oc asks the same
//! function rather than guessing its answer.

use std::ffi::CStr;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

use nix::sys::socket::{SockaddrLike, SockaddrStorage, getpeername};

/// Size of upstream's `ipaddr_buf`, the buffer `getnameinfo` writes into.
///
/// upstream: clientname.c:37 - `static char ipaddr_buf[100]`.
const HOST_BUF_LEN: usize = 100;

/// Renders the peer of `fd` with `getnameinfo(NI_NUMERICHOST)`.
///
/// Returns `Ok(Some(text))` with what the C library wrote, and `Ok(None)` when
/// `getnameinfo` fails; upstream ignores that result and keeps the buffer's
/// previous contents (clientname.c:81).
///
/// # Errors
///
/// Returns the `getpeername` error when `fd` is not a connected socket - a
/// pipe, as sshd hands a command (upstream: clientname.c:319-327).
#[allow(unsafe_code)]
pub fn peer_numeric_host(fd: BorrowedFd<'_>) -> io::Result<Option<String>> {
    let peer: SockaddrStorage = getpeername(fd.as_raw_fd())?;
    let mut host = [0 as libc::c_char; HOST_BUF_LEN];
    // SAFETY: `peer.as_ptr()` and `peer.len()` describe the address nix read
    // from `getpeername`, valid for the duration of the call. `host` is an
    // owned buffer and its true length is passed, so getnameinfo writes at
    // most that many bytes, NUL included. No service is requested, so its
    // buffer is null with length 0, as POSIX allows.
    let status = unsafe {
        libc::getnameinfo(
            peer.as_ptr(),
            peer.len(),
            host.as_mut_ptr(),
            HOST_BUF_LEN as libc::socklen_t,
            std::ptr::null_mut(),
            0,
            libc::NI_NUMERICHOST,
        )
    };
    if status != 0 {
        return Ok(None);
    }
    // SAFETY: on success getnameinfo stores a NUL-terminated string within
    // the `HOST_BUF_LEN` bytes of `host`, which outlives this borrow.
    let text = unsafe { CStr::from_ptr(host.as_ptr()) };
    Ok(Some(text.to_string_lossy().into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::os::fd::AsFd;

    /// An IP peer renders as its numeric address.
    #[test]
    fn tcp_peer_renders_as_its_address() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let stream = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        assert_eq!(
            peer_numeric_host(stream.as_fd())
                .expect("getpeername")
                .as_deref(),
            Some("127.0.0.1")
        );
    }

    /// glibc renders an `AF_UNIX` peer as `localhost`; measured with rsync
    /// 3.5.2's `connect from UNKNOWN (localhost)`.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn unix_peer_renders_as_localhost_on_glibc() {
        let (a, _b) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        assert_eq!(
            peer_numeric_host(a.as_fd())
                .expect("getpeername")
                .as_deref(),
            Some("localhost")
        );
    }

    /// A pipe has no peer, so `getpeername` itself fails.
    #[test]
    fn pipe_has_no_peer() {
        let (reader, _writer) = std::io::pipe().expect("pipe");
        assert!(peer_numeric_host(reader.as_fd()).is_err());
    }
}
