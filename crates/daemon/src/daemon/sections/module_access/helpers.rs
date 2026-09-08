// Helpers for module access - logging, sanitization, bandwidth formatting,
// filter rules, and utilities.
//
// Contains shared functions used across the module access submodules:
// bandwidth limit application, log file management, module identifier
// sanitization, human-readable bandwidth formatting, and daemon-side
// filter rule construction from module config directives.

/// Opens or creates a log file and wraps it in a shared message sink.
///
/// The log file is opened in append mode, creating it if it doesn't exist.
/// Returns a thread-safe [`SharedLogSink`] for concurrent logging.
///
/// upstream: log.c:169 opens the logfile through `open_no_attacker_symlinks()`
/// with `O_WRONLY|O_APPEND|O_CREAT, 0644`, for the reason stated at log.c:165 -
/// `--log-file` and `log file =` are operator-supplied paths that may transit
/// attacker-writable directories, and a planted symlink would redirect the root
/// daemon's log into a file of the attacker's choosing. The ownership walk
/// refuses symlink components not owned by uid 0 or our euid.
pub(crate) fn open_log_sink(path: &Path, brand: Brand) -> Result<SharedLogSink, DaemonError> {
    let file = open_log_file(path).map_err(|error| log_file_error(path, error))?;
    // upstream: log.c:122-132 logit() stamps `%Y/%m/%d %H:%M:%S [pid] ` on
    // every log-file line; the wrapper applies the same shared formatter used
    // by the client `--log-file` sink.
    Ok(Arc::new(Mutex::new(MessageSink::with_brand(
        logging_sink::logfile::LogFileWriter::new(file),
        brand,
    ))))
}

/// Mode applied when the log file is created.
///
/// upstream: log.c:170 passes `0644`. The surrounding `umask(022 | orig_umask)`
/// (log.c:163) is what forbids a group- or world-writable log file under a
/// permissive umask; passing `0644` here reaches the same fixed point without
/// the dance, because `0644` has no group or other write bit for a umask to
/// need to clear.
#[cfg(unix)]
const LOG_FILE_MODE: u32 = 0o644;

/// Opens the log file for appending, refusing untrusted symlink components.
///
/// Windows has no POSIX mode bits and no ownership-walk equivalent, so it keeps
/// the plain open.
fn open_log_file(path: &Path) -> io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        fast_io::operator_open_append(path, LOG_FILE_MODE)
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new().create(true).append(true).open(path)
    }
}

/// Reopens the connection's log sink to the selected module's `log file`.
///
/// upstream: log.c:169-204 `log_init(1)` reopens the daemon logfile to
/// `lp_log_file(module_id)` at module selection (clientserver.c:897). A module's
/// resolved `log file` already inherits the global-section default (finish.rs),
/// so [`ModuleDefinition::module_log_file`] is exactly upstream's
/// `lp_log_file(module_id)`.
///
/// oc opens the startup sink once (from `--log-file`) and shares it across
/// connection threads, so rather than mutate the shared sink this returns a
/// fresh per-connection sink pointing at the module's log file; the caller uses
/// it for the remainder of the connection. A module with no `log file` returns
/// `None`, keeping the startup sink. A failed reopen is non-fatal - upstream
/// (log.c:158-166) logs the failure and keeps serving rather than dropping the
/// connection - so this returns `None` and the caller retains the startup sink.
fn reopen_module_log_sink(
    module: &ModuleDefinition,
    startup_sink: Option<&SharedLogSink>,
) -> Option<SharedLogSink> {
    let path = module.module_log_file()?;
    let brand = startup_sink
        .and_then(|sink| sink.lock().ok().map(|guard| guard.brand()))
        .unwrap_or(Brand::Oc);
    open_log_sink(path, brand).ok()
}

/// Creates a [`DaemonError`] for log file open failures.
///
/// upstream: log.c:163 - log-open failures produce RERR_MESSAGEIO (13).
fn log_file_error(path: &Path, error: io::Error) -> DaemonError {
    let code = ExitCode::MessageIo;
    DaemonError::with_code(
        code,
        rsync_error!(
            code.as_i32(),
            format!("failed to open log file '{}': {}", path.display(), error)
        )
        .with_role(Role::Daemon),
    )
}

/// Creates a [`DaemonError`] for PID file write failures.
fn pid_file_error(path: &Path, error: io::Error) -> DaemonError {
    DaemonError::new(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        rsync_error!(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            format!("failed to write pid file '{}': {}", path.display(), error)
        )
        .with_role(Role::Daemon),
    )
}

/// Creates a [`DaemonError`] for lock file open failures.
#[cfg(test)]
fn lock_file_error(path: &Path, error: io::Error) -> DaemonError {
    DaemonError::new(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        rsync_error!(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            format!("failed to open lock file '{}': {}", path.display(), error)
        )
        .with_role(Role::Daemon),
    )
}

/// Writes a message to the shared log sink with proper locking.
///
/// upstream: log.c:122-132 logit() writes the raw `rwrite()` buffer after the
/// timestamp prefix. FINFO/FLOG bodies carry no severity tag (e.g.
/// `connect from host (addr)`, clientserver.c:1393), so info messages are
/// written as bare text; error and warning bodies already embed their
/// `rsync error:`-style text upstream, so they keep the sink's rendering.
fn log_message(log: &SharedLogSink, message: &Message) {
    let Ok(mut sink) = log.lock() else {
        return;
    };
    let written = if message.severity() == core::message::Severity::Info {
        writeln!(sink.writer_mut(), "{}", message.text()).is_ok()
    } else {
        sink.write(message).is_ok()
    };
    if written {
        let _ = sink.flush();
    }
}

/// Returns a sanitised view of a module identifier suitable for diagnostics.
///
/// Module names originate from user input (daemon operands) or configuration
/// files. When composing diagnostics the value must not embed control
/// characters, otherwise adversarial requests could smuggle terminal control
/// sequences or split log lines. The helper replaces ASCII control characters
/// with a visible `'?'` marker while borrowing clean identifiers to avoid
/// unnecessary allocations.
pub(crate) fn sanitize_module_identifier(input: &str) -> Cow<'_, str> {
    if input.chars().all(|ch| !ch.is_control()) {
        return Cow::Borrowed(input);
    }

    let mut sanitized = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_control() {
            sanitized.push('?');
        } else {
            sanitized.push(ch);
        }
    }

    Cow::Owned(sanitized)
}

/// Reports whether a daemon `dont compress` value collapses to the whole-stream
/// "match all" special case.
///
/// Upstream treats a bare `*` token in the sender's dont-compress match list as
/// a signal to store the entire zlib stream (level 0) rather than compress per
/// block. Any other suffix present alongside the `*` is discarded.
///
/// # Upstream Reference
///
/// - `token.c:206-211`: `init_set_compression()` optimises a `*` match-string,
///   setting `per_file_default_level = skip_compression_level` and clearing the
///   per-file suffix tree.
fn dont_compress_is_match_all(value: &str) -> bool {
    value.split_whitespace().any(|token| token == "*")
}

