// Global-section directive dispatch.
//
// Routes a `key = value` directive that appears before any `[module]` header
// to its handler, including the `include` directive that triggers recursive
// config file parsing and result merging, the daemon-wide socket/auth/logging
// directives, and the P_LOCAL parameter defaults inherited by all modules.

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
        "refuseoptions" => {
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
        "include" | "&include" | "&merge" => {
            // upstream: params.c:parse_directives - `&include` and `&merge` both
            // pull configuration from another file. The historical `include =`
            // form is retained as a synonym so existing oc-rsync configs keep
            // working. `&include` runs under a private global scope (`]push`/
            // `]pop`) so the included file's globals do not leak back, and a
            // directory target globs `*.conf`; `&merge` shares the current scope
            // and globs `*.inc`. `apply_include_directive` implements both.
            apply_include_directive(state, key, value, path, line_number, canonical, stack)?;
        }
        "motdfile" => {
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
        "pidfile" => {
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
        "reverselookup" => {
            let Some(parsed) =
                apply_boolean_directive(value, false, "reverse lookup", path, line_number)
            else {
                return Ok(());
            };

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
            // upstream: loadparm.c - `reverse lookup` is P_LOCAL, so a value in
            // the global section becomes the default every later module inherits
            // (init_section copies Vars.l). state.reverse_lookup above is the
            // daemon template read as `lp_reverse_lookup(-1)`.
            state.module_defaults.reverse_lookup = Some(parsed);
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
        "secretsfile" => {
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
        "incomingchmod" | "incoming-chmod" => {
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
        "outgoingchmod" | "outgoing-chmod" => {
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
        "lockfile" => {
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
        "usechroot" => {
            let Some(parsed) =
                apply_boolean_directive(value, true, "use chroot", path, line_number)
            else {
                return Ok(());
            };

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
        "syslogfacility" => {
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
                // upstream: loadparm.c - `syslog facility` is P_LOCAL, so a
                // global-section value becomes the default every later module
                // inherits (init_section copies Vars.l). state.syslog_facility
                // is the daemon-wide value read as `lp_syslog_facility(-1)`.
                state.module_defaults.syslog_facility = Some(owned.clone());
                state.syslog_facility = Some((owned, origin));
            }
        }
        // upstream: loadparm.c - syslog tag sets the syslog ident prefix.
        "syslogtag" => {
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
                // upstream: loadparm.c - `syslog tag` is P_LOCAL; the
                // global-section value seeds every module's inherited default.
                state.module_defaults.syslog_tag = Some(owned.clone());
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
        "listenbacklog" => {
            let parsed = parse_atoi(value).max(0) as u32;

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
        // oc-rsync extension - number of SO_REUSEPORT listener replicas to bind
        // per address family (default 1). Has no upstream equivalent; changes
        // only kernel socket behaviour, never the wire.
        "acceptorthreads" => {
            let parsed: u32 = value.parse().map_err(|_| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid integer value '{value}' for 'acceptor threads'"),
                )
            })?;
            let threads = NonZeroU32::new(parsed).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    "'acceptor threads' must be at least 1".to_string(),
                )
            })?;

            let origin = ConfigDirectiveOrigin {
                path: canonical.to_path_buf(),
                line: line_number,
            };

            if let Some((existing, existing_origin)) = &state.acceptor_threads {
                if *existing != threads {
                    let existing_line = existing_origin.line;
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!(
                            "duplicate 'acceptor threads' directive in global section (previously defined on line {existing_line})"
                        ),
                    ));
                }
            } else {
                state.acceptor_threads = Some((threads, origin));
            }
        }
        // upstream: daemon-parm.txt - port INTEGER, P_GLOBAL, default 0.
        // Controls the TCP port the daemon listens on.
        "port" | "rsyncport" => {
            let parsed = parse_atoi(value).clamp(0, i32::from(u16::MAX)) as u16;

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
        "socketoptions" => {
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
        "proxyprotocol" => {
            let Some(parsed) =
                apply_boolean_directive(value, false, "proxy protocol", path, line_number)
            else {
                return Ok(());
            };

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
        "daemonchroot" => {
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
        // upstream: loadparm.c - P_LOCAL directives in the global section set
        // default values inherited by all modules that don't override them.
        // When bInGlobalSection is true, parm_ptr = def_ptr, so the value
        // written becomes the default for all subsequent module sections
        // (via init_section -> copy_section).
        "exclude" => {
            if !value.is_empty() {
                state.module_defaults.exclude.push(value.to_owned());
            }
        }
        // Note: "include" as a P_LOCAL default is not handled here because
        // our key=value parser already claims "include" for config file
        // inclusion (upstream uses "&include /path" which doesn't collide).
        "filter" => {
            if !value.is_empty() {
                state.module_defaults.filter.push(value.to_owned());
            }
        }
        "maxverbosity" => {
            state.module_defaults.max_verbosity = Some(parse_atoi(value));
        }
        "transferlogging" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "transfer logging", path, line_number)
            {
                state.module_defaults.transfer_logging = Some(parsed);
            }
        }
        "logformat" => {
            if !value.is_empty() {
                state.module_defaults.log_format = Some(value.to_owned());
            }
        }
        "logfile" => {
            if !value.is_empty() {
                let resolved = resolve_config_relative_path(canonical, value);
                state.module_defaults.log_file = Some(resolved);
            }
        }
        "hostsallow" => {
            let patterns = parse_host_list(value, path, line_number, "hosts allow")?;
            state.module_defaults.hosts_allow = Some(patterns);
        }
        "hostsdeny" => {
            let patterns = parse_host_list(value, path, line_number, "hosts deny")?;
            state.module_defaults.hosts_deny = Some(patterns);
        }
        "timeout" => {
            let timeout = parse_timeout_seconds(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid timeout '{value}'"),
                )
            })?;
            state.module_defaults.timeout = Some(timeout);
        }
        "dontcompress" => {
            if !value.is_empty() {
                state.module_defaults.dont_compress = Some(value.to_owned());
            }
        }
        "readonly" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "read only", path, line_number)
            {
                state.module_defaults.read_only = Some(parsed);
            }
        }
        "writeonly" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "write only", path, line_number)
            {
                state.module_defaults.write_only = Some(parsed);
            }
        }
        "list" => {
            if let Some(parsed) = apply_boolean_directive(value, false, "list", path, line_number) {
                state.module_defaults.listable = Some(parsed);
            }
        }
        "mungesymlinks" => {
            if let Some(parsed) =
                apply_boolean_directive(value, true, "munge symlinks", path, line_number)
            {
                state.module_defaults.munge_symlinks = Some(Some(parsed));
            }
        }
        "numericids" => {
            if let Some(parsed) =
                apply_boolean_directive(value, true, "numeric ids", path, line_number)
            {
                state.module_defaults.numeric_ids = Some(parsed);
            }
        }
        "fakesuper" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "fake super", path, line_number)
            {
                state.module_defaults.fake_super = Some(parsed);
            }
        }
        "maxconnections" => {
            let max = parse_max_connections_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid max connections value '{value}'"),
                )
            })?;
            state.module_defaults.max_connections = Some(max);
        }
        "ignoreerrors" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "ignore errors", path, line_number)
            {
                state.module_defaults.ignore_errors = Some(parsed);
            }
        }
        "ignorenonreadable" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "ignore nonreadable", path, line_number)
            {
                state.module_defaults.ignore_nonreadable = Some(parsed);
            }
        }
        "strictmodes" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "strict modes", path, line_number)
            {
                state.module_defaults.strict_modes = Some(parsed);
            }
        }
        "forwardlookup" => {
            // forward lookup is both P_LOCAL and has a global handler above
            // for reverse_lookup. This arm handles the module-default case.
            if let Some(parsed) =
                apply_boolean_directive(value, false, "forward lookup", path, line_number)
            {
                state.module_defaults.forward_lookup = Some(parsed);
            }
        }
        "opennoatime" => {
            if let Some(parsed) =
                apply_boolean_directive(value, true, "open noatime", path, line_number)
            {
                state.module_defaults.open_noatime = Some(parsed);
            }
        }
        "excludefrom" => {
            if !value.is_empty() {
                let resolved = resolve_config_relative_path(canonical, value);
                state.module_defaults.exclude_from = Some(resolved);
            }
        }
        "includefrom" => {
            if !value.is_empty() {
                let resolved = resolve_config_relative_path(canonical, value);
                state.module_defaults.include_from = Some(resolved);
            }
        }
        "comment" => {
            if !value.is_empty() {
                state.module_defaults.comment = Some(value.to_owned());
            }
        }
        "earlyexec" => {
            if !value.is_empty() {
                state.module_defaults.early_exec = Some(value.to_owned());
            }
        }
        "pre-xferexec" => {
            if !value.is_empty() {
                state.module_defaults.pre_xfer_exec = Some(value.to_owned());
            }
        }
        "post-xferexec" => {
            if !value.is_empty() {
                state.module_defaults.post_xfer_exec = Some(value.to_owned());
            }
        }
        "nameconverter" => {
            if !value.is_empty() {
                state.module_defaults.name_converter = Some(value.to_owned());
            }
        }
        "charset" => {
            if !value.is_empty() {
                state.module_defaults.charset = Some(value.to_owned());
            }
        }
        // P_LOCAL directives that only make sense per-module - silently accepted
        // but not stored as inheritable defaults.
        "path" | "authusers" => {}
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
