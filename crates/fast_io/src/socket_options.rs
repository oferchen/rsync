//! Safe wrappers around platform `setsockopt` for integer-valued options.
//!
//! `socket2` exposes typed setters for the common options (`set_keepalive`,
//! `set_send_buffer_size`, etc.), but a handful of options consumed by
//! `--sockopts` parsing have no typed equivalent (`SO_SNDLOWAT`,
//! `SO_RCVLOWAT`, and the `SO_SNDTIMEO`/`SO_RCVTIMEO` quirk that upstream
//! rsync writes as a plain `int`). This module exposes a single safe entry
//! point that consumer crates can call without breaking their
//! `#![deny(unsafe_code)]` discipline.
//!
//! The module also exposes safe helpers for `TCP_FASTOPEN` (Linux server
//! side) and `TCP_NOTSENT_LOWAT` (Linux and macOS) used by the daemon
//! listener and the client connect path.
//
// upstream: socket.c:set_socket_options() - the OPT_BOOL, OPT_INT, and OPT_ON
// branches all call `setsockopt(fd, level, option, &value, sizeof(int))`, so
// this helper preserves the exact wire semantics for the small number of
// options that have no typed `socket2` setter.

use std::io;
use std::net::{TcpListener, TcpStream};

/// Default queue length for the listener side `TCP_FASTOPEN` socket option.
///
/// Matches `SOMAXCONN` on Linux and is the value most production TFO
/// deployments use. The kernel caches this many pending Fast Open
/// connections in the SYN cookie table.
pub const DEFAULT_TCP_FASTOPEN_QLEN: i32 = 128;

/// Default `TCP_NOTSENT_LOWAT` watermark (64 KiB).
///
/// Limits the unsent bytes the kernel buffers in the socket send buffer.
/// 64 KiB is the value the kernel TCP fast path uses internally and the
/// value recommended by the Linux mailing list for general WAN workloads
/// (low buffer bloat, no measurable throughput loss).
pub const DEFAULT_TCP_NOTSENT_LOWAT: u32 = 64 * 1024;

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
    set_stream_int_option_impl(stream, level, option, value)
}

/// Sets an integer-valued socket option on a `TcpListener`.
///
/// Same semantics as [`set_socket_int_option`] but targets a listener
/// socket. Used by the daemon listener path to enable the server-side
/// `TCP_FASTOPEN` option before `listen(2)`.
///
/// # Errors
///
/// Returns the OS error reported by `setsockopt(2)` (Unix) or `setsockopt`
/// from Winsock (Windows) if the call fails.
pub fn set_listener_int_option(
    listener: &TcpListener,
    level: i32,
    option: i32,
    value: i32,
) -> io::Result<()> {
    set_listener_int_option_impl(listener, level, option, value)
}

/// Enables the server-side `TCP_FASTOPEN` option on a raw socket file
/// descriptor or handle.
///
/// `qlen` is the queue depth the kernel reserves for pending Fast Open
/// connections. A value of zero disables TFO. This call should run before
/// `listen(2)` for best behaviour; the Linux kernel accepts updates after
/// `listen` but the option is most effective when set first.
///
/// Returns `Ok(false)` on platforms where server-side TFO is not
/// implemented (Windows, BSD other than FreeBSD). Returns `Ok(true)` once
/// the kernel accepts the option. Errors from `setsockopt(2)` propagate
/// unchanged.
///
/// upstream: not implemented; this is an oc-rsync-specific perf improvement
/// that is wire-compatible with upstream rsync. TFO only affects the
/// initial SYN exchange, not the rsync protocol that follows.
#[cfg(unix)]
pub fn enable_tcp_fastopen_raw(raw_fd: std::os::fd::RawFd, qlen: i32) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        setsockopt_int_raw(raw_fd, libc::IPPROTO_TCP, libc::TCP_FASTOPEN, qlen)?;
        Ok(true)
    }
    #[cfg(target_os = "freebsd")]
    {
        setsockopt_int_raw(raw_fd, libc::IPPROTO_TCP, libc::TCP_FASTOPEN, qlen)?;
        Ok(true)
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        // Darwin requires a per-process entitlement to enable server-side
        // TFO from user space; treat as unsupported.
        let _ = (raw_fd, qlen);
        Ok(false)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "macos",
        target_os = "ios"
    )))]
    {
        let _ = (raw_fd, qlen);
        Ok(false)
    }
}

