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
    pub const fn bind_address(&self) -> Option<&BindAddress> {
        self.bind_address.as_ref()
    }

    /// Returns the configured socket options, if any.
    #[must_use]
    #[doc(alias = "--sockopts")]
    pub fn sockopts(&self) -> Option<&OsStr> {
        self.sockopts.as_deref()
    }

    /// Returns the requested blocking I/O preference for remote shells.
    #[must_use]
    #[doc(alias = "--blocking-io")]
    #[doc(alias = "--no-blocking-io")]
    pub const fn blocking_io(&self) -> Option<bool> {
        self.blocking_io
    }

    /// Returns the requested bandwidth limit, if any.
    #[must_use]
    pub const fn bandwidth_limit(&self) -> Option<BandwidthLimit> {
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

    /// Returns the configured stop-at deadline, if any.
    #[must_use]
    #[doc(alias = "--stop-after")]
    #[doc(alias = "--stop-at")]
    pub const fn stop_at(&self) -> Option<SystemTime> {
        self.stop_at
    }

    /// Returns the custom remote shell command arguments, if specified.
    #[must_use]
    #[doc(alias = "--rsh")]
    #[doc(alias = "-e")]
    pub fn remote_shell(&self) -> Option<&[OsString]> {
        self.remote_shell.as_deref()
    }

    /// Returns the custom remote rsync binary path, if specified.
    #[must_use]
    #[doc(alias = "--rsync-path")]
    pub fn rsync_path(&self) -> Option<&OsStr> {
        self.rsync_path.as_deref()
    }

    /// Returns the early-input file path, if specified.
    ///
    /// When set, rsync reads from this file immediately before the transfer
    /// starts and makes the content available to the remote rsync process via
    /// the `RSYNC_EARLY_INPUT` environment variable.
    #[must_use]
    #[doc(alias = "--early-input")]
    pub fn early_input(&self) -> Option<&Path> {
        self.early_input.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ClientConfig {
        ClientConfig::default()
    }

    // Tests for address_mode
    #[test]
    fn address_mode_default_is_default() {
        let config = default_config();
        assert_eq!(config.address_mode(), AddressMode::Default);
    }

    // Tests for connect_program
    #[test]
    fn connect_program_default_is_none() {
        let config = default_config();
        assert!(config.connect_program().is_none());
    }

    // Tests for bind_address
    #[test]
    fn bind_address_default_is_none() {
        let config = default_config();
        assert!(config.bind_address().is_none());
    }

    // Tests for sockopts
    #[test]
    fn sockopts_default_is_none() {
        let config = default_config();
        assert!(config.sockopts().is_none());
    }

    // Tests for blocking_io
    #[test]
    fn blocking_io_default_is_none() {
        let config = default_config();
        assert!(config.blocking_io().is_none());
    }

    // Tests for bandwidth_limit
    #[test]
    fn bandwidth_limit_default_is_none() {
        let config = default_config();
        assert!(config.bandwidth_limit().is_none());
    }

    // Tests for timeout
    #[test]
    fn timeout_default_is_default() {
        let config = default_config();
        assert_eq!(config.timeout(), TransferTimeout::Default);
    }

    // Tests for connect_timeout
    #[test]
    fn connect_timeout_default_is_default() {
        let config = default_config();
        assert_eq!(config.connect_timeout(), TransferTimeout::Default);
    }

    // Tests for stop_at
    #[test]
    fn stop_at_default_is_none() {
        let config = default_config();
        assert!(config.stop_at().is_none());
    }

    // Tests for remote_shell
    #[test]
    fn remote_shell_default_is_none() {
        let config = default_config();
        assert!(config.remote_shell().is_none());
    }

    // Tests for rsync_path
    #[test]
    fn rsync_path_default_is_none() {
        let config = default_config();
        assert!(config.rsync_path().is_none());
    }

    // Tests for early_input
    #[test]
    fn early_input_default_is_none() {
        let config = default_config();
        assert!(config.early_input().is_none());
    }
}
