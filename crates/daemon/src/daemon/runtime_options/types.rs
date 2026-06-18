#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeOptions {
    brand: Brand,
    bind_address: IpAddr,
    port: u16,
    max_sessions: Option<NonZeroUsize>,
    /// Maximum number of concurrently active client connections.
    ///
    /// When set, the accept loop consults [`ConnectionCounter::active`] before
    /// spawning a worker thread. If the cap is reached, the new socket is
    /// refused with `@ERROR: max connections (N) reached -- try again later`
    /// and closed without dispatching a session, while the accept loop keeps
    /// running. Distinct from `max_sessions`, which caps total served sessions.
    ///
    /// upstream: clientserver.c:744-756 - `claim_connection()` enforces the
    /// per-module `max connections` directive and emits the same error.
    max_connections: Option<NonZeroUsize>,
    pub(crate) modules: Vec<ModuleDefinition>,
    motd_lines: Vec<String>,
    bandwidth_limit: Option<NonZeroU64>,
    bandwidth_burst: Option<NonZeroU64>,
    bandwidth_limit_configured: bool,
    address_family: Option<AddressFamily>,
    /// Set when both `--ipv4` and `--ipv6` are requested on the CLI.
    ///
    /// Mirrors upstream rsync's `default_af_hint = 0` ("any protocol") when
    /// the operator wants the daemon to bind one listener per family rather
    /// than picking one. The accept-loop interprets this as "iterate IPv6
    /// then IPv4, surface per-family bind failures as warnings, succeed as
    /// long as at least one family bound." See
    /// `target/interop/upstream-src/rsync-3.4.4/socket.c:402-499`
    /// (`open_socket_in`) for the family-iteration loop oc-rsync reproduces.
    pub(crate) dual_stack: bool,
    bind_address_overridden: bool,
    port_overridden: bool,
    log_file: Option<PathBuf>,
    log_file_configured: bool,
    global_refuse_options: Option<Vec<String>>,
    global_secrets_file: Option<PathBuf>,
    global_secrets_from_config: bool,
    global_secrets_from_cli: bool,
    pid_file: Option<PathBuf>,
    pid_file_from_config: bool,
    reverse_lookup: bool,
    reverse_lookup_configured: bool,
    lock_file: Option<PathBuf>,
    lock_file_from_config: bool,
    global_incoming_chmod: Option<String>,
    global_outgoing_chmod: Option<String>,
    syslog_facility: Option<String>,
    syslog_facility_from_config: bool,
    syslog_tag: Option<String>,
    syslog_tag_from_config: bool,
    /// Resolved numeric uid the daemon process should drop to after binding.
    ///
    /// upstream: loadparm.c - global `uid` parameter. Resolved from username
    /// or numeric string at config load time.
    daemon_uid: Option<u32>,
    /// Resolved numeric gid the daemon process should drop to after binding.
    ///
    /// upstream: loadparm.c - global `gid` parameter. Resolved from groupname
    /// or numeric string at config load time.
    daemon_gid: Option<u32>,
    listen_backlog: Option<u32>,
    listen_backlog_from_config: bool,
    /// TCP port from the `port` / `rsync port` global config parameter.
    ///
    /// upstream: daemon-parm.txt - `port` INTEGER, P_GLOBAL, default 0.
    /// When set, overrides the default listening port unless CLI `--port` was given.
    rsync_port: Option<u16>,
    /// Raw socket options string from the `socket options` global parameter.
    ///
    /// upstream: daemon-parm.txt - `socket options` STRING. Comma-separated list
    /// of TCP/IP socket options (e.g., `TCP_NODELAY`, `SO_KEEPALIVE`,
    /// `SO_SNDBUF=65536`) applied to the daemon listener socket.
    socket_options: Option<String>,
    socket_options_from_config: bool,
    /// TCP Fast Open mode applied to the daemon listener and accepted
    /// client sockets. Defaults to [`TcpFastOpenMode::Auto`] which enables
    /// TFO on platforms that support it and silently skips elsewhere.
    /// Wire-compatible with upstream rsync: this option is an oc-rsync
    /// perf improvement that touches only kernel socket behaviour.
    tcp_fastopen: TcpFastOpenMode,
    /// Whether incoming connections require a PROXY protocol header.
    ///
    /// upstream: daemon-parm.h - `proxy_protocol` BOOL, P_GLOBAL, default False.
    proxy_protocol: bool,
    /// Directory the daemon chroots into before forking children.
    ///
    /// upstream: daemon-parm.h - `daemon chroot` STRING, P_GLOBAL.
    daemon_chroot: Option<PathBuf>,
    detach: bool,
    /// Path to the config file loaded at startup, retained for SIGHUP reload.
    ///
    /// When the daemon receives SIGHUP, this path is re-read and re-parsed so
    /// new connections pick up module definition changes without a restart.
    /// `None` when no config file was loaded (all modules from CLI flags).
    config_path: Option<PathBuf>,
    /// CLI verbosity counter incremented per `-v` / `--verbose` flag.
    ///
    /// upstream: options.c:877 - `{"verbose", 'v', POPT_ARG_NONE, 0, 'v', 0, 0}`
    /// in `long_daemon_options`. Stacked `-vv` / `-vvv` increment the same
    /// counter, mirrored by `--no-verbose` / `--no-v` reset to zero.
    pub(crate) verbosity: u8,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            brand: Brand::Oc,
            bind_address: DEFAULT_BIND_ADDRESS,
            port: DEFAULT_PORT,
            max_sessions: None,
            max_connections: None,
            modules: Vec::new(),
            motd_lines: Vec::new(),
            bandwidth_limit: None,
            bandwidth_burst: None,
            bandwidth_limit_configured: false,
            address_family: None,
            dual_stack: false,
            bind_address_overridden: false,
            port_overridden: false,
            log_file: None,
            log_file_configured: false,
            global_refuse_options: None,
            global_secrets_file: None,
            global_secrets_from_config: false,
            global_secrets_from_cli: false,
            pid_file: None,
            pid_file_from_config: false,
            reverse_lookup: true,
            reverse_lookup_configured: false,
            lock_file: None,
            lock_file_from_config: false,
            global_incoming_chmod: None,
            global_outgoing_chmod: None,
            syslog_facility: None,
            syslog_facility_from_config: false,
            syslog_tag: None,
            syslog_tag_from_config: false,
            daemon_uid: None,
            daemon_gid: None,
            listen_backlog: None,
            listen_backlog_from_config: false,
            rsync_port: None,
            socket_options: None,
            socket_options_from_config: false,
            tcp_fastopen: TcpFastOpenMode::Auto,
            proxy_protocol: false,
            daemon_chroot: None,
            detach: cfg!(unix),
            config_path: None,
            verbosity: 0,
        }
    }
}