/// Windows stub for [`enable_tcp_fastopen_raw`].
///
/// Windows exposes TFO via a separate `WSAIoctl` channel that requires
/// per-process opt-in plus an entitlement, so the listener-side path is
/// reported as unsupported and the daemon falls back to standard accept.
#[cfg(windows)]
pub fn enable_tcp_fastopen_raw(
    _raw_socket: std::os::windows::io::RawSocket,
    _qlen: i32,
) -> io::Result<bool> {
    Ok(false)
}

/// Enables the server-side `TCP_FASTOPEN` option on a listener.
///
/// Convenience wrapper around [`enable_tcp_fastopen_raw`] for callers that
/// already hold a `TcpListener` (e.g. the daemon accept loop applying TFO
/// after the socket has been converted from `socket2::Socket`).
pub fn enable_tcp_fastopen_listener(listener: &TcpListener, qlen: i32) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        enable_tcp_fastopen_raw(listener.as_raw_fd(), qlen)
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        enable_tcp_fastopen_raw(listener.as_raw_socket(), qlen)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (listener, qlen);
        Ok(false)
    }
}

/// Sets the `TCP_NOTSENT_LOWAT` watermark on a connected stream.
///
/// Caps the unsent bytes the kernel buffers in the socket send buffer.
/// On platforms without `TCP_NOTSENT_LOWAT` the call is a no-op and
/// returns `Ok(false)`.
///
/// upstream: not implemented; this is an oc-rsync-specific perf
/// improvement that is wire-compatible with upstream rsync.
pub fn set_tcp_notsent_lowat(stream: &TcpStream, bytes: u32) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        set_socket_int_option(
            stream,
            libc::IPPROTO_TCP,
            libc::TCP_NOTSENT_LOWAT,
            bytes as i32,
        )?;
        Ok(true)
    }
    #[cfg(target_os = "macos")]
    {
        // Hardcoded because libc does not export TCP_NOTSENT_LOWAT for
        // Darwin even though the kernel has supported it since 10.10.
        // upstream Darwin header: /usr/include/netinet/tcp.h - TCP_NOTSENT_LOWAT = 0x201.
        const TCP_NOTSENT_LOWAT_DARWIN: i32 = 0x201;
        set_socket_int_option(
            stream,
            libc::IPPROTO_TCP,
            TCP_NOTSENT_LOWAT_DARWIN,
            bytes as i32,
        )?;
        Ok(true)
    }
    #[cfg(target_os = "freebsd")]
    {
        // FreeBSD: TCP_NOTSENT_LOWAT lands as IPPROTO_TCP option 41.
        const TCP_NOTSENT_LOWAT_FREEBSD: i32 = 41;
        set_socket_int_option(
            stream,
            libc::IPPROTO_TCP,
            TCP_NOTSENT_LOWAT_FREEBSD,
            bytes as i32,
        )?;
        Ok(true)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
    {
        let _ = (stream, bytes);
        Ok(false)
    }
}

/// Disables delayed ACKs for the next ACK on a connected stream
/// (`TCP_QUICKACK`).
///
/// One-shot on Linux: the kernel re-enables delayed ACKs after a single ACK,
/// so this is applied once post-handshake to shave a round trip off the early
/// exchange. On platforms without `TCP_QUICKACK` the call is a no-op and
/// returns `Ok(false)`.
///
/// upstream: not implemented; an oc-rsync-specific perf hint that is
/// wire-compatible with upstream rsync.
pub fn set_tcp_quickack(stream: &TcpStream) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        set_socket_int_option(stream, libc::IPPROTO_TCP, libc::TCP_QUICKACK, 1)?;
        Ok(true)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stream;
        Ok(false)
    }
}

