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
const GLOBAL_ONLY_DIRECTIVES: &[&str] = &[
    "address",
    "daemon chroot",
    "daemon gid",
    "daemon uid",
    "motd file",
    "pid file",
    "socket options",
    "listen backlog",
    "port",
    "proxy protocol",
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
            builder.set_path(PathBuf::from(value), path, line_number)?;
        }
        "comment" => {
            let comment = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_comment(comment, path, line_number)?;
        }
        "hostsallow" => {
            let patterns = parse_host_list(value, path, line_number, "hosts allow")?;
            builder.set_hosts_allow(patterns, path, line_number)?;
        }
        "hostsdeny" => {
            let patterns = parse_host_list(value, path, line_number, "hosts deny")?;
            builder.set_hosts_deny(patterns, path, line_number)?;
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
        "bwlimit" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'bwlimit' directive must not be empty",
                ));
            }
            let components = parse_config_bwlimit(value, path, line_number)?;
            builder.set_bandwidth_limit(
                components.rate(),
                components.burst(),
                components.burst_specified(),
                path,
                line_number,
            )?;
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
                builder.set_read_only(parsed, path, line_number)?;
            }
        }
        "writeonly" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "write only", path, line_number)
            {
                builder.set_write_only(parsed, path, line_number)?;
            }
        }
        "usechroot" => {
            if let Some(parsed) =
                apply_boolean_directive(value, true, "use chroot", path, line_number)
            {
                builder.set_use_chroot(parsed, path, line_number)?;
            }
        }
        "numericids" => {
            if let Some(parsed) =
                apply_boolean_directive(value, true, "numeric ids", path, line_number)
            {
                builder.set_numeric_ids(parsed, path, line_number)?;
            }
        }
        "list" => {
            if let Some(parsed) = apply_boolean_directive(value, false, "list", path, line_number) {
                builder.set_listable(parsed, path, line_number)?;
            }
        }
        "fakesuper" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "fake super", path, line_number)
            {
                builder.set_fake_super(parsed, path, line_number)?;
            }
        }
        "mungesymlinks" => {
            if let Some(parsed) =
                apply_boolean_directive(value, true, "munge symlinks", path, line_number)
            {
                builder.set_munge_symlinks(Some(parsed), path, line_number)?;
            }
        }
        "uid" => {
            let uid = parse_numeric_identifier(value).ok_or_else(|| {
                config_parse_error(path, line_number, format!("invalid uid '{value}'"))
            })?;
            builder.set_uid(uid, path, line_number)?;
        }
        "gid" => {
            let gid = parse_numeric_identifier(value).ok_or_else(|| {
                config_parse_error(path, line_number, format!("invalid gid '{value}'"))
            })?;
            builder.set_gid(gid, path, line_number)?;
        }
        "timeout" => {
            let timeout = parse_timeout_seconds(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid timeout '{value}'"),
                )
            })?;
            builder.set_timeout(timeout, path, line_number)?;
        }
        "maxconnections" => {
            let max = parse_max_connections_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid max connections value '{value}'"),
                )
            })?;
            builder.set_max_connections(max, path, line_number)?;
        }
        "incomingchmod" | "incoming-chmod" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'incoming chmod' directive must not be empty",
                ));
            }
            builder.set_incoming_chmod(
                Some(value.to_owned()),
                path,
                line_number,
            )?;
        }
        "outgoingchmod" | "outgoing-chmod" => {
            if value.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "'outgoing chmod' directive must not be empty",
                ));
            }
            builder.set_outgoing_chmod(
                Some(value.to_owned()),
                path,
                line_number,
            )?;
        }
        "maxverbosity" => {
            let parsed = parse_atoi(value);
            builder.set_max_verbosity(parsed, path, line_number)?;
        }
        "ignoreerrors" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "ignore errors", path, line_number)
            {
                builder.set_ignore_errors(parsed, path, line_number)?;
            }
        }
        "ignorenonreadable" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "ignore nonreadable", path, line_number)
            {
                builder.set_ignore_nonreadable(parsed, path, line_number)?;
            }
        }
        "transferlogging" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "transfer logging", path, line_number)
            {
                builder.set_transfer_logging(parsed, path, line_number)?;
            }
        }
        "logformat" => {
            let format = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_log_format(format, path, line_number)?;
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
            builder.set_log_file(resolved, path, line_number)?;
        }
        "dontcompress" => {
            let patterns = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_dont_compress(patterns, path, line_number)?;
        }
        "earlyexec" => {
            let cmd = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_early_exec(cmd, path, line_number)?;
        }
        "pre-xferexec" => {
            let cmd = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_pre_xfer_exec(cmd, path, line_number)?;
        }
        "post-xferexec" => {
            let cmd = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_post_xfer_exec(cmd, path, line_number)?;
        }
        "nameconverter" => {
            let cmd = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_name_converter(cmd, path, line_number)?;
        }
        "tempdir" => {
            let dir = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_temp_dir(dir, path, line_number)?;
        }
        "charset" => {
            let cs = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_charset(cs, path, line_number)?;
        }
        "forwardlookup" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "forward lookup", path, line_number)
            {
                builder.set_forward_lookup(parsed, path, line_number)?;
            }
        }
        "strictmodes" => {
            if let Some(parsed) =
                apply_boolean_directive(value, false, "strict modes", path, line_number)
            {
                builder.set_strict_modes(parsed, path, line_number)?;
            }
        }
        "opennoatime" => {
            if let Some(parsed) =
                apply_boolean_directive(value, true, "open noatime", path, line_number)
            {
                builder.set_open_noatime(parsed, path, line_number)?;
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
            builder.set_exclude_from(resolved, path, line_number)?;
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
            builder.set_include_from(resolved, path, line_number)?;
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
                builder.set_reverse_lookup(parsed, path, line_number)?;
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
            builder.set_lock_file(resolved, path, line_number)?;
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
