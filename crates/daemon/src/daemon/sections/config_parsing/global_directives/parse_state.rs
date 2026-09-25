// Global-section parse state.
//
// Mutable container accumulating all global-section directives during parsing,
// and the conversion into the final parsed result.

/// Mutable context holding all global-section state accumulated during parsing.
///
/// Passed by reference into `apply_global_directive` to avoid a long parameter
/// list on every call.
///
/// upstream: loadparm.c `Vars` - one block shared by every file the parse
/// reads. `Clone` is upstream's `]push`: `&include` saves a copy and restores it
/// afterwards (loadparm.c:do_section:598-614).
#[derive(Clone)]
struct GlobalParseState {
    global_refuse_directives: Vec<(Vec<String>, ConfigDirectiveOrigin)>,
    motd_lines: Vec<String>,
    pid_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    reverse_lookup: Option<(bool, ConfigDirectiveOrigin)>,
    lock_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    /// Global `log file` path, kept separately from the per-module default it
    /// also seeds. Upstream's `log file` is `P_LOCAL` (daemon-parm.h:289) but
    /// `FN_LOCAL_STRING(lp_log_file, log_file)` falls back to the global value
    /// at `module_id < 0`, and `daemon_main` calls `log_init(0)`
    /// (clientserver.c:1768) at STARTUP - so the global value opens a
    /// daemon-wide log before any module is selected. Recording it only as a
    /// module default would lose every pre-module diagnostic.
    log_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    /// Daemon-wide `timeout`, kept separately from the per-module default it
    /// also seeds. `timeout` is `P_LOCAL` (daemon-parm.h), but
    /// `daemon_handshake_timeout(-1)` reads `lp_timeout(-1)`, the GLOBAL value,
    /// at clientserver.c:1441, before any module has been selected, to bound the
    /// pre-module handshake. Recording it only as a module default leaves that
    /// phase with nothing to read and silently falls back to the 60s built-in
    /// bound.
    daemon_timeout: Option<(Option<NonZeroU64>, ConfigDirectiveOrigin)>,
    /// QUIC listener certificate/key paths from the `quic cert file` /
    /// `quic key file` global directives (oc extension, feature-gated).
    #[cfg(feature = "quic")]
    quic_cert_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    #[cfg(feature = "quic")]
    quic_key_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    /// Client-auth CA path from the `quic client ca file` global directive (oc
    /// extension, feature-gated). When set, the QUIC listener requires and
    /// verifies a client certificate against this CA bundle (mutual TLS); unset,
    /// no client certificate is requested. Per-listener, so global-only.
    #[cfg(feature = "quic")]
    quic_client_ca_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    /// QUIC listener port from the `quic port` global directive (oc extension,
    /// feature-gated). Unset shares the daemon TCP `port`; a `quic port = 0` is
    /// coerced to 873 at parse time, mirroring the TCP `port = 0` handling.
    #[cfg(feature = "quic")]
    quic_port: Option<(u16, ConfigDirectiveOrigin)>,
    global_bwlimit: Option<(BandwidthLimitComponents, ConfigDirectiveOrigin)>,
    global_secrets_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    global_incoming_chmod: Option<(String, ConfigDirectiveOrigin)>,
    global_outgoing_chmod: Option<(String, ConfigDirectiveOrigin)>,
    global_use_chroot: Option<(bool, ConfigDirectiveOrigin)>,
    syslog_facility: Option<(String, ConfigDirectiveOrigin)>,
    syslog_tag: Option<(String, ConfigDirectiveOrigin)>,
    bind_address: Option<(IpAddr, ConfigDirectiveOrigin)>,
    daemon_uid: Option<(String, ConfigDirectiveOrigin)>,
    daemon_gid: Option<(String, ConfigDirectiveOrigin)>,
    listen_backlog: Option<(u32, ConfigDirectiveOrigin)>,
    acceptor_threads: Option<(NonZeroU32, ConfigDirectiveOrigin)>,
    socket_options: Option<(String, ConfigDirectiveOrigin)>,
    proxy_protocol: Option<(bool, ConfigDirectiveOrigin)>,
    proxy_protocol_hosts: Option<(Vec<HostPattern>, ConfigDirectiveOrigin)>,
    rsync_port: Option<(u16, ConfigDirectiveOrigin)>,
    daemon_chroot: Option<(PathBuf, ConfigDirectiveOrigin)>,
    /// P_LOCAL parameter defaults from the global section.
    ///
    /// upstream: loadparm.c - P_LOCAL parameters in the global section set
    /// defaults inherited by all modules that don't override them.
    module_defaults: GlobalModuleDefaults,
}

