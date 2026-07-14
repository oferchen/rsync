// Main config file parser.
//
// Entry point for rsyncd.conf parsing with recursive include detection,
// line-by-line dispatch to module or global directive handlers, and
// final assembly of the parsed result.

/// Folds a directive name into the canonical form used to match it against a
/// known parameter, mirroring upstream's whitespace- and case-insensitive
/// comparison.
///
/// Upstream compares a configuration parameter name against the `parm_table`
/// labels with `strwiEQ` (loadparm.c:282), which walks both strings skipping
/// every `isSpace()` character (itypes.h:37) and comparing the remaining
/// characters case-insensitively via `toUpper()` (itypes.h:67). Two names are
/// therefore equal exactly when they are equal after removing all whitespace
/// and lowercasing, so `read only`, `readonly`, `Read Only`, and `read<TAB>only`
/// all resolve to the same parameter (loadparm.c:344 map_parameter).
///
/// Note that the labels themselves are generated with underscores rewritten to
/// spaces (daemon-parm.awk `gsub(/_/, " ", pubname)`), while `strwiEQ` skips
/// only whitespace - never underscores. An underscore is thus significant:
/// `read_only` does NOT match the `read only` parameter upstream, and this
/// helper preserves that by folding only whitespace.
fn normalize_param_name(name: &str) -> String {
    // upstream: itypes.h:37 isSpace() == C isspace(), which also matches the
    // vertical tab that Rust's char::is_ascii_whitespace omits. toUpper()
    // (itypes.h:67) is ASCII-only, so fold case with to_ascii_lowercase.
    name.chars()
        .filter(|c| !matches!(c, ' ' | '\t' | '\n' | '\x0B' | '\x0C' | '\r'))
        .collect::<String>()
        .to_ascii_lowercase()
}

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
        for (line_number, logical_line) in logical_config_lines(&contents) {
            let line = logical_line.trim();

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
                    format!("&{}", normalize_param_name(name)),
                    trimmed_value.trim(),
                )
            } else {
                let (raw_key, raw_value) = line.split_once('=').ok_or_else(|| {
                    config_parse_error(path, line_number, "expected 'key = value' directive")
                })?;
                (normalize_param_name(raw_key), raw_value.trim())
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
        .or(state.inherited_secrets_file.as_deref());
    let default_incoming = state
        .global_incoming_chmod
        .as_ref()
        .map(|(value, _)| value.as_str())
        .or(state.inherited_incoming_chmod.as_deref());
    let default_outgoing = state
        .global_outgoing_chmod
        .as_ref()
        .map(|(value, _)| value.as_str())
        .or(state.inherited_outgoing_chmod.as_deref());
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

/// Splits `contents` into logical config lines, joining backslash-continued
/// physical lines into one.
///
/// upstream: params.c:Continuation() - a physical line whose last
/// non-whitespace character is a backslash continues onto the following line;
/// the backslash and the newline are removed and the two lines are joined into
/// a single logical line. Comment (`#`/`;`) and blank lines are emitted
/// verbatim because upstream consumes them with EatComment/EatWhitespace,
/// neither of which scans for the continuation character. Each logical line is
/// paired with the 1-based number of its first physical line so diagnostics
/// keep pointing at the directive's start.
fn logical_config_lines(contents: &str) -> Vec<(usize, String)> {
    let physical: Vec<&str> = contents.lines().collect();
    let mut logical = Vec::with_capacity(physical.len());
    let mut index = 0;

    while index < physical.len() {
        let start_line = index + 1;
        let first = physical[index];
        index += 1;

        let leading = first.trim_start();
        if leading.is_empty() || leading.starts_with('#') || leading.starts_with(';') {
            logical.push((start_line, first.to_owned()));
            continue;
        }

        let mut joined = first.to_owned();
        while let Some(offset) = continuation_offset(&joined) {
            // Drop the trailing backslash (and any whitespace after it),
            // matching upstream which resumes writing over the '\\' position.
            joined.truncate(offset);
            if index >= physical.len() {
                break;
            }
            joined.push_str(physical[index]);
            index += 1;
        }
        logical.push((start_line, joined));
    }

    logical
}

/// Returns the byte offset of a trailing line-continuation backslash when the
/// last non-whitespace character of `line` is a `\\`.
///
/// upstream: params.c:Continuation() - scans backwards past trailing
/// whitespace and reports the offset of the `\\` (or -1 when it is absent).
fn continuation_offset(line: &str) -> Option<usize> {
    let trimmed = line.trim_end();
    trimmed.ends_with('\\').then(|| trimmed.len() - 1)
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
