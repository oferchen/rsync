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
    //
    // A `merge` token expands to the rules of the named FILE, which is why the
    // loop pushes through [`push_token_rules`] rather than taking one rule per
    // token. `module.path` is upstream's `dirbuf`: `set_filter_dir(module_dir,
    // module_dirlen)` runs immediately before this parse (clientserver.c:922-926)
    // and is what `parse_merge_name()` prepends to a slash-bearing relative name.
    for filter_str in &module.filter {
        for token in split_filter_tokens(filter_str.trim()) {
            push_token_rules(&mut rules, &token, &module.path, 0, RuleXflags::Daemon)?;
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

/// The `xflags` a daemon-side filter parse carries.
///
/// upstream has exactly two shapes on this path and they differ in three
/// observable ways, so one flag describes both:
///
/// * [`Self::Daemon`] - `XFLG_ABS_IF_SLASH | XFLG_DIR2WILD3`, what the five
///   module directives pass (clientserver.c:933-952).
/// * [`Self::MergeFile`] - `XFLG_FATAL_ERRORS` alone, what the records of a
///   `merge` file get (`parse_filter_file(listp, p, rule, XFLG_FATAL_ERRORS)`,
///   exclude.c:1587).
///
/// `add_rule` gates the embedded-slash anchoring (exclude.c:298-306), the
/// `/***` directory suffix (exclude.c:311-317) and the add-time side drop
/// (exclude.c:279-285) on the first shape's bits, so none of the three applies
/// to a merge file's rules. MEASURED against rsync 3.5.0: a module whose
/// `filter = H bait` serves `bait`, while the same rule reached through
/// `filter = merge FILE` HIDES it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RuleXflags {
    /// `XFLG_ABS_IF_SLASH | XFLG_DIR2WILD3`.
    Daemon,
    /// `XFLG_FATAL_ERRORS`.
    MergeFile,
}

/// upstream: `exclude.c:168` - the merge-file recursion limit.
const MAX_MERGE_DEPTH: usize = 32;

/// Appends every rule one filter token expands to.
///
/// A `merge` token is the only one that is not one rule: upstream replaces it
/// with the parsed contents of the named file (exclude.c:1581-1590), so this is
/// the seam where the recursion lives.
fn push_token_rules(
    rules: &mut Vec<FilterRuleWireFormat>,
    token: &str,
    root: &Path,
    depth: usize,
    xflags: RuleXflags,
) -> Result<(), io::Error> {
    if let Some(merge) = merge_rule(token, xflags).map_err(MalformedRule::into_io_error)? {
        return push_merge_file_rules(rules, &merge, root, depth);
    }
    if let Some(rule) =
        parse_daemon_filter_token(token, xflags).map_err(MalformedRule::into_io_error)?
    {
        rules.push(rule);
    }
    Ok(())
}

/// A `merge` / `.` rule: the file to read plus what its records inherit.
///
/// upstream: `FILTRULES_FROM_CONTAINER` (exclude.c:1229-1231) is what a merge
/// file's rules inherit from the merge rule - `FILTRULE_ABS_PATH`,
/// `FILTRULE_INCLUDE`, `FILTRULE_DIRECTORY`, `FILTRULE_NEGATE`,
/// `FILTRULE_PERISHABLE` - and `FILTRULE_NO_PREFIXES` additionally steers how
/// each record is read (exclude.c:1272-1274).
struct MergeRule<'a> {
    /// The merge-file name, before [`merge_file_path`] resolves it.
    name: &'a str,
    /// `-`, `+` or `C` - `FILTRULE_NO_PREFIXES`: every record is a bare pattern.
    no_prefixes: bool,
    /// `+` - `FILTRULE_INCLUDE`, inherited by each record.
    include: bool,
    /// `e` - `FILTRULE_EXCLUDE_SELF`: an exclude of the merge file's basename
    /// is added BEFORE the file is read (exclude.c:1553-1568).
    exclude_self: bool,
}

