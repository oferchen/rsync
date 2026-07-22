/// Runtime override selection for the daemon listener address family.
///
/// Used by the `OC_RSYNC_DAEMON_ADDRESS_FAMILY` environment variable so CI
/// and test fixtures can force a specific family without rebuilding the
/// CLI. The variable is read once at accept-loop entry; later changes do
/// not affect a running daemon. Accepts `ipv4`, `ipv6`, or `both`
/// (case-insensitive); unknown values are ignored so an operator's typo
/// degrades to the compile-time default rather than failing startup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AddressFamilyOverride {
    /// Bind one IPv4 listener only.
    Ipv4,
    /// Bind one IPv6 listener only.
    Ipv6,
    /// Bind one IPv4 listener and one IPv6 listener, surfacing per-family
    /// bind failures as warnings (matches upstream's `default_af_hint = 0`
    /// iteration in `socket.c::open_socket_in`).
    Both,
}

/// Name of the environment variable that overrides the listener address
/// family. See [`AddressFamilyOverride`].
pub(crate) const ADDRESS_FAMILY_ENV: &str = "OC_RSYNC_DAEMON_ADDRESS_FAMILY";

/// Parses an `OC_RSYNC_DAEMON_ADDRESS_FAMILY` value into an
/// [`AddressFamilyOverride`].
///
/// Returns `None` for empty or unrecognised values so the daemon falls
/// back to its compile-time default instead of refusing to start.
pub(crate) fn parse_address_family_env(value: &str) -> Option<AddressFamilyOverride> {
    match value.trim().to_ascii_lowercase().as_str() {
        "ipv4" | "v4" | "4" | "inet" => Some(AddressFamilyOverride::Ipv4),
        "ipv6" | "v6" | "6" | "inet6" => Some(AddressFamilyOverride::Ipv6),
        "both" | "dual" | "dualstack" | "dual-stack" => Some(AddressFamilyOverride::Both),
        _ => None,
    }
}