/// Builds daemon-side filter rules from the module's filter configuration.
///
/// Upstream rsync's `clientserver.c:rsync_module()` builds `daemon_filter_list` from:
/// 1. `filter` - parsed with `FILTRULE_WORD_SPLIT` (full filter rule syntax)
/// 2. `include` - parsed with `FILTRULE_INCLUDE | FILTRULE_WORD_SPLIT`
/// 3. `exclude` - parsed with `FILTRULE_WORD_SPLIT`
/// 4. `include_from` - read from file, one pattern per line (include)
/// 5. `exclude_from` - read from file, one pattern per line (exclude)
///
/// The order matches upstream: filter, include_from, include, exclude_from, exclude.
///
/// upstream: clientserver.c:933-952 - `rsync_module()` builds `daemon_filter_list`.
///
/// Four of the five carry `XFLG_OLD_PREFIXES` - every one except `filter`,
/// which gets the full filter-rule grammar instead. Under that flag a leading
/// `- ` or `+ ` is an ACTION PREFIX that is stripped from the pattern, and a
/// token that is exactly `!` clears the list. Taking the record verbatim
/// instead turns `- foo` into a pattern that matches a file literally named
/// `- foo`, so the entry the operator wrote it to hide is served.
/// [`old_prefix_record_rule`] and [`push_old_prefix_token_rules`] own that
/// decision for the file and string spellings respectively.
fn build_daemon_filter_rules(
    module: &ModuleRuntime,
) -> Result<Vec<FilterRuleWireFormat>, io::Error> {
    let mut rules = Vec::new();

    // 1. filter rules - full filter syntax (e.g., "- *.tmp", "+ *.rs")
    // upstream: clientserver.c:933-935 - parse_filter_str(&daemon_filter_list, lp_filter(i),
    //           rule_template(FILTRULE_WORD_SPLIT), XFLG_ABS_IF_SLASH | XFLG_DIR2WILD3)
    // FILTRULE_WORD_SPLIT means a single filter line can contain multiple
    // space-separated rules: "+ *.txt + *.rs - *" is three rules.
    //
    // This is the ONE module filter parameter upstream does NOT give
    // XFLG_OLD_PREFIXES: `filter` takes the modern rule syntax, where `- ` and
    // `+ ` are rule prefixes already, so `old_prefix_*` below deliberately does
    // not apply here.
    //
    // A malformed rule is REFUSED, not skipped: upstream's `parse_rule_tok`
    // exits `RERR_SYNTAX` (`exclude.c:1130`), so the module is never served.
    // This function already returns `Result`, and its caller already refuses
    // the connection on the file-reading arms below - the refusal PATH is
    // unchanged here; only which tokens enter it is new.
    for filter_str in &module.filter {
        for token in split_filter_tokens(filter_str.trim()) {
            if let Some(rule) =
                parse_daemon_filter_token(&token).map_err(MalformedRule::into_io_error)?
            {
                rules.push(rule);
            }
        }
    }

    // 2. include_from - read patterns from file, one per line
    // upstream: clientserver.c:937-939 - parse_filter_file(&daemon_filter_list,
    //           lp_include_from(i), rule_template(FILTRULE_INCLUDE),
    //           XFLG_ABS_IF_SLASH | XFLG_DIR2WILD3 | XFLG_OLD_PREFIXES | XFLG_FATAL_ERRORS)
    if let Some(ref path) = module.include_from {
        let name = path.display().to_string();
        for (pattern, line) in read_patterns_from_file(path)? {
            let source = filters::RuleSource::File { name: &name, line };
            rules.push(old_prefix_record_rule(&pattern, true, &source)?);
        }
    }

    // 3. include rules - bare patterns, word-split on whitespace
    // upstream: clientserver.c:941-944 - parse_filter_str(&daemon_filter_list, lp_include(i),
    //           rule_template(FILTRULE_INCLUDE | FILTRULE_WORD_SPLIT),
    //           XFLG_ABS_IF_SLASH | XFLG_DIR2WILD3 | XFLG_OLD_PREFIXES)
    for include_str in &module.include {
        push_old_prefix_token_rules(&mut rules, include_str, true)?;
    }

    // 4. exclude_from - read patterns from file, one per line
    // upstream: clientserver.c:946-948 - parse_filter_file(&daemon_filter_list, lp_exclude_from(i),
    //           rule_template(0), XFLG_ABS_IF_SLASH | XFLG_DIR2WILD3 | ...)
    if let Some(ref path) = module.exclude_from {
        let name = path.display().to_string();
        for (pattern, line) in read_patterns_from_file(path)? {
            let source = filters::RuleSource::File { name: &name, line };
            rules.push(old_prefix_record_rule(&pattern, false, &source)?);
        }
    }

    // 5. exclude rules - bare patterns, word-split on whitespace
    // upstream: clientserver.c:950-952 - parse_filter_str(&daemon_filter_list, lp_exclude(i),
    //           rule_template(FILTRULE_WORD_SPLIT),
    //           XFLG_ABS_IF_SLASH | XFLG_DIR2WILD3 | XFLG_OLD_PREFIXES)
    for exclude_str in &module.exclude {
        push_old_prefix_token_rules(&mut rules, exclude_str, false)?;
    }

    Ok(rules)
}

/// The prefix `XFLG_OLD_PREFIXES` recognises at the head of a filter rule.
///
/// upstream: `exclude.c:1276-1284`. Under `XFLG_OLD_PREFIXES` exactly three
/// forms are special, and nothing else is: a literal `- ` (dash, space) makes
/// the rule an exclude, a literal `+ ` makes it an include, and a leading `!`
/// *tentatively* marks a list-clear. Only the first two consume bytes - the
/// `!` arm leaves the cursor where it is, which is why `Clear` carries no
/// remainder here and the caller measures the token including the `!`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OldPrefix {
    /// `- ` - override the template to exclude, consuming two bytes.
    Exclude,
    /// `+ ` - override the template to include, consuming two bytes.
    Include,
    /// A leading `!` - a list-clear only if the whole token is exactly `!`.
    MaybeClear,
    /// No recognised prefix: the rule inherits the template's include flag.
    Inherit,
}

/// Classifies the head of one rule under `XFLG_OLD_PREFIXES`.
///
/// upstream: `exclude.c:1277-1284`. The tests are on raw bytes with no
/// whitespace skipping of their own: under `FILTRULE_WORD_SPLIT` the caller has
/// already advanced past the leading whitespace (`exclude.c:1250-1255`), and the
/// two file-read parameters are *not* word-split, so their record starts at
/// column zero. A `-` or `+` not followed by a space is therefore ordinary
/// pattern text, which is what makes `- - foo` an exclude of the literal
/// pattern `- foo`: exactly one strip happens, never two.
fn old_prefix(rule: &str) -> OldPrefix {
    if rule.starts_with("- ") {
        OldPrefix::Exclude
    } else if rule.starts_with("+ ") {
        OldPrefix::Include
    } else if rule.starts_with('!') {
        OldPrefix::MaybeClear
    } else {
        OldPrefix::Inherit
    }
}

/// A list-clear rule.
///
/// upstream: `exclude.c:1284` sets `FILTRULE_CLEAR_LIST`; `exclude.c:1542-1549`
/// makes the receiving list drop every rule accumulated so far
/// (`pop_filter_list(listp)` at `exclude.c:1548`).
fn clear_list_rule() -> FilterRuleWireFormat {
    FilterRuleWireFormat {
        rule_type: protocol::filters::RuleType::Clear,
        ..FilterRuleWireFormat::default()
    }
}

