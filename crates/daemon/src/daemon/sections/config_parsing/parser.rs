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
    let mut parse = ConfigParse::new();
    parse_config_file(path, &mut stack, &mut parse)?;
    parse.vars.into_result(parse.sections)
}

/// The whole-parse state upstream keeps in loadparm.c's file-scope globals.
///
/// upstream: loadparm.c - `Vars` (the global and P_LOCAL default values),
/// `section_list` (every module section, in creation order) and the
/// `bInGlobalSection`/`iSectionIndex` cursor are shared by every file the parse
/// reads. `&merge` keeps writing into all three; `&include` saves and restores
/// only `Vars` around the included file (params.c:include_config).
struct ConfigParse {
    /// upstream `Vars`: the values a section copies when it is created.
    vars: GlobalParseState,
    /// upstream `section_list`: every module section read so far.
    sections: Vec<PendingModule>,
    /// The section parameters are written to; `None` is `bInGlobalSection`.
    current: Option<usize>,
}

impl ConfigParse {
    fn new() -> Self {
        Self {
            vars: GlobalParseState::new(),
            sections: Vec::new(),
            current: None,
        }
    }
}

/// Parses one config file into the shared `parse` state.
///
/// Sections are not finalized here: a string-typed P_LOCAL parameter a section
/// never set falls back to the global value left standing at the end of the
/// whole parse (upstream: loadparm.c:347-348), so only the top-level
/// [`parse_config_modules`] finalizes them.
fn parse_config_file(
    path: &Path,
    stack: &mut Vec<PathBuf>,
    parse: &mut ConfigParse,
) -> Result<(), DaemonError> {
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

                // upstream: params.c:Section() - once the ']' is found and the
                // section is accepted, EatComment() discards the rest of the
                // line, so `[mod] junk` defines `mod`.

                // upstream: loadparm.c:do_section:497-510 - a section named
                // "global" (whitespace/case-insensitive via strwiEQ) returns to
                // the daemon-wide global scope instead of defining a module:
                // bInGlobalSection = True and no module is added, so directives
                // that follow apply as global defaults.
                parse.current = if normalize_param_name(name) == "global" {
                    None
                } else {
                    Some(open_module_section(parse, name, line_number, path))
                };
                continue;
            }

            // upstream: params.c:Parameter() - directives that start with '&'
            // (e.g. `&include /path/to/file.conf`, `&merge /path/to/snippet.inc`)
            // end their name at the first space, tab or '=' and treat a '='
            // after the space as optional. The untrimmed line is scanned so a
            // trailing space still separates an empty value from the name.
            if let Some(rest) = logical_line.trim_start().strip_prefix('&') {
                let Some((name, raw_value)) = rest.split_once([' ', '\t', '=']) else {
                    warn_badly_formed_line(line, path, line_number);
                    continue;
                };
                let raw_value = raw_value.trim_start();
                let value = raw_value.strip_prefix('=').unwrap_or(raw_value).trim();
                // upstream: params.c:parse_directives - only `&include` and
                // `&merge` exist (strcasecmp); any other name fails the load.
                let key = format!("&{}", name.to_ascii_lowercase());
                if key != "&include" && key != "&merge" {
                    return Err(config_parse_error(
                        path,
                        line_number,
                        format!("Unknown directive: &{name}."),
                    ));
                }
                apply_include_directive(parse, &key, value, path, line_number, &canonical, stack)?;
                continue;
            }

            // upstream: params.c:Parameter() - a line that ends before any '='
            // is logged and skipped, while an empty name before the '=' fails
            // the load.
            let Some((raw_key, raw_value)) = line.split_once('=') else {
                warn_badly_formed_line(line, path, line_number);
                continue;
            };
            let key = normalize_param_name(raw_key);
            if key.is_empty() {
                return Err(config_parse_error(
                    path,
                    line_number,
                    "Invalid parameter name in config file.",
                ));
            }
            let value = raw_value.trim();

            if let Some(index) = parse.current {
                apply_module_directive(
                    &mut parse.sections[index].builder,
                    &key,
                    value,
                    path,
                    line_number,
                )?;
                continue;
            }

            apply_global_directive(&mut parse.vars, &key, value, path, line_number, &canonical)?;
        }

        Ok(())
    })();

    stack.pop();
    result
}

/// Reports a config line that has no '=' and is therefore skipped.
///
/// upstream: params.c:Parameter() - "Ignoring badly formed line in config
/// file", after which parsing continues with the next line.
fn warn_badly_formed_line(line: &str, path: &Path, line_number: usize) {
    eprintln!(
        "Ignoring badly formed line in config file: {line} ('{}' line {line_number})",
        path.display()
    );
}

