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

/// Creates a TCP listener bound to `addr` with an explicit listen backlog.
///
/// Uses `socket2` to create the socket, bind, and call `listen(2)` with the
/// specified backlog, rather than relying on the standard library's hardcoded
/// default (128). This matches upstream rsync's `socket.c` which calls
/// `listen(sp[i], lp_listen_backlog())`.
fn bind_with_backlog(addr: SocketAddr, backlog: i32) -> io::Result<TcpListener> {
    let domain = if addr.is_ipv4() {
        socket2::Domain::IPV4
    } else {
        socket2::Domain::IPV6
    };
    let socket =
        socket2::Socket::new(domain, socket2::Type::STREAM, Some(socket2::Protocol::TCP))?;

    // Allow port reuse so the daemon can restart quickly without waiting for
    // TIME_WAIT sockets to expire. Mirrors standard TcpListener::bind behaviour.
    socket.set_reuse_address(true)?;

    // For IPv6 sockets, set IPV6_V6ONLY to avoid conflicts with the separate
    // IPv4 listener in dual-stack mode.
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }

    socket.bind(&addr.into())?;
    socket.listen(backlog)?;

    Ok(socket.into())
}

/// Configures read/write timeouts on an accepted client stream.
fn configure_stream(stream: &TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))
}
