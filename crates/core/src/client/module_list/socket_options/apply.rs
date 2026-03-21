//! Public entry point for applying socket options to a TCP stream.
//!
//! Parses a comma/whitespace-separated option string and applies each
//! option via platform-specific `setsockopt` calls.
// upstream: socket.c:set_socket_options()

use std::ffi::OsStr;
use std::net::TcpStream;

use super::errors::{socket_option_error, unknown_option};
use super::lookup::{intern_name, lookup_socket_option, parse_socket_option_value};
use super::types::{ParsedSocketOption, SocketOptionKind};
use crate::client::ClientError;

#[cfg(not(target_family = "windows"))]
use super::errors::option_disallows_value;

/// Applies caller-provided socket options to the supplied TCP stream.
///
/// The option string is a comma/whitespace-separated list of names with
/// optional `=value` suffixes:
///
/// - `SO_KEEPALIVE`
/// - `SO_SNDBUF=65536`
/// - `TCP_NODELAY`
/// - `IPTOS_LOWDELAY` (Unix only)
pub(crate) fn apply_socket_options(stream: &TcpStream, options: &OsStr) -> Result<(), ClientError> {
    let list = options.to_string_lossy();

    if list.trim().is_empty() {
        return Ok(());
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

        let kind = lookup_socket_option(name).ok_or_else(|| unknown_option(name))?;

        #[cfg(not(target_family = "windows"))]
        {
            match kind {
                SocketOptionKind::On { .. } if value_str.is_some() => {
                    return Err(option_disallows_value(name));
                }
                SocketOptionKind::On { .. } => {
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
        option
            .apply(stream)
            .map_err(|error| socket_option_error(option.name(), error))?;
    }

    Ok(())
}