/// Upstream's fatal "unexpected end of filter rule" refusal.
///
/// upstream: `exclude.c:1474-1475` - a rule that is empty once its prefix has
/// been consumed calls `filter_rule_err()`, which is `rprintf(FERROR, ...)`
/// followed by `exit_cleanup(RERR_SYNTAX)` (`exclude.c:133-137`). Both file
/// parameters additionally carry `XFLG_FATAL_ERRORS`. Returning an error here
/// reaches the caller's existing abort path, which refuses the module rather
/// than silently dropping the rule - dropping it would serve every file the
/// operator wrote that line to hide.
///
/// The rule text goes through [`filters::RuleSource::rule_text`], upstream's
/// `rule_text()` chokepoint (`exclude.c:134`): text from a STRING parameter is
/// the operator's own and stays verbatim (`rule_src_in_file == 0` while
/// `parse_filter_str` runs on `lp_filter`/`lp_include`/`lp_exclude`,
/// clientserver.c:933-952), while a record read out of an `include from` /
/// `exclude from` FILE is replaced by `<rule from FILE line N>`
/// (`exclude.c:1749-1754` sets `rule_src_file`/`rule_src_line`;
/// `exclude.c:71-86` renders the description).
fn unexpected_end_of_filter_rule(rule: &str, source: &filters::RuleSource<'_>) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("unexpected end of filter rule: {}", source.rule_text(rule)),
    )
}

/// Builds one rule from a whole filter-file record under `XFLG_OLD_PREFIXES`.
///
/// `template_include` is the template's `FILTRULE_INCLUDE` bit:
/// `rule_template(FILTRULE_INCLUDE)` for `include from` (clientserver.c:937-939)
/// and `rule_template(0)` for `exclude from` (clientserver.c:946-948).
///
/// Neither template carries `FILTRULE_WORD_SPLIT`, so the pattern runs to the
/// end of the record: `len = strlen(s)` (`exclude.c:1465`). That is why the
/// whole record is handed to [`build_pattern_rule`] rather than a first word.
fn old_prefix_record_rule(
    record: &str,
    template_include: bool,
    source: &filters::RuleSource<'_>,
) -> Result<FilterRuleWireFormat, io::Error> {
    let (pattern, is_include) = match old_prefix(record) {
        OldPrefix::Exclude => (&record[2..], false),
        OldPrefix::Include => (&record[2..], true),
        // upstream: exclude.c:1467-1473 - the `!` is tentative. `len` is
        // measured from the UNADVANCED cursor, so it counts the `!` itself;
        // `len > 1` clears the flag again and the rule keeps `!...` as literal
        // pattern text. `len == 1` is the bare `!` that really clears the list.
        OldPrefix::MaybeClear if record.len() == 1 => return Ok(clear_list_rule()),
        OldPrefix::MaybeClear | OldPrefix::Inherit => (record, template_include),
    };
    // upstream: exclude.c:1474-1475 - `filter_rule_err("unexpected end of
    // filter rule")`, fatal under XFLG_FATAL_ERRORS.
    //
    // This is LIVE, and it is live because of the ORDER these two changes
    // compose in. `read_patterns_from_file` hands the record over untrimmed
    // (exclude.c:1772-1774 breaks a non-word-split line only on the newline,
    // so trailing whitespace is pattern text), and the prefix strip above then
    // consumes two bytes of it. A `"- "` line therefore arrives here as `""`
    // and upstream refuses it. Reverse the order - split or trim before
    // stripping - and this becomes unreachable, which is what it was while the
    // reader trimmed.
    //
    // Pinned by `exclude_from_empty_after_the_prefix_is_refused`; the
    // string-parameter twin lives in `push_old_prefix_token_rules` and is
    // pinned by `exclude_string_empty_after_the_prefix_is_refused`.
    if pattern.is_empty() {
        return Err(unexpected_end_of_filter_rule(record, source));
    }
    Ok(build_pattern_rule(pattern, is_include))
}

/// Appends the rules a word-split `include` / `exclude` value expands to.
///
/// upstream: `parse_filter_str()` (`exclude.c:1516`) loops `parse_rule_tok()`
/// until the string is consumed. With `FILTRULE_WORD_SPLIT` each iteration
/// skips leading whitespace (`exclude.c:1250-1255`), applies the
/// `XFLG_OLD_PREFIXES` decision, then takes the pattern up to the next
/// whitespace (`exclude.c:1458-1462`). So `exclude = - foo bar` is two rules:
/// an exclude of `foo` and - from the template - an exclude of `bar`.
///
/// The scan is on ASCII whitespace because upstream's is `isspace()` over the
/// raw bytes, which no byte of a multi-byte UTF-8 sequence can satisfy.
fn push_old_prefix_token_rules(
    rules: &mut Vec<FilterRuleWireFormat>,
    value: &str,
    template_include: bool,
) -> Result<(), io::Error> {
    let mut rest = value;
    loop {
        rest = rest.trim_start_matches(|ch: char| ch.is_ascii_whitespace());
        if rest.is_empty() {
            return Ok(());
        }
        let prefix = old_prefix(rest);
        let after_prefix = match prefix {
            OldPrefix::Exclude | OldPrefix::Include => &rest[2..],
            OldPrefix::MaybeClear | OldPrefix::Inherit => rest,
        };
        let end = after_prefix
            .find(|ch: char| ch.is_ascii_whitespace())
            .unwrap_or(after_prefix.len());
        let (token, tail) = after_prefix.split_at(end);
        if prefix == OldPrefix::MaybeClear && token.len() == 1 {
            rules.push(clear_list_rule());
        } else if token.is_empty() {
            // A STRING parameter is the operator's own text: upstream shows it
            // verbatim (`TEXT_FROM_FILE` is false, `exclude.c:67-69,110-117`).
            return Err(unexpected_end_of_filter_rule(
                rest,
                &filters::RuleSource::Argument,
            ));
        } else {
            let is_include = match prefix {
                OldPrefix::Exclude => false,
                OldPrefix::Include => true,
                OldPrefix::MaybeClear | OldPrefix::Inherit => template_include,
            };
            rules.push(build_pattern_rule(token, is_include));
        }
        rest = tail;
    }
}

/// Reads a daemon filter file through the ownership walk.
///
/// upstream: `exclude.c:1680-1684` calls `open_no_attacker_symlinks()`
/// UNCONDITIONALLY for every filter file and toggles only
/// `operator_path_resolve` around it:
///
/// ```text
/// int save_opr = operator_path_resolve;
/// if (!daemon_config_filter_file)
///         operator_path_resolve = 1;
/// fd = open_no_attacker_symlinks(open_path, O_RDONLY, 0);
/// operator_path_resolve = save_opr;
/// ```
///
/// ⚠ THE EXEMPTION UPSTREAM GRANTS `filter` / `include from` / `exclude from`
/// IS FROM MODULE CONFINEMENT, NOT FROM THE WALK. Its comment
/// (`exclude.c:1677-1679`) says those parameters "are operator-configured and
/// legitimately live outside the module (/etc/rsync/excludes and the like)" -
/// which is why this takes the OPERATOR arm rather than the confined one, and
/// why confining these reads to the module root would refuse configurations
/// upstream serves. It is NOT a statement that the path is trusted: the walk
/// still runs. Reading the plain [`fs::read_to_string`] this replaced as
/// "operator files are trusted" is exactly the misreading that left the hole.
///
/// The content becomes filter rules, and a rule the parser cannot read comes
/// back to the peer in the refusal text, so a redirected read both reshapes
/// the transfer and can disclose the target file.
///
/// MEASURED on Linux as root (module `mod` holding `bar` + `keep`;
/// `exclude from` naming a symlink owned by a NON-root uid whose target holds
/// the pattern `keep`): upstream REFUSES the module at exit 5, oc FOLLOWED the
/// link and applied the planted rule, serving only `bar`. With the same
/// symlink owned by ROOT both implementations follow it - that companion is
/// what makes the refusal a statement about OWNERSHIP rather than about
/// symlinks in general.
///
/// Windows has no ownership-walk equivalent, so it keeps the plain read - the
/// same split [`open_log_file`] makes.
fn read_filter_file_contents(path: &Path) -> io::Result<String> {
    #[cfg(unix)]
    {
        fast_io::operator_read_to_string(path)
    }
    #[cfg(not(unix))]
    {
        fs::read_to_string(path)
    }
}

