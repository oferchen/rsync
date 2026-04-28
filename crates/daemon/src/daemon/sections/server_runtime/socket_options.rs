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
    /// Common values: `0x10` (IPTOS_LOWDELAY), `0x08` (IPTOS_THROUGHPUT).
    IpTos(u32),
}

/// Parses a comma-separated socket options string into typed option values.
///
/// Accepts the upstream `rsyncd.conf` format: comma-separated option names with
/// optional `=value` suffixes. Boolean options default to `true` when no value
/// is given, and accept `0`/`1` or `true`/`false` as values.
///
/// upstream: socket.c - `set_socket_options()` supports a similar format.
fn parse_socket_options(options: &str) -> Result<Vec<SocketOption>, String> {
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
            _ => {
                return Err(format!("unknown socket option '{name}'"));
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
/// upstream: socket.c - `set_socket_options()` applies options via `setsockopt(2)`
/// after binding and before accepting connections.
fn apply_socket_options_to_listener(
    listener: &TcpListener,
    options: &[SocketOption],
) -> io::Result<()> {
    apply_socket_options_impl(socket2::SockRef::from(listener), options)
}

/// Applies parsed socket options to an accepted client `TcpStream` via `socket2`.
///
/// upstream: clientserver.c - `set_socket_options()` is called on the accepted
/// client file descriptor before the session handler processes the connection.
fn apply_socket_options_to_stream(
    stream: &TcpStream,
    options: &[SocketOption],
) -> io::Result<()> {
    apply_socket_options_impl(socket2::SockRef::from(stream), options)
}

/// Shared implementation for applying socket options to any socket reference.
fn apply_socket_options_impl(
    sock: socket2::SockRef<'_>,
    options: &[SocketOption],
) -> io::Result<()> {
    for opt in options {
        match opt {
            SocketOption::TcpNoDelay(enabled) => sock.set_tcp_nodelay(*enabled)?,
            SocketOption::SoKeepAlive(enabled) => sock.set_keepalive(*enabled)?,
            SocketOption::SoSndBuf(size) => sock.set_send_buffer_size(*size)?,
            SocketOption::SoRcvBuf(size) => sock.set_recv_buffer_size(*size)?,
            SocketOption::IpTos(tos) => apply_ip_tos(&sock, *tos)?,
        }
    }
    Ok(())
}

/// Sets IP_TOS / IPV6_TCLASS depending on the socket's address family.
///
/// socket2 0.6 split the unified `set_tos` into `set_tos_v4` for IPv4 and
/// `set_tclass_v6` for IPv6 (both take `u32`). Upstream rsync's `socket.c`
/// calls `setsockopt(..., IPPROTO_IP, IP_TOS, ...)` which only succeeds on
/// AF_INET sockets; we mirror that semantics for v4 and apply the equivalent
/// `IPV6_TCLASS` for v6.
fn apply_ip_tos(sock: &socket2::SockRef<'_>, tos: u32) -> io::Result<()> {
    match sock.local_addr()?.as_socket() {
        Some(std::net::SocketAddr::V4(_)) => sock.set_tos_v4(tos),
        Some(std::net::SocketAddr::V6(_)) => sock.set_tclass_v6(tos),
        None => Err(io::Error::other(
            "cannot determine socket address family for IP_TOS",
        )),
    }
}