/// Returns `true` when the running platform implements `TCP_QUICKACK`.
#[must_use]
pub const fn tcp_quickack_supported() -> bool {
    cfg!(target_os = "linux")
}

/// Re-arms `TCP_QUICKACK` on a stream that may not be a TCP socket.
///
/// `TCP_QUICKACK` is one-shot on Linux: the kernel re-enables delayed ACKs
/// after the next ACK. Call this before each blocking read in a multi-round
/// handshake so every round's ACK stays immediate. `None` (TLS, connect
/// program, or stdio transports) and non-Linux platforms are no-ops,
/// matching the best-effort apply pattern of the other helpers here.
pub fn rearm_tcp_quickack(stream: Option<&TcpStream>) {
    if let Some(stream) = stream {
        let _ = set_tcp_quickack(stream);
    }
}

/// Caps the kernel send pace for a connected stream to `bytes_per_sec`
/// (`SO_MAX_PACING_RATE`).
///
/// A complementary kernel hint to the userspace token-bucket bandwidth
/// limiter: the limiter stays authoritative for correctness, while the
/// kernel smooths bursts at the NIC. On platforms without
/// `SO_MAX_PACING_RATE` the call is a no-op and returns `Ok(false)`.
///
/// `SO_MAX_PACING_RATE` is a `u32` kernel field; the value is passed
/// through the `int`-sized setsockopt path, which copies the same four
/// bytes the kernel reads, so rates above `i32::MAX` are wire-correct.
///
/// upstream: not implemented; an oc-rsync-specific perf hint that is
/// wire-compatible with upstream rsync.
pub fn set_so_max_pacing_rate(stream: &TcpStream, bytes_per_sec: u32) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        set_socket_int_option(
            stream,
            libc::SOL_SOCKET,
            libc::SO_MAX_PACING_RATE,
            bytes_per_sec as i32,
        )?;
        Ok(true)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (stream, bytes_per_sec);
        Ok(false)
    }
}

/// Returns `true` when the running platform implements `SO_MAX_PACING_RATE`.
#[must_use]
pub const fn so_max_pacing_rate_supported() -> bool {
    cfg!(target_os = "linux")
}

/// Returns `true` when the running platform implements server-side
/// `TCP_FASTOPEN`.
#[must_use]
pub const fn tcp_fastopen_listener_supported() -> bool {
    cfg!(any(target_os = "linux", target_os = "freebsd"))
}

