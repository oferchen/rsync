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

/// Sets an integer-valued socket option on a borrowed raw file descriptor.
///
/// Same `setsockopt(fd, level, option, &value, sizeof(int))` semantics as
/// [`set_socket_int_option`], but accepts a `RawFd` so a caller holding a
/// `socket2::SockRef` (which unifies the listener and stream apply paths) can
/// set the handful of options that have no typed `socket2` equivalent
/// (`SO_SNDLOWAT`, `SO_RCVLOWAT`, and the `SO_SNDTIMEO`/`SO_RCVTIMEO` quirk).
///
/// # Errors
///
/// Returns the OS error reported by `setsockopt(2)` if the call fails.
#[cfg(unix)]
pub fn set_socket_int_option_raw(
    fd: std::os::fd::RawFd,
    level: i32,
    option: i32,
    value: i32,
) -> io::Result<()> {
    setsockopt_int_raw(fd, level, option, value)
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

/// Enables the client-side `TCP_FASTOPEN_CONNECT` option on a socket file
/// descriptor before `connect(2)`.
///
/// Unlike the server-side [`enable_tcp_fastopen_raw`], this is set on the
/// connecting socket. The Linux kernel (4.11+) then defers the SYN until the
/// first `write(2)`/`send(2)`, folding the request payload into the initial
/// handshake so the connect saves one round trip. The regular
/// `connect`/`write` flow works unchanged; no `sendto(MSG_FASTOPEN)` adapter
/// is required.
///
/// Returns `Ok(false)` on non-Linux platforms (the option does not exist;
/// macOS uses `connectx`, which is wired separately). Errors from
/// `setsockopt(2)` propagate unchanged.
///
/// upstream: not implemented; this is an oc-rsync-specific perf improvement
/// that is wire-compatible with upstream rsync. TFO only affects the initial
/// SYN exchange, not the rsync protocol that follows.
#[cfg(unix)]
pub fn enable_tcp_fastopen_connect_raw(raw_fd: std::os::fd::RawFd) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        setsockopt_int_raw(raw_fd, libc::IPPROTO_TCP, libc::TCP_FASTOPEN_CONNECT, 1)?;
        Ok(true)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = raw_fd;
        Ok(false)
    }
}

/// Windows stub for [`enable_tcp_fastopen_connect_raw`].
///
/// Windows TFO is opt-in through a separate `WSAIoctl`/overlapped path, so the
/// connect-side option is reported as unsupported and the client falls back to
/// a standard `connect`.
#[cfg(windows)]
pub fn enable_tcp_fastopen_connect_raw(
    _raw_socket: std::os::windows::io::RawSocket,
) -> io::Result<bool> {
    Ok(false)
}

/// Returns `true` when the running platform implements client-side
/// `TCP_FASTOPEN_CONNECT`.
#[must_use]
pub const fn tcp_fastopen_connect_supported() -> bool {
    cfg!(target_os = "linux")
}

/// Returns `true` when the running platform implements client-side TCP Fast
/// Open through the Darwin `connectx(2)` path.
///
/// Darwin has no `TCP_FASTOPEN_CONNECT` socket option; instead the connect
/// itself is issued via `connectx` with `CONNECT_RESUME_ON_READ_WRITE`, which
/// defers the SYN until the first `write(2)` and folds the request payload
/// into the handshake. This mirrors the Linux `TCP_FASTOPEN_CONNECT` gate in
/// [`tcp_fastopen_connect_supported`], but keyed on macOS.
#[must_use]
pub const fn connectx_fastopen_supported() -> bool {
    cfg!(target_os = "macos")
}

