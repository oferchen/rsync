use super::*;

impl ClientConfigBuilder {
    /// Configures the local bind address applied to network transports.
    #[must_use]
    #[doc(alias = "--address")]
    pub fn bind_address(mut self, address: Option<BindAddress>) -> Self {
        self.bind_address = address;
        self
    }

    /// Configures socket options that should be forwarded to network transports.
    #[must_use]
    #[doc(alias = "--sockopts")]
    pub fn sockopts(mut self, sockopts: Option<OsString>) -> Self {
        self.sockopts = sockopts;
        self
    }

    /// Controls whether blocking I/O should be forced for remote shells.
    #[must_use]
    #[doc(alias = "--blocking-io")]
    #[doc(alias = "--no-blocking-io")]
    pub fn blocking_io(mut self, blocking: Option<bool>) -> Self {
        self.blocking_io = blocking;
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

    /// Configures the iconv charset conversion behaviour forwarded to downstream transports.
    #[must_use]
    #[doc(alias = "--iconv")]
    pub fn iconv(mut self, setting: IconvSetting) -> Self {
        self.iconv = setting;
        self
    }

    /// Configures a custom remote shell command for SSH transfers.
    ///
    /// This method accepts an iterable of command arguments where the first element
    /// is the program name and subsequent elements are its arguments.
    ///
    /// # Example
    /// ```
    /// # use core::client::ClientConfig;
    /// let config = ClientConfig::builder()
    ///     .set_remote_shell(vec!["ssh", "-p", "2222", "-i", "/path/to/key"])
    ///     .build();
    /// ```
    #[must_use]
    #[doc(alias = "--rsh")]
    #[doc(alias = "-e")]
    pub fn set_remote_shell<I, S>(mut self, spec: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.remote_shell = Some(spec.into_iter().map(Into::into).collect());
        self
    }

    /// Configures the path to the remote rsync binary.
    ///
    /// This overrides the default "rsync" command used on the remote host.
    ///
    /// # Example
    /// ```
    /// # use core::client::ClientConfig;
    /// let config = ClientConfig::builder()
    ///     .set_rsync_path("/opt/rsync/bin/rsync")
    ///     .build();
    /// ```
    #[must_use]
    #[doc(alias = "--rsync-path")]
    pub fn set_rsync_path<S: Into<OsString>>(mut self, path: S) -> Self {
        self.rsync_path = Some(path.into());
        self
    }
}
