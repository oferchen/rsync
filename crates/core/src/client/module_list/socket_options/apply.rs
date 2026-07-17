//! Public entry point for applying socket options to a TCP stream.
//!
//! Parses a comma/whitespace-separated option string and applies each
//! option via platform-specific `setsockopt` calls.
// upstream: socket.c:set_socket_options()

use std::ffi::OsStr;
use std::net::TcpStream;

use super::lookup::{intern_name, lookup_socket_option, parse_socket_option_value};
use super::types::ParsedSocketOption;
#[cfg(not(target_family = "windows"))]
use super::types::SocketOptionKind;

/// Applies caller-provided socket options to the supplied TCP stream.
///
/// The option string is a comma/whitespace-separated list of names with
/// optional `=value` suffixes:
///
/// - `SO_KEEPALIVE`
/// - `SO_SNDBUF=65536`
/// - `TCP_NODELAY`
/// - `IPTOS_LOWDELAY` (Unix only)
///
/// upstream: socket.c:set_socket_options() is `void` - every parse or
/// `setsockopt(2)` failure warns to stderr and `continue`s the loop, so a bad
/// option never aborts the connection. We mirror that warn-and-continue
/// contract rather than propagating a fatal error.
pub(crate) fn apply_socket_options(stream: &TcpStream, options: &OsStr) {
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
        if let Err(error) = option.apply(stream) {
            // upstream: socket.c:730-733 - a failed `setsockopt(2)` reports
            // `rsyserr(FERROR, errno, "failed to set socket option %s")` and
            // keeps applying the remaining options.
            eprintln!("failed to set socket option {}: {error}", option.name());
        }
    }
}