/// Reads patterns from a filter file, one per record.
///
/// upstream: `exclude.c:1601 parse_filter_file()`, which is what
/// `clientserver.c:938,947` calls for the `include from` and `exclude from`
/// module parameters. Both templates are line-parsed - `rule_template(...)`
/// carries no `FILTRULE_WORD_SPLIT` - so the two decisions this reader makes
/// are exactly the ones the shared owners hold:
///
/// * where a record ends - [`filters::filter_file_records`]
///   (`exclude.c:1774-1793`): `\n`, a lone `\r`, or `\r\n` as one terminator.
/// * whether it carries a rule - [`filters::filter_file_line_is_rule`]
///   (`exclude.c:1806`): the FIRST BYTE is tested, with no trimming.
///
/// Nothing is trimmed. The pattern length upstream takes is `strlen(s)`
/// (`exclude.c:1465`), so trailing whitespace is pattern text.
///
/// MEASURED against rsync 3.5.0 with a module whose `exclude from` file holds
/// the single line `a ` (trailing space), over a module directory holding `a`
/// and `a `: upstream hides `a ` and serves `a`; oc trimmed the rule, hid `a`
/// and served `a `. Both at exit 0, with no diagnostic - a silent divergence
/// in which files the daemon exposes.
/// Each pattern is paired with its 1-indexed PHYSICAL line: upstream's
/// `rule_src_line` increments once per record read (`exclude.c:1760-1761`),
/// before the comment/blank test, so comments and blank lines keep their slot
/// and a bad rule after them is reported at the line the operator sees in an
/// editor. That number feeds the `<rule from FILE line N>` provenance.
fn read_patterns_from_file(path: &Path) -> Result<Vec<(String, usize)>, io::Error> {
    let content = read_filter_file_contents(path).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("failed to read filter file '{}': {e}", path.display()),
        )
    })?;

    let patterns = filters::filter_file_records(&content)
        .enumerate()
        // `true`: these two parameters are the non-word-split templates, so a
        // leading `;`/`#` is a comment (`exclude.c:1806`, `word_split ||`).
        .filter(|(_, line)| filters::filter_file_line_is_rule(line, true))
        .map(|(index, line)| (line.to_owned(), index + 1))
        .collect();

    Ok(patterns)
}

/// Every long-form filter keyword the daemon `filter` directive recognises.
///
/// upstream: `exclude.c:1134-1178` maps each of these to a short-form rule
/// character. The list is shared by the tokenizer and, through
/// [`is_rule_keyword`], uses one terminator rule rather than a second copy.
///
/// ⚠ `merge`/`dir-merge` open a token here but `parse_daemon_filter_token` has
/// no arm for them, so such a token falls through to its bare-pattern arm. That
/// is a separate defect, tracked on its own; this list deliberately keeps them
/// so the tokenizer's behaviour on them is unchanged by this commit.
const RULE_KEYWORDS: &[&str] = &[
    "include",
    "exclude",
    "hide",
    "show",
    "protect",
    "risk",
    "clear",
    "merge",
    "dir-merge",
];

/// The bytes that terminate a filter-rule keyword.
///
/// upstream: `exclude.c:1218-1227` `rule_strcmp` - a keyword matches only when
/// the byte after it is whitespace, `_`, `,`, or the end of the string. Any
/// other byte makes the token not that keyword at all.
///
/// oc's CLI parser states the same rule in
/// `crates/cli/src/frontend/filter_rules/parsing/helpers.rs`, whose
/// `is_rule_separator` records why oc treats the whole ASCII whitespace class
/// as a separator where upstream's `rule_strcmp` calls `isspace`. This is the
/// daemon-side statement of that one convention, not a second one.
fn is_keyword_terminator(ch: char) -> bool {
    ch == '_' || ch == ',' || ch.is_ascii_whitespace()
}

/// Returns true when `s` opens with `keyword` AND the keyword is terminated.
fn is_rule_keyword(s: &str, keyword: &str) -> bool {
    match s.strip_prefix(keyword) {
        Some(rest) => rest.chars().next().is_none_or(is_keyword_terminator),
        None => false,
    }
}

/// Splits a filter string with `FILTRULE_WORD_SPLIT` semantics into individual
/// rule tokens.
///
/// A single `filter` line in rsyncd.conf can contain multiple space-separated
/// rules: `"+ *.txt + *.rs - *"` becomes `["+ *.txt", "+ *.rs", "- *"]`.
///
/// Each rule starts with a prefix (`+`, `-`, or a keyword like `include`,
/// `exclude`, `hide`, `show`, `protect`, `risk`, `clear`, `merge`, `dir-merge`)
/// followed by a pattern. The function scans for rule boundaries by looking for
/// these prefixes after whitespace.
///
/// upstream: exclude.c:parse_filter_str() with FILTRULE_WORD_SPLIT flag
fn split_filter_tokens(s: &str) -> Vec<String> {
    let s = s.trim();
    if s.is_empty() {
        return Vec::new();
    }

    /// Returns true if `s` starts with a filter rule prefix.
    ///
    /// The keyword arm asks `is_rule_keyword`, so a keyword terminated by any
    /// separator upstream accepts opens a new token. The table this replaced
    /// carried a MANDATORY TRAILING SPACE on every entry, so `hide_bar` and a
    /// line-final `clear` matched nothing, no token boundary opened, and the
    /// whole remainder collapsed into one rule whose pattern was the literal
    /// compound string.
    ///
    /// The short arm asks the SAME question [`parse_daemon_filter_token`]
    /// asks - does a one-character rule prefix carry a valid modifier run? -
    /// so the splitter and the parser cannot disagree about what a token is.
    /// The hardcoded `["+ ", "- ", "+/", "-/"]` table it replaced recognised
    /// no `P`/`R`/`H`/`S` prefix at all, so `filter = - foo P bar` collapsed
    /// into ONE rule whose pattern was the literal `foo P bar` and the module
    /// served both `foo` and `bar`; upstream hides both (MEASURED, rc 0 with
    /// only `keep` and `ctl` listed).
    fn starts_with_rule_prefix(s: &str) -> bool {
        opens_short_rule(s) || RULE_KEYWORDS.iter().any(|kw| is_rule_keyword(s, kw))
    }

    let mut tokens = Vec::new();
    let mut start = 0;

    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b' ' || bytes[i] == b'\t' {
            let rest = &s[i..].trim_start();
            if !rest.is_empty() && starts_with_rule_prefix(rest) {
                let token = s[start..i].trim();
                if !token.is_empty() {
                    tokens.push(token.to_string());
                }
                let ws_len = s[i..].len() - rest.len();
                start = i + ws_len;
                i = start;
                continue;
            }
        }
        i += 1;
    }

    let token = s[start..].trim();
    if !token.is_empty() {
        tokens.push(token.to_string());
    }

    tokens
}

