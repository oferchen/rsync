// Per-module directive parsing.
//
// Handles the `key = value` directives found inside `[module]` sections of
// rsyncd.conf. Each recognized key is dispatched to the corresponding setter
// on `ModuleDefinitionBuilder`.

/// Public config keys of the daemon parameters upstream classifies `P_GLOBAL`.
///
/// These are only valid in the global section (before the first `[module]`
/// header). The list mirrors the `Globals:` block of upstream
/// `daemon-parm.txt`; each entry is the `parm_table` label (the parameter's
/// public name with underscores rendered as spaces).
///
/// upstream: daemon-parm.txt `Globals:` - `parm_table[]` marks each of these
/// `P_GLOBAL` (loadparm.c `parm_class`). Everything else is `P_LOCAL` and may
/// be set per-module.
///
/// Entries are stored in the whitespace-folded, lowercase form produced by
/// `normalize_param_name`, since `is_global_only_directive` is queried with an
/// already-normalized key. Multi-word names (`daemon chroot`, `motd file`, ...)
/// must appear without spaces or they would never match, and such a directive in
/// a module section would fall through to the generic unknown-directive warning
/// instead of the specific "Global parameter ... found in module section!" path.
const GLOBAL_ONLY_DIRECTIVES: &[&str] = &[
    "address",
    "daemonchroot",
    "daemongid",
    "daemonuid",
    "motdfile",
    "pidfile",
    "socketoptions",
    "listenbacklog",
    "port",
    "proxyprotocol",
];

/// Returns `true` when `key` names an upstream `P_GLOBAL` parameter that is
/// valid only in the global section (see [`GLOBAL_ONLY_DIRECTIVES`]).
fn is_global_only_directive(key: &str) -> bool {
    GLOBAL_ONLY_DIRECTIVES.contains(&key)
}

