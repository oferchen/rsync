use std::ffi::OsString;

use rsync_protocol::ProtocolVersion;

use super::{
    address::{
        decode_daemon_username, decode_host_component, parse_bracketed_host, split_host_port,
    },
    util::strip_prefix_ignore_ascii_case,
};
use crate::client::{ClientError, DaemonAddress, FEATURE_UNAVAILABLE_EXIT_CODE, daemon_error};

/// Specification describing a daemon module listing request parsed from CLI operands.
///
/// The request retains the optional username embedded in the operand so future
/// authentication flows can reuse the caller-supplied identity even though the
/// current module listing implementation performs anonymous queries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleListRequest {
    pub(super) address: DaemonAddress,
    pub(super) username: Option<String>,
    pub(super) protocol: ProtocolVersion,
}

impl ModuleListRequest {
    /// Default TCP port used by rsync daemons when a port is not specified.
    pub const DEFAULT_PORT: u16 = 873;

    /// Attempts to derive a module listing request from CLI-style operands.
    pub fn from_operands(operands: &[OsString]) -> Result<Option<Self>, ClientError> {
        Self::from_operands_with_port(operands, Self::DEFAULT_PORT)
    }

    /// Equivalent to [`Self::from_operands`] but allows overriding the default
    /// daemon port.
    pub fn from_operands_with_port(
        operands: &[OsString],
        default_port: u16,
    ) -> Result<Option<Self>, ClientError> {
        if operands.len() != 1 {
            return Ok(None);
        }

        Self::from_operand(&operands[0], default_port)
    }

    fn from_operand(operand: &OsString, default_port: u16) -> Result<Option<Self>, ClientError> {
        let text = operand.to_string_lossy();

        if let Some(rest) = strip_prefix_ignore_ascii_case(&text, "rsync://") {
            return parse_rsync_url(rest, default_port);
        }

        if let Some((host_part, module_part)) = split_daemon_host_module(&text)? {
            if module_part.is_empty() {
                let target = parse_host_port(host_part, default_port)?;
                return Ok(Some(Self::new(target.address, target.username)));
            }
            return Ok(None);
        }

        Ok(None)
    }

    fn new(address: DaemonAddress, username: Option<String>) -> Self {
        Self {
            address,
            username,
            protocol: ProtocolVersion::NEWEST,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_components(
        address: DaemonAddress,
        username: Option<String>,
        protocol: ProtocolVersion,
    ) -> Self {
        Self {
            address,
            username,
            protocol,
        }
    }

    /// Returns the parsed daemon address.
    #[must_use]
    pub fn address(&self) -> &DaemonAddress {
        &self.address
    }

    /// Returns the optional username supplied in the daemon URL or legacy syntax.
    #[must_use]
    pub fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }

    /// Returns the desired protocol version for daemon negotiation.
    #[must_use]
    pub const fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }

    /// Returns a new request that clamps the negotiation to the provided protocol.
    #[must_use]
    pub const fn with_protocol(mut self, protocol: ProtocolVersion) -> Self {
        self.protocol = protocol;
        self
    }
}

fn parse_rsync_url(
    rest: &str,
    default_port: u16,
) -> Result<Option<ModuleListRequest>, ClientError> {
    let mut parts = rest.splitn(2, '/');
    let host_port = parts.next().unwrap_or("");
    let remainder = parts.next();

    if remainder.is_some_and(|path| !path.is_empty()) {
        return Ok(None);
    }

    let target = parse_host_port(host_port, default_port)?;
    Ok(Some(ModuleListRequest::new(
        target.address,
        target.username,
    )))
}

struct ParsedDaemonTarget {
    address: DaemonAddress,
    username: Option<String>,
}

fn parse_host_port(input: &str, default_port: u16) -> Result<ParsedDaemonTarget, ClientError> {
    const DEFAULT_HOST: &str = "localhost";

    let (username, input) = split_daemon_username(input)?;
    let username = username.map(decode_daemon_username).transpose()?;

    if input.is_empty() {
        let address = DaemonAddress::new(DEFAULT_HOST.to_string(), default_port)?;
        return Ok(ParsedDaemonTarget { address, username });
    }

    if let Some(host) = input.strip_prefix('[') {
        let (address, port) = parse_bracketed_host(host, default_port)?;
        let address = DaemonAddress::new(address, port)?;
        return Ok(ParsedDaemonTarget { address, username });
    }

    if let Some((host, port)) = split_host_port(input) {
        let port = port
            .parse::<u16>()
            .map_err(|_| daemon_error("invalid daemon port", FEATURE_UNAVAILABLE_EXIT_CODE))?;
        let host = decode_host_component(host)?;
        let address = DaemonAddress::new(host, port)?;
        return Ok(ParsedDaemonTarget { address, username });
    }

    let host = decode_host_component(input)?;
    let address = DaemonAddress::new(host, default_port)?;
    Ok(ParsedDaemonTarget { address, username })
}

fn split_daemon_host_module(input: &str) -> Result<Option<(&str, &str)>, ClientError> {
    if !input.contains('[') {
        let segments = input.split("::");
        if segments.clone().count() > 2 {
            return Err(daemon_error(
                "IPv6 daemon addresses must be enclosed in brackets",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            ));
        }
    }

    let mut in_brackets = false;
    let mut previous_colon = None;

    for (idx, ch) in input.char_indices() {
        match ch {
            '[' => {
                in_brackets = true;
                previous_colon = None;
            }
            ']' => {
                in_brackets = false;
                previous_colon = None;
            }
            ':' if !in_brackets => {
                if let Some(prev) = previous_colon.filter(|prev| *prev + 1 == idx) {
                    let host = &input[..prev];
                    if !host.contains('[') {
                        let colon_count = host.chars().filter(|&ch| ch == ':').count();
                        if colon_count > 1 {
                            return Err(daemon_error(
                                "IPv6 daemon addresses must be enclosed in brackets",
                                FEATURE_UNAVAILABLE_EXIT_CODE,
                            ));
                        }
                    }
                    let module = &input[idx + 1..];
                    return Ok(Some((host, module)));
                }
                previous_colon = Some(idx);
            }
            _ => {
                previous_colon = None;
            }
        }
    }

    Ok(None)
}

fn split_daemon_username(input: &str) -> Result<(Option<&str>, &str), ClientError> {
    if let Some(idx) = input.rfind('@') {
        let (user, host) = input.split_at(idx);
        if user.is_empty() {
            return Err(daemon_error(
                "daemon username must be non-empty",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            ));
        }

        return Ok((Some(user), &host[1..]));
    }

    Ok((None, input))
}