/// Issues a client-side TCP Fast Open connect to `target` on the Darwin
/// `connectx(2)` path.
///
/// This is the macOS analogue of the Linux `TCP_FASTOPEN_CONNECT` socket
/// option set in [`enable_tcp_fastopen_connect_raw`]: instead of a
/// `setsockopt` that makes a later `connect(2)` defer the SYN, Darwin folds
/// the request into `connectx` itself. Called with `SAE_ASSOCID_ANY`, the
/// `CONNECT_RESUME_ON_READ_WRITE` flag, and no `iovec` payload, so the SYN is
/// deferred to the first `write(2)`/`send(2)` and the standard connect/write
/// flow that follows works unchanged - exactly like the Linux path.
///
/// `raw_fd` must be an unconnected TCP stream socket of the same address
/// family as `target`. Returns `Ok(true)` once the kernel accepts the
/// (deferred) connect. A non-blocking or in-progress result (`EINPROGRESS`)
/// is reported as success because the SYN is intentionally deferred. Any
/// other `connectx` failure propagates as the OS error so the caller can fall
/// back to a normal blocking `connect(2)`; TFO is a latency optimisation and
/// must never break connectivity.
///
/// Returns `Ok(false)` on non-macOS platforms (the `connectx` syscall does
/// not exist there).
///
/// upstream: not implemented; this is an oc-rsync-specific perf improvement
/// that is wire-compatible with upstream rsync. TFO only affects the initial
/// SYN exchange, not the rsync protocol that follows.
#[cfg(unix)]
pub fn connectx_fastopen_raw(
    raw_fd: std::os::fd::RawFd,
    target: &std::net::SocketAddr,
) -> io::Result<bool> {
    #[cfg(target_os = "macos")]
    {
        connectx_fastopen_darwin(raw_fd, target)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (raw_fd, target);
        Ok(false)
    }
}

