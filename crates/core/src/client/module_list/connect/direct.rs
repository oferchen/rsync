use std::ffi::OsStr;
use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use socket2::{Domain, Protocol, SockAddr, Socket, Type};

use super::super::DaemonAddress;
use super::super::socket_options::apply_socket_options;
use crate::client::{
    AddressMode, ClientError, SOCKET_IO_EXIT_CODE, TcpFastOpenMode, connect_timeout_error,
    daemon_error, socket_error,
};

/// Establishes a direct TCP connection to an rsync daemon.
///
/// Resolves the daemon address, iterates through candidates filtered by
/// `address_mode`, and returns the first successful connection. I/O timeouts
/// are applied to the resulting stream. `sockopts`, when given, is applied to
/// each candidate socket before `connect(2)` (upstream: socket.c:279-280), so
/// options that shape the SYN (e.g. `SO_SNDBUF`/`SO_RCVBUF`) take effect.
pub(crate) fn connect_direct(
    addr: &DaemonAddress,
    connect_timeout: Option<Duration>,
    io_timeout: Option<Duration>,
    address_mode: AddressMode,
    bind_address: Option<SocketAddr>,
    tfo: TcpFastOpenMode,
    sockopts: Option<&OsStr>,
) -> Result<TcpStream, ClientError> {
    let addresses = resolve_daemon_addresses(addr, address_mode)?;
    let mut last_error: Option<(SocketAddr, io::Error)> = None;

    for candidate in addresses {
        match connect_with_optional_bind(candidate, bind_address, connect_timeout, tfo, sockopts) {
            Ok(stream) => {
                if let Some(duration) = io_timeout {
                    stream.set_read_timeout(Some(duration)).map_err(|error| {
                        socket_error("set read timeout on", addr.socket_addr_display(), error)
                    })?;
                    stream.set_write_timeout(Some(duration)).map_err(|error| {
                        socket_error("set write timeout on", addr.socket_addr_display(), error)
                    })?;
                }

                return Ok(stream);
            }
            Err(error) => last_error = Some((candidate, error)),
        }
    }

    let (candidate, error) = last_error.expect("no addresses available for daemon connection");
    Err(map_connect_failure(connect_timeout, candidate, error))
}

/// Maps a final `connect(2)` failure to a [`ClientError`] with the correct exit
/// code.
///
/// A `--contimeout` expiry (`connect_timeout` was `Some`, and `socket2` surfaces
/// the alarm as [`io::ErrorKind::TimedOut`]) maps to `RERR_CONTIMEOUT` (35),
/// matching upstream and the SSH path. Every other failure - including a bare
/// OS SYN timeout when no `--contimeout` was requested - keeps the generic
/// socket-I/O code (10).
///
/// upstream: socket.c:280-282 - `if (connect_timeout < 0) exit_cleanup(RERR_CONTIMEOUT);`
fn map_connect_failure(
    connect_timeout: Option<Duration>,
    candidate: SocketAddr,
    error: io::Error,
) -> ClientError {
    if connect_timeout.is_some() && error.kind() == io::ErrorKind::TimedOut {
        return connect_timeout_error(candidate, error);
    }
    socket_error("connect to", candidate, error)
}

/// Resolves a [`DaemonAddress`] to a list of [`SocketAddr`]s, filtered by address family.
///
/// Returns an error when resolution fails or the requested family yields no
/// results.
pub(crate) fn resolve_daemon_addresses(
    addr: &DaemonAddress,
    mode: AddressMode,
) -> Result<Vec<SocketAddr>, ClientError> {
    let iterator = (addr.host.as_str(), addr.port)
        .to_socket_addrs()
        .map_err(|error| {
            socket_error(
                "resolve daemon address for",
                addr.socket_addr_display(),
                error,
            )
        })?;

    let addresses: Vec<SocketAddr> = iterator.collect();

    if addresses.is_empty() {
        return Err(daemon_error(
            format!(
                "daemon host '{}' did not resolve to any addresses",
                addr.host()
            ),
            SOCKET_IO_EXIT_CODE,
        ));
    }

    let filtered = match mode {
        AddressMode::Default => addresses,
        AddressMode::Ipv4 => {
            let retain: Vec<SocketAddr> = addresses
                .into_iter()
                .filter(|candidate| candidate.is_ipv4())
                .collect();
            if retain.is_empty() {
                return Err(daemon_error(
                    format!("daemon host '{}' does not have IPv4 addresses", addr.host()),
                    SOCKET_IO_EXIT_CODE,
                ));
            }
            retain
        }
        AddressMode::Ipv6 => {
            let retain: Vec<SocketAddr> = addresses
                .into_iter()
                .filter(|candidate| candidate.is_ipv6())
                .collect();
            if retain.is_empty() {
                return Err(daemon_error(
                    format!("daemon host '{}' does not have IPv6 addresses", addr.host()),
                    SOCKET_IO_EXIT_CODE,
                ));
            }
            retain
        }
    };

    Ok(filtered)
}

