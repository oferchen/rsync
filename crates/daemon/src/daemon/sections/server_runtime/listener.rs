/// Logs a systemd notification failure if a log sink is available.
fn log_sd_notify_failure(log: Option<&SharedLogSink>, context: &str, error: &io::Error) {
    if let Some(sink) = log {
        let payload = format!("failed to notify systemd about {context}: {error}");
        let message = rsync_warning!(payload).with_role(Role::Daemon);
        log_message(sink, &message);
    }
}

/// Formats a human-readable connection status message for systemd notification.
///
/// Returns appropriate singular/plural forms based on the connection count.
pub(crate) fn format_connection_status(active: usize) -> String {
    match active {
        0 => String::from("Idle; waiting for connections"),
        1 => String::from("Serving 1 connection"),
        count => format!("Serving {count} connections"),
    }
}

/// Normalizes a socket address by converting IPv4-mapped IPv6 addresses to pure IPv4.
///
/// When running in dual-stack mode and accepting IPv4 connections on an IPv6 listener,
/// the kernel reports peer addresses as IPv4-mapped IPv6 (e.g., `::ffff:127.0.0.1`).
/// This function converts such addresses back to their IPv4 equivalents for consistent
/// logging and host matching.
const fn normalize_peer_address(addr: SocketAddr) -> SocketAddr {
    match addr.ip() {
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                SocketAddr::new(IpAddr::V4(v4), addr.port())
            } else {
                addr
            }
        }
        IpAddr::V4(_) => addr,
    }
}

/// Default TCP listen backlog when not overridden by `listen backlog` in the
/// daemon configuration file.
///
/// Upstream rsync defaults to 5 (`daemon-parm.txt`), which is too low for
/// production workloads - the kernel drops SYN packets once the backlog queue
/// is full, creating a hard ceiling around 250 concurrent connections.
/// A backlog of 128 matches `SOMAXCONN` on most Linux systems and is the
/// standard default for production TCP servers.
const DEFAULT_LISTEN_BACKLOG: i32 = 128;

/// Interval between signal flag checks in the accept loop.
///
/// The listener uses a non-blocking timeout so the loop can periodically
/// inspect signal flags (SIGHUP, SIGTERM/SIGINT, SIGUSR1, SIGUSR2) without
/// waiting indefinitely for a new connection.
const SIGNAL_CHECK_INTERVAL: Duration = Duration::from_millis(500);

/// Logs a progress summary when SIGUSR2 is received.
///
/// Outputs the number of active worker threads, total connections served, and
/// daemon uptime. Mirrors upstream rsync's SIGUSR2 behaviour of dumping
/// transfer statistics to the log.
/// upstream: main.c - SIGUSR2 handler outputs transfer progress info.
fn log_progress_summary(
    log: Option<&SharedLogSink>,
    active_workers: usize,
    served: usize,
    start_time: SystemTime,
) {
    let uptime_secs = start_time
        .elapsed()
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let hours = uptime_secs / 3600;
    let minutes = (uptime_secs % 3600) / 60;
    let seconds = uptime_secs % 60;

    let text = format!(
        "progress: {active_workers} active connection(s), \
         {served} total served, uptime {hours}h {minutes}m {seconds}s"
    );

    if let Some(sink) = log {
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(sink, &message);
    } else {
        eprintln!("{text} [daemon={}]", env!("CARGO_PKG_VERSION"));
    }
}

/// Returns the upstream-equivalent address-family integer for `addr`.
///
/// Mirrors upstream's `resp->ai_family` value passed to the
/// `"socket(...) failed"` / `"bind() failed: ... (address-family %d)"`
/// debug emissions. Linux uses `AF_INET = 2` and `AF_INET6 = 10` (see
/// `<bits/socket.h>`), which oc-rsync reproduces verbatim so the trace
/// output stays byte-comparable with upstream `socket.c:432-470`.
const fn address_family_int(addr: SocketAddr) -> i32 {
    if addr.is_ipv4() { 2 } else { 10 }
}

/// `SOCK_STREAM` numeric value used by upstream `socket(2)` calls.
///
/// upstream: `socket.c:429-430` - `socket(resp->ai_family, resp->ai_socktype,
/// resp->ai_protocol)` where `ai_socktype == SOCK_STREAM` for the TCP
/// listener path (`open_socket_in(SOCK_STREAM, ...)` at
/// `clientserver.c:1099`). Linux defines `SOCK_STREAM = 1`.
const UPSTREAM_SOCK_STREAM: i32 = 1;
/// `IPPROTO_TCP` numeric value used by upstream `socket(2)` calls.
///
/// upstream: `socket.c:429-430` - `resp->ai_protocol` is set by
/// `getaddrinfo` to `IPPROTO_TCP` (`6`) when `ai_socktype == SOCK_STREAM`.
const UPSTREAM_IPPROTO_TCP: i32 = 6;