/// Windows stub for [`connectx_fastopen_raw`]. `connectx` is Darwin-only, so
/// Windows always reports the path as unavailable and the caller performs a
/// standard `connect`.
#[cfg(windows)]
pub fn connectx_fastopen_raw(
    _raw_socket: std::os::windows::io::RawSocket,
    _target: &std::net::SocketAddr,
) -> io::Result<bool> {
    Ok(false)
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn connectx_fastopen_darwin(
    raw_fd: std::os::fd::RawFd,
    target: &std::net::SocketAddr,
) -> io::Result<bool> {
    use std::net::SocketAddr;

    // Materialise the destination into a `sockaddr_storage` so the pointer
    // handed to `connectx` outlives the syscall. `connectx` only reads the
    // destination address bytes; it never retains the pointer.
    // SAFETY: an all-zero `sockaddr_storage` is a valid initialised value; the
    // family-specific fields are written below before the address is used.
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let dstaddrlen: libc::socklen_t = match target {
        SocketAddr::V4(v4) => {
            let sin = std::ptr::from_mut(&mut storage).cast::<libc::sockaddr_in>();
            // SAFETY: `storage` is a zeroed `sockaddr_storage` large enough to
            // hold a `sockaddr_in`; writing through the reinterpreted pointer
            // stays within its allocation.
            unsafe {
                (*sin).sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
                (*sin).sin_family = libc::AF_INET as libc::sa_family_t;
                (*sin).sin_port = v4.port().to_be();
                (*sin).sin_addr = libc::in_addr {
                    s_addr: u32::from_ne_bytes(v4.ip().octets()),
                };
            }
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t
        }
        SocketAddr::V6(v6) => {
            let sin6 = std::ptr::from_mut(&mut storage).cast::<libc::sockaddr_in6>();
            // SAFETY: `storage` is a zeroed `sockaddr_storage` large enough to
            // hold a `sockaddr_in6`; writing through the reinterpreted pointer
            // stays within its allocation.
            unsafe {
                (*sin6).sin6_len = std::mem::size_of::<libc::sockaddr_in6>() as u8;
                (*sin6).sin6_family = libc::AF_INET6 as libc::sa_family_t;
                (*sin6).sin6_port = v6.port().to_be();
                (*sin6).sin6_flowinfo = v6.flowinfo();
                (*sin6).sin6_addr = libc::in6_addr {
                    s6_addr: v6.ip().octets(),
                };
                (*sin6).sin6_scope_id = v6.scope_id();
            }
            std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t
        }
    };

    let endpoints = libc::sa_endpoints_t {
        sae_srcif: 0,
        sae_srcaddr: std::ptr::null(),
        sae_srcaddrlen: 0,
        sae_dstaddr: std::ptr::from_ref(&storage).cast::<libc::sockaddr>(),
        sae_dstaddrlen: dstaddrlen,
    };

    // SAFETY: `raw_fd` is a valid, unconnected TCP socket borrowed for the
    // duration of this synchronous call. `endpoints` and the `sockaddr_storage`
    // it points at live on this stack frame and outlive the syscall. No `iovec`
    // payload is supplied (null/0), so `CONNECT_RESUME_ON_READ_WRITE` defers the
    // SYN to the first write; the out-params are null because the association
    // and connection ids are unused. `connectx` reads the inputs and writes
    // nothing back through the null pointers.
    let ret = unsafe {
        libc::connectx(
            raw_fd,
            std::ptr::from_ref(&endpoints),
            libc::SAE_ASSOCID_ANY,
            libc::CONNECT_RESUME_ON_READ_WRITE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };

    if ret == -1 {
        let err = io::Error::last_os_error();
        // A deferred Fast Open connect legitimately reports EINPROGRESS: the
        // SYN is held back until the first write, so treat it as success.
        if err.raw_os_error() == Some(libc::EINPROGRESS) {
            Ok(true)
        } else {
            Err(err)
        }
    } else {
        Ok(true)
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
        logging::debug_log!(Sockopt, 1, "TCP_QUICKACK set");
        Ok(true)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stream;
        logging::debug_log!(
            Sockopt,
            1,
            "TCP_QUICKACK unsupported on this platform: skipped"
        );
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
/// handshake so every round's ACK stays immediate. `None` (connect
/// program or stdio transports) and non-Linux platforms are no-ops,
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
        logging::debug_log!(Sockopt, 1, "SO_MAX_PACING_RATE set: {bytes_per_sec} B/s");
        Ok(true)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (stream, bytes_per_sec);
        logging::debug_log!(
            Sockopt,
            1,
            "SO_MAX_PACING_RATE unsupported on this platform: skipped"
        );
        Ok(false)
    }
}

/// Returns `true` when the running platform implements `SO_MAX_PACING_RATE`.
#[must_use]
pub const fn so_max_pacing_rate_supported() -> bool {
    cfg!(target_os = "linux")
}

/// Enables kernel busy-polling on a connected stream (`SO_BUSY_POLL`).
///
/// `usecs` is the microsecond budget the kernel spins polling the NIC for new
/// packets before yielding, trading CPU for lower receive latency. A value of
/// zero disables busy-polling. On platforms without `SO_BUSY_POLL` the call is
/// a no-op and returns `Ok(false)`.
///
/// `SO_BUSY_POLL` requires `CAP_NET_ADMIN` on many kernels. When the kernel
/// rejects the option with `EPERM` (missing capability) or `ENOPROTOOPT`
/// (busy-poll compiled out), this returns `Ok(false)` rather than an error,
/// matching the best-effort apply pattern of the other helpers here. This is
/// opt-in, default-off infrastructure and is not wired into the connect path.
///
/// upstream: not implemented; an oc-rsync-specific perf hint that is
/// wire-compatible with upstream rsync.
pub fn set_so_busy_poll(stream: &TcpStream, usecs: u32) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        match set_socket_int_option(stream, libc::SOL_SOCKET, libc::SO_BUSY_POLL, usecs as i32) {
            Ok(()) => Ok(true),
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::EPERM) | Some(libc::ENOPROTOOPT)
                ) =>
            {
                // SO_BUSY_POLL needs CAP_NET_ADMIN on many kernels and may be
                // compiled out; treat both as gracefully unsupported.
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (stream, usecs);
        Ok(false)
    }
}

/// Returns `true` when the running platform implements `SO_BUSY_POLL`.
///
/// Note that even where the option exists, applying it may still require
/// `CAP_NET_ADMIN`; [`set_so_busy_poll`] tolerates that at runtime.
#[must_use]
pub const fn so_busy_poll_supported() -> bool {
    cfg!(target_os = "linux")
}

