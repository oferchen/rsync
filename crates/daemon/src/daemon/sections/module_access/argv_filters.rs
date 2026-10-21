// Filter options a client passes in the daemon's server argv.
//
// An upstream client never sends filter rules in the argv on its own; they
// travel on the filter list (`exclude.c:1944`). `-M` / `--remote-option`
// hands any option to the daemon's `parse_arguments()`, which applies these
// exactly as a command line would, so each one is a peer-chosen rule and the
// `-from` spellings are peer-chosen file opens.

/// The message upstream sends when a client argv filter option names a file
/// the daemon's filter hides.
///
/// upstream: `options.c:2684-2686`.
const ARGV_FILTER_OPTIONS_REJECTED: &str = "Your options have been rejected by the server.";

/// One filter option from the client's server argv, in argv order.
///
/// upstream: `options.c:1620-1656` and the `-F` arm at `options.c:1718-1727`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArgvFilter<'a> {
    /// `--filter=RULE` / `-f RULE`: one rule in the full grammar.
    Rule(&'a str),
    /// `--exclude=PATTERN`, read under `XFLG_OLD_PREFIXES`.
    Exclude(&'a str),
    /// `--include=PATTERN`, read under `XFLG_OLD_PREFIXES`.
    Include(&'a str),
    /// `--exclude-from=FILE`.
    ExcludeFrom(&'a str),
    /// `--include-from=FILE`.
    IncludeFrom(&'a str),
    /// One `-F`.
    RsyncFilter,
}

/// Why a client argv filter option ends the session.
#[derive(Debug)]
enum ArgvFilterRefusal {
    /// A `-from` file the daemon's filter hides, or an empty `-from` name.
    ///
    /// upstream: `options.c:1636-1652` jumps to `options_rejected`.
    Rejected,
    /// A malformed rule or an unreadable file.
    ///
    /// upstream: `filter_rule_err()` exits `RERR_SYNTAX` (`exclude.c:1130`);
    /// `XFLG_FATAL_ERRORS` makes a failed open exit `RERR_FILEIO`
    /// (`exclude.c:1714-1719`).
    Fatal(io::Error),
}

/// A `-from` or `merge` file the daemon could not open.
///
/// Both an open failure and a malformed rule travel as [`io::Error`]; this
/// inner type keeps them apart, because upstream exits `RERR_FILEIO` for the
/// first (`exclude.c:1714-1719`) and `RERR_SYNTAX` for the second
/// (`exclude.c:136`).
#[derive(Debug)]
struct FilterFileOpenFailure(String);

impl fmt::Display for FilterFileOpenFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FilterFileOpenFailure {}

/// The code the daemon exits with for an [`ArgvFilterRefusal::Fatal`] error.
fn fatal_exit_code(err: &io::Error) -> ExitCode {
    if err
        .get_ref()
        .is_some_and(|inner| inner.is::<FilterFileOpenFailure>())
    {
        ExitCode::FileIo
    } else {
        ExitCode::Syntax
    }
}

/// Collects the filter options that precede the `.` operand separator.
///
/// Both spellings popt accepts are recognised: `--opt=VALUE` and `--opt VALUE`
/// (and `-fVALUE` / `-f VALUE`). `-FF` counts as two `-F`.
fn collect_argv_filters(client_args: &[String]) -> Vec<ArgvFilter<'_>> {
    let end = client_args
        .iter()
        .position(|arg| arg == ".")
        .unwrap_or(client_args.len());
    let mut options = client_args[..end].iter().map(String::as_str);
    let mut filters = Vec::new();
    while let Some(arg) = options.next() {
        if let Some(make) = separate_value_filter(arg) {
            if let Some(value) = options.next() {
                filters.push(make(value));
            }
        } else if let Some(filter) = joined_value_filter(arg) {
            filters.push(filter);
        } else if arg == "-F" {
            filters.push(ArgvFilter::RsyncFilter);
        } else if arg == "-FF" {
            filters.extend([ArgvFilter::RsyncFilter, ArgvFilter::RsyncFilter]);
        }
    }
    filters
}

/// The filter an option spelled `--opt VALUE` builds from its next argument.
fn separate_value_filter<'a>(arg: &str) -> Option<fn(&'a str) -> ArgvFilter<'a>> {
    match arg {
        "--filter" | "-f" => Some(ArgvFilter::Rule),
        "--exclude" => Some(ArgvFilter::Exclude),
        "--include" => Some(ArgvFilter::Include),
        "--exclude-from" => Some(ArgvFilter::ExcludeFrom),
        "--include-from" => Some(ArgvFilter::IncludeFrom),
        _ => None,
    }
}

/// The filter an option spelled `--opt=VALUE` or `-fVALUE` builds.
fn joined_value_filter(arg: &str) -> Option<ArgvFilter<'_>> {
    if let Some(rule) = arg.strip_prefix("--filter=") {
        Some(ArgvFilter::Rule(rule))
    } else if let Some(pattern) = arg.strip_prefix("--exclude=") {
        Some(ArgvFilter::Exclude(pattern))
    } else if let Some(pattern) = arg.strip_prefix("--include=") {
        Some(ArgvFilter::Include(pattern))
    } else if let Some(file) = arg.strip_prefix("--exclude-from=") {
        Some(ArgvFilter::ExcludeFrom(file))
    } else if let Some(file) = arg.strip_prefix("--include-from=") {
        Some(ArgvFilter::IncludeFrom(file))
    } else {
        arg.strip_prefix("-f")
            .filter(|rule| !rule.is_empty())
            .map(ArgvFilter::Rule)
    }
}

