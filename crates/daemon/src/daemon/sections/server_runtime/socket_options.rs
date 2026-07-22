/// A parsed TCP/IP socket option ready for application to a socket.
///
/// upstream: socket.c - `set_socket_options()` parses a comma-separated string
/// of option names and optional `=value` suffixes, then applies them via
/// `setsockopt(2)`.
#[derive(Clone, Debug, Eq, PartialEq)]
enum SocketOption {
    /// `TCP_NODELAY` - disable Nagle's algorithm.
    TcpNoDelay(bool),
    /// `SO_KEEPALIVE` - enable TCP keepalive probes.
    SoKeepAlive(bool),
    /// `SO_SNDBUF=<size>` - set the send buffer size.
    SoSndBuf(usize),
    /// `SO_RCVBUF=<size>` - set the receive buffer size.
    SoRcvBuf(usize),
    /// `IP_TOS=<value>` - set the IP Type of Service field.
    ///
    /// upstream: socket.c - `IP_TOS` sets the TOS byte in the IP header.
    /// The `IPTOS_LOWDELAY` / `IPTOS_THROUGHPUT` symbolic option names are
    /// upstream `OPT_ON` presets that resolve to this variant with values
    /// `0x10` and `0x08` respectively.
    IpTos(u32),
    /// `SO_BROADCAST` - allow sending broadcast datagrams.
    ///
    /// upstream: socket.c:socket_options[] `SO_BROADCAST` (OPT_BOOL).
    SoBroadcast(bool),
    /// `SO_SNDLOWAT=<n>` - send low-water mark.
    ///
    /// upstream: socket.c:socket_options[] `SO_SNDLOWAT` (OPT_INT). Guarded by
    /// `#ifdef SO_SNDLOWAT`; unavailable on Linux, so gated to the platforms
    /// whose `libc` defines it, mirroring the client-side applier.
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
    SoSndLoWat(i32),
    /// `SO_RCVLOWAT=<n>` - receive low-water mark.
    ///
    /// upstream: socket.c:socket_options[] `SO_RCVLOWAT` (OPT_INT).
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
    SoRcvLoWat(i32),
    /// `SO_SNDTIMEO=<n>` - send timeout, written as a plain `int` exactly as
    /// upstream does (`setsockopt(..., &value, sizeof(int))`).
    ///
    /// upstream: socket.c:socket_options[] `SO_SNDTIMEO` (OPT_INT).
    #[cfg(unix)]
    SoSndTimeo(i32),
    /// `SO_RCVTIMEO=<n>` - receive timeout, written as a plain `int`.
    ///
    /// upstream: socket.c:socket_options[] `SO_RCVTIMEO` (OPT_INT).
    #[cfg(unix)]
    SoRcvTimeo(i32),
}

impl SocketOption {
    /// Returns the upstream option name used in warning messages.
    ///
    /// upstream: socket.c:730-733 - `set_socket_options()` reports a failed
    /// `setsockopt(2)` as "failed to set socket option %s" using the option's
    /// `socket_options[].name`.
    fn name(&self) -> &'static str {
        match self {
            SocketOption::TcpNoDelay(_) => "TCP_NODELAY",
            SocketOption::SoKeepAlive(_) => "SO_KEEPALIVE",
            SocketOption::SoSndBuf(_) => "SO_SNDBUF",
            SocketOption::SoRcvBuf(_) => "SO_RCVBUF",
            SocketOption::IpTos(_) => "IP_TOS",
            SocketOption::SoBroadcast(_) => "SO_BROADCAST",
            #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
            SocketOption::SoSndLoWat(_) => "SO_SNDLOWAT",
            #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
            SocketOption::SoRcvLoWat(_) => "SO_RCVLOWAT",
            #[cfg(unix)]
            SocketOption::SoSndTimeo(_) => "SO_SNDTIMEO",
            #[cfg(unix)]
            SocketOption::SoRcvTimeo(_) => "SO_RCVTIMEO",
        }
    }
}

