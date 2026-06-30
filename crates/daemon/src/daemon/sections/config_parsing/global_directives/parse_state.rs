// Global-section parse state.
//
// Mutable container accumulating all global-section directives during parsing,
// plus the constructors that seed inherited defaults across `&include`/`&merge`
// boundaries and the conversion into the final parsed result.

/// Mutable context holding all global-section state accumulated during parsing.
///
/// Passed by reference into `apply_global_directive` to avoid a long parameter
/// list on every call.
struct GlobalParseState {
    global_refuse_directives: Vec<(Vec<String>, ConfigDirectiveOrigin)>,
    global_refuse_line: Option<usize>,
    motd_lines: Vec<String>,
    pid_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
    reverse_lookup: Option<(bool, ConfigDirectiveOrigin)>,
    lock_file: Option<(PathBuf, ConfigDirectiveOrigin)>,
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
    rsync_port: Option<(u16, ConfigDirectiveOrigin)>,
    daemon_chroot: Option<(PathBuf, ConfigDirectiveOrigin)>,
    modules: Vec<ModuleDefinition>,
    /// P_LOCAL parameter defaults from the global section.
    ///
    /// upstream: loadparm.c - P_LOCAL parameters in the global section set
    /// defaults inherited by all modules that don't override them.
    module_defaults: GlobalModuleDefaults,
    /// Snapshot of the parent file's globals when this state is the body
    /// of an `&include`/`&merge` target. Modules declared in the included
    /// file use these as fallbacks when no value is set in this file, so
    /// they inherit the parent's defaults the same way upstream's shared
    /// `Vars` block carries the parent state across the `]push`/`]pop`
    /// boundary in `params.c::Parse`.
    inherited_use_chroot: Option<bool>,
    inherited_secrets_file: Option<PathBuf>,
    inherited_incoming_chmod: Option<String>,
    inherited_outgoing_chmod: Option<String>,
}

impl GlobalParseState {
    fn new() -> Self {
        Self {
            global_refuse_directives: Vec::new(),
            global_refuse_line: None,
            motd_lines: Vec::new(),
            pid_file: None,
            reverse_lookup: None,
            lock_file: None,
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
            rsync_port: None,
            daemon_chroot: None,
            modules: Vec::new(),
            module_defaults: GlobalModuleDefaults::default(),
            inherited_use_chroot: None,
            inherited_secrets_file: None,
            inherited_incoming_chmod: None,
            inherited_outgoing_chmod: None,
        }
    }

    /// Builds a fresh parse state seeded with the parent file's global
    /// defaults so modules declared inside an `&include`/`&merge` target
    /// inherit the same P_LOCAL defaults the parent file already
    /// established.
    ///
    /// upstream: params.c:Parse / loadparm.c::do_section - `&include`
    /// wraps the recursive parse in `]push`/`]pop` calls that snapshot
    /// the shared `Vars` block; modules added by the included file
    /// finalize against the live `Vars` state, which still carries the
    /// parent file's defaults until the matching `]pop` restores the
    /// snapshot. Mirror that by stashing the inheritable defaults into
    /// dedicated fallback slots, leaving the duplicate-detection state
    /// for explicit per-file directives untouched so the include can
    /// still redeclare a global without colliding with the parent's
    /// origin.
    fn inherited_from(parent: &Self) -> Self {
        let mut state = Self::new();
        state.inherited_use_chroot = parent
            .global_use_chroot
            .as_ref()
            .map(|(value, _)| *value)
            .or(parent.inherited_use_chroot);
        state.inherited_secrets_file = parent
            .global_secrets_file
            .as_ref()
            .map(|(value, _)| value.clone())
            .or_else(|| parent.inherited_secrets_file.clone());
        state.inherited_incoming_chmod = parent
            .global_incoming_chmod
            .as_ref()
            .map(|(value, _)| value.clone())
            .or_else(|| parent.inherited_incoming_chmod.clone());
        state.inherited_outgoing_chmod = parent
            .global_outgoing_chmod
            .as_ref()
            .map(|(value, _)| value.clone())
            .or_else(|| parent.inherited_outgoing_chmod.clone());
        state.module_defaults = parent.module_defaults.clone();
        state
    }

    /// Converts the accumulated global state into the final parsed result.
    fn into_result(self) -> ParsedConfigModules {
        ParsedConfigModules {
            modules: self.modules,
            global_refuse_options: self.global_refuse_directives,
            motd_lines: self.motd_lines,
            pid_file: self.pid_file,
            reverse_lookup: self.reverse_lookup,
            lock_file: self.lock_file,
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
            rsync_port: self.rsync_port,
            daemon_chroot: self.daemon_chroot,
        }
    }
}
