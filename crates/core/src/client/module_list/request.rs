use std::ffi::{OsStr, OsString};
use std::net::SocketAddr;

use rsync_protocol::ProtocolVersion;

use super::super::{AddressMode, ClientError};
use super::parsing::{parse_host_port, split_daemon_host_module, strip_prefix_ignore_ascii_case};
use super::types::DaemonAddress;

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
            return Self::parse_rsync_url(rest, default_port);
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

    fn parse_rsync_url(rest: &str, default_port: u16) -> Result<Option<Self>, ClientError> {
        let mut parts = rest.splitn(2, '/');
        let host_port = parts.next().unwrap_or("");
        let remainder = parts.next();

        if remainder.is_some_and(|path| !path.is_empty()) {
            return Ok(None);
        }

        let target = parse_host_port(host_port, default_port)?;
        Ok(Some(Self::new(target.address, target.username)))
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

/// Configuration toggles that influence daemon module listings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleListOptions {
    suppress_motd: bool,
    address_mode: AddressMode,
    connect_program: Option<OsString>,
    bind_address: Option<SocketAddr>,
    sockopts: Option<OsString>,
    blocking_io: Option<bool>,
}

impl ModuleListOptions {
    /// Creates a new options structure with all toggles disabled.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            suppress_motd: false,
            address_mode: AddressMode::Default,
            connect_program: None,
            bind_address: None,
            sockopts: None,
            blocking_io: None,
        }
    }

    /// Returns a new configuration that suppresses daemon MOTD lines.
    #[must_use]
    pub const fn suppress_motd(mut self, suppress: bool) -> Self {
        self.suppress_motd = suppress;
        self
    }

    /// Returns whether MOTD lines should be suppressed.
    #[must_use]
    pub const fn suppresses_motd(&self) -> bool {
        self.suppress_motd
    }

    /// Configures the preferred address family for the daemon connection.
    #[must_use]
    #[doc(alias = "--ipv4")]
    #[doc(alias = "--ipv6")]
    pub const fn with_address_mode(mut self, mode: AddressMode) -> Self {
        self.address_mode = mode;
        self
    }

    /// Returns the preferred address family.
    #[must_use]
    pub const fn address_mode(&self) -> AddressMode {
        self.address_mode
    }

    /// Supplies an explicit connect program command.
    #[must_use]
    #[doc(alias = "--connect-program")]
    pub fn with_connect_program(mut self, program: Option<OsString>) -> Self {
        self.connect_program = program;
        self
    }

    /// Returns the configured connect program command, if any.
    #[must_use]
    pub fn connect_program(&self) -> Option<&std::ffi::OsStr> {
        self.connect_program.as_deref()
    }

    /// Configures additional socket options that should be applied to daemon connections.
    #[must_use]
    #[doc(alias = "--sockopts")]
    pub fn with_sockopts(mut self, sockopts: Option<OsString>) -> Self {
        self.sockopts = sockopts;
        self
    }

    /// Returns the configured socket options, if any.
    #[must_use]
    pub fn sockopts(&self) -> Option<&OsStr> {
        self.sockopts.as_deref()
    }

    /// Configures the desired blocking I/O mode for daemon TCP sockets.
    #[must_use]
    #[doc(alias = "--blocking-io")]
    #[doc(alias = "--no-blocking-io")]
    pub const fn with_blocking_io(mut self, blocking: Option<bool>) -> Self {
        self.blocking_io = blocking;
        self
    }

    /// Returns the configured blocking I/O preference, if any.
    #[must_use]
    pub const fn blocking_io(&self) -> Option<bool> {
        self.blocking_io
    }

    /// Configures the bind address used when contacting the daemon directly or via a proxy.
    #[must_use]
    pub const fn with_bind_address(mut self, address: Option<SocketAddr>) -> Self {
        self.bind_address = address;
        self
    }

    /// Returns the configured bind address, if any.
    #[must_use]
    pub const fn bind_address(&self) -> Option<SocketAddr> {
        self.bind_address
    }
}

impl Default for ModuleListOptions {
    fn default() -> Self {
        Self::new()
    }
}