/// Parses a comma-separated socket options string into typed option values.
///
/// Accepts the upstream `rsyncd.conf` format: comma-separated option names with
/// optional `=value` suffixes. Boolean options default to `true` when no value
/// is given, and accept `0`/`1` or `true`/`false` as values.
///
/// upstream: socket.c:set_socket_options() is `void`. An unknown option name
/// (socket.c:704-707) warns and `continue`s; an `OPT_ON` preset given a value
/// (socket.c:717-727) warns but is still applied. Neither aborts the daemon, so
/// those cases warn through `log_sink` and keep parsing rather than returning an
/// error. Malformed numeric values remain a local (non-upstream) hard error.
fn parse_socket_options(
    options: &str,
    log_sink: Option<&SharedLogSink>,
) -> Result<Vec<SocketOption>, String> {
    let mut result = Vec::new();

    for part in options.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }

        let (name, value) = if let Some(eq_pos) = trimmed.find('=') {
            let n = trimmed[..eq_pos].trim();
            let v = trimmed[eq_pos + 1..].trim();
            (n, Some(v))
        } else {
            (trimmed, None)
        };

        let upper = name.to_uppercase();
        let upper = upper.replace('-', "_");
        match upper.as_str() {
            "TCP_NODELAY" => {
                let enabled = parse_bool_option_value(value, "TCP_NODELAY")?;
                result.push(SocketOption::TcpNoDelay(enabled));
            }
            "SO_KEEPALIVE" => {
                let enabled = parse_bool_option_value(value, "SO_KEEPALIVE")?;
                result.push(SocketOption::SoKeepAlive(enabled));
            }
            "SO_SNDBUF" => {
                let size = parse_size_option_value(value, "SO_SNDBUF")?;
                result.push(SocketOption::SoSndBuf(size));
            }
            "SO_RCVBUF" => {
                let size = parse_size_option_value(value, "SO_RCVBUF")?;
                result.push(SocketOption::SoRcvBuf(size));
            }
            "IP_TOS" => {
                let tos = parse_tos_option_value(value)?;
                result.push(SocketOption::IpTos(tos));
            }
            // upstream: socket.c:socket_options[] - `IPTOS_LOWDELAY` and
            // `IPTOS_THROUGHPUT` are OPT_ON presets that must not take a value
            // and resolve to fixed IP_TOS bytes (0x10 / 0x08).
            #[cfg(not(target_family = "windows"))]
            "IPTOS_LOWDELAY" => {
                warn_opt_on_value(value, "IPTOS_LOWDELAY", log_sink);
                result.push(SocketOption::IpTos(0x10));
            }
            #[cfg(not(target_family = "windows"))]
            "IPTOS_THROUGHPUT" => {
                warn_opt_on_value(value, "IPTOS_THROUGHPUT", log_sink);
                result.push(SocketOption::IpTos(0x08));
            }
            "SO_BROADCAST" => {
                let enabled = parse_bool_option_value(value, "SO_BROADCAST")?;
                result.push(SocketOption::SoBroadcast(enabled));
            }
            #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
            "SO_SNDLOWAT" => {
                let n = parse_int_option_value(value, "SO_SNDLOWAT")?;
                result.push(SocketOption::SoSndLoWat(n));
            }
            #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
            "SO_RCVLOWAT" => {
                let n = parse_int_option_value(value, "SO_RCVLOWAT")?;
                result.push(SocketOption::SoRcvLoWat(n));
            }
            #[cfg(unix)]
            "SO_SNDTIMEO" => {
                let n = parse_int_option_value(value, "SO_SNDTIMEO")?;
                result.push(SocketOption::SoSndTimeo(n));
            }
            #[cfg(unix)]
            "SO_RCVTIMEO" => {
                let n = parse_int_option_value(value, "SO_RCVTIMEO")?;
                result.push(SocketOption::SoRcvTimeo(n));
            }
            _ => {
                // upstream: socket.c:704-707 - `rprintf(FERROR,"Unknown socket
                // option %s\n",tok)` then `continue`; never fatal.
                warn_socket_option(log_sink, format!("Unknown socket option {name}"));
            }
        }
    }

    Ok(result)
}

/// Parses a boolean value for a socket option.
///
/// When no value is provided, defaults to `true` (matching upstream behavior
/// where `TCP_NODELAY` alone means enable it).
fn parse_bool_option_value(value: Option<&str>, name: &str) -> Result<bool, String> {
    match value {
        None => Ok(true),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES") => Ok(true),
        Some("0") | Some("false") | Some("FALSE") | Some("no") | Some("NO") => Ok(false),
        Some(other) => Err(format!(
            "invalid boolean value '{other}' for {name} (expected 0/1, true/false, or yes/no)"
        )),
    }
}

/// Parses a required size value for a buffer-size socket option.
fn parse_size_option_value(value: Option<&str>, name: &str) -> Result<usize, String> {
    match value {
        None => Err(format!("{name} requires a numeric value (e.g., {name}=65536)")),
        Some(s) => s
            .parse::<usize>()
            .map_err(|_| format!("invalid numeric value '{s}' for {name}")),
    }
}

