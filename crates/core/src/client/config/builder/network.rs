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

    builder_setter! {
        /// Controls whether blocking I/O should be forced for remote shells.
        #[doc(alias = "--blocking-io")]
        #[doc(alias = "--no-blocking-io")]
        blocking_io: Option<bool>,

        /// Sets the timeout configuration that should apply to network transfers.
        #[doc(alias = "--timeout")]
        timeout: TransferTimeout,

        /// Configures the connection timeout applied to network handshakes.
        #[doc(alias = "--contimeout")]
        connect_timeout: TransferTimeout,

        /// Selects the preferred address family for network operations.
        #[doc(alias = "--ipv4")]
        #[doc(alias = "--ipv6")]
        address_mode: AddressMode,

        /// Overrides automatic AES-GCM cipher selection for SSH connections.
        ///
        /// `Some(true)` forces AES-GCM regardless of hardware detection,
        /// `Some(false)` disables automatic cipher selection entirely,
        /// `None` (default) uses runtime hardware detection.
        #[doc(alias = "--aes")]
        prefer_aes_gcm: Option<bool>,

        /// Enables or disables protect-args (secluded-args) for SSH connections.
        ///
        /// When `Some(true)`, arguments are sent over stdin after the SSH
        /// connection is established instead of on the remote command line.
        /// When `Some(false)`, protect-args is explicitly disabled.
        /// When `None`, the default behavior applies.
        #[doc(alias = "--protect-args")]
        #[doc(alias = "--secluded-args")]
        #[doc(alias = "-s")]
        protect_args: Option<bool>,
    }

    /// Configures the embedded SSH transport options.
    ///
    /// These options override `SshConfig` defaults when the `embedded-ssh`
    /// feature is enabled and the transfer target uses an `ssh://` URL.
    #[cfg(feature = "embedded-ssh")]
    #[must_use]
    pub fn embedded_ssh_config(
        mut self,
        config: Option<super::super::client::EmbeddedSshOptions>,
    ) -> Self {
        self.embedded_ssh_config = config;
        self
    }

    /// Configures the deadline at which the transfer should stop.
    #[must_use]
    #[doc(alias = "--stop-after")]
    #[doc(alias = "--stop-at")]
    pub const fn stop_at(mut self, deadline: Option<SystemTime>) -> Self {
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

    /// Configures the OpenSSH ProxyJump hosts forwarded to `ssh -J`.
    ///
    /// Accepts a comma-separated list of `[user@]host[:port]` entries which is
    /// forwarded verbatim to the SSH command line before the destination
    /// operand. `None` (the default) leaves SSH to honour the user's
    /// `ssh_config` `ProxyJump` settings.
    #[must_use]
    #[doc(alias = "--jump-host")]
    #[doc(alias = "-J")]
    pub fn set_jump_hosts<S: Into<OsString>>(mut self, value: Option<S>) -> Self {
        self.jump_hosts = value.map(Into::into).filter(|v| !v.is_empty());
        self
    }

    /// Sets the early-input file path.
    ///
    /// When set, rsync reads from this file immediately before the transfer
    /// starts and makes the content available to the remote rsync process via
    /// the `RSYNC_EARLY_INPUT` environment variable.
    #[must_use]
    #[doc(alias = "--early-input")]
    pub fn early_input(mut self, path: Option<PathBuf>) -> Self {
        self.early_input = path;
        self
    }

    /// Forces a specific protocol version for daemon handshake negotiation.
    ///
    /// When set, the client advertises this version instead of
    /// `ProtocolVersion::NEWEST`, clamping the negotiation ceiling. This
    /// mirrors the upstream `--protocol` flag.
    #[must_use]
    #[doc(alias = "--protocol")]
    pub const fn protocol_version(mut self, version: Option<protocol::ProtocolVersion>) -> Self {
        self.protocol_version = version;
        self
    }
}