/// Parses a single daemon filter token in filter rule syntax.
///
/// Supports both short-form prefixes (`+`, `-`) and long-form keyword
/// prefixes (`include`, `exclude`, `hide`, `show`, `protect`, `risk`,
/// `clear`, `dir-merge`, `merge`). The pattern follows the prefix after
/// optional whitespace.
///
/// `Ok(None)` is an EMPTY token - nothing to add, no error. `Err` is a
/// MALFORMED rule, which upstream refuses: `parse_rule_tok` calls
/// `rprintf(FERROR, "Invalid... rule: %s\n", ..)` then
/// `exit_cleanup(RERR_SYNTAX)` (`exclude.c:1130`, `:1170`).
///
/// ⚠ This doc block previously claimed the opposite - *"Returns `None` for
/// unrecognised tokens (silently skipped, matching upstream's lenient parsing
/// of daemon filter strings)"*. There is no lenient parsing to match.
/// MEASURED against rsync 3.5.0: a module with `filter = -foo` is refused with
/// exit 5, while oc served the module and silently excluded `foo`. The comment
/// is what made the fallback below read as deliberate.
///
/// # Upstream Reference
///
/// - `exclude.c:1096-1131` - the modifier scan that rejects `-foo` on `f`
/// - `exclude.c:1134-1178` - long-form keyword to short-form char mapping
fn parse_daemon_filter_token(token: &str) -> Result<Option<FilterRuleWireFormat>, MalformedRule> {
    // Short-form prefixes. upstream's `default:` arm makes ANY single
    // `+ - P R S H . : !` character a rule character (exclude.c:1325-1329):
    //
    //     default:
    //             ch = *s;
    //             if (s[1] == ',')
    //                     s++;
    //             break;
    //
    // `s` is left ON the rule character (or on the `,` that follows it), so
    // the modifier scan's `*++s` starts at the very next byte - a short prefix
    // takes a modifier run with NO comma (`-p foo`), where a multi-char
    // keyword can only reach the scan through one (see `strip_matched_keyword`).
    if let Some((prefix, ch)) = short_rule_prefix(token) {
        let rest = &token[ch.len_utf8()..];
        // `if (s[1] == ',') s++` - the comma is consumed as part of the
        // prefix, so the scan begins one byte later and upstream's reported
        // modifier POSITION shifts with it.
        let (run, base) = match rest.strip_prefix(',') {
            Some(after_comma) => (after_comma, 2),
            None => (rest, 1),
        };
        match scan_modifiers(run, prefix.specifies_side, base, token) {
            Ok((modifiers, used)) => {
                return build_prefixed_rule(prefix, modifiers, run, used, token);
            }
            // ⚠ THE TWO CHAR CLASSES PART COMPANY HERE, and only here.
            //
            // `+`/`-` cannot open a bare pattern - upstream reads them as rule
            // characters unconditionally and oc has refused `-foo` since the
            // modifier scan landed - so an invalid modifier is the refusal
            // upstream reports.
            //
            // `P R S H` DO compete with the bare-word fall-through at the
            // bottom of this function: `Pictures` is a rule character followed
            // by the invalid modifier `i` upstream (rc 5, MEASURED), but oc
            // still serves that module with `Pictures` excluded literally.
            // Refusing it here would be a second, unrelated behaviour change
            // riding on this one, so a failed scan hands such a token back to
            // the fall-through unchanged; that residual belongs to the
            // bare-word class.
            //
            // ⚠ NOT when a `,` follows the character. `,` is a keyword
            // TERMINATOR (`rule_strcmp`, exclude.c:1224-1225), so `P,` is a rule
            // prefix under exactly the test `strip_matched_keyword` applies to
            // the long keywords - there is no bare word to compete with, and
            // the fall-through would serve a module upstream refuses. MEASURED:
            // `filter = P,r bar` is rc 5 upstream (`r` after a side-naming
            // prefix, exclude.c:1424-1425) and, with the fall-through taken
            // unconditionally, was rc 0 in oc with every file served.
            Err(err) => {
                if !prefix.competes_with_bare_words || rest.starts_with(',') {
                    return Err(err);
                }
            }
        }
    }

    // `!` is upstream's clear rule. It takes no pattern, and because a daemon
    // `filter` directive carries neither FILTRULE_NO_PREFIXES nor
    // XFLG_OLD_PREFIXES, both conjuncts of the guard at exclude.c:1468-1470
    // hold and a non-empty remainder refuses.
    //
    // ⚠ Upstream's modifier scan is `while (ch != '!' && ...)`, so it is
    // SKIPPED for `!` - this refusal comes from the trailing-characters check
    // AFTER the loop, not from the modifier arm above. The two arms report
    // different things and are not interchangeable.
    //
    // ⚠ A BARE `!` is deliberately left on its existing path: oc does not
    // implement it as a clear at all, so no cell measured here can
    // discriminate what oc does with it. That is task 1155's question.
    if token.len() > 1 && token.starts_with('!') {
        return Err(MalformedRule::ClearWithTrailingCharacters {
            token: token.to_owned(),
        });
    }

    // upstream: exclude.c:1288-1330 - keyword-to-short-form mapping;
    // `hide`/`show` set FILTRULE_SENDER_SIDE (exclude.c:1345-1351),
    // `protect`/`risk` FILTRULE_RECEIVER_SIDE (exclude.c:1352-1358). All four
    // side keywords set `prefix_specifies_side` (exclude.c:1350, :1357), which
    // narrows the modifier alphabet their `,` form accepts.
    for &(keyword, prefix) in KEYWORD_PREFIXES {
        if let Some(rest) = strip_matched_keyword(token, keyword) {
            // ⚠ A multi-char keyword can only reach the modifier scan through
            // a `,`. `rule_strcmp` returns `str + rule_len - 1` for a space,
            // `_` or end-of-string terminator (exclude.c:1222-1223) - one byte
            // BEFORE the terminator - so the scan's first `*++s` lands ON the
            // terminator and stops at once. Only the `,` terminator returns
            // `str + rule_len` (exclude.c:1224-1225), putting the scan inside
            // the modifier run, and any byte outside the alphabet then exits
            // RERR_SYNTAX (exclude.c:1371-1380).
            //
            // MEASURED against a real rsync 3.5.0 daemon (module holding
            // `foo` + `keep` + `bar`, pinned 3.5.0 client, loopback TCP):
            //   filter = - foo hide       -> rc 5 "unexpected end of filter rule: hide"
            //   filter = - foo hide,keep  -> rc 5 "invalid modifier 'k' at position 5 in filter rule: hide,keep"
            //   filter = protect,keep     -> rc 5 "invalid modifier 'k' at position 8 in filter rule: protect,keep"
            //   filter = hide,r keep      -> rc 5 "invalid modifier 'r' at position 5 in filter rule: hide,r keep"
            //   filter = hide,p keep      -> rc 0 (valid modifier; rule then dropped at add time)
            //   filter = hide, keep       -> rc 0 (empty modifier run is valid)
            //   filter = exclude          -> rc 5 "unexpected end of filter rule: exclude"
            //   filter = exclude,r keep   -> rc 0, excludes `keep` - the run is
            //                                NOT part of the pattern
            // oc served every refused config above (rc 0), handing out the
            // very files the operator's filter names.
            let (run, base) = match rest.strip_prefix(',') {
                Some(after_comma) => (after_comma, keyword.len() + 1),
                None => (rest, keyword.len()),
            };
            let (modifiers, used) = scan_modifiers(run, prefix.specifies_side, base, token)?;
            return build_prefixed_rule(prefix, modifiers, run, used, token);
        }
    }

    if let Some(rest) = strip_matched_keyword(token, "clear") {
        // `clear` maps to `!` (exclude.c:1290-1291), and `!` SKIPS the
        // modifier scan (`while (ch != '!' && ...)`, exclude.c:1365). The
        // refusal comes from the check AFTER it: with neither
        // FILTRULE_NO_PREFIXES nor XFLG_OLD_PREFIXES in play here, any
        // non-empty trailing text refuses (exclude.c:1467-1471).
        //
        // The accepted spellings are exactly `clear` and `clear,`: a `,`
        // terminator leaves `s` on the comma and the `if (*s) s++` step
        // (exclude.c:1444-1445) consumes it, so the token ends with `len == 0`.
        // MEASURED against rsync 3.5.0:
        //   filter = - foo clear,      -> rc 0, list cleared, all files served
        //   filter = - foo clear,- keep -> rc 5 "'!' rule has trailing characters: clear,- keep"
        //   filter = clear,x           -> rc 5 "'!' rule has trailing characters: clear,x"
        //   filter = clear_            -> rc 5 "'!' rule has trailing characters: clear_"
        // oc built the clear rule and IGNORED the trailer, so `- foo clear,- keep`
        // wiped the exclude and served every file of a module upstream refuses.
        //
        // Only the token's FIRST whitespace-delimited word decides: upstream's
        // word-split loop hands later words to parse_rule_tok as fresh tokens
        // (`clear P keep` is a clear plus a valid `P keep` rule, measured
        // rc 0), and oc's splitter glues such words onto this token - that
        // gluing is the tracked bare-word/merge gap, not this refusal's.
        let trailer = rest
            .split(|c: char| c.is_ascii_whitespace())
            .next()
            .unwrap_or("");
        if trailer.is_empty() || trailer == "," {
            return Ok(Some(clear_list_rule()));
        }
        return Err(MalformedRule::ClearWithTrailingCharacters {
            token: token.to_owned(),
        });
    }

    // A bare token falls through to a literal exclude. ⚠ NOT upstream
    // behaviour: upstream refuses an unrecognised word ("Unknown filter rule",
    // exclude.c:1363) and honours `merge`/`dir-merge`, both of which land here
    // today. That gap is tracked on its own; refusing bare words before the
    // merge keywords grow an arm would turn `filter = merge FILE` - a config
    // upstream serves - into a refusal, which is why this arm must outlive
    // this commit unchanged.
    if token.is_empty() {
        return Ok(None);
    }
    Ok(Some(build_pattern_rule(token, false)))
}