/// Parses a required signed integer value for an `OPT_INT` socket option.
///
/// upstream: socket.c:set_socket_options() applies `atoi()` and passes the
/// result as a plain `int`. `SO_SNDLOWAT`/`SO_RCVLOWAT` and the
/// `SO_SNDTIMEO`/`SO_RCVTIMEO` quirk are written this way.
#[cfg(unix)]
fn parse_int_option_value(value: Option<&str>, name: &str) -> Result<i32, String> {
    match value {
        None => Err(format!("{name} requires a numeric value (e.g., {name}=1)")),
        Some(s) => s
            .parse::<i32>()
            .map_err(|_| format!("invalid numeric value '{s}' for {name}")),
    }
}

/// Warns when an `OPT_ON` preset option is given an `=value` suffix, then lets
/// the caller apply the preset anyway.
///
/// upstream: socket.c:717-727 - an `OPT_ON` entry such as `IPTOS_LOWDELAY` that
/// receives a value prints `syntax error -- %s does not take a value` but still
/// runs the `setsockopt(2)` with its fixed value. The warning is advisory, not
/// fatal.
#[cfg(not(target_family = "windows"))]
fn warn_opt_on_value(value: Option<&str>, name: &str, log_sink: Option<&SharedLogSink>) {
    if value.is_some() {
        warn_socket_option(
            log_sink,
            format!("syntax error -- {name} does not take a value"),
        );
    }
}

/// Emits a non-fatal socket-option warning through the daemon log sink.
///
/// upstream: socket.c:set_socket_options() reports parse and `setsockopt(2)`
/// problems via `rprintf(FERROR, ...)` / `rsyserr(FERROR, ...)` and continues.
/// The daemon routes those through its log sink at warning level.
fn warn_socket_option(log_sink: Option<&SharedLogSink>, text: String) {
    if let Some(log) = log_sink {
        let message = rsync_warning!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }
}

/// Parses a `IP_TOS` value, accepting both decimal and `0x`-prefixed hex.
///
/// upstream: socket.c - `IP_TOS` accepts numeric values; common presets are
/// `0x10` (IPTOS_LOWDELAY) and `0x08` (IPTOS_THROUGHPUT).
fn parse_tos_option_value(value: Option<&str>) -> Result<u32, String> {
    match value {
        None => Err("IP_TOS requires a numeric value (e.g., IP_TOS=0x10)".to_string()),
        Some(s) => {
            let trimmed = s.trim();
            if let Some(hex) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
                u32::from_str_radix(hex, 16)
                    .map_err(|_| format!("invalid hex value '{trimmed}' for IP_TOS"))
            } else {
                trimmed
                    .parse::<u32>()
                    .map_err(|_| format!("invalid numeric value '{trimmed}' for IP_TOS"))
            }
        }
    }
}

/// Applies parsed socket options to a TCP listener via `socket2`.
///
/// upstream: socket.c:449-452 - `set_socket_options()` runs before `bind(2)`
/// (socket.c:465), before the listener can process a SYN. The real daemon
/// startup path (`listener.rs::bind_with_backlog`) applies options to the
/// pre-connect `socket2::Socket` directly for that reason; this
/// `&TcpListener` entry point exists for the test-injected pre-bound-listener
/// path, where the listener is already bound (and listening) by the time
/// socket options can be applied.
fn apply_socket_options_to_listener(
    listener: &TcpListener,
    options: &[SocketOption],
    log_sink: Option<&SharedLogSink>,
) {
    apply_socket_options_impl(socket2::SockRef::from(listener), options, log_sink);
}

/// Applies parsed socket options to an accepted client `TcpStream` via `socket2`.
///
/// upstream: clientserver.c - `set_socket_options()` is called on the accepted
/// client file descriptor before the session handler processes the connection.
fn apply_socket_options_to_stream(
    stream: &TcpStream,
    options: &[SocketOption],
    log_sink: Option<&SharedLogSink>,
) {
    apply_socket_options_impl(socket2::SockRef::from(stream), options, log_sink);
}

/// Unconditionally enables `SO_KEEPALIVE` on a freshly accepted client stream.
///
/// upstream: clientserver.c:1396 - daemon unconditionally enables SO_KEEPALIVE
/// on the accepted client socket via `set_socket_options(f_in, "SO_KEEPALIVE")`
/// in `start_daemon()`, before the protocol handshake and independent of the
/// per-module `socket options` config (which is a separate concern applied via
/// `lp_socket_options()`). Without it, idle daemon connections can be silently
/// dropped by NAT/firewall timeouts. Best-effort: a failed `setsockopt(2)`
/// warns and the session still proceeds, mirroring upstream's warn-and-continue
/// in socket.c:730-733.
fn enable_accepted_stream_keepalive(stream: &TcpStream, log_sink: Option<&SharedLogSink>) {
    if let Err(error) = socket2::SockRef::from(stream).set_keepalive(true) {
        warn_socket_option(
            log_sink,
            format!("failed to set socket option SO_KEEPALIVE: {error}"),
        );
    }
}