/// Reads [`ADDRESS_FAMILY_ENV`] and converts it to an
/// [`AddressFamilyOverride`].
///
/// Returns `None` when the variable is unset, empty, or holds an
/// unrecognised value.
fn read_address_family_env_override() -> Option<AddressFamilyOverride> {
    std::env::var(ADDRESS_FAMILY_ENV)
        .ok()
        .as_deref()
        .and_then(parse_address_family_env)
}

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
/// Mirrors upstream rsync's default of 5: `daemon-parm.txt` declares
/// `INTEGER listen_backlog 5`, and `socket.c:554` passes `lp_listen_backlog()`
/// to `listen(2)`. An operator who needs a deeper accept queue raises it with
/// the `listen backlog` directive, exactly as upstream allows.
const DEFAULT_LISTEN_BACKLOG: i32 = 5;

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
    let uptime_secs = start_time.elapsed().map(|d| d.as_secs()).unwrap_or(0);

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
/// `socket_options` (the `socket options =` config directive / `--sockopts`)
/// is applied before `bind(2)` - upstream: socket.c:447-465 applies
/// `SO_REUSEADDR` then `set_socket_options(s, sockopts)` then `bind(2)`.
/// Applying user socket options only after `listen(2)` (as a prior version of
/// this function did, via a caller-side post-bind pass) is a no-op for
/// options that shape the window scale advertised in the SYN-ACK the kernel
/// sends once the socket starts accepting connections.
///
/// Failed `socket(2)` and `bind(2)` syscalls are reported through the
/// `--debug=BIND` producer (`protocol::bind::trace`), mirroring upstream
/// `socket.c:432-470` per-address-family accumulation. The errors are
/// propagated to the caller unchanged.
fn bind_with_backlog(
    addr: SocketAddr,
    backlog: i32,
    tcp_fastopen: TcpFastOpenMode,
    reuse_port: bool,
    socket_options: &[SocketOption],
    log_sink: Option<&SharedLogSink>,
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
    // TIME_WAIT sockets to expire.
    // upstream: socket.c:447 - open_socket_in() sets SO_REUSEADDR (and only
    // SO_REUSEADDR) on every listener.
    socket.set_reuse_address(true)?;

    // SO_REUSEPORT is set ONLY for the opt-in multi-acceptor daemon (more than
    // one replica socket per address), where several listener sockets must
    // share the bind address and the kernel load-balances accepts across them.
    // The default single-listener daemon (`reuse_port == false`) must NOT set
    // it: upstream (socket.c:447) sets only SO_REUSEADDR, so a second daemon
    // attempting to bind the same port is refused with EADDRINUSE rather than
    // silently co-binding. Best-effort when enabled: a failure downgrades to a
    // debug log. socket2's setter is Unix-only; Windows has no equivalent, so
    // the flag is consumed but unused there.
    #[cfg(not(unix))]
    let _ = reuse_port;
    #[cfg(unix)]
    if reuse_port {
        if fast_io::reuse_port_supported() {
            match socket.set_reuse_port(true) {
                Ok(()) => logging::debug_log!(Sockopt, 1, "SO_REUSEPORT set on listener {addr}"),
                Err(_) => logging::debug_log!(
                    Sockopt,
                    1,
                    "SO_REUSEPORT apply failed on listener {addr}: single-listener fallback"
                ),
            }
        } else {
            logging::debug_log!(
                Sockopt,
                1,
                "SO_REUSEPORT unsupported on this platform: skipped for listener {addr}"
            );
        }
    }

    // upstream: socket.c:449-452 - set_socket_options(s, sockopts) runs here,
    // after SO_REUSEADDR and before bind(2), so options that shape the SYN-ACK
    // (e.g. SO_SNDBUF/SO_RCVBUF window scaling) take effect from the first
    // connection the listener accepts.
    apply_socket_options_impl(socket2::SockRef::from(&socket), socket_options, log_sink);

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

/// Emits a one-shot warning when a per-family bind fails while another
/// family in the dual-stack startup is still available to try.
///
/// upstream: `socket.c:463-465` in rsync-3.4.1 logs the per-family failure
/// via `asprintf(&errmsgs[ecnt++], "bind() failed: %s (address-family %d)")`
/// and prints it through `rwrite(FLOG, ...)` either when all addresses fail
/// or when running with `-vv` debug. oc-rsync surfaces it unconditionally on
/// the assumption that operators of a long-lived daemon want to know that the
/// dual-stack listener is degraded - GitHub Actions runners that have IPv6
/// partially configured but unroutable trigger this path, and the silent
/// fallback made the failure mode invisible until the daemon test exited 10.
fn warn_per_family_bind_failure(
    log: Option<&SharedLogSink>,
    requested_addr: SocketAddr,
    error: &io::Error,
) {
    let family = if requested_addr.is_ipv6() {
        "IPv6"
    } else {
        "IPv4"
    };
    let payload = format!(
        "{family} bind for {requested_addr} failed: {error}; \
         continuing with remaining address families"
    );
    let message = rsync_warning!(payload).with_role(Role::Daemon);

    if let Some(sink) = log {
        log_message(sink, &message);
    } else {
        eprintln!("{message}");
    }
}

/// Emits a warning when one family's acceptor thread dies while another
/// family is still serving connections in a dual-stack listener.
///
/// This is the GitHub Actions exit-10 failure mode: `bind(2)` to `[::]:873`
/// succeeds on the runner but `accept(2)` later returns an unexpected
/// address family or `EAFNOSUPPORT`, the IPv6 acceptor exits, and prior
/// code treated that as fatal even though the IPv4 acceptor was healthy.
/// Surfacing the family-specific failure as a warning preserves operator
/// visibility into the degraded listener while keeping the daemon
/// servicing traffic on the surviving family.
fn warn_per_family_accept_failure(
    log: Option<&SharedLogSink>,
    local_addr: SocketAddr,
    error: &io::Error,
) {
    let family = if local_addr.is_ipv6() { "IPv6" } else { "IPv4" };
    let payload = format!(
        "{family} accept on {local_addr} failed: {error}; \
         remaining address families continue to serve connections"
    );
    let message = rsync_warning!(payload).with_role(Role::Daemon);

    if let Some(sink) = log {
        log_message(sink, &message);
    } else {
        eprintln!("{message}");
    }
}

/// Emits a warning when `accept(2)` fails with a transient, per-connection
/// error that the accept loop deliberately survives.
///
/// upstream: `socket.c:593` - `if (fd < 0) continue;`. The daemon accept loop
/// ignores every `accept(2)` failure and keeps serving. Errors such as
/// `ECONNABORTED` (a client reset between the TCP handshake and `accept`) or
/// `EMFILE`/`ENFILE` (a transient descriptor shortage under a connection
/// burst) are per-connection: the affected client is dropped, but the listener
/// stays up. Prior to this the single-listener engine escalated any such error
/// to a fatal daemon exit, so one aborted connection under load could tear the
/// whole daemon down. Surfacing the error at warning level keeps operator
/// visibility into the churn without implying the daemon is degraded.
fn warn_transient_accept_failure(
    log: Option<&SharedLogSink>,
    local_addr: SocketAddr,
    error: &io::Error,
) {
    let payload = format!(
        "accept on {local_addr} failed: {error}; \
         dropping the connection and continuing to serve"
    );
    let message = rsync_warning!(payload).with_role(Role::Daemon);

    if let Some(sink) = log {
        log_message(sink, &message);
    } else {
        eprintln!("{message}");
    }
}

/// Binds one TCP listener per entry in `bind_addresses`, tolerating per-family
/// failures while at least one family still binds successfully.
///
/// Mirrors upstream `socket.c::open_socket_in` (rsync-3.4.1, lines 428-498):
/// the loop attempts every getaddrinfo result, emits a per-family diagnostic
/// when a bind fails, and only returns an error when zero sockets bound. The
/// dual-stack default (IPv6 then IPv4) tolerates GitHub Actions runners where
/// IPv6 loopback is partially configured but unroutable - the IPv6 bind fails
/// with `EADDRNOTAVAIL` or `EAFNOSUPPORT`, oc-rsync logs the per-family error,
/// and the listener degrades to IPv4 instead of producing an opaque exit 10.
///
/// Returns the listeners in `bind_addresses` order (skipping families that
/// failed) along with the matching `local_addr()` for status reporting. Returns
/// `Err(io::Error)` only when every family in `bind_addresses` failed to bind;
/// callers map that to a `DaemonError` with the first requested address as
/// context, matching the existing `bind_error` contract.
fn bind_listeners_per_family(
    bind_addresses: &[IpAddr],
    port: u16,
    backlog: i32,
    tcp_fastopen: TcpFastOpenMode,
    acceptor_threads: u32,
    socket_options: &[SocketOption],
    log_sink: Option<&SharedLogSink>,
) -> Result<(Vec<TcpListener>, Vec<SocketAddr>), io::Error> {
    let replicas = acceptor_threads.max(1) as usize;
    // SO_REUSEPORT is only needed when more than one replica socket must
    // co-bind the same address (the opt-in multi-acceptor extension). The
    // default single-listener daemon binds with SO_REUSEADDR only, matching
    // upstream socket.c:447 so a duplicate bind is refused with EADDRINUSE.
    let reuse_port = replicas > 1;
    let mut listeners = Vec::with_capacity(bind_addresses.len() * replicas);
    let mut bound_addresses = Vec::with_capacity(bind_addresses.len() * replicas);
    let dual_stack = bind_addresses.len() > 1;
    let mut last_error: Option<io::Error> = None;

    for addr in bind_addresses {
        let requested_addr = SocketAddr::new(*addr, port);

        // Bind up to `replicas` SO_REUSEPORT sockets for this family. The kernel
        // load-balances accepted connections across them, each driven by its own
        // acceptor thread. With replicas == 1 this is the historical
        // single-listener-per-family behaviour.
        let mut family_bound = 0usize;
        let mut family_error: Option<io::Error> = None;
        for _ in 0..replicas {
            match bind_with_backlog(
                requested_addr,
                backlog,
                tcp_fastopen,
                reuse_port,
                socket_options,
                log_sink,
            ) {
                Ok(listener) => {
                    let local_addr = listener.local_addr().unwrap_or(requested_addr);
                    bound_addresses.push(local_addr);
                    listeners.push(listener);
                    family_bound += 1;
                }
                Err(error) => {
                    family_error = Some(error);
                    break;
                }
            }
        }

        if family_bound == 0 {
            // Whole family failed to bind even once - apply the existing
            // per-family tolerance: warn and continue in dual-stack mode so a
            // surviving family keeps the daemon up, else propagate.
            let error = family_error.expect("a bind failure was recorded");
            if dual_stack {
                warn_per_family_bind_failure(log_sink, requested_addr, &error);
                last_error = Some(error);
                continue;
            }
            return Err(error);
        }

        // The family is serving with at least one replica. If a later replica
        // failed, surface it as a warning but keep the bound replicas.
        if family_bound < replicas
            && let Some(error) = family_error
        {
            warn_per_family_bind_failure(log_sink, requested_addr, &error);
        }
    }

    if listeners.is_empty() {
        let error = last_error.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "no addresses available to bind",
            )
        });
        return Err(error);
    }

    Ok((listeners, bound_addresses))
}

/// Applies the `TCP_NOTSENT_LOWAT` perf option to an accepted client
/// stream, ignoring unsupported platforms and best-effort errors.
fn apply_accepted_stream_tcp_notsent_lowat(stream: &TcpStream) {
    if fast_io::tcp_notsent_lowat_supported() {
        let _ = fast_io::set_tcp_notsent_lowat(stream, fast_io::DEFAULT_TCP_NOTSENT_LOWAT);
    }
    if fast_io::tcp_quickack_supported() {
        let _ = fast_io::set_tcp_quickack(stream);
    }
}