/// Creates a TCP listener bound to `addr` with an explicit listen backlog.
///
/// Uses `socket2` to create the socket, bind, and call `listen(2)` with the
/// specified backlog, rather than relying on the standard library's hardcoded
/// default (128). This matches upstream rsync's `socket.c` which calls
/// `listen(sp[i], lp_listen_backlog())`.
///
/// Failed `socket(2)` and `bind(2)` syscalls are reported through the
/// `--debug=BIND` producer (`protocol::bind::trace`), mirroring upstream
/// `socket.c:432-470` per-address-family accumulation. The errors are
/// propagated to the caller unchanged.
fn bind_with_backlog(
    addr: SocketAddr,
    backlog: i32,
    tcp_fastopen: TcpFastOpenMode,
) -> io::Result<TcpListener> {
    let domain = if addr.is_ipv4() {
        socket2::Domain::IPV4
    } else {
        socket2::Domain::IPV6
    };
    let family = address_family_int(addr);
    let socket = socket2::Socket::new(domain, socket2::Type::STREAM, Some(socket2::Protocol::TCP))
        .inspect_err(|err| {
            protocol::bind::trace_socket_failure(
                family,
                UPSTREAM_SOCK_STREAM,
                UPSTREAM_IPPROTO_TCP,
                err,
            );
        })?;

    // Allow port reuse so the daemon can restart quickly without waiting for
    // TIME_WAIT sockets to expire. Mirrors standard TcpListener::bind behaviour.
    socket.set_reuse_address(true)?;

    // For IPv6 sockets, set IPV6_V6ONLY to avoid conflicts with the separate
    // IPv4 listener in dual-stack mode.
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }

    socket
        .bind(&addr.into())
        .inspect_err(|err| protocol::bind::trace_bind_failure(family, err))?;

    // Apply TCP Fast Open server side before `listen(2)` so the kernel
    // installs the SYN cookie cache from the start of the listener
    // lifetime. Errors are downgraded to a debug log: TFO is an
    // optimisation, not a correctness requirement, and a failing
    // setsockopt must not prevent the daemon from accepting connections.
    if tcp_fastopen.is_enabled() && fast_io::tcp_fastopen_listener_supported() {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let _ = fast_io::enable_tcp_fastopen_raw(
                socket.as_raw_fd(),
                fast_io::DEFAULT_TCP_FASTOPEN_QLEN,
            );
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            let _ = fast_io::enable_tcp_fastopen_raw(
                socket.as_raw_socket(),
                fast_io::DEFAULT_TCP_FASTOPEN_QLEN,
            );
        }
    }

    socket.listen(backlog)?;

    Ok(socket.into())
}

/// Configures read/write timeouts on an accepted client stream.
fn configure_stream(stream: &DaemonStream) -> io::Result<()> {
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))
}

/// One-shot guard so the `--tcp-fastopen=on` unsupported-platform warning
/// fires at most once per daemon process.
static TCP_FASTOPEN_UNSUPPORTED_WARNED: std::sync::Once = std::sync::Once::new();

/// Emits a single startup warning when `--tcp-fastopen=on` is requested on
/// a platform that does not implement server-side TFO.
fn warn_tcp_fastopen_unsupported(log: Option<&SharedLogSink>) {
    let mut should_emit = false;
    TCP_FASTOPEN_UNSUPPORTED_WARNED.call_once(|| {
        should_emit = true;
    });

    if !should_emit {
        return;
    }

    let payload = format!(
        "--tcp-fastopen=on requested but TCP Fast Open is not supported on \
         this platform ({}); the daemon will accept connections without TFO",
        std::env::consts::OS
    );
    let message = rsync_warning!(payload).with_role(Role::Daemon);

    if let Some(sink) = log {
        log_message(sink, &message);
    } else {
        eprintln!("{message}");
    }
}

/// Applies the `TCP_NOTSENT_LOWAT` perf option to an accepted client
/// stream, ignoring unsupported platforms and best-effort errors.
fn apply_accepted_stream_tcp_notsent_lowat(stream: &TcpStream) {
    if fast_io::tcp_notsent_lowat_supported() {
        let _ = fast_io::set_tcp_notsent_lowat(stream, fast_io::DEFAULT_TCP_NOTSENT_LOWAT);
    }
}