/// A merge file named by a rule in the client's argv (`--filter='merge F'`).
///
/// upstream: `parse_merge_name()` (`exclude.c:696-753`) resolves the name
/// against the module root and, because a module without chroot sets
/// `sanitize_paths` (`clientserver.c:1067-1068`), sanitizes it there; a
/// chrooted daemon reaches the same file through the chroot. The open is
/// gated by `daemon_filter_list` alone and confined beneath the module root
/// (`exclude.c:1639-1690`).
struct ClientArgvMerges<'a> {
    /// The module root every name resolves beneath.
    root: &'a Path,
    /// The module's own rules: upstream's `daemon_filter_list`.
    daemon_rules: &'a [FilterRuleWireFormat],
}

impl MergeFileSource for ClientArgvMerges<'_> {
    fn read(&self, name: &str, _rules: &[FilterRuleWireFormat]) -> io::Result<Option<String>> {
        let relative = filters::sanitize_path::sanitize_path(name);
        if merge_file_hidden_by_daemon_rules(self.daemon_rules, Path::new(&relative), self.root) {
            return Ok(None);
        }
        read_module_file(self.root, &relative)
            .map(Some)
            .map_err(|err| open_failure(false, &relative, &err))
    }
}

/// Builds the rules the client's argv filter options add, in argv order.
///
/// Every file the options name - a `-from` file or a `merge` file - resolves
/// beneath `module_root` and is opened through the confined resolver, so a
/// client can read neither outside the module nor a file the module's rules
/// hide. The patterns are parsed with upstream's argv xflags: `0` for
/// `--filter` and `XFLG_OLD_PREFIXES` for the rest, neither of which applies
/// the module-directive `XFLG_ABS_IF_SLASH | XFLG_DIR2WILD3` shaping, which is
/// what [`RuleXflags::MergeFile`] models.
///
/// # Errors
///
/// [`ArgvFilterRefusal::Rejected`] for a hidden `-from` file and
/// [`ArgvFilterRefusal::Fatal`] for a malformed rule or an unreadable file.
///
/// # Upstream Reference
///
/// - `options.c:1620-1656` - the `--filter`, `--exclude`, `--include` and
///   `-from` arms of `parse_arguments()`.
/// - `options.c:1718-1727` - the first `-F` adds `: /.rsync-filter`, the
///   second `- .rsync-filter`.
fn build_argv_filter_rules(
    client_args: &[String],
    daemon_rules: &[FilterRuleWireFormat],
    module_root: &Path,
) -> Result<Vec<FilterRuleWireFormat>, ArgvFilterRefusal> {
    let merges = ClientArgvMerges {
        root: module_root,
        daemon_rules,
    };
    let mut rules = Vec::new();
    let mut rsync_filter_count = 0usize;
    for filter in collect_argv_filters(client_args) {
        match filter {
            ArgvFilter::Rule(rule) => {
                push_token_rules(&mut rules, rule, &merges, 0, RuleXflags::MergeFile)
                    .map_err(ArgvFilterRefusal::Fatal)?;
            }
            ArgvFilter::Exclude(pattern) | ArgvFilter::Include(pattern) => {
                let include = matches!(filter, ArgvFilter::Include(_));
                let rule = old_prefix_record_rule(
                    pattern,
                    include,
                    &filters::RuleSource::Argument,
                    RuleXflags::MergeFile,
                )
                .map_err(ArgvFilterRefusal::Fatal)?;
                rules.push(rule);
            }
            ArgvFilter::ExcludeFrom(file) | ArgvFilter::IncludeFrom(file) => {
                let include = matches!(filter, ArgvFilter::IncludeFrom(_));
                push_argv_from_file_rules(&mut rules, file, include, &merges)?;
            }
            ArgvFilter::RsyncFilter => {
                rsync_filter_count += 1;
                let rule = match rsync_filter_count {
                    1 => ": /.rsync-filter",
                    2 => "- .rsync-filter",
                    _ => continue,
                };
                push_token_rules(&mut rules, rule, &merges, 0, RuleXflags::MergeFile)
                    .map_err(ArgvFilterRefusal::Fatal)?;
            }
        }
    }
    Ok(rules)
}