/// Classifies a token as an EAGER merge rule - `merge` / `.` - or `None`.
///
/// upstream: `exclude.c:1292-1294` maps `merge` to `.`, and `exclude.c:1336-1338`
/// gives it `FILTRULE_MERGE_FILE`. `parse_filter_str` reads such a file once, at
/// parse time (exclude.c:1581-1590), which is what this function feeds.
///
/// ⚠ THE PER-DIRECTORY SPELLINGS `:` / `dir-merge` ARE NOT EAGER, so they are
/// deliberately not answered here. `add_rule` registers every
/// `FILTRULE_PERDIR_MERGE` rule into the GLOBAL `mergelist_parents`
/// (exclude.c:349-391) whatever list it was added to, so a rule in
/// `daemon_filter_list` is registered too; `push_local_filters` then fills that
/// rule's own `u.mergelist` per directory and `check_filter` recurses into it.
/// MEASURED against rsync 3.5.0 (module with `sub/.rsync-filter` holding
/// `- bait.txt`): `filter = : .rsync-filter` HIDES `sub/bait.txt`, and so does
/// `filter = dir-merge rules` with the merge file and the bait at the module
/// root. That is a `RuleType::DirMerge` wire rule carried to the walk, not a
/// parse-time read, so [`short_rule_prefix`] and [`KEYWORD_PREFIXES`] own those
/// two spellings and [`build_prefixed_rule`] builds the rule.
///
/// ⚠ `.` is NOT a token-boundary opener in [`split_filter_tokens`], and
/// deliberately so - see [`opens_clear_rule`] for the same reasoning applied to
/// `!`. `filter = - .git` is ONE rule whose pattern is `.git`; opening a token
/// on the `.` would leave a patternless `-` and refuse the commonest daemon
/// filter there is. A `merge` token therefore has to lead its value, or follow
/// the `merge` KEYWORD spelling that [`RULE_KEYWORDS`] does open.
fn merge_rule<'a>(
    token: &'a str,
    xflags: RuleXflags,
) -> Result<Option<MergeRule<'a>>, MalformedRule> {
    let (rest, base) = if let Some(rest) = token.strip_prefix('.') {
        (rest, 1)
    } else if let Some(rest) = strip_matched_keyword(token, "merge") {
        (rest, "merge".len())
    } else {
        return Ok(None);
    };

    // `if (s[1] == ',') s++` (exclude.c:1327-1328) for the short characters and
    // the `,` arm of `rule_strcmp` (exclude.c:1224-1225) for the keywords: both
    // put the modifier scan one byte past the comma.
    let (run, base) = match rest.strip_prefix(',') {
        Some(after_comma) => (after_comma, base + 1),
        None => (rest, base),
    };
    let (modifiers, used) = scan_merge_modifiers(run, base, token)?;
    let name = rule_pattern(run, used, xflags);
    // upstream: exclude.c:1474-1476. `filter = merge` and `filter = merge,` are
    // both `unexpected end of filter rule` (MEASURED, rc 5). The `.cvsignore`
    // default at exclude.c:1583-1586 is unreachable from here: it needs
    // `FILTRULE_CVS_IGNORE`, which only the `C` modifier sets, and `C` implies a
    // pattern of its own.
    if name.is_empty() {
        return Err(MalformedRule::UnexpectedEnd {
            token: token.to_owned(),
        });
    }

    Ok(Some(MergeRule {
        name,
        no_prefixes: modifiers.no_prefixes,
        include: modifiers.include,
        exclude_self: modifiers.exclude_self,
    }))
}

/// What the modifier run of a merge rule raises.
#[derive(Clone, Copy, Default)]
struct MergeModifiers {
    no_prefixes: bool,
    include: bool,
    exclude_self: bool,
}