/// A daemon filter token upstream's parser refuses.
///
/// Carries the offending token so the refusal names it, as upstream's
/// `"Invalid filter rule: %s"` does (`exclude.c:1130`).
/// A daemon filter token upstream's parser refuses.
///
/// ⚠ THREE variants, not one, because upstream reports three DIFFERENT things
/// and reaches them by three different routes. Collapsing them to a single
/// "invalid rule" message would lose the distinction upstream draws.
#[derive(Debug)]
enum MalformedRule {
    /// A rule character followed by a byte that is not a known modifier.
    ///
    /// upstream: `exclude.c:1373-1378` - the `default: invalid` arm of the
    /// modifier scan.
    InvalidModifier {
        modifier: char,
        position: usize,
        token: String,
    },
    /// A rule character with no pattern after it.
    ///
    /// upstream: `exclude.c:1474-1476` - `else if (!len && !CVS_IGNORE)`.
    UnexpectedEnd { token: String },
    /// A `!` clear rule carrying a pattern it cannot take.
    ///
    /// upstream: `exclude.c:1468-1470`.
    ClearWithTrailingCharacters { token: String },
}

impl MalformedRule {
    fn into_io_error(self) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, self.to_string())
    }
}

/// Renders upstream's diagnostic text.
///
/// `Display` is the SINGLE owner of the wording so the message a test reads
/// and the message [`MalformedRule::into_io_error`] delivers cannot drift
/// apart.
///
/// Two of the three go through upstream's `filter_rule_err`, which renders
/// `"{msg}: {rule text}"` and then `exit_cleanup(RERR_SYNTAX)`
/// (`exclude.c:133-137`); the modifier arm builds its own line at
/// `exclude.c:1373-1378`.
///
/// These variants render the token VERBATIM, and that matches upstream: they
/// are reached only from the `filter` STRING parameter, whose text is the
/// operator's own configuration. Upstream's `rule_text()` chokepoint redacts
/// only text that came out of a file's contents (`TEXT_FROM_FILE`,
/// `exclude.c:67-69`), and `rule_src_in_file` is 0 while `parse_filter_str`
/// runs on `lp_filter` (clientserver.c:933-935) - so no `<rule from ...>`
/// envelope applies here. The FILE parameters' provenance lives in
/// `unexpected_end_of_filter_rule` via [`filters::RuleSource`].
impl std::fmt::Display for MalformedRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidModifier {
                modifier,
                position,
                token,
            } => write!(
                f,
                "invalid modifier '{modifier}' at position {position} in filter rule: {token}"
            ),
            Self::UnexpectedEnd { token } => {
                write!(f, "unexpected end of filter rule: {token}")
            }
            Self::ClearWithTrailingCharacters { token } => {
                write!(f, "'!' rule has trailing characters: {token}")
            }
        }
    }
}

/// Strips a keyword from a token, returning the remainder WITH its terminator.
///
/// Returns `None` when the token does not start with the keyword, or starts
/// with it but is not TERMINATED by it - `hideout` is not a `hide` rule.
///
/// upstream: `exclude.c:1218-1227` `rule_strcmp`, via [`is_keyword_terminator`].
/// The set an earlier version used was `' '` or `','` alone, so `_` and tab -
/// both separators upstream accepts - made the token not-a-keyword and it fell
/// through to the bare-pattern arm.
///
/// The terminator itself STAYS in the returned remainder because the two
/// terminator classes lead to different places, and `rule_strcmp` says so with
/// the address it returns:
///
/// ```c
/// if (isspace(str[rule_len]) || str[rule_len] == '_' || !str[rule_len])
///         return str + rule_len - 1;
/// if (str[rule_len] == ',')
///         return str + rule_len;
/// ```
///
/// The `- 1` for a space/`_`/end terminator puts `s` one byte BEFORE it, so the
/// modifier scan's first `*++s` lands on the terminator and stops immediately:
/// a multi-char keyword can carry modifiers ONLY through the `,` form.
///
/// ⚠ The callers trim a whole RUN of separators off the pattern, where upstream
/// consumes exactly ONE (`if (*s) s++`, `exclude.c:1444-1445`). MEASURED for
/// this directive against a real rsync 3.5.0 daemon, and the question turns out
/// to be UNASKABLE here rather than answered either way:
///
/// - `filter = exclude bar` (ONE space) builds the pattern `bar`, NOT ` bar`.
///   Discriminated with a module holding both `bar` and ` bar`: upstream hides
///   `bar` and serves ` bar`, and oc agrees. So on the only shape where the
///   two readings differ observably, oc is faithful.
/// - `filter = exclude  bar` (TWO spaces) and `filter = exclude\tbar` make
///   upstream REFUSE the module - `unexpected end of filter rule` - because
///   the word-split loop (`exclude.c:1250-1255`) splits on the whitespace RUN
///   first, leaving the keyword with no pattern at all. Neither candidate
///   pattern is ever built.
///
/// So a doubled separator cannot produce a divergent PATTERN in this mode; it
/// produces a refusal upstream and an accepted rule in oc. That residual gap
/// belongs to the gluing splitter above, which is tracked with the bare-word
/// class, not to this function's run-vs-one-separator behaviour.
fn strip_matched_keyword<'a>(token: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = token.strip_prefix(keyword)?;
    match rest.chars().next() {
        None => Some(rest),
        Some(ch) if is_keyword_terminator(ch) => Some(rest),
        Some(_) => None,
    }
}