/// Appends the records of an argv `--exclude-from` / `--include-from` file.
///
/// upstream: `options.c:1636-1655`. The name is sanitized, refused when empty
/// or hidden by `daemon_filter_list` (only when that list is non-empty), and
/// the file is then read with `XFLG_FATAL_ERRORS | XFLG_OLD_PREFIXES`, one
/// rule per line with `;`/`#` comments.
fn push_argv_from_file_rules(
    rules: &mut Vec<FilterRuleWireFormat>,
    file: &str,
    include: bool,
    merges: &ClientArgvMerges<'_>,
) -> Result<(), ArgvFilterRefusal> {
    let relative = filters::sanitize_path::sanitize_path(file);
    if !merges.daemon_rules.is_empty()
        && (relative.is_empty()
            || merge_file_hidden_by_daemon_rules(
                merges.daemon_rules,
                Path::new(&relative),
                merges.root,
            ))
    {
        return Err(ArgvFilterRefusal::Rejected);
    }
    let content = read_module_file(merges.root, &relative)
        .map_err(|err| ArgvFilterRefusal::Fatal(open_failure(include, &relative, &err)))?;
    let records = filters::filter_file_records(&content)
        .enumerate()
        .filter(|(_, record)| filters::filter_file_line_is_rule(record, true));
    for (index, record) in records {
        let source = filters::RuleSource::File {
            name: &relative,
            line: index + 1,
        };
        let rule = old_prefix_record_rule(record, include, &source, RuleXflags::MergeFile)
            .map_err(ArgvFilterRefusal::Fatal)?;
        rules.push(rule);
    }
    Ok(())
}

/// Reads `relative` beneath the module root, never leaving it.
///
/// On Unix the open goes through the confined resolver, so a symlink or `..`
/// cannot lead outside the module. Elsewhere the sanitized name, which can no
/// longer be absolute or climb, is joined onto the root.
fn read_module_file(root: &Path, relative: &str) -> io::Result<String> {
    #[cfg(unix)]
    {
        use std::io::Read as _;
        let mut file = fast_io::open_source_confined(
            root,
            Path::new(relative),
            fast_io::LeafPolicy::FollowConfined,
            false,
        )?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        Ok(content)
    }
    #[cfg(not(unix))]
    {
        fs::read_to_string(root.join(relative))
    }
}

/// upstream: `exclude.c:1714-1719` - `failed to open %sclude file %s`.
fn open_failure(include: bool, name: &str, err: &io::Error) -> io::Error {
    let kind = if include { "in" } else { "ex" };
    io::Error::new(
        err.kind(),
        FilterFileOpenFailure(format!("failed to open {kind}clude file {name}: {err}")),
    )
}

/// Ends the session over a client argv filter option.
///
/// A rejected `-from` name travels as a multiplexed error, the same way an
/// unparseable client argument does: `@RSYNCD: OK` is already on the wire.
/// A fatal error exits from inside upstream's `parse_arguments()`, before the
/// protocol setup that could carry it back, so the client sees only EOF and
/// the reason reaches the daemon log alone, followed by the totals line every
/// daemon exit logs.
///
/// upstream: `clientserver.c:1254-1267` reports the rejection's `err_buf` and
/// exits; `exclude.c:136` and `exclude.c:1714-1719` call `exit_cleanup()`
/// from `parse_arguments()` (`clientserver.c:1160`), ahead of the
/// `setup_protocol()` at `clientserver.c:1226`; `cleanup.c:222-226` runs
/// `log_exit()`, whose totals line is `log.c:942`.
fn refuse_argv_filters(
    ctx: &mut ModuleRequestContext<'_>,
    refusal: &ArgvFilterRefusal,
    protocol_version: Option<ProtocolVersion>,
    client_args: &[String],
) -> io::Result<()> {
    match refusal {
        ArgvFilterRefusal::Rejected => {
            if let Some(log) = ctx.log_sink {
                let text = format!("module '{}': {ARGV_FILTER_OPTIONS_REJECTED}", ctx.request);
                log_message(log, &rsync_warning!(text).with_role(Role::Daemon));
            }
            let error = AtError::message(ARGV_FILTER_OPTIONS_REJECTED);
            handle_client_arg_rejection_post_handshake(
                ctx,
                &error.line(),
                protocol_version,
                client_args,
            )
        }
        ArgvFilterRefusal::Fatal(err) => {
            let code = fatal_exit_code(err);
            *ctx.session_exit_code = Some(code);
            if let Some(log) = ctx.log_sink {
                log_message(
                    log,
                    &rsync_error!(code.as_i32(), err.to_string()).with_role(Role::Daemon),
                );
                let totals = "sent 0 bytes  received 0 bytes  total size 0";
                log_message(log, &rsync_info!(totals).with_role(Role::Daemon));
            }
            Ok(())
        }
    }
}