/// Runs upstream's modifier scan over a merge rule's run.
///
/// upstream: `exclude.c:1365-1443`. The alphabet is the same loop
/// [`scan_modifiers`] mirrors, read through `FILTRULE_MERGE_FILE`: `-`/`+`/`C`
/// set `FILTRULE_NO_PREFIXES` (and `+` adds `FILTRULE_INCLUDE`), `e`/`n`/`w`
/// become legal, and `!` becomes ILLEGAL - "negation really goes with the
/// pattern, so it isn't useful as a merge-file default" (exclude.c:1396-1399).
/// MEASURED: `filter = .! FILE` is rc 5, `invalid modifier '!' at position 1`.
///
/// ⚠ Several modifiers are ACCEPTED here and not expressed, because upstream
/// serves the module with them and refusing would break a configuration it
/// honours. All were MEASURED against rsync 3.5.0 (module holding `bait` +
/// `keep` + `ctl`, merge file holding `- bait`):
///
/// - `n` (`FILTRULE_NO_INHERIT`), `p` (`FILTRULE_PERISHABLE`), `x`
///   (`FILTRULE_XATTR`), `s`/`r` (the sides): rc 0 with `bait` hidden, i.e.
///   indistinguishable from a plain merge on the daemon list. `n` steers
///   per-directory inheritance only, `p` steers `--delete` only, and `x` plus
///   the sides ride on the merge rule itself, which upstream frees once the
///   file is read (exclude.c:1588). Ignoring them IS upstream's outcome.
/// - `/` (`FILTRULE_ABS_PATH`) is inherited by every record
///   (`FILTRULES_FROM_CONTAINER`, exclude.c:1229) and is still inert HERE: the
///   only thing `rule_matches` does with the bit is prepend `curr_dir` past the
///   module root (exclude.c:1022-1027), and the daemon list is checked with a
///   module-relative name. MEASURED: `filter = ./ FILE` and `filter = . FILE`
///   over a merge file holding `- sub/f` hide the same set - `sub/f` AND
///   `deep/sub/f`.
/// - `w` (`FILTRULE_WORD_SPLIT`) and `e` (`FILTRULE_EXCLUDE_SELF`) are
///   accepted and NOT reproduced; each has its own measured divergence
///   recorded at its site in [`push_merge_file_rules`].
fn scan_merge_modifiers(
    run: &str,
    base: usize,
    token: &str,
) -> Result<(MergeModifiers, usize), MalformedRule> {
    let mut modifiers = MergeModifiers::default();
    for (offset, ch) in run.char_indices() {
        if ch == '_' || ch.is_ascii_whitespace() {
            return Ok((modifiers, offset));
        }
        match ch {
            // upstream: exclude.c:1381-1391. `-` and `+` are rejected once
            // FILTRULE_NO_PREFIXES is already set (`BITS_SETnUNSET`).
            '-' | '+' | 'C' if modifiers.no_prefixes => {
                return Err(MalformedRule::InvalidModifier {
                    modifier: ch,
                    position: base + offset,
                    token: token.to_owned(),
                });
            }
            '-' => modifiers.no_prefixes = true,
            '+' => {
                modifiers.no_prefixes = true;
                modifiers.include = true;
            }
            // `C` also sets FILTRULE_WORD_SPLIT | FILTRULE_NO_INHERIT |
            // FILTRULE_CVS_IGNORE (exclude.c:1409-1415). MEASURED with a merge
            // file holding the bare word `bait`: rc 0, `bait` hidden - which
            // the NO_PREFIXES half alone reproduces. The CVS_IGNORE half also
            // loads the default cvsignore list (`get_cvs_excludes`,
            // exclude.c:1596-1598); that is NOT reproduced here.
            'C' => modifiers.no_prefixes = true,
            'e' => modifiers.exclude_self = true,
            // Accepted and inert - see this function's doc block.
            '/' | 'n' | 'p' | 'w' | 'x' | 'r' | 's' => {}
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

/// Appends the rules a merge file expands to.
///
/// upstream: `exclude.c:1553-1590` - the `FILTRULE_MERGE_FILE` arm of
/// `parse_filter_str`, which resolves the name, hands the file to
/// `parse_filter_file(listp, p, rule, XFLG_FATAL_ERRORS)` and then frees the
/// merge rule itself. The rules land in the SAME list, in file order, at the
/// point the merge rule occupied - so an earlier `filter = - ctl merge FILE`
/// keeps both, and a `!` record inside the file clears what came before it
/// (MEASURED: a file holding `- bait`, `!`, `- ctl` serves `bait` and hides
/// `ctl`).
///
/// A file that cannot be read REFUSES the module: `XFLG_FATAL_ERRORS` makes the
/// failed open `exit_cleanup(RERR_FILEIO)` (exclude.c:1714-1719). MEASURED:
/// `filter = merge /nonexistent` is rc 5 upstream, where oc served the module
/// with the operator's rules silently absent.
fn push_merge_file_rules(
    rules: &mut Vec<FilterRuleWireFormat>,
    merge: &MergeRule<'_>,
    root: &Path,
    depth: usize,
) -> Result<(), io::Error> {
    if merge.exclude_self {
        // upstream: exclude.c:1557-1567 adds an exclude of the merge file's
        // BASENAME before the file is read.
        //
        // ⚠ MEASURED DIVERGENCE. Upstream's own `parse_filter_file` then tests
        // the merge file against `daemon_filter_list` (exclude.c:1636-1657) and,
        // finding it hidden by the rule just added, treats it as non-existent -
        // so `filter = .e FILE` upstream adds the basename exclude and reads
        // NOTHING (rc 0, `bait` served). oc adds the exclude and still reads the
        // file, which hides strictly MORE than upstream. Reproducing the
        // self-suppression needs the daemon list's own matcher at parse time,
        // which this path does not have.
        let base = merge.name.rsplit('/').next().unwrap_or(merge.name);
        rules.push(build_pattern_rule(base, false, RuleXflags::MergeFile));
    }

    // upstream: exclude.c:1619-1628 - the depth check precedes the open, and is
    // fatal under XFLG_FATAL_ERRORS. MEASURED: a merge file naming itself is
    // rc 5 upstream, where oc-base served the module.
    if depth >= MAX_MERGE_DEPTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "merge-file include depth limit ({MAX_MERGE_DEPTH}) exceeded at {}",
                merge.name
            ),
        ));
    }

    let path = merge_file_path(merge.name, root);
    let content = read_filter_file_contents(&path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("failed to open exclude file '{}': {err}", path.display()),
        )
    })?;

    for record in filters::filter_file_records(&content) {
        // `true`: a plain merge is line-parsed, so a leading `;`/`#` is a
        // comment (`exclude.c:1806`, `word_split ||`).
        //
        // ⚠ MEASURED DIVERGENCE for the `w` modifier: upstream word-splits such
        // a file AND stops recognising comments. oc reads a `merge,w` file
        // line-by-line, so a file holding `- bait - ctl` becomes ONE rule whose
        // pattern is `bait - ctl` and every file is served, where upstream
        // refuses the module (`unexpected end of filter rule`, rc 5, because the
        // word-split leaves a patternless `-`).
        if !filters::filter_file_line_is_rule(record, true) {
            continue;
        }
        if merge.no_prefixes {
            rules.push(build_pattern_rule(
                record,
                merge.include,
                RuleXflags::MergeFile,
            ));
        } else {
            push_token_rules(rules, record, root, depth + 1, RuleXflags::MergeFile)?;
        }
    }

    Ok(())
}

