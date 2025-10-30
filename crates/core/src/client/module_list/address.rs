use std::fmt;

use crate::client::{ClientError, FEATURE_UNAVAILABLE_EXIT_CODE, daemon_error};

/// Target daemon address used for module listing requests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonAddress {
    host: String,
    port: u16,
}

impl DaemonAddress {
    /// Creates a new daemon address from the supplied host and port.
    pub fn new(host: String, port: u16) -> Result<Self, ClientError> {
        let trimmed = host.trim();
        if trimmed.is_empty() {
            return Err(daemon_error(
                "daemon host must be non-empty",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            ));
        }
        Ok(Self {
            host: trimmed.to_string(),
            port,
        })
    }

    /// Returns the daemon host name or address.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Returns the daemon TCP port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    pub(super) fn socket_addr_display(&self) -> SocketAddrDisplay<'_> {
        SocketAddrDisplay {
            host: &self.host,
            port: self.port,
        }
    }
}

pub(super) struct SocketAddrDisplay<'a> {
    pub(super) host: &'a str,
    pub(super) port: u16,
}

impl fmt::Display for SocketAddrDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') && !self.host.starts_with('[') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

pub(super) fn parse_bracketed_host(
    host: &str,
    default_port: u16,
) -> Result<(String, u16), ClientError> {
    let (addr, remainder) = host.split_once(']').ok_or_else(|| {
        daemon_error(
            "invalid bracketed daemon host",
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    let decoded = decode_host_component(addr)?;

    if remainder.is_empty() {
        return Ok((decoded, default_port));
    }

    let port = remainder
        .strip_prefix(':')
        .ok_or_else(|| {
            daemon_error(
                "invalid bracketed daemon host",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            )
        })?
        .parse::<u16>()
        .map_err(|_| daemon_error("invalid daemon port", FEATURE_UNAVAILABLE_EXIT_CODE))?;

    Ok((decoded, port))
}

pub(super) fn decode_host_component(input: &str) -> Result<String, ClientError> {
    decode_percent_component(
        input,
        invalid_percent_encoding_error,
        invalid_host_utf8_error,
    )
}

pub(super) fn decode_daemon_username(input: &str) -> Result<String, ClientError> {
    decode_percent_component(
        input,
        invalid_username_percent_encoding_error,
        invalid_username_utf8_error,
    )
}

pub(super) fn split_host_port(input: &str) -> Option<(&str, &str)> {
    let idx = input.rfind(':')?;
    let (host, port) = input.split_at(idx);
    if host.contains(':') {
        return None;
    }
    Some((host, &port[1..]))
}

fn decode_percent_component(
    input: &str,
    truncated_error: fn() -> ClientError,
    invalid_utf8_error: fn() -> ClientError,
) -> Result<String, ClientError> {
    if !input.contains('%') {
        return Ok(input.to_string());
    }

    let mut decoded = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(truncated_error());
            }

            let hi = hex_value(bytes[index + 1]);
            let lo = hex_value(bytes[index + 2]);

            if let (Some(hi), Some(lo)) = (hi, lo) {
                decoded.push((hi << 4) | lo);
                index += 3;
                continue;
            }
        }

        decoded.push(bytes[index]);
        index += 1;
    }

    String::from_utf8(decoded).map_err(|_| invalid_utf8_error())
}

pub(super) fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(10 + byte - b'a'),
        b'A'..=b'F' => Some(10 + byte - b'A'),
        _ => None,
    }
}

fn invalid_percent_encoding_error() -> ClientError {
    daemon_error(
        "invalid percent-encoding in daemon host",
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}

fn invalid_host_utf8_error() -> ClientError {
    daemon_error(
        "daemon host contains invalid UTF-8 after percent-decoding",
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}

fn invalid_username_percent_encoding_error() -> ClientError {
    daemon_error(
        "invalid percent-encoding in daemon username",
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}

fn invalid_username_utf8_error() -> ClientError {
    daemon_error(
        "daemon username contains invalid UTF-8 after percent-decoding",
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}