/// Maximum socket buffer size accepted by [`set_socket_buffer_sizes`] (256 MiB).
///
/// Guards against absurd requests that would either fail the syscall or pin an
/// unreasonable amount of kernel memory. Kernels apply their own `wmem_max` /
/// `rmem_max` ceiling below this; the clamp only rejects nonsense inputs.
pub const MAX_SOCKET_BUFFER_SIZE: usize = 256 * 1024 * 1024;

/// Sets the send and/or receive socket buffer sizes on a connected stream
/// (`SO_SNDBUF` / `SO_RCVBUF`).
///
/// Each size is opt-in: `Some(n)` issues a `setsockopt` for that direction,
/// clamped to [`MAX_SOCKET_BUFFER_SIZE`]; `None` leaves the kernel default
/// untouched and issues no syscall. When both are `None` this is a no-op.
///
/// The option is portable (Linux, macOS, Windows via std/libc). Kernels
/// typically round up and often double the requested value, so a later
/// read-back reports a size greater than or equal to the request.
///
/// This is opt-in infrastructure; it changes no default and performs no
/// BDP-aware auto-sizing.
///
/// upstream: not implemented; an oc-rsync-specific perf hint that is
/// wire-compatible with upstream rsync.
///
/// # Errors
///
/// Returns the OS error reported by `setsockopt(2)` (Unix) or Winsock
/// (Windows) if a requested size is rejected.
pub fn set_socket_buffer_sizes(
    stream: &TcpStream,
    sndbuf: Option<usize>,
    rcvbuf: Option<usize>,
) -> io::Result<()> {
    #[cfg(unix)]
    let (sndbuf_opt, rcvbuf_opt) = (libc::SO_SNDBUF, libc::SO_RCVBUF);
    #[cfg(windows)]
    let (sndbuf_opt, rcvbuf_opt) = (0x1001_i32, 0x1002_i32);
    #[cfg(unix)]
    let level = libc::SOL_SOCKET;
    #[cfg(windows)]
    let level = 0xFFFF_i32;

    if let Some(size) = sndbuf {
        let clamped = size.min(MAX_SOCKET_BUFFER_SIZE).min(i32::MAX as usize) as i32;
        set_socket_int_option(stream, level, sndbuf_opt, clamped)?;
    }
    if let Some(size) = rcvbuf {
        let clamped = size.min(MAX_SOCKET_BUFFER_SIZE).min(i32::MAX as usize) as i32;
        set_socket_int_option(stream, level, rcvbuf_opt, clamped)?;
    }
    Ok(())
}

/// Toggles TCP output corking on a connected stream.
///
/// When `cork` is `true`, the kernel coalesces small writes into full
/// segments (`TCP_CORK` on Linux, `TCP_NOPUSH` on macOS/FreeBSD) instead of
/// emitting one segment per write. Pass `false` to uncork and flush any
/// buffered partial segment. Callers MUST uncork on a flush boundary and on
/// error so data is never held indefinitely. On platforms without a cork
/// option the call is a no-op and returns `Ok(false)`.
///
/// upstream: not implemented; an oc-rsync-specific perf hint that is
/// wire-compatible with upstream rsync.
pub fn set_tcp_cork(stream: &TcpStream, cork: bool) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        set_socket_int_option(stream, libc::IPPROTO_TCP, libc::TCP_CORK, i32::from(cork))?;
        logging::debug_log!(Sockopt, 1, "TCP_CORK set: cork={cork}");
        Ok(true)
    }
    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    {
        set_socket_int_option(stream, libc::IPPROTO_TCP, libc::TCP_NOPUSH, i32::from(cork))?;
        logging::debug_log!(Sockopt, 1, "TCP_NOPUSH set: cork={cork}");
        Ok(true)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
    {
        let _ = (stream, cork);
        logging::debug_log!(
            Sockopt,
            1,
            "TCP output corking unsupported on this platform: skipped"
        );
        Ok(false)
    }
}

