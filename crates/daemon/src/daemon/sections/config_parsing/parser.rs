// Main config file parser.
//
// Entry point for rsyncd.conf parsing with recursive include detection,
// line-by-line dispatch to module or global directive handlers, and
// final assembly of the parsed result.

/// Parses the `rsyncd.conf` at `path` into module definitions and global settings.
pub(crate) fn parse_config_modules(path: &Path) -> Result<ParsedConfigModules, DaemonError> {
    let mut stack = Vec::new();
    parse_config_modules_inner(path, &mut stack, None)
}

fn parse_config_modules_inner(
    path: &Path,
    stack: &mut Vec<PathBuf>,
    inherited: Option<&GlobalParseState>,
) -> Result<ParsedConfigModules, DaemonError> {
    let canonical = path
        .canonicalize()
        .map_err(|error| config_io_error("read", path, error))?;

    if stack.iter().any(|seen| seen == &canonical) {
        return Err(config_parse_error(
            path,
            0,
            format!("recursive include detected for '{}'", canonical.display()),
        ));
    }

    let contents = fs::read_to_string(&canonical)
        .map_err(|error| config_io_error("read", &canonical, error))?;
    stack.push(canonical.clone());

    // upstream: loadparm.c::lp_load() &include handling - the included file
    // continues parsing against the shared `Vars` block, so modules declared
    // there inherit the parent's P_LOCAL defaults (use chroot, hosts allow,
    // secrets file, ...). Seed the child state from the parent so
    // `finish_module_builder` resolves defaults the same way as upstream.
    let mut state = match inherited {
        Some(parent) => GlobalParseState::inherited_from(parent),
        None => GlobalParseState::new(),
    };
    let mut current: Option<ModuleDefinitionBuilder> = None;

    let result = (|| -> Result<ParsedConfigModules, DaemonError> {
        for (index, raw_line) in contents.lines().enumerate() {
            let line_number = index + 1;
            let line = raw_line.trim();

            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }

            if line.starts_with('[') {
                let end = line.find(']').ok_or_else(|| {
                    config_parse_error(path, line_number, "unterminated module header")
                })?;
                let name = line[1..end].trim();

                if name.is_empty() {
                    return Err(config_parse_error(
                        path,
                        line_number,
                        "module name must be non-empty",
                    ));
                }

                ensure_valid_module_name(name)
                    .map_err(|msg| config_parse_error(path, line_number, msg))?;

                let trailing = line[end + 1..].trim();
                if !trailing.is_empty() && !trailing.starts_with('#') && !trailing.starts_with(';')
                {
                    return Err(config_parse_error(
                        path,
                        line_number,
                        "unexpected characters after module header",
                    ));
                }

                if let Some(builder) = current.take() {
                    state.modules.push(finish_module_builder(builder, path, &state)?);
                }

                current = Some(ModuleDefinitionBuilder::new(name.to_owned(), line_number));
                continue;
            }

            // upstream: params.c:Parameter() - directives that start with '&'
            // (e.g. `&include /path/to/file.conf`, `&merge /path/to/snippet.inc`)
            // use whitespace as the name/value separator and treat a following
            // '=' as optional. Detect them before the regular `key = value`
            // dispatch so the file inclusion syntax is accepted as-written.
            let (key, value) = if let Some(rest) = line.strip_prefix('&') {
                let (name, raw_value) = rest
                    .split_once(|c: char| c.is_whitespace() || c == '=')
                    .ok_or_else(|| {
                        config_parse_error(
                            path,
                            line_number,
                            "expected '&directive value' or '&directive = value'",
                        )
                    })?;
                let trimmed_value = raw_value
                    .trim_start()
                    .strip_prefix('=')
                    .unwrap_or(raw_value);
                (
                    format!("&{}", name.trim().to_ascii_lowercase()),
                    trimmed_value.trim(),
                )
            } else {
                let (raw_key, raw_value) = line.split_once('=').ok_or_else(|| {
                    config_parse_error(path, line_number, "expected 'key = value' directive")
                })?;
                (raw_key.trim().to_ascii_lowercase(), raw_value.trim())
            };

            // upstream: params.c:Parse() - the `&include`/`&merge` directives
            // are dispatched from the top-level switch and apply to the global
            // configuration regardless of any open module section. Forward
            // them to the global-directive handler rather than the per-module
            // setter so the recursive include works after a `[name]` line.
            let is_amp_directive = key.starts_with('&');
            if !is_amp_directive
                && let Some(builder) = current.as_mut()
            {
                apply_module_directive(builder, &key, value, path, line_number, &canonical)?;
                continue;
            }

            // Finish the open module before recursing into an included file so
            // the parent module is recorded ahead of any modules pulled in by
            // `&include`/`&merge`, matching upstream's declaration order.
            if is_amp_directive
                && let Some(builder) = current.take()
            {
                state.modules.push(finish_module_builder(builder, path, &state)?);
            }

            apply_global_directive(&mut state, &key, value, path, line_number, &canonical, stack)?;
        }

        if let Some(builder) = current {
            state.modules.push(finish_module_builder(builder, path, &state)?);
        }

        Ok(state.into_result())
    })();

    stack.pop();
    result
}

/// Finalizes a module builder using the current global defaults.
///
/// Explicit globals declared in the same file win over inherited values
/// from a parent file (set when this state is the body of an
/// `&include`/`&merge` target), matching upstream's shared-`Vars`
/// semantics where the includer's defaults serve as fallbacks until the
/// included file overrides them.
fn finish_module_builder(
    builder: ModuleDefinitionBuilder,
    path: &Path,
    state: &GlobalParseState,
) -> Result<ModuleDefinition, DaemonError> {
    let default_secrets = state
        .global_secrets_file
        .as_ref()
        .map(|(p, _)| p.as_path())
        .or_else(|| state.inherited_secrets_file.as_deref());
    let default_incoming = state
        .global_incoming_chmod
        .as_ref()
        .map(|(value, _)| value.as_str())
        .or_else(|| state.inherited_incoming_chmod.as_deref());
    let default_outgoing = state
        .global_outgoing_chmod
        .as_ref()
        .map(|(value, _)| value.as_str())
        .or_else(|| state.inherited_outgoing_chmod.as_deref());
    let default_use_chroot = state
        .global_use_chroot
        .as_ref()
        .map(|(v, _)| *v)
        .or(state.inherited_use_chroot);
    builder.finish(
        path,
        default_secrets,
        default_incoming,
        default_outgoing,
        default_use_chroot,
        &state.module_defaults,
    )
}

fn resolve_config_relative_path(config_path: &Path, value: &str) -> PathBuf {
    let candidate = Path::new(value);
    if candidate.is_absolute() {
        return candidate.to_path_buf();
    }

    if let Some(parent) = config_path.parent() {
        parent.join(candidate)
    } else {
        candidate.to_path_buf()
    }
}