/// Opens a TCP connection to `target`, optionally binding to a local address first.
///
/// When `bind_address` is provided its port is forced to `0` so the OS picks
/// an ephemeral port. A `connect_timeout`, when given, is forwarded to the
/// underlying socket. The connection is always built through a `socket2`
/// socket so a `TCP_FASTOPEN_CONNECT` option can be set before `connect(2)`
/// when `tfo` requests it, and so `sockopts` (`--sockopts`), when given, can
/// be applied before `connect(2)` too.
///
/// upstream: socket.c:267-280 `open_socket_out()` - `try_bind_local()` runs
/// first, then `set_socket_options(s, sockopts)`, then `connect(s, ...)`.
/// Applying `--sockopts` after `connect(2)` returns is a no-op for options
/// that shape the SYN (e.g. `SO_SNDBUF`/`SO_RCVBUF` window scaling), so the
/// order here must match: bind, then sockopts, then connect.
pub(crate) fn connect_with_optional_bind(
    target: SocketAddr,
    bind_address: Option<SocketAddr>,
    timeout: Option<Duration>,
    tfo: TcpFastOpenMode,
    sockopts: Option<&OsStr>,
) -> io::Result<TcpStream> {
    let domain = if target.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;

    if let Some(bind) = bind_address {
        if target.is_ipv4() != bind.is_ipv4() {
            return Err(io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "bind address family does not match target",
            ));
        }

        let mut bind_addr = bind;
        match &mut bind_addr {
            SocketAddr::V4(addr) => addr.set_port(0),
            SocketAddr::V6(addr) => addr.set_port(0),
        }
        socket.bind(&SockAddr::from(bind_addr))?;
    }

    // upstream: socket.c:279 - set_socket_options(s, sockopts) runs here,
    // after the optional bind and before connect(2).
    if let Some(options) = sockopts {
        apply_socket_options(&socket, options);
    }

    // Request client-side TCP Fast Open before connect. On Linux this sets
    // TCP_FASTOPEN_CONNECT so the kernel defers the SYN until the first write,
    // folding the request into the handshake and saving a round trip. This
    // supersedes the older MSG_FASTOPEN-on-sendto mechanism, which is why the
    // standard connect/write flow below works unchanged. Best-effort: an
    // unsupported or failing setsockopt leaves the connect to proceed normally.
    apply_tcp_fastopen_connect(&socket, tfo);

    // macOS has no TCP_FASTOPEN_CONNECT socket option; the SYN is deferred via
    // connectx(CONNECT_RESUME_ON_READ_WRITE) instead. Mirror the Linux gate on
    // the same `tfo` flag, at this same chokepoint. If the connectx path
    // succeeds the socket is already (deferred-)connected, so skip the normal
    // connect below. Any failure falls through to a standard blocking connect:
    // TFO is a latency optimisation and must never break connectivity.
    if try_connectx_fastopen(&socket, target, tfo) {
        return Ok(socket.into());
    }

    let target_addr = SockAddr::from(target);
    if let Some(duration) = timeout {
        socket.connect_timeout(&target_addr, duration)?;
    } else {
        socket.connect(&target_addr)?;
    }

    Ok(socket.into())
}

/// Sets `TCP_FASTOPEN_CONNECT` on `socket` before connect when `tfo` enables
/// it and the platform supports it. A no-op on platforms without client-side
/// TFO (the strict-mode unsupported warning is surfaced at config time).
fn apply_tcp_fastopen_connect(socket: &Socket, tfo: TcpFastOpenMode) {
    if !tfo.is_enabled() || !fast_io::tcp_fastopen_connect_supported() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let _ = fast_io::enable_tcp_fastopen_connect_raw(socket.as_raw_fd());
    }
    #[cfg(not(unix))]
    let _ = socket;
}

