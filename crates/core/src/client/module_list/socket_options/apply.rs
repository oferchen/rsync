//! Public entry point for applying socket options to a pre-connect socket.
//!
//! Parses a comma/whitespace-separated option string and applies each
//! option via platform-specific `setsockopt` calls.
// upstream: socket.c:set_socket_options()

use std::ffi::OsStr;

use super::lookup::{intern_name, lookup_socket_option, parse_socket_option_value};
use super::types::ParsedSocketOption;
#[cfg(not(target_family = "windows"))]
use super::types::SocketOptionKind;

/// Applies caller-provided socket options to the supplied socket.
///
/// The option string is a comma/whitespace-separated list of names with
/// optional `=value` suffixes:
///
/// - `SO_KEEPALIVE`
/// - `SO_SNDBUF=65536`
/// - `TCP_NODELAY`
/// - `IPTOS_LOWDELAY` (Unix only)
///
/// Must be called on `socket` before `connect(2)` - upstream:
/// socket.c:279-280 applies `set_socket_options(s, sockopts)` immediately
/// before `connect(s, ...)` so options that shape the SYN (e.g.
/// `SO_SNDBUF`/`SO_RCVBUF` window scaling) actually take effect; setting
/// them after `connect(2)` returns is a no-op for the handshake.
///
/// upstream: socket.c:set_socket_options() is `void` - every parse or
/// `setsockopt(2)` failure warns to stderr and `continue`s the loop, so a bad
/// option never aborts the connection. We mirror that warn-and-continue
/// contract rather than propagating a fatal error.
pub(crate) fn apply_socket_options(socket: &socket2::Socket, options: &OsStr) {
    let list = options.to_string_lossy();

    if list.trim().is_empty() {
        return;
    }

    let mut parsed = Vec::new();

    for token in list
        .split(|ch: char| ch == ',' || ch.is_ascii_whitespace())
        .filter(|token| !token.is_empty())
    {
        let (name, value_str) = match token.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (token, None),
        };

        let Some(kind) = lookup_socket_option(name) else {
            // upstream: socket.c:704-707 - an unknown option name reports
            // `rprintf(FERROR,"Unknown socket option %s\n",tok)` and `continue`s.
            eprintln!("Unknown socket option {name}");
            continue;
        };

        #[cfg(not(target_family = "windows"))]
        {
            match kind {
                SocketOptionKind::On { .. } => {
                    // upstream: socket.c:717-727 - an OPT_ON option given a
                    // value warns (`syntax error -- %s does not take a value`)
                    // but still applies its fixed value.
                    if value_str.is_some() {
                        eprintln!("syntax error -- {name} does not take a value");
                    }
                    parsed.push(ParsedSocketOption {
                        kind,
                        explicit_value: None,
                        name: intern_name(name),
                    });
                }
                SocketOptionKind::Bool { .. } | SocketOptionKind::Int { .. } => {
                    let parsed_value = value_str.map_or(1, parse_socket_option_value);
                    parsed.push(ParsedSocketOption {
                        kind,
                        explicit_value: Some(parsed_value),
                        name: intern_name(name),
                    });
                }
            }
        }

        #[cfg(target_family = "windows")]
        {
            let parsed_value = value_str
                .map(parse_socket_option_value)
                .unwrap_or(libc::c_int::from(1));
            parsed.push(ParsedSocketOption {
                kind,
                explicit_value: Some(parsed_value),
                name: intern_name(name),
            });
        }
    }

    for option in parsed {
        if let Err(error) = option.apply(socket) {
            // upstream: socket.c:730-733 - a failed `setsockopt(2)` reports
            // `rsyserr(FERROR, errno, "failed to set socket option %s")` and
            // keeps applying the remaining options.
            eprintln!("failed to set socket option {}: {error}", option.name());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::apply_socket_options;

    /// Builds a connected `socket2::Socket`, mirroring the pre-connect socket
    /// `apply_socket_options` runs against (upstream: socket.c:279-280 -
    /// `set_socket_options()` must run before `connect(2)`). The accept thread
    /// only needs to complete the handshake; the options under test are applied
    /// (and read back) after connect for test convenience, which does not
    /// change the option semantics being exercised here.
    fn connected_socket() -> (socket2::Socket, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let addr = listener.local_addr().expect("addr");

        let handle = std::thread::spawn(move || {
            let _ = listener.accept();
        });

        let stream = std::net::TcpStream::connect(addr).expect("connect");
        (socket2::Socket::from(stream), handle)
    }

    #[test]
    fn apply_socket_options_sets_send_buffer_size() {
        let (socket, handle) = connected_socket();

        apply_socket_options(&socket, std::ffi::OsStr::new("SO_SNDBUF=32768"));

        let reported = socket.send_buffer_size().expect("query send buffer size");
        assert!(reported >= 32768);

        drop(socket);
        handle.join().expect("accept thread completes");
    }

    /// upstream: socket.c:704-707 - an unknown option name warns
    /// (`Unknown socket option %s`) and `continue`s; `set_socket_options()` is
    /// `void`, so a bogus name must never abort the connection. A later valid
    /// option in the same string must still be applied, proving the loop
    /// continued past the unknown token instead of bailing out.
    #[test]
    fn apply_socket_options_warns_and_continues_on_unknown_name() {
        let (socket, handle) = connected_socket();

        apply_socket_options(&socket, std::ffi::OsStr::new("SO_NOTREAL=1,SO_SNDBUF=32768"));

        assert!(
            socket.send_buffer_size().expect("query send buffer size") >= 32768,
            "valid option after an unknown one must still apply"
        );

        drop(socket);
        handle.join().expect("accept thread completes");
    }

    /// upstream: socket.c:717-727 - an OPT_ON option (e.g. `IPTOS_LOWDELAY`) given
    /// a value warns (`syntax error -- %s does not take a value`) but still applies
    /// its fixed value. The value must not turn the option into a fatal error.
    #[cfg(not(target_family = "windows"))]
    #[test]
    fn apply_socket_options_opt_on_with_value_still_applies() {
        let (socket, handle) = connected_socket();

        // IPTOS_LOWDELAY is an IPv4 IP_TOS preset; supplying `=5` is a user error
        // upstream warns about but still applies the 0x10 preset on an AF_INET
        // socket. This must return normally (no panic / no fatal error).
        apply_socket_options(&socket, std::ffi::OsStr::new("IPTOS_LOWDELAY=5"));

        drop(socket);
        handle.join().expect("accept thread completes");
    }
}
