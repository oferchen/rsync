// Global-section directive parsing.
//
// Handles `key = value` directives that appear before any `[module]` header
// (the global section), including the `include` directive that triggers
// recursive config file parsing and result merging.

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
    socket_options: Option<(String, ConfigDirectiveOrigin)>,
    proxy_protocol: Option<(bool, ConfigDirectiveOrigin)>,
    rsync_port: Option<(u16, ConfigDirectiveOrigin)>,
    daemon_chroot: Option<(PathBuf, ConfigDirectiveOrigin)>,
    modules: Vec<ModuleDefinition>,
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
            socket_options: None,
            proxy_protocol: None,
            rsync_port: None,
            daemon_chroot: None,
            modules: Vec::new(),
        }
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
            socket_options: self.socket_options,
            proxy_protocol: self.proxy_protocol,
            rsync_port: self.rsync_port,
            daemon_chroot: self.daemon_chroot,
        }
    }
}

/// Applies a single global-section directive, updating `state` accordingly.
///
/// The `stack` parameter is threaded through for recursive `include` handling.
fn apply_global_directive(
    state: &mut GlobalParseState,
    key: &str,
    value: &str,
    path: &Path,
    line_number: usize,
    canonical: &Path,
    stack: &mut Vec<PathBuf>,
) -> Result<(), DaemonError> {
    match key {
        "refuse options" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'refuse options' directive must not be empty",
                ));
            }
            let options = parse_refuse_option_list(value).map_err(|error| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid 'refuse options' directive: {error}"),
                )
            })?;

            if let Some(existing_line) = state.global_refuse_line {
                return Err(config_parse_error(
                    path,
                    line_number,
                    format!(
                        "duplicate 'refuse options' directive in global section (previously defined on line {existing_line})"
                    ),
                ));
            }

            state.global_refuse_line = Some(line_number);
            state.global_refuse_directives.push((
                options,
                ConfigDirectiveOrigin {
                    path: canonical.to_path_buf(),
                    line: line_number,
                },
            ));
        }
        "include" => {
            apply_include_directive(state, value, path, line_number, canonical, stack)?;
        }
        "motd file" => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'motd file' directive must not be empty",
                ));
            }

            let motd_path = resolve_config_relative_path(path, trimmed);
            let contents = fs::read_to_string(&motd_path).map_err(|error| {
                let motd_display = motd_path.display();
                config_parse_error(
                    path,
                    line_number,
                    format!("failed to read motd file '{motd_display}': {error}"),
                )
            })?;

            for raw_line in contents.lines() {
                state.motd_lines.push(raw_line.trim_end_matches('\r').to_owned());
            }
        }
        "motd" => {
            state.motd_lines.push(value.trim_end_matches(['\r', '\n']).to_owned());
        }
        "pid file" => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'pid file' directive must not be empty",
                ));
            }

            let resolved = resolve_config_relative_path(path, trimmed);
            if let Some((existing, origin)) = &state.pid_file {
                if existing != &resolved {
                    let existing_line = origin.line;
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!(
                            "duplicate 'pid file' directive in global section (previously defined on line {existing_line})"
                        ),
                    ));
                }
            } else {
                state.pid_file = Some((
                    resolved,
                    ConfigDirectiveOrigin {
                        path: canonical.to_path_buf(),
                        line: line_number,
                    },
                ));
            }
        }
        "reverse lookup" => {
            let parsed = parse_boolean_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid boolean value '{value}' for 'reverse lookup'"),
                )
            })?;

            let origin = ConfigDirectiveOrigin {
                path: canonical.to_path_buf(),
                line: line_number,
            };

            if let Some((existing, existing_origin)) = &state.reverse_lookup {
                if *existing != parsed {
                    let existing_line = existing_origin.line;
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!(
                            "duplicate 'reverse lookup' directive in global section (previously defined on line {existing_line})"
                        ),
                    ));
                }
            } else {
                state.reverse_lookup = Some((parsed, origin));
            }
        }
        "bwlimit" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'bwlimit' directive must not be empty",
                ));
            }

            let components = parse_config_bwlimit(value, path, line_number)?;
            let origin = ConfigDirectiveOrigin {
                path: canonical.to_path_buf(),
                line: line_number,
            };

            if let Some((existing, existing_origin)) = &state.global_bwlimit {
                if existing != &components {
                    let existing_line = existing_origin.line;
                    return Err(config_parse_error(
                        &origin.path,
                        origin.line,
                        format!(
                            "duplicate 'bwlimit' directive in global section (previously defined on line {existing_line})"
                        ),
                    ));
                }
            } else {
                state.global_bwlimit = Some((components, origin));
            }
        }
        "secrets file" => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'secrets file' directive must not be empty",
                ));
            }

            let resolved = resolve_config_relative_path(path, trimmed);
            let validated = validate_secrets_file(&resolved, path, line_number)?;
            let origin = ConfigDirectiveOrigin {
                path: canonical.to_path_buf(),
                line: line_number,
            };

            if let Some((existing, existing_origin)) = &state.global_secrets_file {
                if existing != &validated {
                    let existing_line = existing_origin.line;
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!(
                            "duplicate 'secrets file' directive in global section (previously defined on line {existing_line})"
                        ),
                    ));
                }
            } else {
                state.global_secrets_file = Some((validated, origin));
            }
        }
        "incoming chmod" | "incoming-chmod" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'incoming chmod' directive must not be empty",
                ));
            }

            let origin = ConfigDirectiveOrigin {
                path: canonical.to_path_buf(),
                line: line_number,
            };

            if let Some((existing, existing_origin)) = &state.global_incoming_chmod {
                if existing != value {
                    let existing_line = existing_origin.line;
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!(
                            "duplicate 'incoming chmod' directive in global section (previously defined on line {existing_line})"
                        ),
                    ));
                }
            } else {
                let mut owned = String::new();
                value.clone_into(&mut owned);
                state.global_incoming_chmod = Some((owned, origin));
            }
        }
        "outgoing chmod" | "outgoing-chmod" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'outgoing chmod' directive must not be empty",
                ));
            }

            let origin = ConfigDirectiveOrigin {
                path: canonical.to_path_buf(),
                line: line_number,
            };

            if let Some((existing, existing_origin)) = &state.global_outgoing_chmod {
                if existing != value {
                    let existing_line = existing_origin.line;
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!(
                            "duplicate 'outgoing chmod' directive in global section (previously defined on line {existing_line})"
                        ),
                    ));
                }
            } else {
                let mut owned = String::new();
                value.clone_into(&mut owned);
                state.global_outgoing_chmod = Some((owned, origin));
            }
        }
        "lock file" => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'lock file' directive must not be empty",
                ));
            }

            let resolved = resolve_config_relative_path(path, trimmed);
            let origin = ConfigDirectiveOrigin {
                path: canonical.to_path_buf(),
                line: line_number,
            };

            if let Some((existing, existing_origin)) = &state.lock_file {
                if existing != &resolved {
                    let existing_line = existing_origin.line;
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!(
                            "duplicate 'lock file' directive in global section (previously defined on line {existing_line})"
                        ),
                    ));
                }
            } else {
                state.lock_file = Some((resolved, origin));
            }
        }
        // upstream: loadparm.c - use chroot is valid in the global section as a
        // default that applies to all modules which do not override it explicitly.
        "use chroot" => {
            let parsed = parse_boolean_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid boolean value '{value}' for 'use chroot'"),
                )
            })?;

            let origin = ConfigDirectiveOrigin {
                path: canonical.to_path_buf(),
                line: line_number,
            };

            if let Some((existing, existing_origin)) = &state.global_use_chroot {
                if *existing != parsed {
                    let existing_line = existing_origin.line;
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!(
                            "duplicate 'use chroot' directive in global section (previously defined on line {existing_line})"
                        ),
                    ));
                }
            } else {
                state.global_use_chroot = Some((parsed, origin));
            }
        }
        // upstream: loadparm.c - syslog facility sets the syslog facility
        // for daemon log messages (e.g., "daemon", "local0"-"local7").
        "syslog facility" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'syslog facility' directive must not be empty",
                ));
            }

            let origin = ConfigDirectiveOrigin {
                path: canonical.to_path_buf(),
                line: line_number,
            };

            if let Some((existing, existing_origin)) = &state.syslog_facility {
                if existing != value {
                    let existing_line = existing_origin.line;
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!(
                            "duplicate 'syslog facility' directive in global section (previously defined on line {existing_line})"
                        ),
                    ));
                }
            } else {
                let mut owned = String::new();
                value.clone_into(&mut owned);
                state.syslog_facility = Some((owned, origin));
            }
        }
        // upstream: loadparm.c - syslog tag sets the syslog ident prefix.
        "syslog tag" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'syslog tag' directive must not be empty",
                ));
            }

            let origin = ConfigDirectiveOrigin {
                path: canonical.to_path_buf(),
                line: line_number,
            };

            if let Some((existing, existing_origin)) = &state.syslog_tag {
                if existing != value {
                    let existing_line = existing_origin.line;
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!(
                            "duplicate 'syslog tag' directive in global section (previously defined on line {existing_line})"
                        ),
                    ));
                }
            } else {
                let mut owned = String::new();
                value.clone_into(&mut owned);
                state.syslog_tag = Some((owned, origin));
            }
        }
        // upstream: loadparm.c - `address` sets the bind address for
        // the daemon listener.
        "address" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'address' directive must not be empty",
                ));
            }

            let parsed_addr = parse_bind_address(&OsString::from(value))
                .map_err(|_| {
                    config_parse_error(
                        path,
                        line_number,
                        format!("invalid bind address '{value}'"),
                    )
                })?;

            let origin = ConfigDirectiveOrigin {
                path: canonical.to_path_buf(),
                line: line_number,
            };

            if let Some((existing, existing_origin)) = &state.bind_address {
                if *existing != parsed_addr {
                    let existing_line = existing_origin.line;
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!(
                            "duplicate 'address' directive in global section (previously defined on line {existing_line})"
                        ),
                    ));
                }
            } else {
                state.bind_address = Some((parsed_addr, origin));
            }
        }
        // upstream: loadparm.c - `uid` in the global section sets the
        // daemon process uid after binding and daemonizing.
        "uid" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'uid' directive must not be empty",
                ));
            }

            let origin = ConfigDirectiveOrigin {
                path: canonical.to_path_buf(),
                line: line_number,
            };

            if let Some((existing, existing_origin)) = &state.daemon_uid {
                if existing != value {
                    let existing_line = existing_origin.line;
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!(
                            "duplicate 'uid' directive in global section (previously defined on line {existing_line})"
                        ),
                    ));
                }
            } else {
                let mut owned = String::new();
                value.clone_into(&mut owned);
                state.daemon_uid = Some((owned, origin));
            }
        }
        // upstream: loadparm.c - `gid` in the global section sets the
        // daemon process gid after binding and daemonizing.
        "gid" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'gid' directive must not be empty",
                ));
            }

            let origin = ConfigDirectiveOrigin {
                path: canonical.to_path_buf(),
                line: line_number,
            };

            if let Some((existing, existing_origin)) = &state.daemon_gid {
                if existing != value {
                    let existing_line = existing_origin.line;
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!(
                            "duplicate 'gid' directive in global section (previously defined on line {existing_line})"
                        ),
                    ));
                }
            } else {
                let mut owned = String::new();
                value.clone_into(&mut owned);
                state.daemon_gid = Some((owned, origin));
            }
        }
        // upstream: daemon-parm.txt - listen_backlog INTEGER, default 5.
        // Controls the backlog argument passed to listen(2).
        "listen backlog" => {
            let parsed: u32 = value.parse().map_err(|_| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid integer value '{value}' for 'listen backlog'"),
                )
            })?;

            let origin = ConfigDirectiveOrigin {
                path: canonical.to_path_buf(),
                line: line_number,
            };

            if let Some((existing, existing_origin)) = &state.listen_backlog {
                if *existing != parsed {
                    let existing_line = existing_origin.line;
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!(
                            "duplicate 'listen backlog' directive in global section (previously defined on line {existing_line})"
                        ),
                    ));
                }
            } else {
                state.listen_backlog = Some((parsed, origin));
            }
        }
        // upstream: daemon-parm.txt - port INTEGER, P_GLOBAL, default 0.
        // Controls the TCP port the daemon listens on.
        "port" | "rsync port" => {
            let parsed: u16 = value.parse().map_err(|_| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid port number '{value}' for 'port'"),
                )
            })?;

            let origin = ConfigDirectiveOrigin {
                path: canonical.to_path_buf(),
                line: line_number,
            };

            if let Some((existing, existing_origin)) = &state.rsync_port {
                if *existing != parsed {
                    let existing_line = existing_origin.line;
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!(
                            "conflicting 'port' directive in global section (previously defined as {existing} on line {existing_line})"
                        ),
                    ));
                }
            } else {
                state.rsync_port = Some((parsed, origin));
            }
        }
        // upstream: daemon-parm.txt - socket options STRING.
        // Comma-separated TCP/IP socket options for the listener.
        "socket options" => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'socket options' directive must not be empty",
                ));
            }

            let origin = ConfigDirectiveOrigin {
                path: canonical.to_path_buf(),
                line: line_number,
            };

            if let Some((existing, existing_origin)) = &state.socket_options {
                if existing != trimmed {
                    let existing_line = existing_origin.line;
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!(
                            "duplicate 'socket options' directive in global section (previously defined on line {existing_line})"
                        ),
                    ));
                }
            } else {
                state.socket_options = Some((trimmed.to_string(), origin));
            }
        }
        "proxy protocol" => {
            let parsed = parse_boolean_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid boolean value '{value}' for 'proxy protocol'"),
                )
            })?;

            let origin = ConfigDirectiveOrigin {
                path: canonical.to_path_buf(),
                line: line_number,
            };

            if let Some((existing, existing_origin)) = &state.proxy_protocol {
                if *existing != parsed {
                    let existing_line = existing_origin.line;
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!(
                            "duplicate 'proxy protocol' directive in global section (previously defined on line {existing_line})"
                        ),
                    ));
                }
            } else {
                state.proxy_protocol = Some((parsed, origin));
            }
        }
        "daemon chroot" => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'daemon chroot' must not be empty",
                ));
            }

            let origin = ConfigDirectiveOrigin {
                path: canonical.to_path_buf(),
                line: line_number,
            };

            if let Some((existing, existing_origin)) = &state.daemon_chroot {
                if existing != Path::new(trimmed) {
                    let existing_line = existing_origin.line;
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!(
                            "duplicate 'daemon chroot' directive in global section (previously defined on line {existing_line})"
                        ),
                    ));
                }
            } else {
                state.daemon_chroot = Some((PathBuf::from(trimmed), origin));
            }
        }
        _ => {
            eprintln!(
                "warning: unknown global directive '{}' in '{}' line {} [daemon={}]",
                key,
                path.display(),
                line_number,
                env!("CARGO_PKG_VERSION"),
            );
        }
    }
    Ok(())
}
