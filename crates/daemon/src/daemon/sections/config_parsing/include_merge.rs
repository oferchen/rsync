// Include directive handling and result merging.
//
// Processes the global `include = <path>` directive by recursively parsing the
// referenced file and merging its results into the current global state,
// detecting duplicate conflicting directives across files.

/// Processes a global `include` directive, recursively parsing the target file
/// and merging its results into the current parse state.
fn apply_include_directive(
    state: &mut GlobalParseState,
    value: &str,
    path: &Path,
    line_number: usize,
    canonical: &Path,
    stack: &mut Vec<PathBuf>,
) -> Result<(), DaemonError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(config_parse_error(
            path,
            line_number,
            "'include' directive must not be empty",
        ));
    }

    let include_path = resolve_config_relative_path(canonical, trimmed);
    let included = parse_config_modules_inner(&include_path, stack)?;

    if !included.modules.is_empty() {
        state.modules.extend(included.modules);
    }

    if !included.motd_lines.is_empty() {
        state.motd_lines.extend(included.motd_lines);
    }

    if !included.global_refuse_options.is_empty() {
        state.global_refuse_directives.extend(included.global_refuse_options);
    }

    merge_optional_directive(
        &mut state.global_bwlimit,
        included.global_bandwidth_limit,
        "bwlimit",
    )?;

    merge_optional_directive(
        &mut state.global_secrets_file,
        included.global_secrets_file,
        "secrets file",
    )?;

    merge_optional_directive(
        &mut state.global_incoming_chmod,
        included.global_incoming_chmod,
        "incoming chmod",
    )?;

    merge_optional_directive(
        &mut state.global_outgoing_chmod,
        included.global_outgoing_chmod,
        "outgoing chmod",
    )?;

    merge_optional_directive(
        &mut state.bind_address,
        included.bind_address,
        "address",
    )?;

    merge_optional_directive(
        &mut state.daemon_uid,
        included.daemon_uid,
        "uid",
    )?;

    merge_optional_directive(
        &mut state.daemon_gid,
        included.daemon_gid,
        "gid",
    )?;

    merge_optional_directive(
        &mut state.listen_backlog,
        included.listen_backlog,
        "listen backlog",
    )?;

    merge_optional_directive(
        &mut state.socket_options,
        included.socket_options,
        "socket options",
    )?;

    merge_optional_directive(
        &mut state.proxy_protocol,
        included.proxy_protocol,
        "proxy protocol",
    )?;

    if let Some((port_val, origin)) = included.rsync_port {
        if let Some((existing, existing_origin)) = &state.rsync_port {
            if *existing != port_val {
                let existing_line = existing_origin.line;
                return Err(config_parse_error(
                    &origin.path,
                    origin.line,
                    format!(
                        "conflicting 'port' directive in global section (previously defined as {existing} on line {existing_line})"
                    ),
                ));
            }
        } else {
            state.rsync_port = Some((port_val, origin));
        }
    }

    merge_optional_directive(
        &mut state.daemon_chroot,
        included.daemon_chroot,
        "daemon chroot",
    )?;

    Ok(())
}

/// Merges an optional directive from an included file into the current state.
///
/// If the target slot is empty, the included value is adopted. If both the
/// target and included value are present but differ, a duplicate error is
/// reported using the directive name.
fn merge_optional_directive<T: PartialEq>(
    target: &mut Option<(T, ConfigDirectiveOrigin)>,
    incoming: Option<(T, ConfigDirectiveOrigin)>,
    directive_name: &str,
) -> Result<(), DaemonError> {
    if let Some((new_val, origin)) = incoming {
        if let Some((existing, existing_origin)) = target.as_ref() {
            if existing != &new_val {
                let existing_line = existing_origin.line;
                return Err(config_parse_error(
                    &origin.path,
                    origin.line,
                    format!(
                        "duplicate '{directive_name}' directive in global section (previously defined on line {existing_line})"
                    ),
                ));
            }
        } else {
            *target = Some((new_val, origin));
        }
    }
    Ok(())
}