/// What a filter-rule prefix - long keyword or short character - decides.
///
/// upstream: the second `switch (ch)` of `parse_rule_tok`
/// (`exclude.c:1331-1364`). The long keywords are pure aliases for the short
/// characters (`exclude.c:1288-1330`), so ONE table describes both.
#[derive(Clone, Copy)]
struct RulePrefix {
    /// `FILTRULE_INCLUDE` (`+`, `S`, `R`).
    is_include: bool,
    /// `FILTRULE_SENDER_SIDE` (`H`, `S`).
    sender_side: bool,
    /// `prefix_specifies_side` (`H`, `S`, `P`, `R`), which makes `C`, `r` and
    /// `s` invalid modifiers (`exclude.c:1403-1404`, `:1424-1425`, `:1429-1430`).
    specifies_side: bool,
    /// True for the SHORT characters that a bare pattern could also start
    /// with. See the failed-scan arm in [`parse_daemon_filter_token`].
    competes_with_bare_words: bool,
}

/// The long-form keywords, in upstream's own mapping order.
const KEYWORD_PREFIXES: &[(&str, RulePrefix)] = &[
    ("exclude", SHORT_EXCLUDE),
    ("include", SHORT_INCLUDE),
    ("hide", SHORT_HIDE),
    ("show", SHORT_SHOW),
    ("protect", SHORT_PROTECT),
    ("risk", SHORT_RISK),
];

const SHORT_EXCLUDE: RulePrefix = RulePrefix {
    is_include: false,
    sender_side: false,
    specifies_side: false,
    competes_with_bare_words: false,
};
const SHORT_INCLUDE: RulePrefix = RulePrefix {
    is_include: true,
    ..SHORT_EXCLUDE
};
const SHORT_HIDE: RulePrefix = RulePrefix {
    sender_side: true,
    specifies_side: true,
    competes_with_bare_words: true,
    ..SHORT_EXCLUDE
};
const SHORT_SHOW: RulePrefix = RulePrefix {
    is_include: true,
    ..SHORT_HIDE
};
const SHORT_PROTECT: RulePrefix = RulePrefix {
    sender_side: false,
    ..SHORT_HIDE
};
const SHORT_RISK: RulePrefix = RulePrefix {
    is_include: true,
    ..SHORT_PROTECT
};

/// Matches the one-character rule prefixes upstream's `default:` arm accepts.
///
/// upstream: `exclude.c:1325-1329` makes any single character a rule
/// character, and the second switch (`exclude.c:1331-1364`) gives
/// `+ - S H R P` their meanings. `.`, `:` and `!` are the merge and clear
/// characters; oc handles `!` on its own arm and does not implement the merge
/// characters at all, so they are deliberately absent here - adding them would
/// be the merge change, not this one.
///
/// ⚠ `competes_with_bare_words` is what keeps the four UPPERCASE characters
/// from swallowing the bare-word fall-through; only a token whose modifier run
/// VALIDATES is taken as a rule.
fn short_rule_prefix(token: &str) -> Option<(RulePrefix, char)> {
    let ch = token.chars().next()?;
    let prefix = match ch {
        '+' => SHORT_INCLUDE,
        '-' => SHORT_EXCLUDE,
        'S' => SHORT_SHOW,
        'H' => SHORT_HIDE,
        'R' => SHORT_RISK,
        'P' => SHORT_PROTECT,
        _ => return None,
    };
    Some((prefix, ch))
}

/// Returns true when `s` opens with a one-character rule prefix whose modifier
/// run is valid - the token-boundary question, asked through the same two
/// helpers the parser uses so the two cannot drift apart.
fn opens_short_rule(s: &str) -> bool {
    let Some((prefix, ch)) = short_rule_prefix(s) else {
        return false;
    };
    let rest = &s[ch.len_utf8()..];
    // A `,` terminates the prefix outright (`rule_strcmp`, exclude.c:1224-1225), so
    // the token IS a rule token even when its modifier run is one upstream
    // refuses - the same split the parser's failed-scan arm makes.
    rest.starts_with(',') || scan_modifiers(rest, prefix.specifies_side, 1, s).is_ok()
}

/// The flags upstream's modifier scan can raise on a daemon `filter` rule.
#[derive(Clone, Copy, Default)]
struct RuleModifiers {
    /// `/` - `FILTRULE_ABS_PATH` (`exclude.c:1392-1394`).
    abs_path: bool,
    /// `s` - `FILTRULE_SENDER_SIDE` (`exclude.c:1428-1432`).
    sender_side: bool,
    /// `!`, `x` or `C`: upstream ACCEPTS these and gives them meanings this
    /// daemon path cannot express. See [`scan_modifiers`].
    inexpressible: bool,
}

/// Runs upstream's modifier scan over `run` and reports what it raised.
///
/// upstream: `exclude.c:1365-1443`
///
/// ```c
/// while (ch != '!' && *++s && *s != ' ' && *s != '_') {
/// ```
///
/// The scan reads every byte up to the first space, `_` or end of token as a
/// MODIFIER character; `base` is the offset of `run[0]` within `token`, which
/// is what upstream's `(int)(s - (const uchar *)*rulestr_ptr)` reports.
/// Returns the flags and the byte length of the run consumed, so the caller can
/// step over the one separator `if (*s) s++` (`exclude.c:1444-1445`) eats and
/// take the PATTERN from what is left - the split oc previously did not make,
/// gluing `r keep` into the pattern of `filter = exclude,r keep` where upstream
/// builds an exclude of `keep`.
///
/// Modifiers this daemon path cannot express are flagged rather than refused,
/// because upstream ACCEPTS all three and refusing them would break configs it
/// serves. MEASURED against rsync 3.5.0 (module `foo`+`bar`+`keep`+`ctl`,
/// 3.5.0 client, loopback TCP, both directions):
///
/// - `filter = exclude,x keep` - rc 0, `keep` SERVED. `x` makes the rule match
///   xattr names only (`FILTRULE_XATTR`), so it never matches the file. The
///   daemon's own `xattr_only` bit is read on the sender path only
///   (`generator/filters.rs`) and dropped on the receiver path
///   (`receiver/pipeline_setup.rs`), so setting it would hide `keep` on a push.
/// - `filter = exclude,C keep` - rc 0, `keep` SERVED. `C` turns the rule into a
///   CVS-ignore rule whose OWN pattern is never matched (`check_filter`
///   short-circuits on `FILTRULE_CVS_IGNORE`, `exclude.c:1201-1206`).
/// - `filter = exclude,! keep` - rc 0, ONLY `keep` served. `!` negates, and the
///   `negate` bit has the same sender-only plumbing as `xattr_only`.
///
/// Dropping the rule reproduces upstream EXACTLY for `x` and `C`. For `!` it
/// does not: upstream serves only `keep` where oc then serves everything. That
/// residual is left open rather than half-closed, because expressing `!` needs
/// the receiver-side plumbing this change does not touch, and a half-expressed
/// `!` would INVERT the served set on a push.
fn scan_modifiers(
    run: &str,
    specifies_side: bool,
    base: usize,
    token: &str,
) -> Result<(RuleModifiers, usize), MalformedRule> {
    let mut modifiers = RuleModifiers::default();
    for (offset, ch) in run.char_indices() {
        // upstream stops at `' '` or `'_'`, and `FILTRULE_WORD_SPLIT` - which
        // every daemon module directive sets (clientserver.c:934) - stops at
        // any `isspace` byte too (exclude.c:1366-1368).
        if ch == '_' || ch.is_ascii_whitespace() {
            return Ok((modifiers, offset));
        }
        match ch {
            '/' => modifiers.abs_path = true,
            // `p` is FILTRULE_PERISHABLE, which steers --delete only and never
            // the name match this list performs.
            'p' => {}
            '!' | 'x' => modifiers.inexpressible = true,
            'C' if !specifies_side => modifiers.inexpressible = true,
            // `r`/`s` are the SIDE modifiers. `r` (receiver) is kept, matching
            // side-blind like `protect`; `s` (sender) is dropped at add time.
            // Both are invalid once the prefix already named a side.
            'r' if !specifies_side => {}
            's' if !specifies_side => modifiers.sender_side = true,
            _ => {
                return Err(MalformedRule::InvalidModifier {
                    modifier: ch,
                    position: base + offset,
                    token: token.to_owned(),
                });
            }
        }
    }
    Ok((modifiers, run.len()))
}

