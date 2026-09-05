// Global-section directive dispatch.
//
// Routes a `key = value` directive that appears before any `[module]` header
// to its handler, including the `include` directive that triggers recursive
// config file parsing and result merging, the daemon-wide socket/auth/logging
// directives, and the P_LOCAL parameter defaults inherited by all modules.

/// Records a global-section directive value, overwriting whatever an earlier
/// occurrence of the same directive stored.
///
/// upstream: loadparm.c:379-470 do_parameter() - the parameter pointer is
/// resolved from parm_table and assigned unconditionally, with no seen-set and
/// no duplicate check, so a directive repeated in one config file keeps its
/// last value. Routing every global slot through this one helper keeps that
/// policy identical across directives.
fn store_global_directive<T>(
    slot: &mut Option<(T, ConfigDirectiveOrigin)>,
    value: T,
    canonical: &Path,
    line_number: usize,
) {
    *slot = Some((
        value,
        ConfigDirectiveOrigin {
            path: canonical.to_path_buf(),
            line: line_number,
        },
    ));
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

            // The list is not additive: a second `refuse options` directive
            // replaces the first, leaving exactly one global entry for the
            // modules to inherit.
            state.global_refuse_directives.clear();
            state.global_refuse_directives.push((
                options,
                ConfigDirectiveOrigin {
                    path: canonical.to_path_buf(),
                    line: line_number,
                },
            ));
        }
        "&include" | "&merge" => {
            // upstream: params.c:parse_directives - `&include` and `&merge` both
            // pull configuration from another file. `&include` runs under a
            // private global scope (`]push`/`]pop`) so the included file's globals
            // do not leak back, and a directory target globs `*.conf`; `&merge`
            // shares the current scope and globs `*.inc`. `apply_include_directive`
            // implements both. A bare `include` (no `&`) is NOT file inclusion -
            // it is the P_LOCAL `include` filter parameter, handled below.
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
            // upstream: clientserver.c:188 reads the motd through
            // `open_no_attacker_symlinks()`. The daemon is typically root here,
            // so a symlink planted at any component of an operator-named motd
            // path would otherwise splice an arbitrary file into the greeting.
            // An unreadable motd is NOT fatal. upstream opens the file per
            // connection and keeps the failure local to the greeting
            // (clientserver.c:188-197):
            //
            //     int motd_fd = open_no_attacker_symlinks(motd, O_RDONLY, 0);
            //     FILE *f = motd_fd >= 0 ? fdopen(motd_fd, "r") : NULL;
            //     while (f && !feof(f)) { ... }
            //
            // `f` is NULL for every failure mode - a refused symlink, ENOENT,
            // EACCES alike - so the read loop is skipped and the connection
            // proceeds with no motd content. The daemon never declines to
            // start over it. Propagating the error here instead killed the
            // daemon during config parse, before `listen()`, which is what
            // the 3.5.0 `daemon-config-symlink` cell observes as
            // `rsyncd exited before listening`.
            let contents =
                crate::daemon::operator_file::read_to_string(&motd_path).unwrap_or_default();

            // upstream: loadparm.c `motd_file` is a P_STRING slot written with
            // string_set(), so a second `motd file` directive replaces the
            // first rather than appending to it - only the last file's lines
            // are greeted with.
            state.motd_lines = contents
                .lines()
                .map(|raw_line| raw_line.trim_end_matches('\r').to_owned())
                .collect();
        }
        "motd" => {
            // `motd` has NO upstream counterpart - daemon-parm.txt declares only
            // `motd_file`, so loadparm.c's last-wins rule says nothing about how
            // an inline value composes with a file. Appending is this
            // implementation's established behaviour and is left untouched here;
            // only the upstream-backed `motd file` slot gained last-wins
            // semantics.
            state
                .motd_lines
                .push(value.trim_end_matches(['\r', '\n']).to_owned());
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

            let resolved = daemon_parameter_path(trimmed);
            store_global_directive(&mut state.pid_file, resolved, canonical, line_number);
        }
        "reverselookup" => {
            let Some(parsed) =
                apply_boolean_directive(value, false, "reverse lookup", path, line_number)
            else {
                return Ok(());
            };

            store_global_directive(&mut state.reverse_lookup, parsed, canonical, line_number);
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
            store_global_directive(
                &mut state.global_bwlimit,
                components,
                canonical,
                line_number,
            );
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
            store_global_directive(
                &mut state.global_secrets_file,
                validated,
                canonical,
                line_number,
            );
        }
        "incomingchmod" | "incoming-chmod" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'incoming chmod' directive must not be empty",
                ));
            }

            store_global_directive(
                &mut state.global_incoming_chmod,
                value.to_owned(),
                canonical,
                line_number,
            );
        }
        "outgoingchmod" | "outgoing-chmod" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'outgoing chmod' directive must not be empty",
                ));
            }

            store_global_directive(
                &mut state.global_outgoing_chmod,
                value.to_owned(),
                canonical,
                line_number,
            );
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
            store_global_directive(&mut state.lock_file, resolved, canonical, line_number);
        }
        // oc extension (no upstream counterpart - upstream terminates no TLS in
        // the daemon). `quic cert file` / `quic key file` name the certificate
        // and private key the QUIC listener presents. They describe the shared
        // per-listener identity, so they are global-only; a module section that
        // sets one is rejected in `apply_module_directive`. Path handling mirrors
        // `pid file` / `lock file` exactly (config-relative resolution, with any
        // `%` token left verbatim for expansion at listener-bind time).
        // See docs/design/quic-transport-policy.md (decision A).
        #[cfg(feature = "quic")]
        "quiccertfile" => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'quic cert file' directive must not be empty",
                ));
            }

            let resolved = resolve_config_relative_path(path, trimmed);
            store_global_directive(&mut state.quic_cert_file, resolved, canonical, line_number);
        }
        #[cfg(feature = "quic")]
        "quickeyfile" => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'quic key file' directive must not be empty",
                ));
            }

            let resolved = resolve_config_relative_path(path, trimmed);
            store_global_directive(&mut state.quic_key_file, resolved, canonical, line_number);
        }
        // oc extension (no upstream counterpart). `quic port` selects the port
        // the QUIC listener binds; unset, the listener shares the daemon TCP
        // `port` (873 by default). Value handling mirrors the TCP `port`
        // directive: `parse_atoi` clamped into the u16 range, then a 0 coerced
        // to the well-known rsync port 873, matching the `port = 0` / `--port 0`
        // coercion in parsing.rs (upstream options.c). Global-only like the QUIC
        // identity directives. See docs/design/quic-transport-policy.md.
        #[cfg(feature = "quic")]
        "quicport" => {
            let mut parsed = parse_atoi(value).clamp(0, i32::from(u16::MAX)) as u16;
            if parsed == 0 {
                parsed = DEFAULT_PORT;
            }

            store_global_directive(&mut state.quic_port, parsed, canonical, line_number);
        }
        // upstream: loadparm.c - use chroot is valid in the global section as a
        // default that applies to all modules which do not override it explicitly.
        "usechroot" => {
            let Some(parsed) =
                apply_boolean_directive(value, true, "use chroot", path, line_number)
            else {
                return Ok(());
            };

            store_global_directive(&mut state.global_use_chroot, parsed, canonical, line_number);
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

            // upstream: loadparm.c - `syslog facility` is P_LOCAL, so a
            // global-section value becomes the default every later module
            // inherits (init_section copies Vars.l). state.syslog_facility is
            // the daemon-wide value read as `lp_syslog_facility(-1)`.
            state.module_defaults.syslog_facility = Some(value.to_owned());
            store_global_directive(
                &mut state.syslog_facility,
                value.to_owned(),
                canonical,
                line_number,
            );
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

            // upstream: loadparm.c - `syslog tag` is P_LOCAL; the
            // global-section value seeds every module's inherited default.
            state.module_defaults.syslog_tag = Some(value.to_owned());
            store_global_directive(
                &mut state.syslog_tag,
                value.to_owned(),
                canonical,
                line_number,
            );
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

            let parsed_addr = parse_bind_address(&OsString::from(value)).map_err(|_| {
                config_parse_error(path, line_number, format!("invalid bind address '{value}'"))
            })?;

            store_global_directive(&mut state.bind_address, parsed_addr, canonical, line_number);
        }
        // upstream: daemon-parm.txt `Locals:` `uid` is P_LOCAL. A value in the
        // global section is the default `lp_uid(module_id)` every module
        // inherits (clientserver.c:781); it is NOT a process-wide daemon drop.
        // Conflating it with the P_GLOBAL `daemon uid` made a root daemon drop
        // every uid-less module to `nobody` (clientserver.c:781 `am_root ?
        // NOBODY_USER`) even when the operator set a global `uid = 0`.
        "uid" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'uid' directive must not be empty",
                ));
            }
            let uid = parse_uid_setting(value).ok_or_else(|| {
                config_parse_error(path, line_number, format!("invalid uid '{value}'"))
            })?;
            state.module_defaults.uid = Some(uid);
        }
        // upstream: daemon-parm.txt `Locals:` `gid` is P_LOCAL - the global
        // value is the default `lp_gid(module_id)` every module inherits
        // (clientserver.c:790), distinct from the P_GLOBAL `daemon gid` drop.
        "gid" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'gid' directive must not be empty",
                ));
            }
            let gid = parse_gid_setting(value).map_err(|reason| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid gid '{value}': {reason}"),
                )
            })?;
            state.module_defaults.gid = Some(gid);
        }
        // upstream: daemon-parm.txt `Globals:` `daemon_uid`/`daemon_gid` are
        // P_GLOBAL. `daemon uid` sets the process-wide uid the listener drops
        // to once, before the accept loop (clientserver.c:1376 `lp_daemon_uid`).
        "daemonuid" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'daemon uid' directive must not be empty",
                ));
            }

            store_global_directive(
                &mut state.daemon_uid,
                value.to_owned(),
                canonical,
                line_number,
            );
        }
        // upstream: clientserver.c:1500 `lp_daemon_gid` - `daemon gid` sets the
        // process-wide gid the listener drops to before the accept loop.
        "daemongid" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'daemon gid' directive must not be empty",
                ));
            }

            store_global_directive(
                &mut state.daemon_gid,
                value.to_owned(),
                canonical,
                line_number,
            );
        }
        // upstream: daemon-parm.txt - listen_backlog INTEGER, default 5.
        // Controls the backlog argument passed to listen(2).
        "listenbacklog" => {
            let parsed = parse_atoi(value).max(0) as u32;

            store_global_directive(&mut state.listen_backlog, parsed, canonical, line_number);
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

            store_global_directive(&mut state.acceptor_threads, threads, canonical, line_number);
        }
        // upstream: daemon-parm.txt - port INTEGER, P_GLOBAL, default 0.
        // Controls the TCP port the daemon listens on.
        "port" | "rsyncport" => {
            let parsed = parse_atoi(value).clamp(0, i32::from(u16::MAX)) as u16;

            store_global_directive(&mut state.rsync_port, parsed, canonical, line_number);
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

            store_global_directive(
                &mut state.socket_options,
                trimmed.to_owned(),
                canonical,
                line_number,
            );
        }
        "proxyprotocol" => {
            let Some(parsed) =
                apply_boolean_directive(value, false, "proxy protocol", path, line_number)
            else {
                return Ok(());
            };

            store_global_directive(&mut state.proxy_protocol, parsed, canonical, line_number);
        }
        "proxyprotocolhosts" => {
            // upstream: access.c:300-306 `allow_proxy_protocol_peer()` reads
            // the list with `if (!list || !*list) return 0;`, so an empty
            // value is legal config that trusts nobody - which the empty
            // pattern set expresses, since nothing matches against it.
            let patterns = parse_host_list(value, path, line_number, "proxy protocol hosts")?;

            store_global_directive(
                &mut state.proxy_protocol_hosts,
                patterns,
                canonical,
                line_number,
            );
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

            store_global_directive(
                &mut state.daemon_chroot,
                PathBuf::from(trimmed),
                canonical,
                line_number,
            );
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
        // upstream: daemon-parm.txt `Locals:` `include` is P_STRING/P_LOCAL
        // (loadparm.c registers it via daemon-parm.h), NOT file inclusion -
        // upstream reserves `&include` for pulling in another file (params.c).
        // A bare global `include` therefore seeds every module's include-filter
        // default, mirroring `exclude` above.
        "include" => {
            if !value.is_empty() {
                state.module_defaults.include.push(value.to_owned());
            }
        }
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
        // `log file` feeds TWO consumers, because upstream reads it through two
        // different lenses. It is `P_LOCAL` (daemon-parm.h:289), so the global
        // value is the default every module inherits - that is the
        // `module_defaults` half. But `FN_LOCAL_STRING(lp_log_file, log_file)`
        // also resolves to the global value at `module_id < 0`, and
        // `daemon_main` calls `log_init(0)` (clientserver.c:1768) at startup,
        // before any connection selects a module. That startup read is the
        // `state.log_file` half, and it is what opens the daemon-wide log that
        // carries the listening banner and every pre-module diagnostic
        // (`clientserver.c:1394`'s proxy rejection among them).
        //
        // Recording only the module default leaves `log_init`'s equivalent with
        // nothing to open, so the log file is never created and those lines are
        // lost - measured against rsync 3.5.0, which writes three of them here.
        "logfile" => {
            if !value.is_empty() {
                let resolved = daemon_parameter_path(value);
                state.module_defaults.log_file = Some(resolved.clone());
                store_global_directive(&mut state.log_file, resolved, canonical, line_number);
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
        // `timeout` feeds TWO consumers, for the same reason `log file` does.
        // It is `P_LOCAL`, so the global value is every module's default - the
        // `module_defaults` half. But `daemon_handshake_timeout(-1)`
        // (clientserver.c:92-100) reads `lp_timeout(-1)`, i.e. the GLOBAL value,
        // and clientserver.c:1441 arms it BEFORE any module has been selected,
        // to bound the pre-module handshake. Recording only the module default
        // leaves that phase reading nothing and silently falling back to the
        // 60s built-in bound.
        "timeout" => {
            let timeout = parse_timeout_seconds(value).ok_or_else(|| {
                config_parse_error(path, line_number, format!("invalid timeout '{value}'"))
            })?;
            state.module_defaults.timeout = Some(timeout);
            store_global_directive(&mut state.daemon_timeout, timeout, canonical, line_number);
        }
        "dontcompress" => {
            if !value.is_empty() {
                state.module_defaults.dont_compress = Some(value.to_owned());
            }
        }
        "tempdir" => {
            // upstream: daemon-parm.txt marks `temp dir` P_LOCAL, so a value in
            // the global section becomes the default every module inherits
            // (loadparm.c init_section copies Vars.l). P_PATH: trailing slashes
            // are stripped. Without this arm a global `temp dir` fell through to
            // the unknown-directive warning and modules never inherited it.
            if !value.is_empty() {
                state.module_defaults.temp_dir = Some(strip_trailing_slashes(value));
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
        "insecurelinks" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "insecure links", path, line_number)
            {
                state.module_defaults.insecure_links = Some(parsed);
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
        // upstream: daemon-parm.h:262 - `auth users` is P_LOCAL, so a value in
        // the global section is the default every module inherits unless it sets
        // its own list (loadparm.c init_section/copy_section). Modules that omit
        // `auth users` must then still authenticate (authenticate.c:228
        // auth_server reads lp_auth_users), so store the list as the default.
        "authusers" => {
            let users = parse_auth_user_list(value).map_err(|error| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid 'auth users' directive: {error}"),
                )
            })?;
            state.module_defaults.auth_users = Some(users);
        }
        // upstream: daemon-parm.h:273 - `auth digest` is P_LOCAL, so a global
        // value becomes the floor every module inherits unless it sets its own.
        "authdigest" => {
            state.module_defaults.auth_digest = normalize_auth_digest(value);
        }
        // `path` only makes sense per-module - silently accepted, not inherited.
        "path" => {}
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
