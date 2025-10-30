use super::*;

impl ClientConfig {
    /// Returns the preferred address family used for daemon or remote-shell connections.
    #[must_use]
    #[doc(alias = "--ipv4")]
    #[doc(alias = "--ipv6")]
    pub const fn address_mode(&self) -> AddressMode {
        self.address_mode
    }

    /// Returns the configured connect program, if any.
    #[must_use]
    #[doc(alias = "--connect-program")]
    pub fn connect_program(&self) -> Option<&OsStr> {
        self.connect_program.as_deref()
    }

    /// Returns the configured bind address, if any.
    #[must_use]
    #[doc(alias = "--address")]
    pub fn bind_address(&self) -> Option<&BindAddress> {
        self.bind_address.as_ref()
    }

    /// Returns the requested bandwidth limit, if any.
    #[must_use]
    pub fn bandwidth_limit(&self) -> Option<BandwidthLimit> {
        self.bandwidth_limit
    }

    /// Returns the configured transfer timeout.
    #[must_use]
    #[doc(alias = "--timeout")]
    pub const fn timeout(&self) -> TransferTimeout {
        self.timeout
    }

    /// Returns the configured connection timeout.
    #[must_use]
    #[doc(alias = "--contimeout")]
    pub const fn connect_timeout(&self) -> TransferTimeout {
        self.connect_timeout
    }
}
