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

/// Collapses a `[section]` header name the way upstream's config scanner does:
/// leading and trailing whitespace is dropped and every internal whitespace run
/// becomes exactly one space (0x20).
///
/// upstream: params.c:Section() - `EatWhitespace()` skips the run right after
/// the '[', the `end`/`i` split drops the run before the ']', and the
/// `isspace(c)` arm of the character switch writes a single ' ' per whitespace
/// region before eating the rest of that region. `[my   module]` therefore
/// defines the module named `my module`, which upstream loads without any
/// warning and serves under exactly that single-spaced name.
fn collapse_section_name(name: &str) -> String {
    // upstream: params.c:Section() tests C `isspace()` on single bytes, which
    // covers the same ASCII set as `normalize_param_name` (including the
    // vertical tab that `char::is_ascii_whitespace` omits) and never matches a
    // multi-byte character.
    name.split([' ', '\t', '\n', '\x0B', '\x0C', '\r'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<&str>>()
        .join(" ")
}

/// Parses the `rsyncd.conf` at `path` into module definitions and global settings.
pub(crate) fn parse_config_modules(path: &Path) -> Result<ParsedConfigModules, DaemonError> {
    let mut stack = Vec::new();
    parse_config_modules_inner(path, &mut stack, None)?.into_result()
}

/// Parses one config file, returning the raw parse state with its module
/// sections still unfinalized.
///
/// Only the top-level [`parse_config_modules`] finalizes them, because a
/// string-typed P_LOCAL parameter resolves its default against the global state
/// left standing at the end of the whole parse (upstream: loadparm.c:347-348).
fn parse_config_modules_inner(
    path: &Path,
    stack: &mut Vec<PathBuf>,
    inherited: Option<&GlobalParseState>,
) -> Result<GlobalParseState, DaemonError> {
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

    // upstream: params.c:586 opens the config file with
    // `open_no_attacker_symlinks()` - the path as *given*, walked component by
    // component. Read `path`, not `canonical`: `canonicalize()` follows every
    // symlink, so reading the resolved path is precisely the redirect this
    // guards against. `canonical` stays the cycle-detection key only.
    let contents = crate::daemon::operator_file::read_to_string(path)
        .map_err(|error| config_io_error("read", &canonical, error))?;
    stack.push(canonical.clone());

    // upstream: loadparm.c::lp_load() &include handling - the included file
    // continues parsing against the shared `Vars` block, so modules declared
    // there inherit the parent's P_LOCAL defaults (use chroot, hosts allow,
    // secrets file, ...). Seed the child state from the parent so
    // `PendingModule` resolves defaults the same way as upstream.
    let mut state = match inherited {
        Some(parent) => GlobalParseState::inherited_from(parent),
        None => GlobalParseState::new(),
    };
    let mut pending: Vec<PendingModule> = Vec::new();
    let mut current: Option<usize> = None;

    let result = (|| -> Result<(), DaemonError> {
        for (line_number, logical_line) in logical_config_lines(&contents) {
            let line = logical_line.trim();

            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }

            if line.starts_with('[') {
                let end = line.find(']').ok_or_else(|| {
                    config_parse_error(path, line_number, "unterminated module header")
                })?;
                let collapsed = collapse_section_name(&line[1..end]);
                let name = collapsed.as_str();

                if name.is_empty() {
                    return Err(config_parse_error(
                        path,
                        line_number,
                        "module name must be non-empty",
                    ));
                }

                ensure_valid_section_name(name)
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

                current = None;

                // upstream: loadparm.c:do_section:497-510 - a section named
                // "global" (whitespace/case-insensitive via strwiEQ) returns to
                // the daemon-wide global scope instead of defining a module:
                // bInGlobalSection = True and no module is added, so directives
                // that follow apply as global defaults. Leaving `current` as None
                // routes them through the global dispatch path, exactly as the
                // directives before the first `[module]` header are handled.
                if normalize_param_name(name) == "global" {
                    continue;
                }

                // upstream: loadparm.c:add_a_section:320-339 - "it might already
                // exist": a repeated `[name]` header re-opens the section that
                // header already created instead of adding a second one, so the
                // directives that follow merge into the existing module.
                current = Some(open_module_section(
                    &mut pending,
                    name,
                    line_number,
                    path,
                    &state,
                ));
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
            if !is_amp_directive && let Some(index) = current {
                apply_module_directive(
                    &mut pending[index].builder,
                    &key,
                    value,
                    path,
                    line_number,
                    &canonical,
                )?;
                continue;
            }

            // Finish the open modules before recursing into an included file so
            // they are recorded ahead of any modules pulled in by
            // `&include`/`&merge`, matching upstream's declaration order.
            if is_amp_directive {
                current = None;
                flush_pending_modules(&mut pending, &mut state);
            }

            apply_global_directive(
                &mut state,
                &key,
                value,
                path,
                line_number,
                &canonical,
                stack,
            )?;
        }

        flush_pending_modules(&mut pending, &mut state);

        Ok(())
    })();

    stack.pop();
    result.map(|()| state)
}

/// A module section that has been opened but not yet finalized, together with
/// the global defaults captured when its `[name]` header was first seen.
///
/// upstream: loadparm.c:add_a_section:329 - a new section is initialized from
/// `sDefault`, the running snapshot of the P_LOCAL defaults set in the global
/// section. Later global directives never rewrite a section that already
/// exists, so bool- and integer-typed P_LOCAL defaults are pinned here at
/// creation time. String-typed ones are not: they fall back to the live
/// `Vars.l` at access time, so they are resolved from the final global state
/// instead - see `GlobalModuleDefaults::resolve`.
struct PendingModule {
    builder: ModuleDefinitionBuilder,
    /// The config file the `[name]` header was read from, so a validation
    /// failure still names the file that declared the module even though the
    /// section is finalized once the whole config tree has been parsed.
    config_path: PathBuf,
    defaults: CapturedModuleDefaults,
}

/// The global defaults a module section inherits, snapshotted when the section
/// is created.
///
/// Explicit globals declared in the same file win over inherited values from a
/// parent file (set when the parse state is the body of an `&include`/`&merge`
/// target), matching upstream's shared-`Vars` semantics where the includer's
/// defaults serve as fallbacks until the included file overrides them.
///
/// `module_defaults` supplies only the bool- and integer-typed P_LOCAL
/// defaults; the string-typed ones in that bag are read from the final global
/// state instead, via `GlobalModuleDefaults::resolve`. The four slots below are
/// the directives the parser tracks separately from that bag (each carries a
/// `ConfigDirectiveOrigin` for diagnostics) and stay creation-time here.
struct CapturedModuleDefaults {
    secrets_file: Option<PathBuf>,
    incoming_chmod: Option<String>,
    outgoing_chmod: Option<String>,
    use_chroot: Option<bool>,
    module_defaults: GlobalModuleDefaults,
}

impl CapturedModuleDefaults {
    /// Snapshots the defaults a module section created right now would inherit.
    fn capture(state: &GlobalParseState) -> Self {
        Self {
            secrets_file: state
                .global_secrets_file
                .as_ref()
                .map(|(value, _)| value.clone())
                .or_else(|| state.inherited_secrets_file.clone()),
            incoming_chmod: state
                .global_incoming_chmod
                .as_ref()
                .map(|(value, _)| value.clone())
                .or_else(|| state.inherited_incoming_chmod.clone()),
            outgoing_chmod: state
                .global_outgoing_chmod
                .as_ref()
                .map(|(value, _)| value.clone())
                .or_else(|| state.inherited_outgoing_chmod.clone()),
            use_chroot: state
                .global_use_chroot
                .as_ref()
                .map(|(value, _)| *value)
                .or(state.inherited_use_chroot),
            module_defaults: state.module_defaults.clone(),
        }
    }
}

/// Returns the index of the open module section named `name`, creating it when
/// this is the first `[name]` header in the file.
///
/// upstream: loadparm.c:add_a_section:320-339 - "it might already exist": the
/// existing section index is returned instead of a second section being added,
/// so a repeated header merges rather than conflicting.
fn open_module_section(
    pending: &mut Vec<PendingModule>,
    name: &str,
    line_number: usize,
    config_path: &Path,
    state: &GlobalParseState,
) -> usize {
    if let Some(index) = pending
        .iter()
        .position(|module| module.builder.name == name)
    {
        return index;
    }

    pending.push(PendingModule {
        builder: ModuleDefinitionBuilder::new(name.to_owned(), line_number),
        config_path: config_path.to_path_buf(),
        defaults: CapturedModuleDefaults::capture(state),
    });
    pending.len() - 1
}

/// Closes every open module section in declaration order and records it on the
/// parse state.
///
/// The sections are not finalized here: a string-typed P_LOCAL default is only
/// looked up once the whole config tree has been read, so finalization happens
/// in [`GlobalParseState::into_result`] at the top level of the parse.
fn flush_pending_modules(pending: &mut Vec<PendingModule>, state: &mut GlobalParseState) {
    state.modules.append(pending);
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