/// Returns `true` when the running platform implements TCP output corking.
#[must_use]
pub const fn tcp_cork_supported() -> bool {
    cfg!(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd"
    ))
}

/// Returns `true` when the running platform implements `SO_REUSEPORT`.
///
/// `SO_REUSEPORT` lets multiple listener sockets bind the same address and
/// port so the kernel load-balances incoming connections across them (Linux
/// 3.9+, Android, the BSDs, macOS). Windows has no equivalent, so the daemon
/// listener skips it there.
#[must_use]
pub const fn reuse_port_supported() -> bool {
    cfg!(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "macos",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ))
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

    #[cfg(unix)]
    #[test]
    fn enable_tcp_fastopen_connect_reports_supported_platforms() {
        use std::os::fd::{FromRawFd, OwnedFd};

        // TCP_FASTOPEN_CONNECT must be set before connect(2), so exercise it on
        // a fresh unconnected TCP socket rather than a connected stream.
        // SAFETY: socket(2) returns a fresh owned fd; wrap it in OwnedFd so it
        // is closed when the test ends.
        let raw = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        assert!(raw >= 0, "socket(2) failed: {}", io::Error::last_os_error());
        let owned = unsafe { OwnedFd::from_raw_fd(raw) };
        use std::os::fd::AsRawFd;

        let result = enable_tcp_fastopen_connect_raw(owned.as_raw_fd());

        match result {
            Ok(true) => assert!(tcp_fastopen_connect_supported()),
            Ok(false) => assert!(!tcp_fastopen_connect_supported()),
            Err(error) => {
                // A Linux kernel with client TFO disabled
                // (`/proc/sys/net/ipv4/tcp_fastopen` bit 0 unset) may return
                // EOPNOTSUPP; only Linux exposes the option at all.
                assert!(
                    tcp_fastopen_connect_supported(),
                    "unexpected TFO connect error on unsupported platform: {error}"
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
    fn set_tcp_cork_returns_supported_flag() {
        let (stream, handle) = connected_stream();

        // Cork on, then uncork (flush): both must succeed on cork-capable
        // platforms and no-op elsewhere.
        for cork in [true, false] {
            match set_tcp_cork(&stream, cork) {
                Ok(true) => assert!(tcp_cork_supported()),
                Ok(false) => assert!(!tcp_cork_supported()),
                Err(error) => panic!("unexpected error from TCP cork ({cork}): {error}"),
            }
        }

        drop(stream);
        handle.join().expect("accept thread completes");
    }

    #[test]
    fn set_so_busy_poll_never_errors() {
        let (stream, handle) = connected_stream();

        // SO_BUSY_POLL needs CAP_NET_ADMIN on many kernels and is Linux-only;
        // the helper must always resolve to Ok (true when applied, false when
        // gracefully unsupported / permission-denied), never an error.
        match set_so_busy_poll(&stream, 50) {
            Ok(true) => assert!(so_busy_poll_supported()),
            Ok(false) => {}
            Err(error) => panic!("set_so_busy_poll must be best-effort, got: {error}"),
        }

        drop(stream);
        handle.join().expect("accept thread completes");
    }

    #[cfg(unix)]
    fn get_socket_int_option(stream: &TcpStream, level: i32, option: i32) -> i32 {
        use std::os::fd::AsRawFd;

        let mut value: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: `stream` is a live TcpStream for the duration of the call;
        // `&mut value`/`&mut len` are valid stack pointers sized to match a
        // `c_int` option. `getsockopt` only writes up to `len` bytes.
        let ret = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                level,
                option,
                std::ptr::from_mut(&mut value).cast::<libc::c_void>(),
                &mut len,
            )
        };
        assert_eq!(ret, 0, "getsockopt failed: {}", io::Error::last_os_error());
        value
    }

    #[test]
    fn set_socket_buffer_sizes_applies_requested_values() {
        let (stream, handle) = connected_stream();

        // None/None is a pure no-op and must succeed.
        set_socket_buffer_sizes(&stream, None, None).expect("no-op succeeds");

        let request = 256 * 1024;
        set_socket_buffer_sizes(&stream, Some(request), Some(request)).expect("sizes set");

        #[cfg(unix)]
        {
            // Kernels round up and often double the request, so assert the
            // read-back is at least the requested size, not an exact match.
            let snd = get_socket_int_option(&stream, libc::SOL_SOCKET, libc::SO_SNDBUF);
            let rcv = get_socket_int_option(&stream, libc::SOL_SOCKET, libc::SO_RCVBUF);
            assert!(
                snd as usize >= request,
                "SO_SNDBUF {snd} below requested {request}"
            );
            assert!(
                rcv as usize >= request,
                "SO_RCVBUF {rcv} below requested {request}"
            );
        }

        drop(stream);
        handle.join().expect("accept thread completes");
    }

    #[test]
    fn set_socket_buffer_sizes_clamps_absurd_request() {
        let (stream, handle) = connected_stream();

        // A wildly oversized request is clamped to MAX_SOCKET_BUFFER_SIZE
        // (and i32::MAX) so setsockopt still succeeds.
        set_socket_buffer_sizes(&stream, Some(usize::MAX), None).expect("clamped request succeeds");

        drop(stream);
        handle.join().expect("accept thread completes");
    }

    #[test]
    fn reuse_port_supported_is_self_consistent() {
        let expected = cfg!(any(
            target_os = "linux",
            target_os = "android",
            target_os = "freebsd",
            target_os = "macos",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "dragonfly"
        ));
        assert_eq!(reuse_port_supported(), expected);
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

    #[test]
    fn connectx_fastopen_supported_matches_platform() {
        assert_eq!(connectx_fastopen_supported(), cfg!(target_os = "macos"));
    }

    #[cfg(not(target_os = "macos"))]
    #[cfg(unix)]
    #[test]
    fn connectx_fastopen_is_noop_off_darwin() {
        use std::net::SocketAddr;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        // SAFETY: socket(2) returns a fresh owned fd; OwnedFd closes it on drop.
        let raw = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        assert!(raw >= 0, "socket(2) failed: {}", io::Error::last_os_error());
        let owned = unsafe { OwnedFd::from_raw_fd(raw) };
        let target: SocketAddr = "127.0.0.1:0".parse().expect("addr");

        // connectx does not exist off Darwin, so the wrapper reports the path
        // as unavailable rather than erroring.
        assert!(
            !connectx_fastopen_raw(owned.as_raw_fd(), &target).expect("no error off Darwin"),
            "connectx wrapper must be a no-op off Darwin"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn connectx_fastopen_connects_and_data_flows() {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};

        assert!(connectx_fastopen_supported());

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let addr = listener.local_addr().expect("addr");
        let accept = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 5];
            peer.read_exact(&mut buf).expect("server reads payload");
            assert_eq!(&buf, b"hello");
            peer.write_all(b"world").expect("server echoes");
        });

        // SAFETY: socket(2) returns a fresh owned fd; OwnedFd closes it on drop.
        let raw = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        assert!(raw >= 0, "socket(2) failed: {}", io::Error::last_os_error());
        let owned = unsafe { OwnedFd::from_raw_fd(raw) };

        // The Darwin Fast Open connect defers the SYN to the first write.
        let connected =
            connectx_fastopen_raw(owned.as_raw_fd(), &addr).expect("connectx succeeds on macOS");
        assert!(connected, "connectx should report the socket connected");

        // SAFETY: `owned` is a live, connected TCP socket fd; hand ownership to
        // the std stream so it manages the lifetime from here.
        let mut stream = unsafe { TcpStream::from_raw_fd(owned.into_raw_fd()) };

        // The first write carries the deferred SYN payload; data must flow.
        stream.write_all(b"hello").expect("client write");
        let mut reply = [0u8; 5];
        stream.read_exact(&mut reply).expect("client reads echo");
        assert_eq!(&reply, b"world");

        drop(stream);
        accept.join().expect("accept thread completes");
    }
}
