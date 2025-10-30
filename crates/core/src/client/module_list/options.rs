use std::ffi::{OsStr, OsString};
use std::net::SocketAddr;

use crate::client::AddressMode;

/// Configuration toggles that influence daemon module listings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleListOptions {
    suppress_motd: bool,
    address_mode: AddressMode,
    connect_program: Option<OsString>,
    bind_address: Option<SocketAddr>,
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
    pub fn connect_program(&self) -> Option<&OsStr> {
        self.connect_program.as_deref()
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
