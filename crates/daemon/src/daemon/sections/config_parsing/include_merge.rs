// `&include` / `&merge` directive handling.
//
// Recursively parses the referenced file (or every matching file in a
// referenced directory) into the shared parse state, saving and restoring the
// global values around an `&include` the way upstream's `]push`/`]pop` does.

/// Processes an `&include` or `&merge` directive, parsing the target into
/// `parse`.
///
/// `directive` is the normalized directive name (`&include` or `&merge`) so any
/// error message names the syntax the user actually wrote.
///
/// upstream: params.c:parse_directives - `&include` maps to
/// include_config(val, 1) and `&merge` to include_config(val, 0).
fn apply_include_directive(
    parse: &mut ConfigParse,
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
    let manage_globals = directive == "&include";

    // upstream: params.c:include_config - when the target is a directory, every
    // matching entry is pulled in: "*.conf" for `&include`, "*.inc" for
    // `&merge`, processed in sorted (strcmp) order.
    let files = if fs::metadata(&include_path)
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
        // upstream: params.c:include_config - an empty directory returns
        // before any `]push`, leaving the section cursor untouched.
        if entries.is_empty() {
            return Ok(());
        }
        entries
    } else {
        vec![include_path]
    };

    if !manage_globals {
        // `&merge`: no `]push`/`]pop`. The merged file writes straight into the
        // shared `Vars`, section list and section cursor, so a `&merge` inside
        // `[mod]` keeps applying to `mod` until the merged file opens another
        // section, and every value it sets stays in force afterwards.
        for file in &files {
            include_config_file(parse, directive, file, path, line_number, stack)?;
        }
        return Ok(());
    }

    // `&include`: upstream: loadparm.c:do_section:598-614 - `]push` saves
    // `Vars`, `]reset` restores it before each later directory entry, and
    // `]pop` restores it at the end. Each of them also sets bInGlobalSection,
    // so the included file starts in, and the includer resumes in, the global
    // section. Sections the file added survive; its global values do not.
    let saved = parse.vars.clone();
    let mut result = Ok(());
    for (index, file) in files.iter().enumerate() {
        if index > 0 {
            parse.vars = saved.clone();
        }
        parse.current = None;
        result = include_config_file(parse, directive, file, path, line_number, stack);
        if result.is_err() {
            break;
        }
    }
    parse.vars = saved;
    parse.current = None;
    result
}

/// Parses a single included config file into `parse`, naming the directive
/// site in any error.
fn include_config_file(
    parse: &mut ConfigParse,
    directive: &str,
    include_path: &Path,
    path: &Path,
    line_number: usize,
    stack: &mut Vec<PathBuf>,
) -> Result<(), DaemonError> {
    parse_config_file(include_path, stack, parse).map_err(|error| {
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
    })
}