/// Resolves a merge-file name the way `parse_merge_name()` does.
///
/// upstream: `exclude.c:696-753`, called with `prefix_skip == 0` and
/// `parent_dirscan == 0` for a daemon `filter` merge (exclude.c:1586). Three
/// cases, all MEASURED against rsync 3.5.0:
///
/// * absolute - used as written.
/// * relative WITH a slash - `dirbuf` (the module root) is prepended and the
///   result cleaned, so `merge ./rules` reads the module's own `rules`.
/// * relative WITHOUT a slash - returned unchanged (exclude.c:704-715), so the
///   open resolves against the daemon's working directory, NOT the module. The
///   daemon has not entered the module yet at this point in `rsync_module()`;
///   `filter = merge rules` is rc 5 `failed to open exclude file rules` even
///   with a `rules` file sitting in the module root.
fn merge_file_path(name: &str, root: &Path) -> PathBuf {
    let path = Path::new(name);
    if path.is_absolute() || !name.contains('/') {
        path.to_path_buf()
    } else {
        filters::collapse_dot_dot_dirs(&root.join(path))
    }
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
    Ok(build_pattern_rule(pattern, is_include, RuleXflags::Daemon))
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
            rules.push(build_pattern_rule(token, is_include, RuleXflags::Daemon));
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
/// Both merge spellings are openers here, and each is answered on its own path.
/// `merge` reaches [`merge_rule`], which [`push_token_rules`] consults before
/// the single-rule parser, because upstream replaces such a token with the
/// parsed contents of the file. `dir-merge` reaches [`KEYWORD_PREFIXES`], which
/// maps it to [`SHORT_DIR_MERGE`]: a per-directory rule is carried on the wire,
/// not read here.
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
        opens_short_rule(s)
            || opens_clear_rule(s)
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
fn parse_daemon_filter_token(
    token: &str,
    xflags: RuleXflags,
) -> Result<Option<FilterRuleWireFormat>, MalformedRule> {
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
        match scan_modifiers(run, prefix, base, token) {
            Ok((modifiers, used)) => {
                return build_prefixed_rule(prefix, modifiers, run, used, token, xflags);
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
            //
            // ⚠ NOR inside a merge FILE, where there is no bare-word
            // fall-through to compete with: upstream's own scan refuses the
            // token there (MEASURED, a merge file holding `Pictures` is rc 5
            // `invalid modifier 'i'`).
            Err(err) => {
                if !prefix.competes_with_bare_words
                    || rest.starts_with(',')
                    || xflags == RuleXflags::MergeFile
                {
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
    // The two accepted spellings are exactly `!` and `!,`, for the reason the
    // `clear` arm below records: the `,` is consumed as part of the prefix
    // (`if (s[1] == ',') s++`, exclude.c:1327-1328) and the `if (*s) s++` step
    // then leaves `len == 0`. MEASURED against rsync 3.5.0, module holding
    // `bait` + `keep` + `ctl` + a file literally named `!`:
    //   filter = !            -> rc 0, every file served, INCLUDING `!`
    //   filter = !,           -> rc 0, every file served
    //   filter = - bait !     -> rc 0, `bait` served: the clear wiped the exclude
    //   filter = !x           -> rc 5 "'!' rule has trailing characters"
    // oc-base implemented none of it: a bare `!` fell through to the
    // bare-pattern arm and became an EXCLUDE of the literal pattern `!`, so the
    // file named `!` vanished from a module upstream serves it from, while
    // `- bait !` kept hiding `bait`. `!,` was refused outright.
    if let Some(rest) = token.strip_prefix('!') {
        return clear_rule_or_error(token, rest);
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
            let (modifiers, used) = scan_modifiers(run, prefix, base, token)?;
            return build_prefixed_rule(prefix, modifiers, run, used, token, xflags);
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
        return clear_rule_or_error(token, rest);
    }

    if token.is_empty() {
        return Ok(None);
    }

    // A bare token falls through to a literal exclude. ⚠ NOT upstream
    // behaviour: upstream refuses an unrecognised word ("Unknown filter rule",
    // exclude.c:1363). The arm survives for the DAEMON directive only, where
    // oc's splitter glues a trailing word onto the rule before it and refusing
    // would turn configurations upstream serves into refusals.
    //
    // Inside a merge FILE there is no splitter and no gluing - each record is
    // one rule - so upstream's refusal is reproduced there instead. MEASURED: a
    // merge file holding the bare word `bait` is rc 5 upstream, `Unknown filter
    // rule: <rule from FILE line 1>`, where oc-base excluded `bait`.
    if xflags == RuleXflags::MergeFile {
        return Err(MalformedRule::UnknownRule {
            token: token.to_owned(),
        });
    }
    Ok(Some(build_pattern_rule(token, false, xflags)))
}

/// Builds the clear rule a `!` or `clear` token names, or upstream's refusal.
///
/// `rest` is what follows the rule character or keyword, with its terminator.
/// upstream: `exclude.c:1467-1471` - with neither `FILTRULE_NO_PREFIXES` nor
/// `XFLG_OLD_PREFIXES` in play, any surviving pattern text refuses.
///
/// Only the token's FIRST whitespace-delimited word decides, because upstream's
/// word-split loop hands later words to `parse_rule_tok` as fresh tokens
/// (`clear P keep` is a clear plus a valid `P keep` rule, MEASURED rc 0). Where
/// oc's splitter glues such a word on instead, it is dropped here - that gluing
/// is the tracked bare-word residual, not this refusal's.
fn clear_rule_or_error(
    token: &str,
    rest: &str,
) -> Result<Option<FilterRuleWireFormat>, MalformedRule> {
    let trailer = rest
        .split(|c: char| c.is_ascii_whitespace())
        .next()
        .unwrap_or("");
    if trailer.is_empty() || trailer == "," {
        return Ok(Some(clear_list_rule()));
    }
    Err(MalformedRule::ClearWithTrailingCharacters {
        token: token.to_owned(),
    })
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
    /// A record of a merge file that names no rule upstream knows.
    ///
    /// upstream: `exclude.c:1363` - the `default:` arm of the second switch.
    UnknownRule { token: String },
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
            Self::UnknownRule { token } => write!(f, "Unknown filter rule: {token}"),
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
    /// `FILTRULE_MERGE_FILE` (`:`, `dir-merge`), which is what opens the
    /// merge-only half of the modifier alphabet - `-`, `+`, `e`, `n` and `w`
    /// are `goto invalid` without it, and `!` is `goto invalid` WITH it
    /// (`exclude.c:1381-1440`).
    merge_file: bool,
}

/// The long-form keywords, in upstream's own mapping order.
const KEYWORD_PREFIXES: &[(&str, RulePrefix)] = &[
    ("exclude", SHORT_EXCLUDE),
    ("include", SHORT_INCLUDE),
    ("hide", SHORT_HIDE),
    ("show", SHORT_SHOW),
    ("protect", SHORT_PROTECT),
    ("risk", SHORT_RISK),
    // upstream: `exclude.c:1310-1311` maps `dir-merge` to `:`.
    ("dir-merge", SHORT_DIR_MERGE),
];

const SHORT_EXCLUDE: RulePrefix = RulePrefix {
    is_include: false,
    sender_side: false,
    specifies_side: false,
    competes_with_bare_words: false,
    merge_file: false,
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
/// `:` / `dir-merge` - `FILTRULE_PERDIR_MERGE | FILTRULE_MERGE_FILE`.
///
/// upstream: `exclude.c:1331-1338` - `case ':'` sets `FILTRULE_PERDIR_MERGE`
/// plus `FILTRULE_FINISH_SETUP` and FALLS THROUGH to `case '.'`, which is what
/// adds `FILTRULE_MERGE_FILE`. It names no side, so `specifies_side` stays
/// false and the full `r`/`s`/`C` half of the alphabet remains open.
///
/// `competes_with_bare_words` is FALSE: a failed modifier scan REFUSES rather
/// than falling back to a bare-word exclude. Upstream's scan sends any byte
/// outside the alphabet to `invalid:` -> `"invalid modifier '%c' at position
/// %d"` and `RERR_SYNTAX` (`exclude.c:1370-1379`), so `:` belongs to the
/// `+`/`-` class, not to the `P R S H` class that competes with bare words.
const SHORT_DIR_MERGE: RulePrefix = RulePrefix {
    merge_file: true,
    ..SHORT_EXCLUDE
};

/// Matches the one-character rule prefixes upstream's `default:` arm accepts.
///
/// upstream: `exclude.c:1325-1329` makes any single character a rule
/// character, and the second switch (`exclude.c:1331-1364`) gives
/// `+ - S H R P :` their meanings. `!` is the clear character and oc handles it
/// on its own arm. `.` - the EAGER `merge` character - is absent because it is
/// answered EARLIER: [`push_token_rules`] consults [`merge_rule`] first, which
/// reads the named file at parse time, so a `.` token never reaches this
/// function.
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
        // upstream: exclude.c:1331-1338.
        ':' => SHORT_DIR_MERGE,
        _ => return None,
    };
    Some((prefix, ch))
}

/// Returns true when `s` opens a `!` clear rule, and nothing else.
///
/// upstream: `exclude.c:1325-1329` reads a leading `!` as a rule character
/// unconditionally, and `FILTRULE_WORD_SPLIT` makes every whitespace-delimited
/// word its own rule (exclude.c:1250-1255) - so `filter = - bait !` really is an
/// exclude followed by a list clear.
///
/// ⚠ The test is deliberately NARROWER than upstream's: a token opens only when
/// the `!` is the whole word, in one of the two spellings `parse_rule_tok`
/// accepts as a clear (`!`, and the `!,` whose comma the prefix consumes).
/// oc's splitter decides boundaries by looking at the word AFTER whitespace and
/// cannot tell a rule character from the PATTERN of the rule before it.
/// MEASURED against rsync 3.5.0: `filter = - !bait` is ONE rule whose pattern is
/// `!bait` (rc 0, every file served). Opening a token on every leading `!` would
/// split that into a patternless `-` and refuse a config upstream serves.
fn opens_clear_rule(s: &str) -> bool {
    let word = s
        .split(|ch: char| ch.is_ascii_whitespace())
        .next()
        .unwrap_or("");
    word == "!" || word == "!,"
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
    rest.starts_with(',') || scan_modifiers(rest, prefix, 1, s).is_ok()
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
    /// `r` - `FILTRULE_RECEIVER_SIDE` (`exclude.c:1424-1427`).
    ///
    /// Kept rather than ignored: a `:r` dir-merge is receiver-side, which
    /// `DirMergeConfig::with_receiver_only` expresses. On a non-merge rule the
    /// daemon list matches side-blind, so the bit stays unread there.
    receiver_side: bool,
    /// `p` - `FILTRULE_PERISHABLE` (`exclude.c:1421-1423`).
    perishable: bool,
    /// `-` or `+` - `FILTRULE_NO_PREFIXES`; merge rules only
    /// (`exclude.c:1381-1391`).
    no_prefixes: bool,
    /// The `+` variant of [`Self::no_prefixes`], which also sets
    /// `FILTRULE_INCLUDE` on each record.
    no_prefixes_include: bool,
    /// `n` - `FILTRULE_NO_INHERIT`; merge rules only (`exclude.c:1415-1418`).
    no_inherit: bool,
    /// `e` - `FILTRULE_EXCLUDE_SELF`; merge rules only
    /// (`exclude.c:1411-1414`).
    exclude_self: bool,
    /// `w` - `FILTRULE_WORD_SPLIT`; merge rules only (`exclude.c:1433-1436`).
    word_split: bool,
    /// `C` - `FILTRULE_CVS_IGNORE` on a MERGE rule, where it is expressible as
    /// `DirMergeConfig`'s CVS mode. On a non-merge rule the same character sets
    /// [`Self::inexpressible`] instead; see [`scan_modifiers`].
    cvs_ignore: bool,
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
///
/// A MERGE prefix (`:`, `dir-merge`) opens the merge-only half of the alphabet
/// and closes one character. upstream gates each arm on `FILTRULE_MERGE_FILE`:
/// `-`/`+` (`exclude.c:1381-1391`), `e` (`:1411-1414`), `n` (`:1415-1418`) and
/// `w` (`:1433-1436`) are `goto invalid` WITHOUT it, and `!` is `goto invalid`
/// WITH it - "negation really goes with the pattern, so it isn't useful as a
/// merge-file default" (`exclude.c:1404-1410`). `C` stops being inexpressible
/// there too, because `DirMergeConfig` has a CVS mode for it.
fn scan_modifiers(
    run: &str,
    prefix: RulePrefix,
    base: usize,
    token: &str,
) -> Result<(RuleModifiers, usize), MalformedRule> {
    let specifies_side = prefix.specifies_side;
    let merge = prefix.merge_file;
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
            // `p` is FILTRULE_PERISHABLE. On a non-merge rule it steers
            // --delete only and never the name match this list performs; a
            // merge rule passes it down to every record it reads.
            'p' => modifiers.perishable = true,
            // `!` is NEGATE on a plain rule and INVALID on a merge rule.
            '!' if merge => {
                return Err(MalformedRule::InvalidModifier {
                    modifier: ch,
                    position: base + offset,
                    token: token.to_owned(),
                });
            }
            // `x` is FILTRULE_XATTR. It is NOT in `FILTRULES_FROM_CONTAINER`
            // (exclude.c:1229-1231), so a merge rule does not pass it down to
            // the records it reads, and upstream frees the merge rule itself.
            // The eager `merge` arm therefore accepts it and expresses nothing
            // (MEASURED: `filter = .x FILE` hides what the file names); a
            // per-directory merge takes the SAME reading, rather than dropping
            // the whole rule as `inexpressible` and honouring no merge at all.
            'x' if merge => {}
            '!' | 'x' => modifiers.inexpressible = true,
            'C' if merge => modifiers.cvs_ignore = true,
            'C' if !specifies_side => modifiers.inexpressible = true,
            // The merge-only arms. Each is `goto invalid` without
            // FILTRULE_MERGE_FILE, so they must stay behind the guard.
            '-' if merge => modifiers.no_prefixes = true,
            '+' if merge => {
                modifiers.no_prefixes = true;
                modifiers.no_prefixes_include = true;
            }
            'n' if merge => modifiers.no_inherit = true,
            'e' if merge => modifiers.exclude_self = true,
            'w' if merge => modifiers.word_split = true,
            // `r`/`s` are the SIDE modifiers. `s` (sender) is dropped at add
            // time. `r` (receiver) is recorded, but only a merge rule reads it:
            // a kept non-merge daemon rule matches side-blind, like `protect`.
            // Both are invalid once the prefix already named a side.
            'r' if !specifies_side => modifiers.receiver_side = true,
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
    xflags: RuleXflags,
) -> Result<Option<FilterRuleWireFormat>, MalformedRule> {
    let pattern = rule_pattern(run, used, xflags);
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
    //
    // ⚠ NOT inside a merge FILE. That add-time drop is gated on
    // `XFLG_ANCHORED2ABS|XFLG_ABS_IF_SLASH` (exclude.c:279-285) and a merge
    // file's records carry `XFLG_FATAL_ERRORS` alone, so upstream KEEPS a
    // sender-side rule there and matches it side-blind. MEASURED against rsync
    // 3.5.0: `filter = H bait` serves `bait`, while a merge file holding
    // `H bait` HIDES it.
    let side_dropped =
        xflags == RuleXflags::Daemon && (prefix.sender_side || modifiers.sender_side);
    if side_dropped || modifiers.inexpressible {
        return Ok(None);
    }

    // A MERGE prefix names a FILE, not a pattern, so it must not go through
    // `build_pattern_rule`: that helper's anchoring and XFLG_DIR2WILD3 rewrites
    // are for match patterns and would turn `: sub/.rsync-filter` into an
    // anchored rule and `: rules/` into `rules/***`. upstream skips both for a
    // merge rule - `add_rule`'s ABS_PATH-from-slash branch tests
    // `!(rule->rflags & (FILTRULE_ABS_PATH | FILTRULE_MERGE_FILE))`
    // (exclude.c:297-300), so an embedded slash in a merge FILENAME sets
    // nothing, and only the explicit `/` MODIFIER does.
    if prefix.merge_file {
        return Ok(Some(FilterRuleWireFormat {
            rule_type: protocol::filters::RuleType::DirMerge,
            pattern: pattern.into(),
            anchored: modifiers.abs_path,
            no_inherit: modifiers.no_inherit,
            cvs_exclude: modifiers.cvs_ignore,
            word_split: modifiers.word_split,
            exclude_from_merge: modifiers.exclude_self,
            receiver_side: modifiers.receiver_side,
            perishable: modifiers.perishable,
            no_prefixes: modifiers.no_prefixes,
            no_prefixes_include: modifiers.no_prefixes_include,
            ..FilterRuleWireFormat::default()
        }));
    }

    let mut rule = build_pattern_rule(pattern, prefix.is_include, xflags);
    if modifiers.abs_path {
        // upstream: `/` presets FILTRULE_ABS_PATH, which makes add_rule's
        // XFLG_ABS_IF_SLASH branch a no-op and leaves the pattern anchored at
        // the module root (exclude.c:296-306). MEASURED: `filter = -/ keep`
        // hides the module-root `keep`.
        rule.anchored = true;
    }
    Ok(Some(rule))
}

/// Takes the pattern that follows a rule prefix and its modifier run.
///
/// upstream consumes exactly ONE separator (`if (*s) s++`, exclude.c:1444-1445)
/// and then takes the rest of the token: `strlen(s)` for a line-parsed file
/// (exclude.c:1465), or up to the next whitespace under `FILTRULE_WORD_SPLIT`.
///
/// The trim is therefore a DAEMON-only step, and it stays because oc's splitter
/// leaves the token's own trailing whitespace in place. A merge file's record is
/// taken verbatim: upstream's pattern there runs to the end of the line, so
/// trailing whitespace is pattern text. MEASURED against rsync 3.5.0 - a merge
/// file holding `- bait ` (one trailing space), over a module holding `bait`,
/// serves `bait`; trimming would hide it.
fn rule_pattern(run: &str, used: usize, xflags: RuleXflags) -> &str {
    let pattern = run.get(used + 1..).unwrap_or("");
    match xflags {
        RuleXflags::Daemon => pattern.trim(),
        RuleXflags::MergeFile => pattern,
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
///
/// Both transformations are keyed on [`RuleXflags`]: a merge file's records
/// carry `XFLG_FATAL_ERRORS` alone, so neither the embedded-slash anchoring nor
/// the `/***` suffix applies to them.
fn build_pattern_rule(pattern: &str, is_include: bool, xflags: RuleXflags) -> FilterRuleWireFormat {
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
    //
    // A merge file's record keeps only the LEADING-slash case. `*pat == '/'`
    // needs XFLG_ANCHORED2ABS|XFLG_ABS_IF_SLASH too (exclude.c:298-300), so
    // upstream sets no FILTRULE_ABS_PATH there either - but the pattern still
    // carries its leading `/`, which its matcher anchors on all the same.
    // MEASURED: a merge file holding `- /keep` hides the module-root `keep`,
    // while one holding `- sub/f` hides `sub/f` AND `deep/sub/f`.
    let anchored = !pattern.starts_with("**")
        && (pattern.starts_with('/') || (xflags == RuleXflags::Daemon && pattern.contains('/')));
    let directory_only = pattern.ends_with('/');

    // upstream: exclude.c:212-213 - XFLG_DIR2WILD3 applies only to
    // directory-only exclude rules (BITS_SETnUNSET(FILTRULE_DIRECTORY, FILTRULE_INCLUDE)).
    if directory_only && !is_include && xflags == RuleXflags::Daemon {
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