/// Issues the macOS `connectx(2)` Fast Open connect when `tfo` enables it and
/// the platform supports it. Returns `true` when the socket has been
/// (deferred-)connected via connectx so the caller can skip the standard
/// `connect(2)`. Returns `false` when TFO is disabled, the platform lacks
/// connectx (Linux/Windows/others), or connectx failed - in which case the
/// caller falls back to a normal blocking connect. This mirrors the Linux
/// `apply_tcp_fastopen_connect` gate on the same `tfo` flag.
fn try_connectx_fastopen(socket: &Socket, target: SocketAddr, tfo: TcpFastOpenMode) -> bool {
    if !tfo.is_enabled() || !fast_io::connectx_fastopen_supported() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        matches!(
            fast_io::connectx_fastopen_raw(socket.as_raw_fd(), &target),
            Ok(true)
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (socket, target);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    fn candidate() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 873)
    }

    // upstream: socket.c:280-282 - a --contimeout expiry mid-connect exits with
    // RERR_CONTIMEOUT (35). When a connect bound was requested and connect(2)
    // times out, oc must report 35, not the generic socket-I/O code 10.
    #[test]
    fn connect_timeout_expiry_maps_to_contimeout_exit_code() {
        let error = io::Error::new(io::ErrorKind::TimedOut, "connection timed out");
        let mapped = map_connect_failure(Some(Duration::from_secs(5)), candidate(), error);
        assert_eq!(mapped.exit_code(), 35);
    }

    // Without --contimeout (connect_timeout is None), an OS-level SYN timeout is
    // an ordinary socket failure: upstream never arms the contimeout alarm, so
    // the exit code stays RERR_SOCKETIO (10), not RERR_CONTIMEOUT.
    #[test]
    fn os_timeout_without_contimeout_stays_socket_io() {
        let error = io::Error::new(io::ErrorKind::TimedOut, "connection timed out");
        let mapped = map_connect_failure(None, candidate(), error);
        assert_eq!(mapped.exit_code(), 10);
    }

    // A non-timeout failure (e.g. connection refused) is never a contimeout even
    // when --contimeout was set; only ErrorKind::TimedOut is the alarm.
    #[test]
    fn non_timeout_failure_with_contimeout_stays_socket_io() {
        let error = io::Error::new(io::ErrorKind::ConnectionRefused, "refused");
        let mapped = map_connect_failure(Some(Duration::from_secs(5)), candidate(), error);
        assert_eq!(mapped.exit_code(), 10);
    }

    // upstream: socket.c:279-280 - set_socket_options(s, sockopts) runs before
    // connect(s, ...), so options that shape the SYN (e.g. SO_SNDBUF/SO_RCVBUF
    // window scaling) take effect. connect_with_optional_bind applies sockopts
    // to the socket2::Socket before either connect() or connect_timeout() is
    // called; a --sockopts value applied only after connect(2) returns would
    // be a no-op for the handshake. This proves --sockopts is wired all the
    // way through the connect helper and takes effect on the live socket
    // (setting it post-connect on an already-established stream would report
    // the identical read-back value, so this also guards against the
    // parameter silently failing to reach the socket).
    #[test]
    fn connect_with_optional_bind_applies_sockopts() {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let target = listener.local_addr().expect("listener addr");
        let handle = std::thread::spawn(move || {
            let _ = listener.accept();
        });

        // TFO Off so connect() performs a full handshake; with TFO the handshake
        // is deferred to the first write (which this test never issues), so on
        // some platforms the listener's accept() would block until the test
        // times out. Sockopt application is independent of the TFO mode.
        let stream = connect_with_optional_bind(
            target,
            None,
            None,
            TcpFastOpenMode::Off,
            Some(std::ffi::OsStr::new("SO_SNDBUF=131072")),
        )
        .expect("connect with sockopts");

        let reported = socket2::SockRef::from(&stream)
            .send_buffer_size()
            .expect("query send buffer size");
        assert!(
            reported >= 131_072,
            "SO_SNDBUF=131072 must be applied via --sockopts, got {reported}"
        );

        drop(stream);
        handle.join().expect("accept thread completes");
    }
}
