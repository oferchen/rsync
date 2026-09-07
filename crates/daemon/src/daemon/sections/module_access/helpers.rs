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
        let patterns = read_patterns_from_file(path)?;
        for pattern in patterns {
            rules.push(old_prefix_record_rule(&pattern, true)?);
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
        let patterns = read_patterns_from_file(path)?;
        for pattern in patterns {
            rules.push(old_prefix_record_rule(&pattern, false)?);
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
fn unexpected_end_of_filter_rule(rule: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("unexpected end of filter rule: {rule}"),
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
        return Err(unexpected_end_of_filter_rule(record));
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
            return Err(unexpected_end_of_filter_rule(rest));
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
fn read_patterns_from_file(path: &Path) -> Result<Vec<String>, io::Error> {
    let content = read_filter_file_contents(path).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("failed to read filter file '{}': {e}", path.display()),
        )
    })?;

    let patterns = filters::filter_file_records(&content)
        // `true`: these two parameters are the non-word-split templates, so a
        // leading `;`/`#` is a comment (`exclude.c:1806`, `word_split ||`).
        .filter(|line| filters::filter_file_line_is_rule(line, true))
        .map(str::to_owned)
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

    // Prefixes that start a new rule token when found after whitespace.
    const SHORT_PREFIXES: &[&str] = &["+ ", "- ", "+/", "-/"];

    /// Returns true if `s` starts with a filter rule prefix.
    ///
    /// The keyword arm asks `is_rule_keyword`, so a keyword terminated by any
    /// separator upstream accepts opens a new token. The table this replaced
    /// carried a MANDATORY TRAILING SPACE on every entry, so `hide_bar` and a
    /// line-final `clear` matched nothing, no token boundary opened, and the
    /// whole remainder collapsed into one rule whose pattern was the literal
    /// compound string.
    fn starts_with_rule_prefix(s: &str) -> bool {
        SHORT_PREFIXES.iter().any(|p| s.starts_with(p))
            || RULE_KEYWORDS.iter().any(|kw| is_rule_keyword(s, kw))
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
    // Short-form prefixes: `+` and `-` are RULE CHARACTERS, and upstream's
    // parser requires a separator after them. `parse_rule_tok` reads the
    // prefix, then `while (*s && *s != ' ')` scans modifier characters and
    // rejects any byte that is not a known modifier (exclude.c:1096-1131) -
    // so `-foo` refuses on `f`, it does not silently become an exclude of
    // `foo`.
    //
    // ⚠ The `.or_else(|| strip_prefix('+'))` fallback this replaces accepted
    // the separator-less spelling and, for `-foo`, SILENTLY EXCLUDED `foo`
    // from a module whose config upstream refuses to serve at all.
    if let Some(pattern) = token.strip_prefix("+ ") {
        return Ok(non_empty_pattern_rule(pattern, true));
    }
    if let Some(pattern) = token.strip_prefix("- ") {
        return Ok(non_empty_pattern_rule(pattern, false));
    }
    // The `+ `/`- ` arms above consumed the separator spelling, so a `+`/`-`
    // reaching here is one upstream does NOT read as a bare include/exclude.
    // Two upstream arms split on what follows it.
    if let Some(rest) = token.strip_prefix(['+', '-']) {
        return Err(match rest.chars().next() {
            // `-foo`: upstream reads the rule character, then scans MODIFIER
            // characters up to the first space and rejects any byte that is
            // not a known modifier (exclude.c:1364-1379). `f` is not one, so
            // it refuses at position 1.
            //
            // ⚠ oc accepted this spelling and SILENTLY EXCLUDED `foo` from a
            // module upstream refuses to serve at all.
            Some(c) => MalformedRule::InvalidModifier {
                modifier: c,
                position: 1,
                token: token.to_owned(),
            },
            // A BARE `-` or `+`: the modifier scan ends immediately, leaving
            // an empty pattern, and upstream's `else if (!len ...)` arm
            // refuses (exclude.c:1474-1476).
            None => MalformedRule::UnexpectedEnd {
                token: token.to_owned(),
            },
        });
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

    // upstream: exclude.c:1134-1178 - keyword-to-short-form mapping.
    // (keyword, is_include, sender_side, receiver_side)
    const KEYWORDS: &[(&str, bool, bool, bool)] = &[
        ("exclude", false, false, false),
        ("include", true, false, false),
        ("hide", false, true, false),    // sender-side exclude
        ("show", true, true, false),     // sender-side include
        ("protect", false, false, true), // receiver-side exclude
        ("risk", true, false, true),     // receiver-side include
    ];

    for &(keyword, is_include, sender, receiver) in KEYWORDS {
        if let Some(pattern) = strip_keyword_prefix(token, keyword) {
            let pattern = pattern.trim();
            if pattern.is_empty() {
                return Ok(None);
            }
            let mut rule = build_pattern_rule(pattern, is_include);
            rule.sender_side = sender;
            rule.receiver_side = receiver;
            return Ok(Some(rule));
        }
    }

    if strip_keyword_prefix(token, "clear").is_some() {
        return Ok(Some(clear_list_rule()));
    }

    // Bare pattern defaults to exclude (upstream behaviour)
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
/// ⚠ SCOPE: what this change was measured on is the REFUSAL - whether the
/// module is served - not the wording. The daemon's filter diagnostics also
/// lack upstream's `<rule from FILE line N>` provenance envelope, which is
/// task 1156 and deliberately NOT attempted here.
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

/// Returns a rule if the trimmed pattern is non-empty, `None` otherwise.
fn non_empty_pattern_rule(pattern: &str, is_include: bool) -> Option<FilterRuleWireFormat> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return None;
    }
    Some(build_pattern_rule(pattern, is_include))
}

/// Strips a keyword prefix from a token, returning the remainder.
///
/// Returns `None` when the token does not start with the keyword, or starts
/// with it but is not TERMINATED by it - `hideout` is not a `hide` rule.
///
/// upstream: `exclude.c:1218-1227` `rule_strcmp`, via [`is_keyword_terminator`].
/// The set this replaced was `' '` or `','` alone, so `_` and tab - both
/// separators upstream accepts - made the token not-a-keyword and it fell
/// through to the bare-pattern arm.
///
/// ⚠ The trailing `trim_start` consumes a whole RUN of whitespace, where
/// upstream consumes exactly ONE separator (`if (*s) s++`,
/// `exclude.c:1444-1445`) and takes the remainder verbatim - a rule oc's CLI
/// parser measured against rsync 3.5.0 for the NON-word-split `--filter`.
///
/// MEASURED for THIS directive against a real rsync 3.5.0 daemon, and the
/// question turns out to be UNASKABLE here rather than answered either way:
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
/// produces a refusal upstream and an accepted rule in oc. That gap belongs to
/// the daemon's broader accept-where-upstream-refuses class, which is tracked
/// separately, not to this function's run-vs-one-separator behaviour. The
/// `trim_start` is therefore left as it is, now on measurement rather than on
/// the absence of one.
fn strip_keyword_prefix<'a>(token: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = token.strip_prefix(keyword)?;
    let mut chars = rest.chars();
    match chars.next() {
        None => Some(rest),
        Some(ch) if is_keyword_terminator(ch) => Some(chars.as_str().trim_start()),
        Some(_) => None,
    }
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
