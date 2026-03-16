//! Main config file parser.
//!
//! Entry point for rsyncd.conf parsing with recursive include detection,
//! line-by-line dispatch to module or global directive handlers, and
//! final assembly of the parsed result.

/// Parses the `rsyncd.conf` at `path` into module definitions and global settings.
pub(crate) fn parse_config_modules(path: &Path) -> Result<ParsedConfigModules, DaemonError> {
    let mut stack = Vec::new();
    parse_config_modules_inner(path, &mut stack)
}

fn parse_config_modules_inner(
    path: &Path,
    stack: &mut Vec<PathBuf>,
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

    let mut state = GlobalParseState::new();
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

            let (key, value) = line.split_once('=').ok_or_else(|| {
                config_parse_error(path, line_number, "expected 'key = value' directive")
            })?;
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();

            if let Some(builder) = current.as_mut() {
                apply_module_directive(builder, &key, value, path, line_number, &canonical)?;
                continue;
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
fn finish_module_builder(
    builder: ModuleDefinitionBuilder,
    path: &Path,
    state: &GlobalParseState,
) -> Result<ModuleDefinition, DaemonError> {
    let default_secrets = state.global_secrets_file.as_ref().map(|(p, _)| p.as_path());
    let default_incoming = state
        .global_incoming_chmod
        .as_ref()
        .map(|(value, _)| value.as_str());
    let default_outgoing = state
        .global_outgoing_chmod
        .as_ref()
        .map(|(value, _)| value.as_str());
    let default_use_chroot = state.global_use_chroot.as_ref().map(|(v, _)| *v);
    builder.finish(
        path,
        default_secrets,
        default_incoming,
        default_outgoing,
        default_use_chroot,
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
