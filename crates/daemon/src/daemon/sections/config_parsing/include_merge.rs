// Include directive handling and result merging.
//
// Processes the global `include = <path>` directive by recursively parsing the
// referenced file and merging its results into the current global state,
// detecting duplicate conflicting directives across files.

/// Processes a global `include` directive, recursively parsing the target file
/// and merging its results into the current parse state.
///
/// `directive` is the literal directive name (`include`, `&include`, or
/// `&merge`) so any error message names the syntax the user actually wrote.
fn apply_include_directive(
    state: &mut GlobalParseState,
    directive: &str,
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
            format!("'{directive}' directive must not be empty"),
        ));
    }

    let include_path = resolve_config_relative_path(canonical, trimmed);

    // upstream: params.c:parse_directives - `&include` maps to
    // include_config(val, 1) (a private global scope, `]push`/`]pop`) while
    // `&merge` maps to include_config(val, 0) (the current scope is shared).
    // The legacy `include =` synonym follows `&include`.
    let manage_globals = directive != "&merge";

    // upstream: params.c:include_config - when the target is a directory, every
    // matching entry is pulled in: "*.conf" for `&include`, "*.inc" for
    // `&merge`, processed in sorted (strcmp) order.
    if fs::metadata(&include_path)
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
    {
        let suffix = if manage_globals { ".conf" } else { ".inc" };
        let mut entries = Vec::new();
        let dir = fs::read_dir(&include_path)
            .map_err(|error| config_io_error("read", &include_path, error))?;
        for entry in dir {
            let entry = entry.map_err(|error| config_io_error("read", &include_path, error))?;
            if entry.file_name().to_string_lossy().ends_with(suffix) {
                entries.push(entry.path());
            }
        }
        entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        for entry in entries {
            include_config_file(
                state,
                directive,
                &entry,
                path,
                line_number,
                stack,
                manage_globals,
            )?;
        }
        return Ok(());
    }

    include_config_file(
        state,
        directive,
        &include_path,
        path,
        line_number,
        stack,
        manage_globals,
    )
}

/// Parses a single included config file and folds its results into `state`.
///
/// `manage_globals` selects the upstream scope semantics: `&include`
/// (`manage_globals = true`) runs under `]push`/`]pop`, so only the included
/// modules survive and the file's global directives are discarded; `&merge`
/// shares the scope and merges the included globals into the current state.
fn include_config_file(
    state: &mut GlobalParseState,
    directive: &str,
    include_path: &Path,
    path: &Path,
    line_number: usize,
    stack: &mut Vec<PathBuf>,
    manage_globals: bool,
) -> Result<(), DaemonError> {
    // upstream: params.c:include_config - the recursive parse continues against
    // the parent's globals so modules declared in the included file inherit the
    // parent's P_LOCAL defaults (use chroot, hosts allow, secrets file, ...),
    // matching the shared `Vars` that `]push` copies (not resets).
    let included = parse_config_modules_inner(include_path, stack, Some(state)).map_err(|error| {
        // Wrap inner failures so the user sees both the directive site that
        // triggered the include and the underlying parse error from the
        // included file. Missing-file and recursive-include errors already
        // name the offending path; this wrap adds the parent line context.
        let display = include_path.display();
        config_parse_error(
            path,
            line_number,
            format!("failed to process '{directive} {display}': {error}"),
        )
    })?;

    if manage_globals {
        // `&include`: `]pop` restores the parent's globals afterwards, so the
        // included file's global directives do not leak back into the caller.
        // Only the modules (upstream's section list) survive.
        state.modules.extend(included.modules);
        return Ok(());
    }

    // `&merge`: the scope is shared, so the included file's globals merge into
    // the current state (later duplicate detection matches the shared `Vars`).
    merge_included_globals(state, included)
}

/// Merges an included file's modules and global directives into `state`.
///
/// Used for `&merge` (and directory entries pulled in via `&merge`), where the
/// included file shares the current scope.
fn merge_included_globals(
    state: &mut GlobalParseState,
    included: ParsedConfigModules,
) -> Result<(), DaemonError> {
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
