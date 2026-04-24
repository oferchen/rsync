impl RuntimeOptions {
    /// Returns whether the daemon should fork and detach from the terminal.
    ///
    /// Defaults to `true` on Unix (matching upstream `become_daemon()`) and
    /// `false` on Windows where `fork` is not available.
    pub(crate) fn detach(&self) -> bool {
        self.detach
    }

    /// Returns the configured TCP listen backlog.
    ///
    /// Upstream: `daemon-parm.txt` - `listen_backlog` INTEGER, default 5.
    pub(crate) fn listen_backlog(&self) -> Option<u32> {
        self.listen_backlog
    }

    /// Returns the configured TCP port from the config file `port` directive.
    ///
    /// Upstream: `daemon-parm.txt` - `port` INTEGER, P_GLOBAL, default 0.
    #[allow(dead_code)] // REASON: accessor for daemon listener; wired when async daemon starts
    pub(crate) fn rsync_port(&self) -> Option<u16> {
        self.rsync_port
    }

    /// Returns the configured socket options string.
    ///
    /// Upstream: `daemon-parm.txt` - `socket options` STRING. Comma-separated
    /// list of TCP/IP socket options applied to the daemon listener socket
    /// (e.g., `TCP_NODELAY`, `SO_KEEPALIVE`, `SO_SNDBUF=65536`).
    pub(crate) fn socket_options(&self) -> Option<&str> {
        self.socket_options.as_deref()
    }

    /// Returns whether incoming connections require a PROXY protocol header.
    ///
    /// upstream: clientserver.c:1298 - checked before accepting client data.
    #[allow(dead_code)] // REASON: accessor for daemon listener; wired when async daemon starts
    pub(crate) fn proxy_protocol(&self) -> bool {
        self.proxy_protocol
    }

    /// Returns the directory the daemon chroots into before forking children.
    ///
    /// upstream: daemon-parm.h - `daemon chroot` STRING, P_GLOBAL.
    #[allow(dead_code)] // REASON: accessor for daemon listener; wired when async daemon starts
    pub(crate) fn daemon_chroot(&self) -> Option<&Path> {
        self.daemon_chroot.as_deref()
    }
}

#[cfg(test)]
#[allow(dead_code)]
impl RuntimeOptions {
    pub(super) fn modules(&self) -> &[ModuleDefinition] {
        &self.modules
    }

    pub(super) fn bandwidth_limit(&self) -> Option<NonZeroU64> {
        self.bandwidth_limit
    }

    pub(super) fn bandwidth_burst(&self) -> Option<NonZeroU64> {
        self.bandwidth_burst
    }

    pub(super) fn brand(&self) -> Brand {
        self.brand
    }

    pub(super) fn bandwidth_limit_configured(&self) -> bool {
        self.bandwidth_limit_configured
    }

    pub(super) fn bind_address(&self) -> IpAddr {
        self.bind_address
    }

    pub(super) fn address_family(&self) -> Option<AddressFamily> {
        self.address_family
    }

    pub(super) fn motd_lines(&self) -> &[String] {
        &self.motd_lines
    }

    pub(super) fn log_file(&self) -> Option<&PathBuf> {
        self.log_file.as_ref()
    }

    pub(super) fn pid_file(&self) -> Option<&Path> {
        self.pid_file.as_deref()
    }

    pub(super) fn reverse_lookup(&self) -> bool {
        self.reverse_lookup
    }

    pub(super) fn lock_file(&self) -> Option<&Path> {
        self.lock_file.as_deref()
    }

    pub(super) fn global_secrets_file(&self) -> Option<&Path> {
        self.global_secrets_file.as_deref()
    }

    /// Returns the configured syslog facility, or "daemon" if not set.
    pub(super) fn syslog_facility(&self) -> &str {
        self.syslog_facility.as_deref().unwrap_or("daemon")
    }

    /// Returns the configured syslog tag, or "oc-rsyncd" if not set.
    pub(super) fn syslog_tag(&self) -> &str {
        self.syslog_tag.as_deref().unwrap_or("oc-rsyncd")
    }

    pub(super) fn config_path(&self) -> Option<&Path> {
        self.config_path.as_deref()
    }

    /// Returns the resolved daemon-level uid, if configured.
    pub(super) fn daemon_uid(&self) -> Option<u32> {
        self.daemon_uid
    }

    /// Returns the resolved daemon-level gid, if configured.
    pub(super) fn daemon_gid(&self) -> Option<u32> {
        self.daemon_gid
    }
}