/// Turns a matched prefix plus its modifier run into a wire rule.
///
/// `run` is the text the modifier scan read, `used` how much of it the scan
/// consumed; the pattern is what follows the ONE separator upstream then eats
/// (`if (*s) s++`, `exclude.c:1444-1445`).
///
/// A rule that never reaches a pattern is REFUSED, on both the short-character
/// and the long-keyword path: upstream's `else if (!len && !CVS_IGNORE)`
/// (`exclude.c:1474-1476`) exits RERR_SYNTAX for `filter = -`, `filter = P`,
/// `filter = exclude` and `filter = hide,` alike. There is one answer here, not
/// one per path.
fn build_prefixed_rule(
    prefix: RulePrefix,
    modifiers: RuleModifiers,
    run: &str,
    used: usize,
    token: &str,
) -> Result<Option<FilterRuleWireFormat>, MalformedRule> {
    let pattern = run.get(used + 1..).unwrap_or("").trim();
    if pattern.is_empty() {
        return Err(MalformedRule::UnexpectedEnd {
            token: token.to_owned(),
        });
    }

    // The side test happens HERE, at add time, never at match time.
    // upstream: `add_rule` drops a rule whose side flags equal
    // `am_sender ? FILTRULE_RECEIVER_SIDE : FILTRULE_SENDER_SIDE` when the
    // parse passes XFLG_ANCHORED2ABS/XFLG_ABS_IF_SLASH (exclude.c:279-285),
    // and every daemon module directive passes XFLG_ABS_IF_SLASH
    // (clientserver.c:933-952). Those directives parse before
    // `parse_arguments` runs, so `am_sender` is still its static 0
    // WHATEVER role the transfer later takes: `hide`/`show`/`H`/`S` and the
    // `s` MODIFIER are dropped, `protect`/`risk`/`P`/`R` and the `r` modifier
    // are kept.
    //
    // The kept rules then match SIDE-BLIND. The only match-time side mechanism
    // upstream has is `elide` (exclude.c:1010), armed exclusively by
    // `send_rules` (exclude.c:1912) - the daemon list is never transmitted, so
    // its rules keep `elide = 0` forever. A kept `protect` therefore hides
    // files from a pulling client and refuses incoming writes alike, which is
    // why no side flag goes on the wire rule here.
    //
    // MEASURED against rsync 3.5.0, module `foo`+`bar`+`keep`+`ctl`:
    //   filter = P bar          -> list serves foo,keep,ctl; push refuses bar
    //   filter = H bar          -> both directions serve everything
    //   filter = exclude,r keep -> list serves foo,bar,ctl; push refuses keep
    //   filter = exclude,s keep -> both directions serve everything
    if prefix.sender_side || modifiers.sender_side || modifiers.inexpressible {
        return Ok(None);
    }

    let mut rule = build_pattern_rule(pattern, prefix.is_include);
    if modifiers.abs_path {
        // upstream: `/` presets FILTRULE_ABS_PATH, which makes add_rule's
        // XFLG_ABS_IF_SLASH branch a no-op and leaves the pattern anchored at
        // the module root (exclude.c:296-306). MEASURED: `filter = -/ keep`
        // hides the module-root `keep`.
        rule.anchored = true;
    }
    Ok(Some(rule))
}

/// Constructs a `FilterRuleWireFormat` from a pattern string.
///
/// Handles anchored patterns (leading `/`) and directory-only patterns
/// (trailing `/`). For daemon exclude rules on directory-only patterns,
/// applies the `XFLG_DIR2WILD3` transformation: the trailing `/` is replaced
/// with `/***` to recursively exclude the directory and all its contents.
///
/// upstream: exclude.c:211-217 - when `XFLG_DIR2WILD3` is set and the rule is
/// a directory-only exclude (not include), the `FILTRULE_DIRECTORY` flag is
/// cleared and `/***` is appended to the pattern.
fn build_pattern_rule(pattern: &str, is_include: bool) -> FilterRuleWireFormat {
    // upstream: exclude.c:200-202 - XFLG_ABS_IF_SLASH sets FILTRULE_ABS_PATH
    // when the pattern starts with '/' or contains any embedded '/'. Patterns
    // like "subdir/file.txt" are anchored relative to the module root.
    //
    // A pattern that starts with `**` is an exception: upstream sets
    // FILTRULE_WILD2_PREFIX independently of FILTRULE_ABS_PATH (exclude.c:241-242
    // checks the raw pattern's leading `**`), and the WILD2_PREFIX prepend
    // (exclude.c:929-931) is what lets `**/*.o` match a root-level `build.o`.
    // oc-rsync encodes anchoring by prepending `/` to the pattern, which would
    // turn `**/*.o` into `/**/*.o` and destroy the leading-`**` that WILD2_PREFIX
    // depends on. Since root-anchoring of a `**`-prefixed pattern is already
    // implied by WILD2_PREFIX, leave such patterns unanchored.
    let anchored =
        !pattern.starts_with("**") && (pattern.starts_with('/') || pattern.contains('/'));
    let directory_only = pattern.ends_with('/');

    // upstream: exclude.c:212-213 - XFLG_DIR2WILD3 applies only to
    // directory-only exclude rules (BITS_SETnUNSET(FILTRULE_DIRECTORY, FILTRULE_INCLUDE)).
    if directory_only && !is_include {
        let wild3_pattern = format!("{pattern}***");
        let mut rule = FilterRuleWireFormat::exclude(wild3_pattern);
        rule.anchored = anchored;
        rule.directory_only = false;
        rule
    } else if is_include {
        let mut rule = FilterRuleWireFormat::include(pattern.to_string());
        rule.anchored = anchored;
        rule.directory_only = directory_only;
        rule
    } else {
        let mut rule = FilterRuleWireFormat::exclude(pattern.to_string());
        rule.anchored = anchored;
        rule.directory_only = directory_only;
        rule
    }
}
