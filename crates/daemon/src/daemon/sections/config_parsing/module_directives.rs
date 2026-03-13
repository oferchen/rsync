// Per-module directive parsing.
//
// Handles the `key = value` directives found inside `[module]` sections of
// rsyncd.conf. Each recognized key is dispatched to the corresponding setter
// on `ModuleDefinitionBuilder`.

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
        "hosts allow" => {
            let patterns = parse_host_list(value, path, line_number, "hosts allow")?;
            builder.set_hosts_allow(patterns, path, line_number)?;
        }
        "hosts deny" => {
            let patterns = parse_host_list(value, path, line_number, "hosts deny")?;
            builder.set_hosts_deny(patterns, path, line_number)?;
        }
        "auth users" => {
            let users = parse_auth_user_list(value).map_err(|error| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid 'auth users' directive: {error}"),
                )
            })?;
            builder.set_auth_users(users, path, line_number)?;
        }
        "secrets file" => {
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
        "refuse options" => {
            let options = parse_refuse_option_list(value).map_err(|error| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid 'refuse options' directive: {error}"),
                )
            })?;
            builder.set_refuse_options(options, path, line_number)?;
        }
        "read only" => {
            let parsed = parse_boolean_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid boolean value '{value}' for 'read only'"),
                )
            })?;
            builder.set_read_only(parsed, path, line_number)?;
        }
        "write only" => {
            let parsed = parse_boolean_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid boolean value '{value}' for 'write only'"),
                )
            })?;
            builder.set_write_only(parsed, path, line_number)?;
        }
        "use chroot" => {
            let parsed = parse_boolean_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid boolean value '{value}' for 'use chroot'"),
                )
            })?;
            builder.set_use_chroot(parsed, path, line_number)?;
        }
        "numeric ids" => {
            let parsed = parse_boolean_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid boolean value '{value}' for 'numeric ids'"),
                )
            })?;
            builder.set_numeric_ids(parsed, path, line_number)?;
        }
        "list" => {
            let parsed = parse_boolean_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid boolean value '{value}' for 'list'"),
                )
            })?;
            builder.set_listable(parsed, path, line_number)?;
        }
        "fake super" => {
            let parsed = parse_boolean_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid boolean value '{value}' for 'fake super'"),
                )
            })?;
            builder.set_fake_super(parsed, path, line_number)?;
        }
        "munge symlinks" => {
            let parsed = parse_boolean_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid boolean value '{value}' for 'munge symlinks'"),
                )
            })?;
            builder.set_munge_symlinks(Some(parsed), path, line_number)?;
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
        "max connections" => {
            let max = parse_max_connections_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid max connections value '{value}'"),
                )
            })?;
            builder.set_max_connections(max, path, line_number)?;
        }
        "incoming chmod" | "incoming-chmod" => {
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
        "outgoing chmod" | "outgoing-chmod" => {
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
        "max verbosity" => {
            let parsed: i32 = value.parse().map_err(|_| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid integer value '{value}' for 'max verbosity'"),
                )
            })?;
            builder.set_max_verbosity(parsed, path, line_number)?;
        }
        "ignore errors" => {
            let parsed = parse_boolean_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid boolean value '{value}' for 'ignore errors'"),
                )
            })?;
            builder.set_ignore_errors(parsed, path, line_number)?;
        }
        "ignore nonreadable" => {
            let parsed = parse_boolean_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid boolean value '{value}' for 'ignore nonreadable'"),
                )
            })?;
            builder.set_ignore_nonreadable(parsed, path, line_number)?;
        }
        "transfer logging" => {
            let parsed = parse_boolean_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid boolean value '{value}' for 'transfer logging'"),
                )
            })?;
            builder.set_transfer_logging(parsed, path, line_number)?;
        }
        "log format" => {
            let format = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_log_format(format, path, line_number)?;
        }
        "log file" => {
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
        "dont compress" => {
            let patterns = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_dont_compress(patterns, path, line_number)?;
        }
        "early exec" => {
            let cmd = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_early_exec(cmd, path, line_number)?;
        }
        "pre-xfer exec" => {
            let cmd = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_pre_xfer_exec(cmd, path, line_number)?;
        }
        "post-xfer exec" => {
            let cmd = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_post_xfer_exec(cmd, path, line_number)?;
        }
        "name converter" => {
            let cmd = if value.is_empty() {
                None
            } else {
                Some(value.to_owned())
            };
            builder.set_name_converter(cmd, path, line_number)?;
        }
        "temp dir" => {
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
        "forward lookup" => {
            let parsed = parse_boolean_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid boolean value '{value}' for 'forward lookup'"),
                )
            })?;
            builder.set_forward_lookup(parsed, path, line_number)?;
        }
        "strict modes" => {
            let parsed = parse_boolean_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid boolean value '{value}' for 'strict modes'"),
                )
            })?;
            builder.set_strict_modes(parsed, path, line_number)?;
        }
        "open noatime" => {
            let parsed = parse_boolean_directive(value).ok_or_else(|| {
                config_parse_error(
                    path,
                    line_number,
                    format!("invalid boolean value '{value}' for 'open noatime'"),
                )
            })?;
            builder.set_open_noatime(parsed, path, line_number)?;
        }
        // upstream: daemon-parm.txt - `exclude_from` STRING, default NULL.
        // Loaded via parse_filter_file() in clientserver.c.
        "exclude from" => {
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
        "include from" => {
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
        _ => {
            eprintln!(
                "warning: unknown per-module directive '{}' in '{}' line {}",
                key,
                path.display(),
                line_number
            );
        }
    }
    Ok(())
}