/// Shared implementation for applying socket options to any socket reference.
///
/// upstream: socket.c:730-733 - `set_socket_options()` applies each option
/// independently; on a failed `setsockopt(2)` it emits a warning
/// (`rsyserr(FERROR, errno, "failed to set socket option %s", tok)`) and
/// `continue`s to the next option. A single failed option never aborts the
/// connection, so we warn-and-continue rather than propagate an error.
fn apply_socket_options_impl(
    sock: socket2::SockRef<'_>,
    options: &[SocketOption],
    log_sink: Option<&SharedLogSink>,
) {
    for opt in options {
        let result: io::Result<()> = match opt {
            SocketOption::TcpNoDelay(enabled) => sock.set_tcp_nodelay(*enabled),
            SocketOption::SoKeepAlive(enabled) => sock.set_keepalive(*enabled),
            SocketOption::SoBroadcast(enabled) => sock.set_broadcast(*enabled),
            SocketOption::SoSndBuf(size) => sock.set_send_buffer_size(*size),
            SocketOption::SoRcvBuf(size) => sock.set_recv_buffer_size(*size),
            SocketOption::IpTos(tos) => apply_ip_tos(&sock, *tos),
            #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
            SocketOption::SoSndLoWat(n) => {
                apply_raw_int(&sock, libc::SOL_SOCKET, libc::SO_SNDLOWAT, *n)
            }
            #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
            SocketOption::SoRcvLoWat(n) => {
                apply_raw_int(&sock, libc::SOL_SOCKET, libc::SO_RCVLOWAT, *n)
            }
            #[cfg(unix)]
            SocketOption::SoSndTimeo(n) => {
                apply_raw_int(&sock, libc::SOL_SOCKET, libc::SO_SNDTIMEO, *n)
            }
            #[cfg(unix)]
            SocketOption::SoRcvTimeo(n) => {
                apply_raw_int(&sock, libc::SOL_SOCKET, libc::SO_RCVTIMEO, *n)
            }
        };

        if let Err(error) = result {
            // upstream: socket.c:730-733 - warn and keep applying the rest.
            warn_socket_option(
                log_sink,
                format!("failed to set socket option {}: {error}", opt.name()),
            );
        }
    }
}

/// Applies an integer socket option that has no typed `socket2` setter.
///
/// `socket2::SockRef` unifies the listener and stream apply paths, so the raw
/// `setsockopt(2)` call is routed through `fast_io` (the only crate permitted
/// to hold the unsafe FFI) using the socket's borrowed file descriptor.
///
/// upstream: socket.c:set_socket_options() writes these OPT_INT entries via
/// `setsockopt(fd, level, option, &value, sizeof(int))`.
#[cfg(unix)]
fn apply_raw_int(
    sock: &socket2::SockRef<'_>,
    level: libc::c_int,
    option: libc::c_int,
    value: i32,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    fast_io::set_socket_int_option_raw(sock.as_raw_fd(), level, option, value)
}

/// Sets IP_TOS / IPV6_TCLASS depending on the socket's address family.
///
/// socket2 0.6 split the unified `set_tos` into `set_tos_v4` for IPv4 and
/// `set_tclass_v6` for IPv6 (both take `u32`). Upstream rsync's `socket.c`
/// calls `setsockopt(..., IPPROTO_IP, IP_TOS, ...)` which only succeeds on
/// AF_INET sockets; we mirror that semantics for v4 and apply the equivalent
/// `IPV6_TCLASS` for v6 on Unix. socket2 only exposes `set_tclass_v6` on
/// Unix targets, so on Windows we skip v6 to mirror upstream's v4-only
/// IP_TOS behaviour.
fn apply_ip_tos(sock: &socket2::SockRef<'_>, tos: u32) -> io::Result<()> {
    match sock.local_addr()?.as_socket() {
        Some(std::net::SocketAddr::V4(_)) => sock.set_tos_v4(tos),
        #[cfg(unix)]
        Some(std::net::SocketAddr::V6(_)) => sock.set_tclass_v6(tos),
        #[cfg(not(unix))]
        Some(std::net::SocketAddr::V6(_)) => Ok(()),
        None => Err(io::Error::other(
            "cannot determine socket address family for IP_TOS",
        )),
    }
}