impl GlobalParseState {
    fn new() -> Self {
        Self {
            global_refuse_directives: Vec::new(),
            motd_lines: Vec::new(),
            pid_file: None,
            reverse_lookup: None,
            lock_file: None,
            log_file: None,
            daemon_timeout: None,
            #[cfg(feature = "quic")]
            quic_cert_file: None,
            #[cfg(feature = "quic")]
            quic_client_ca_file: None,
            #[cfg(feature = "quic")]
            quic_key_file: None,
            #[cfg(feature = "quic")]
            quic_port: None,
            global_bwlimit: None,
            global_secrets_file: None,
            global_incoming_chmod: None,
            global_outgoing_chmod: None,
            global_use_chroot: None,
            syslog_facility: None,
            syslog_tag: None,
            bind_address: None,
            daemon_uid: None,
            daemon_gid: None,
            listen_backlog: None,
            acceptor_threads: None,
            socket_options: None,
            proxy_protocol: None,
            proxy_protocol_hosts: None,
            rsync_port: None,
            daemon_chroot: None,
            module_defaults: GlobalModuleDefaults::default(),
        }
    }

    /// Converts the final `Vars` into the parsed result, finalizing every
    /// module section in `sections` against it.
    ///
    /// upstream: loadparm.c:347-348 - `FN_LOCAL_STRING` falls back to
    /// `Vars.l.<param>` when the section's own copy is NULL, and
    /// clientserver.c:781-783 performs that lookup when a client picks the
    /// module, i.e. after `lp_load()` has read the whole file. A global
    /// declared below a `[section]` therefore still supplies a string the
    /// section never had, but never replaces one the section copied or set.
    fn into_result(self, sections: Vec<PendingModule>) -> Result<ParsedConfigModules, DaemonError> {
        let latest = &self.module_defaults;
        let latest_secrets = self.global_secrets_file.as_ref().map(|(v, _)| v.as_path());
        let latest_incoming = self.global_incoming_chmod.as_ref().map(|(v, _)| v.as_str());
        let latest_outgoing = self.global_outgoing_chmod.as_ref().map(|(v, _)| v.as_str());
        let mut modules = Vec::with_capacity(sections.len());
        for module in sections {
            let defaults = module.defaults;
            let mut builder = module.builder;
            if builder.refuse_options.is_none() {
                builder.refuse_options = defaults.refuse_options;
            }
            modules.push(builder.finish(
                &module.config_path,
                defaults.secrets_file.as_deref().or(latest_secrets),
                defaults.incoming_chmod.as_deref().or(latest_incoming),
                defaults.outgoing_chmod.as_deref().or(latest_outgoing),
                defaults.use_chroot,
                &GlobalModuleDefaults::resolve(&defaults.module_defaults, latest),
            )?);
        }

        Ok(ParsedConfigModules {
            modules,
            global_refuse_options: self.global_refuse_directives,
            motd_lines: self.motd_lines,
            pid_file: self.pid_file,
            reverse_lookup: self.reverse_lookup,
            lock_file: self.lock_file,
            log_file: self.log_file,
            daemon_timeout: self.daemon_timeout,
            #[cfg(feature = "quic")]
            quic_cert_file: self.quic_cert_file,
            #[cfg(feature = "quic")]
            quic_client_ca_file: self.quic_client_ca_file,
            #[cfg(feature = "quic")]
            quic_key_file: self.quic_key_file,
            #[cfg(feature = "quic")]
            quic_port: self.quic_port,
            global_bandwidth_limit: self.global_bwlimit,
            global_secrets_file: self.global_secrets_file,
            global_incoming_chmod: self.global_incoming_chmod,
            global_outgoing_chmod: self.global_outgoing_chmod,
            syslog_facility: self.syslog_facility,
            syslog_tag: self.syslog_tag,
            bind_address: self.bind_address,
            daemon_uid: self.daemon_uid,
            daemon_gid: self.daemon_gid,
            listen_backlog: self.listen_backlog,
            acceptor_threads: self.acceptor_threads,
            socket_options: self.socket_options,
            proxy_protocol: self.proxy_protocol,
            proxy_protocol_hosts: self.proxy_protocol_hosts,
            rsync_port: self.rsync_port,
            daemon_chroot: self.daemon_chroot,
        })
    }
}
