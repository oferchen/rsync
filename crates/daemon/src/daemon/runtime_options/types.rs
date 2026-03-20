#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeOptions {
    brand: Brand,
    bind_address: IpAddr,
    port: u16,
    max_sessions: Option<NonZeroUsize>,
    pub(crate) modules: Vec<ModuleDefinition>,
    motd_lines: Vec<String>,
    bandwidth_limit: Option<NonZeroU64>,
    bandwidth_burst: Option<NonZeroU64>,
    bandwidth_limit_configured: bool,
    address_family: Option<AddressFamily>,
    bind_address_overridden: bool,
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
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            brand: Brand::Oc,
            bind_address: DEFAULT_BIND_ADDRESS,
            port: DEFAULT_PORT,
            max_sessions: None,
            modules: Vec::new(),
            motd_lines: Vec::new(),
            bandwidth_limit: None,
            bandwidth_burst: None,
            bandwidth_limit_configured: false,
            address_family: None,
            bind_address_overridden: false,
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
            proxy_protocol: false,
            daemon_chroot: None,
            detach: cfg!(unix),
            config_path: None,
        }
    }
}