/// Applies a single per-module directive to the builder.
///
/// Returns `Ok(true)` if the key was recognized (even if unknown and warned),
/// `Ok(false)` is never returned - unknown keys are warned and accepted.
fn apply_module_directive(
    builder: &mut ModuleDefinitionBuilder,
    key: &str,
    value: &str,
    path: &Path,
    line_number: usize,
    canonical: &Path,
) -> Result<(), DaemonError> {
    match key {
        "path" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "module path directive must not be empty",
                ));
            }
            builder.set_path(PathBuf::from(strip_trailing_slashes(value)));
        }
        "comment" => {
            let comment = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_comment(comment);
        }
        "hostsallow" => {
            let patterns = parse_host_list(value, path, line_number, "hosts allow")?;
            builder.set_hosts_allow(patterns);
        }
        "hostsdeny" => {
            let patterns = parse_host_list(value, path, line_number, "hosts deny")?;
            builder.set_hosts_deny(patterns);
        }
        "authusers" => {
            let users = parse_auth_user_list(value).map_err(|error| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid 'auth users' directive: {error}"),
                )
            })?;
            builder.set_auth_users(users, path, line_number)?;
        }
        "authdigest" => {
            builder.set_auth_digest(value);
        }
        "secretsfile" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'secrets file' directive must not be empty",
                ));
            }
            builder.set_secrets_file(PathBuf::from(value), path, line_number)?;
        }
        "refuseoptions" => {
            let options = parse_refuse_option_list(value).map_err(|error| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid 'refuse options' directive: {error}"),
                )
            })?;
            builder.set_refuse_options(options, path, line_number)?;
        }
        "readonly" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "read only", path, line_number)
            {
                builder.set_read_only(parsed);
            }
        }
        "writeonly" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "write only", path, line_number)
            {
                builder.set_write_only(parsed);
            }
        }
        "usechroot" => {
            if let Some(parsed) =
                apply_boolean_directive(value, true, "use chroot", path, line_number)
            {
                builder.set_use_chroot(parsed);
            }
        }
        "numericids" => {
            if let Some(parsed) =
                apply_boolean_directive(value, true, "numeric ids", path, line_number)
            {
                builder.set_numeric_ids(parsed);
            }
        }
        "list" => {
            if let Some(parsed) = apply_boolean_directive(value, false, "list", path, line_number) {
                builder.set_listable(parsed);
            }
        }
        "fakesuper" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "fake super", path, line_number)
            {
                builder.set_fake_super(parsed);
            }
        }
        "insecurelinks" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "insecure links", path, line_number)
            {
                builder.set_insecure_links(parsed);
            }
        }
        "mungesymlinks" => {
            if let Some(parsed) =
                apply_boolean_directive(value, true, "munge symlinks", path, line_number)
            {
                builder.set_munge_symlinks(Some(parsed));
            }
        }
        "uid" => {
            let uid = parse_uid_setting(value).ok_or_else(|| {
                config_parse_error(path, line_number, format!("invalid uid '{value}'"))
            })?;
            builder.set_uid(uid);
        }
        "gid" => {
            let gid = parse_gid_setting(value).map_err(|reason| {
                config_parse_error(path, line_number, format!("invalid gid '{value}': {reason}"))
            })?;
            builder.set_gid(gid);
        }
        "timeout" => {
            let timeout = parse_timeout_seconds(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid timeout '{value}'"),
                )
            })?;
            builder.set_timeout(timeout);
        }
        "maxconnections" => {
            let max = parse_max_connections_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid max connections value '{value}'"),
                )
            })?;
            builder.set_max_connections(max);
        }
        "incomingchmod" | "incoming-chmod" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'incoming chmod' directive must not be empty",
                ));
            }
            builder.set_incoming_chmod(Some(value.to_owned()));
        }
        "outgoingchmod" | "outgoing-chmod" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'outgoing chmod' directive must not be empty",
                ));
            }
            builder.set_outgoing_chmod(Some(value.to_owned()));
        }
        "maxverbosity" => {
            let parsed = parse_atoi(value);
            builder.set_max_verbosity(parsed);
        }
        "ignoreerrors" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "ignore errors", path, line_number)
            {
                builder.set_ignore_errors(parsed);
            }
        }
        "ignorenonreadable" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "ignore nonreadable", path, line_number)
            {
                builder.set_ignore_nonreadable(parsed);
            }
        }
        "transferlogging" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "transfer logging", path, line_number)
            {
                builder.set_transfer_logging(parsed);
            }
        }
        "logformat" => {
            let format = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_log_format(format);
        }
        "logfile" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'log file' directive must not be empty",
                ));
            }
            let resolved = resolve_config_relative_path(canonical, value);
            builder.set_log_file(resolved);
        }
        "dontcompress" => {
            let patterns = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_dont_compress(patterns);
        }
        "earlyexec" => {
            let cmd = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_early_exec(cmd);
        }
        "pre-xferexec" => {
            let cmd = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_pre_xfer_exec(cmd);
        }
        "post-xferexec" => {
            let cmd = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_post_xfer_exec(cmd);
        }
        "nameconverter" => {
            let cmd = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_name_converter(cmd);
        }
        "tempdir" => {
            let dir = if value.is_empty() {
                None
            } else {
                Some(strip_trailing_slashes(value))
            };
            builder.set_temp_dir(dir);
        }
        "charset" => {
            let cs = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_charset(cs);
        }
        "forwardlookup" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "forward lookup", path, line_number)
            {
                builder.set_forward_lookup(parsed);
            }
        }
        "strictmodes" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "strict modes", path, line_number)
            {
                builder.set_strict_modes(parsed);
            }
        }
        "opennoatime" => {
            if let Some(parsed) =
                apply_boolean_directive(value, true, "open noatime", path, line_number)
            {
                builder.set_open_noatime(parsed);
            }
        }
        // upstream: daemon-parm.txt - `exclude_from` STRING, default NULL.
        // Loaded via parse_filter_file() in clientserver.c.
        "excludefrom" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'exclude from' directive must not be empty",
                ));
            }
            let resolved = resolve_config_relative_path(canonical, value);
            builder.set_exclude_from(resolved);
        }
        // upstream: daemon-parm.txt - `include_from` STRING, default NULL.
        // Loaded via parse_filter_file() in clientserver.c.
        "includefrom" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'include from' directive must not be empty",
                ));
            }
            let resolved = resolve_config_relative_path(canonical, value);
            builder.set_include_from(resolved);
        }
        // upstream: daemon-parm.h - `filter` STRING, P_LOCAL.
        // Repeatable: multiple directives accumulate rules.
        "filter" => {
            if !value.is_empty() {
                builder.filter.push(value.to_owned());
            }
        }
        // upstream: daemon-parm.h - `exclude` STRING, P_LOCAL.
        "exclude" => {
            if !value.is_empty() {
                builder.exclude.push(value.to_owned());
            }
        }
        // upstream: daemon-parm.h - `include` STRING, P_LOCAL.
        "include" => {
            if !value.is_empty() {
                builder.include.push(value.to_owned());
            }
        }
        // upstream: daemon-parm.h:78 `reverse_lookup` BOOL, P_LOCAL. Consumed
        // per-module at clientserver.c:723 `lp_reverse_lookup(i)`.
        "reverselookup" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "reverse lookup", path, line_number)
            {
                builder.set_reverse_lookup(parsed);
            }
        }
        // upstream: daemon-parm.h:46 `lock_file` STRING, P_LOCAL. Consumed
        // per-module at clientserver.c:746 `claim_connection(lp_lock_file(i), ...)`.
        "lockfile" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'lock file' directive must not be empty",
                ));
            }
            let resolved = resolve_config_relative_path(canonical, value);
            builder.set_lock_file(resolved);
        }
        // upstream: loadparm.c syslog_tag (P_STRING, P_LOCAL). Consumed
        // per-module at log.c:143 `openlog(lp_syslog_tag(module_id), ...)`.
        "syslogtag" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'syslog tag' directive must not be empty",
                ));
            }
            builder.set_syslog_tag(value.to_owned());
        }
        // upstream: loadparm.c syslog_facility (P_ENUM, P_LOCAL). Consumed
        // per-module at log.c:143 `openlog(..., lp_syslog_facility(module_id))`.
        "syslogfacility" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'syslog facility' directive must not be empty",
                ));
            }
            // upstream: loadparm.c:456-467 `case P_ENUM` - a name that matches a
            // facility is stored canonically; an unrecognised name that parses as
            // a positive integer (`atoi(value) > 0`) is stored as that raw
            // numeric facility; any other unrecognised name leaves the inherited
            // value unchanged (no config error).
            if let Some(canonical) = logging_sink::canonical_syslog_facility(value) {
                builder.set_syslog_facility(canonical.to_owned());
            } else if parse_atoi(value) > 0 {
                builder.set_syslog_facility(value.to_owned());
            }
        }
        // oc extension (docs/design/quic-transport-policy.md, decision A): the
        // QUIC listener presents one certificate on a socket shared by every
        // module, so a per-module `quic cert file` / `quic key file` cannot be
        // honoured. Unlike upstream's P_GLOBAL directives - which loadparm.c
        // merely reports and ignores in a module section - a misplaced identity
        // directive is a hard config error: silently dropping it would leave the
        // operator believing a certificate they named is in force. Reuse the
        // shared `config_parse_error` surface every other directive uses.
        #[cfg(feature = "quic")]
        "quiccertfile" | "quickeyfile" | "quicport" => {
            let directive = match key {
                "quiccertfile" => "quic cert file",
                "quickeyfile" => "quic key file",
                _ => "quic port",
            };
            return Err(config_parse_error(
                path,
                line_number,
                format!(
                    "'{directive}' is a global-only directive and cannot appear in a module section"
                ),
            ));
        }
        _ if is_global_only_directive(key) => {
            // upstream: loadparm.c:do_parameter - a known P_GLOBAL parameter
            // that appears inside a module section is reported and ignored,
            // never applied to the module (loadparm.c: "Global parameter %s
            // found in module section!").
            eprintln!("Global parameter {key} found in module section!");
        }
        _ => {
            eprintln!(
                "warning: unknown per-module directive '{}' in '{}' line {} [daemon={}]",
                key,
                path.display(),
                line_number,
                env!("CARGO_PKG_VERSION"),
            );
        }
    }
    Ok(())
}