/// A module section, together with the values it copied from `Vars` when its
/// `[name]` header was first seen.
///
/// upstream: loadparm.c:init_section:394-398 - a new section is a copy of
/// `Vars.l` at the moment it is created. Bool-, integer- and enum-typed P_LOCAL
/// values are read from that copy only (FN_LOCAL_BOOL/INTEGER,
/// loadparm.c:351-356). A string-typed one is read from the copy when it is
/// non-NULL and from the final `Vars.l` otherwise (FN_LOCAL_STRING,
/// loadparm.c:347-348) - see `GlobalModuleDefaults::resolve`.
struct PendingModule {
    builder: ModuleDefinitionBuilder,
    /// The config file the `[name]` header was read from, so a validation
    /// failure still names the file that declared the module even though the
    /// section is finalized once the whole config tree has been parsed.
    config_path: PathBuf,
    defaults: CapturedModuleDefaults,
}

/// The global values a module section copied when it was created.
///
/// `module_defaults` holds the P_LOCAL defaults the dispatcher keeps in one bag;
/// the other slots are the P_LOCAL directives the parser tracks separately from
/// that bag (each carries a `ConfigDirectiveOrigin` for diagnostics).
struct CapturedModuleDefaults {
    secrets_file: Option<PathBuf>,
    incoming_chmod: Option<String>,
    outgoing_chmod: Option<String>,
    refuse_options: Option<Vec<String>>,
    use_chroot: Option<bool>,
    module_defaults: GlobalModuleDefaults,
}

impl CapturedModuleDefaults {
    /// Snapshots the values a module section created right now would copy.
    fn capture(vars: &GlobalParseState) -> Self {
        Self {
            secrets_file: vars.global_secrets_file.as_ref().map(|(v, _)| v.clone()),
            incoming_chmod: vars.global_incoming_chmod.as_ref().map(|(v, _)| v.clone()),
            outgoing_chmod: vars.global_outgoing_chmod.as_ref().map(|(v, _)| v.clone()),
            refuse_options: vars.global_refuse_directives.last().map(|(v, _)| v.clone()),
            use_chroot: vars.global_use_chroot.as_ref().map(|(v, _)| *v),
            module_defaults: vars.module_defaults.clone(),
        }
    }
}

/// Returns the index of the module section named `name`, creating it when no
/// earlier header in any file of the parse named it.
///
/// upstream: loadparm.c:add_a_section:439-459 - "it might already exist":
/// getsectionbyname() searches the whole `section_list` with the
/// whitespace- and case-insensitive `strwiEQ`, and a match is returned as-is,
/// so a repeated header re-opens that section instead of adding a second one.
fn open_module_section(
    parse: &mut ConfigParse,
    name: &str,
    line_number: usize,
    config_path: &Path,
) -> usize {
    let folded = normalize_param_name(name);
    if let Some(index) = parse
        .sections
        .iter()
        .rposition(|module| normalize_param_name(&module.builder.name) == folded)
    {
        return index;
    }

    parse.sections.push(PendingModule {
        builder: ModuleDefinitionBuilder::new(name.to_owned(), line_number),
        config_path: config_path.to_path_buf(),
        defaults: CapturedModuleDefaults::capture(&parse.vars),
    });
    parse.sections.len() - 1
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

/// Resolves an operator-supplied daemon path parameter.
///
/// loadparm.c stores every parameter value verbatim, and each consumer opens
/// that string as given, so a relative value is resolved against the daemon's
/// working directory - never against the config file's directory:
///
/// - `log file`: log.c:216-218 `log_init()` takes `logfile_name` from
///   `lp_log_file(module_id)`, and log.c:169 `logfile_open()` hands it straight
///   to `open_no_attacker_symlinks()`.
/// - `pid file`: clientserver.c:1584 `create_pid_file()` opens
///   `lp_pid_file()`'s value directly.
/// - `exclude from` / `include from`: clientserver.c:934-951
///   `parse_filter_file()` opens the stored value before change_dir runs, so
///   a relative path resolves against the daemon's launch cwd.
///
/// These sites share this function because they must agree. `log file`'s
/// module and global halves are read through one `lp_log_file()` accessor, so
/// a rule applied to one and not the other would open two different files for
/// one config line; `pid file` obeys the same storage rule, and rebasing it
/// creates a pid file the operator cannot find.
fn daemon_parameter_path(value: &str) -> PathBuf {
    PathBuf::from(value)
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