/// Returns `true` when the running platform implements `TCP_NOTSENT_LOWAT`.
#[must_use]
pub const fn tcp_notsent_lowat_supported() -> bool {
    cfg!(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd"
    ))
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn setsockopt_int_raw(raw_fd: libc::c_int, level: i32, option: i32, value: i32) -> io::Result<()> {
    // SAFETY: `raw_fd` is a valid file descriptor borrowed from a live
    // `TcpStream` or `TcpListener` for the duration of this synchronous
    // call. `&value` points to a stack `c_int` that outlives the syscall,
    // and the size argument matches its actual size. `setsockopt` only
    // reads `optval`; it performs no aliasing or allocation.
    let ret = unsafe {
        libc::setsockopt(
            raw_fd,
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

#[cfg(unix)]
fn set_stream_int_option_impl(
    stream: &TcpStream,
    level: i32,
    option: i32,
    value: i32,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    setsockopt_int_raw(stream.as_raw_fd(), level, option, value)
}

#[cfg(unix)]
fn set_listener_int_option_impl(
    listener: &TcpListener,
    level: i32,
    option: i32,
    value: i32,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    setsockopt_int_raw(listener.as_raw_fd(), level, option, value)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn setsockopt_int_raw_winsock(
    raw_socket: windows_sys::Win32::Networking::WinSock::SOCKET,
    level: i32,
    option: i32,
    value: i32,
) -> io::Result<()> {
    use windows_sys::Win32::Networking::WinSock::{SOCKET_ERROR, setsockopt};

    let value_bytes = std::ptr::from_ref(&value).cast::<u8>();
    // SAFETY: `raw_socket` is a valid socket handle borrowed from a live
    // `TcpStream`/`TcpListener` for the duration of this synchronous call.
    // `value_bytes` points to a stack `i32` that outlives the syscall and
    // is read-only for the kernel. The length argument matches its actual
    // size.
    let ret = unsafe {
        setsockopt(
            raw_socket,
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

#[cfg(windows)]
fn set_stream_int_option_impl(
    stream: &TcpStream,
    level: i32,
    option: i32,
    value: i32,
) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;

    use windows_sys::Win32::Networking::WinSock::SOCKET;

    setsockopt_int_raw_winsock(stream.as_raw_socket() as SOCKET, level, option, value)
}

#[cfg(windows)]
fn set_listener_int_option_impl(
    listener: &TcpListener,
    level: i32,
    option: i32,
    value: i32,
) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;

    use windows_sys::Win32::Networking::WinSock::SOCKET;

    setsockopt_int_raw_winsock(listener.as_raw_socket() as SOCKET, level, option, value)
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

    #[test]
    fn set_listener_int_option_sets_send_buffer_size() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");

        #[cfg(unix)]
        let (level, option) = (libc::SOL_SOCKET, libc::SO_SNDBUF);
        #[cfg(windows)]
        let (level, option) = (0xFFFF_i32, 0x1001_i32);

        set_listener_int_option(&listener, level, option, 32_768).expect("setsockopt succeeds");
    }

    #[test]
    fn enable_tcp_fastopen_listener_reports_supported_platforms() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");

        let result = enable_tcp_fastopen_listener(&listener, DEFAULT_TCP_FASTOPEN_QLEN);

        match result {
            Ok(true) => assert!(tcp_fastopen_listener_supported()),
            Ok(false) => assert!(!tcp_fastopen_listener_supported()),
            Err(error) => {
                // Some Linux kernels disable TFO server side
                // (`/proc/sys/net/ipv4/tcp_fastopen` bit 1 unset); accept
                // the EPERM/EOPNOTSUPP path on Linux as well.
                assert!(
                    tcp_fastopen_listener_supported(),
                    "unexpected TFO error on unsupported platform: {error}"
                );
            }
        }
    }

    #[test]
    fn set_tcp_notsent_lowat_returns_supported_flag() {
        let (stream, handle) = connected_stream();

        let result = set_tcp_notsent_lowat(&stream, DEFAULT_TCP_NOTSENT_LOWAT);

        match result {
            Ok(true) => assert!(tcp_notsent_lowat_supported()),
            Ok(false) => assert!(!tcp_notsent_lowat_supported()),
            Err(error) => panic!("unexpected error from TCP_NOTSENT_LOWAT: {error}"),
        }

        drop(stream);
        handle.join().expect("accept thread completes");
    }

    #[test]
    fn set_tcp_quickack_returns_supported_flag() {
        let (stream, handle) = connected_stream();

        match set_tcp_quickack(&stream) {
            Ok(true) => assert!(tcp_quickack_supported()),
            Ok(false) => assert!(!tcp_quickack_supported()),
            Err(error) => panic!("unexpected error from TCP_QUICKACK: {error}"),
        }

        drop(stream);
        handle.join().expect("accept thread completes");
    }

    #[test]
    fn set_so_max_pacing_rate_returns_supported_flag() {
        let (stream, handle) = connected_stream();

        match set_so_max_pacing_rate(&stream, 1_000_000) {
            Ok(true) => assert!(so_max_pacing_rate_supported()),
            Ok(false) => assert!(!so_max_pacing_rate_supported()),
            Err(error) => panic!("unexpected error from SO_MAX_PACING_RATE: {error}"),
        }

        drop(stream);
        handle.join().expect("accept thread completes");
    }

    #[test]
    fn rearm_tcp_quickack_is_noop_on_none_and_some() {
        // None (non-TCP transports) must be a silent no-op.
        rearm_tcp_quickack(None);

        // Some(&stream) re-arms best-effort and must not panic regardless of
        // platform support.
        let (stream, handle) = connected_stream();
        rearm_tcp_quickack(Some(&stream));

        drop(stream);
        handle.join().expect("accept thread completes");
    }
}
