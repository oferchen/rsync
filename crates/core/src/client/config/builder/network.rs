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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::num::NonZeroU64;
    use std::time::{Duration, SystemTime};

    fn builder() -> ClientConfigBuilder {
        ClientConfigBuilder::default()
    }

    #[test]
    fn bind_address_sets_value() {
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 0);
        let addr = BindAddress::new(OsString::from("192.168.1.1"), socket);
        let config = builder().bind_address(Some(addr.clone())).build();
        assert!(config.bind_address().is_some());
    }

    #[test]
    fn bind_address_none_clears_value() {
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 0);
        let addr = BindAddress::new(OsString::from("192.168.1.1"), socket);
        let config = builder()
            .bind_address(Some(addr))
            .bind_address(None)
            .build();
        assert!(config.bind_address().is_none());
    }

    #[test]
    fn sockopts_sets_value() {
        let config = builder().sockopts(Some(OsString::from("SO_SNDBUF=65536"))).build();
        assert!(config.sockopts().is_some());
    }

    #[test]
    fn sockopts_none_clears_value() {
        let config = builder()
            .sockopts(Some(OsString::from("SO_SNDBUF=65536")))
            .sockopts(None)
            .build();
        assert!(config.sockopts().is_none());
    }

    #[test]
    fn blocking_io_sets_some_true() {
        let config = builder().blocking_io(Some(true)).build();
        assert_eq!(config.blocking_io(), Some(true));
    }

    #[test]
    fn blocking_io_sets_some_false() {
        let config = builder().blocking_io(Some(false)).build();
        assert_eq!(config.blocking_io(), Some(false));
    }

    #[test]
    fn blocking_io_none_leaves_default() {
        let config = builder().blocking_io(None).build();
        assert!(config.blocking_io().is_none());
    }

    #[test]
    fn timeout_sets_default() {
        let config = builder().timeout(TransferTimeout::Default).build();
        assert_eq!(config.timeout(), TransferTimeout::Default);
    }

    #[test]
    fn timeout_sets_disabled() {
        let config = builder().timeout(TransferTimeout::Disabled).build();
        assert_eq!(config.timeout(), TransferTimeout::Disabled);
    }

    #[test]
    fn timeout_sets_seconds() {
        let seconds = NonZeroU64::new(60).unwrap();
        let config = builder().timeout(TransferTimeout::Seconds(seconds)).build();
        assert_eq!(config.timeout().as_seconds(), Some(seconds));
    }

    #[test]
    fn connect_timeout_sets_value() {
        let seconds = NonZeroU64::new(30).unwrap();
        let config = builder().connect_timeout(TransferTimeout::Seconds(seconds)).build();
        assert_eq!(config.connect_timeout().as_seconds(), Some(seconds));
    }

    #[test]
    fn stop_at_sets_deadline() {
        let deadline = SystemTime::now() + Duration::from_secs(3600);
        let config = builder().stop_at(Some(deadline)).build();
        assert!(config.stop_at().is_some());
    }

    #[test]
    fn stop_at_none_clears_deadline() {
        let deadline = SystemTime::now() + Duration::from_secs(3600);
        let config = builder()
            .stop_at(Some(deadline))
            .stop_at(None)
            .build();
        assert!(config.stop_at().is_none());
    }

    #[test]
    fn connect_program_sets_value() {
        let config = builder().connect_program(Some(OsString::from("/usr/bin/nc"))).build();
        assert!(config.connect_program().is_some());
    }

    #[test]
    fn connect_program_none_clears_value() {
        let config = builder()
            .connect_program(Some(OsString::from("/usr/bin/nc")))
            .connect_program(None)
            .build();
        assert!(config.connect_program().is_none());
    }

    #[test]
    fn address_mode_sets_default() {
        let config = builder().address_mode(AddressMode::Default).build();
        assert_eq!(config.address_mode(), AddressMode::Default);
    }

    #[test]
    fn address_mode_sets_ipv4() {
        let config = builder().address_mode(AddressMode::Ipv4).build();
        assert_eq!(config.address_mode(), AddressMode::Ipv4);
    }

    #[test]
    fn address_mode_sets_ipv6() {
        let config = builder().address_mode(AddressMode::Ipv6).build();
        assert_eq!(config.address_mode(), AddressMode::Ipv6);
    }

    #[test]
    fn set_remote_shell_sets_args() {
        let config = builder()
            .set_remote_shell(vec!["ssh", "-p", "2222"])
            .build();
        assert!(config.remote_shell().is_some());
        let shell = config.remote_shell().unwrap();
        assert_eq!(shell.len(), 3);
    }

    #[test]
    fn set_rsync_path_sets_value() {
        let config = builder()
            .set_rsync_path("/opt/rsync/bin/rsync")
            .build();
        assert!(config.rsync_path().is_some());
    }

    #[test]
    fn early_input_sets_path() {
        let config = builder()
            .early_input(Some(PathBuf::from("/tmp/early-input")))
            .build();
        assert!(config.early_input().is_some());
    }

    #[test]
    fn early_input_none_clears_path() {
        let config = builder()
            .early_input(Some(PathBuf::from("/tmp/early-input")))
            .early_input(None)
            .build();
        assert!(config.early_input().is_none());
    }

    #[test]
    fn default_bind_address_is_none() {
        let config = builder().build();
        assert!(config.bind_address().is_none());
    }

    #[test]
    fn default_address_mode_is_default() {
        let config = builder().build();
        assert_eq!(config.address_mode(), AddressMode::Default);
    }

    #[test]
    fn default_timeout_is_default() {
        let config = builder().build();
        assert_eq!(config.timeout(), TransferTimeout::Default);
    }
}
