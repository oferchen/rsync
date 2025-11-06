use super::*;

impl ClientConfigBuilder {
    /// Configures the local bind address applied to network transports.
    #[must_use]
    #[doc(alias = "--address")]
    pub fn bind_address(mut self, address: Option<BindAddress>) -> Self {
        self.bind_address = address;
        self
    }

    /// Sets the timeout configuration that should apply to network transfers.
    #[must_use]
    #[doc(alias = "--timeout")]
    pub const fn timeout(mut self, timeout: TransferTimeout) -> Self {
        self.timeout = timeout;
        self
    }

    /// Configures the connection timeout applied to network handshakes.
    #[must_use]
    #[doc(alias = "--contimeout")]
    pub const fn connect_timeout(mut self, timeout: TransferTimeout) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Configures the deadline at which the transfer should stop.
    #[must_use]
    #[doc(alias = "--stop-after")]
    #[doc(alias = "--stop-at")]
    pub fn stop_at(mut self, deadline: Option<SystemTime>) -> Self {
        self.stop_deadline = deadline;
        self
    }

    /// Configures the command used to reach rsync:// daemons.
    #[must_use]
    #[doc(alias = "--connect-program")]
    pub fn connect_program(mut self, program: Option<OsString>) -> Self {
        self.connect_program = program;
        self
    }

    /// Selects the preferred address family for network operations.
    #[must_use]
    #[doc(alias = "--ipv4")]
    #[doc(alias = "--ipv6")]
    pub const fn address_mode(mut self, mode: AddressMode) -> Self {
        self.address_mode = mode;
        self
    }
}
