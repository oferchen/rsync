// Helpers for module access - logging, sanitization, bandwidth formatting,
// filter rules, and utilities.
//
// Contains shared functions used across the module access submodules:
// bandwidth limit application, log file management, module identifier
// sanitization, human-readable bandwidth formatting, and daemon-side
// filter rule construction from module config directives.

/// Opens the daemon's `log file`, or returns the refusal to report once the
/// daemon has fallen back to syslog.
///
/// upstream: log.c:169 opens the logfile through `open_no_attacker_symlinks()`
/// with `O_WRONLY|O_APPEND|O_CREAT, 0644`, for the reason stated at log.c:165 -
/// `--log-file` and `log file =` are operator-supplied paths that may transit
/// attacker-writable directories, and a planted symlink would redirect the root
/// daemon's log into a file of the attacker's choosing. The ownership walk
/// refuses symlink components not owned by uid 0 or our euid.
///
/// upstream: log.c:175-182 `logfile_open()` - when the open fails, upstream
/// calls `syslog_init()`, reports the failure, clears `logfile_name` so every
/// later line goes to syslog, and keeps serving. It never aborts the daemon
/// over its own log file, including when the open is refused because the path
/// runs through an untrusted symlink.
pub(crate) fn open_daemon_log_sink(
    path: &Path,
    brand: Brand,
) -> Result<SharedLogSink, LogFileFallback> {
    open_log_file(path)
        .map(|file| {
            // upstream: log.c:125-141 logit() stamps `%Y/%m/%d %H:%M:%S [pid] `
            // on every log-file line; the wrapper applies the same shared
            // formatter used by the client `--log-file` sink.
            Arc::new(Mutex::new(MessageSink::with_brand(
                logging_sink::logfile::LogFileWriter::new(file),
                brand,
            )))
        })
        .map_err(|error| LogFileFallback::new(path, &error, brand))
}

/// A `log file` that could not be opened, reported the way upstream does.
#[derive(Debug)]
pub(crate) struct LogFileFallback {
    failure: String,
}

impl LogFileFallback {
    /// upstream: log.c:178-180 - `rsyserr(FERROR, errno, "failed to open
    /// log-file %s")` followed by `rprintf(FINFO, "Ignoring \"log file\"
    /// setting.\n")`. The `[Receiver]` tag is what `who_am_i()` yields for the
    /// daemon process, as the upstream 3.5.1 daemon logs it.
    fn new(path: &Path, error: &io::Error, brand: Brand) -> Self {
        Self {
            failure: format!(
                "{}: [Receiver] failed to open log-file {}: {}",
                brand.client_program_name(),
                path.display(),
                logging::upstream_errno_text(error)
            ),
        }
    }

    /// The two lines upstream logs, in order.
    pub(crate) fn lines(&self) -> [&str; 2] {
        [&self.failure, "Ignoring \"log file\" setting."]
    }

    /// Writes the refusal to syslog, where the daemon now logs.
    ///
    /// A no-op when this entry point has not opened syslog.
    pub(crate) fn report(&self) {
        let [failure, ignoring] = self.lines();
        #[cfg(unix)]
        {
            use logging_sink::syslog::{SyslogPriority, syslog_message};
            syslog_message(SyslogPriority::Error, failure);
            syslog_message(SyslogPriority::Info, ignoring);
        }
        #[cfg(not(unix))]
        {
            eprintln!("{failure}");
            eprintln!("{ignoring}");
        }
    }
}

/// Mode applied when the log file is created.
///
/// upstream: log.c:170 passes `0644`. The surrounding `umask(022 | orig_umask)`
/// (log.c:164) is what forbids a group- or world-writable log file under a
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
/// upstream: log.c:186-205 `log_init(1)` reopens the daemon logfile to
/// `lp_log_file(module_id)` at module selection (clientserver.c:956). A module's
/// resolved `log file` already inherits the global-section default (finish.rs),
/// so [`ModuleDefinition::module_log_file`] is exactly upstream's
/// `lp_log_file(module_id)`.
///
/// oc opens the startup sink once (from `--log-file`) and shares it across
/// connection threads, so rather than mutate the shared sink this returns a
/// fresh per-connection sink pointing at the module's log file; the caller uses
/// it for the remainder of the connection. A module with no `log file` returns
/// `None`, keeping the startup sink. A failed reopen is non-fatal - upstream
/// (log.c:175-183) logs the failure and keeps serving rather than dropping the
/// connection - so this returns `None` and the caller retains the startup sink.
fn reopen_module_log_sink(
    module: &ModuleDefinition,
    startup_sink: Option<&SharedLogSink>,
) -> Option<SharedLogSink> {
    let path = module.module_log_file()?;
    let brand = startup_sink
        .and_then(|sink| sink.lock().ok().map(|guard| guard.brand()))
        .unwrap_or(Brand::Oc);
    open_daemon_log_sink(path, brand)
        .map_err(|fallback| fallback.report())
        .ok()
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
/// upstream: log.c:125-141 logit() writes the raw `rwrite()` buffer after the
/// timestamp prefix. FINFO/FLOG bodies carry no severity tag (e.g.
/// `connect from host (addr)`, clientserver.c:1526), so info messages are
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

/// Writes one raw message line to the shared log sink with proper locking.
///
/// upstream: log.c:125-141 logit() hands the bytes to `filtered_fwrite()`
/// unchanged, so a file name that is not valid UTF-8 is logged as-is; the
/// sink's writer applies the same control-character escaping.
fn log_raw_line(log: &SharedLogSink, line: &[u8]) {
    let Ok(mut sink) = log.lock() else {
        return;
    };
    let mut record = Vec::with_capacity(line.len() + 1);
    record.extend_from_slice(line);
    record.push(b'\n');
    if sink.writer_mut().write_all(&record).is_ok() {
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
/// - `token.c:213-218`: `init_set_compression()` optimises a `*` match-string,
///   setting `per_file_default_level = skip_compression_level` and clearing the
///   per-file suffix tree.
fn dont_compress_is_match_all(value: &str) -> bool {
    value.split_whitespace().any(|token| token == "*")
}

/// Builds daemon-side filter rules from the module's filter configuration.
///
/// upstream: clientserver.c:929-954 - `rsync_module()` builds
/// `daemon_filter_list` from `filter`, `include from`, `include`, `exclude from`
/// and `exclude`, in that order, through the same parse_filter_str() every
/// other rule takes. The parser is core's; this module supplies only where the
/// files the directives name are read from ([`ModuleConfigFiles`]).
fn build_daemon_filter_rules(
    module: &ModuleRuntime,
) -> Result<Vec<FilterRuleWireFormat>, ModuleFilterRefusal> {
    use core::client::filter_args::{ModuleFilterDirectives, module_filter_rules};

    let directives = ModuleFilterDirectives {
        filter: &module.filter,
        include_from: module.include_from.as_deref(),
        include: &module.include,
        exclude_from: module.exclude_from.as_deref(),
        exclude: &module.exclude,
    };
    let files = ModuleConfigFiles { root: &module.path };
    module_filter_rules(&directives, &files).map_err(ModuleFilterRefusal)
}

/// A module filter directive the parse refused, as upstream reports it.
///
/// upstream: filter_rule_err() exits RERR_SYNTAX (exclude.c:133-137) and a
/// failed open under XFLG_FATAL_ERRORS exits RERR_FILEIO (exclude.c:1714-1719).
#[derive(Debug)]
struct ModuleFilterRefusal(Message);

impl ModuleFilterRefusal {
    /// The exit code the daemon's session ends with.
    fn exit_code(&self) -> ExitCode {
        if self.0.code() == Some(ExitCode::FileIo.as_i32()) {
            ExitCode::FileIo
        } else {
            ExitCode::Syntax
        }
    }
}

impl std::fmt::Display for ModuleFilterRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.text())
    }
}

/// The files a module's own filter directives name.
///
/// upstream: clientserver.c:922-926 sets `dirbuf` to the module root before
/// the parse, so that is the base a slash-bearing merge name joins. The rules
/// built so far ARE `daemon_filter_list`, so they gate every open
/// (exclude.c:1639-1664), and the read takes the operator arm of the ownership
/// walk rather than module confinement (exclude.c:1677-1684).
struct ModuleConfigFiles<'a> {
    /// The module root, upstream's `dirbuf` for this parse.
    root: &'a Path,
}

impl core::client::filter_args::FilterFileSource for ModuleConfigFiles<'_> {
    fn base_dir(&self) -> PathBuf {
        self.root.to_path_buf()
    }

    fn open(
        &self,
        path: &Path,
        _kind: core::client::filter_args::FilterFileKind,
        list: &[core::client::FilterRuleSpec],
    ) -> io::Result<Box<dyn Read>> {
        if !list.is_empty() {
            let rules = core::client::filter_args::daemon_list_rules(list)
                .map_err(|message| io::Error::other(message.text().to_owned()))?;
            if merge_file_hidden_by_daemon_rules(&rules, path, self.root) {
                return Ok(Box::new(io::empty()));
            }
        }
        let content = read_filter_file_contents(path)?;
        Ok(Box::new(io::Cursor::new(content.into_bytes())))
    }
}

/// Answers upstream's pre-open `check_filter(&daemon_filter_list, ...)` gate.
///
/// upstream: `exclude.c:1639-1664`. The name tested is the RESOLVED merge-file
/// path with the module root's LENGTH skipped when it is absolute (`dir = line +
/// (*line == '/' ? module_dirlen : 0)`), because the daemon list matches
/// module-relative names. The skip is by length alone, so an absolute name
/// outside the module is tested as whatever its tail happens to be - which is
/// why `filter = .e /etc/rsync/rules` still reads its file: the self-exclude
/// `rules` does not match that tail. MEASURED against rsync 3.5.1.
///
/// Returns `false` when no rules have accumulated yet - upstream guards the
/// whole block on `if (daemon_filter_list.head)`, so the FIRST merge rule in a
/// module's configuration is never gated.
fn merge_file_hidden_by_daemon_rules(
    rules: &[FilterRuleWireFormat],
    path: &Path,
    root: &Path,
) -> bool {
    if rules.is_empty() {
        return false;
    }
    let tested = if path.is_absolute() {
        let text = path.to_string_lossy();
        match text.get(root.as_os_str().len()..) {
            Some(tail) => PathBuf::from(tail),
            None => return false,
        }
    } else {
        path.to_path_buf()
    };
    let name = tested.strip_prefix("/").unwrap_or(&tested);
    if name.as_os_str().is_empty() {
        return false;
    }
    match daemon_filter_set(rules) {
        Some(set) => !set.allows(name, false),
        None => false,
    }
}

/// Compiles the rules accumulated so far into the matcher the gate consults.
///
/// The wire rules are the daemon list in `FilterRuleWireFormat` form; a `Clear`
/// among them pops what came before it, exactly as it does for every other
/// consumer of this list.
fn daemon_filter_set(rules: &[FilterRuleWireFormat]) -> Option<filters::FilterSet> {
    use filters::FilterRule;
    use protocol::filters::RuleType;

    let compiled: Vec<FilterRule> = rules
        .iter()
        .flat_map(|wire_rule| {
            // A `-C` marker (empty pattern + `cvs_exclude`) stands in for the
            // built-in `DEFAULT_CVSIGNORE` set. upstream's `check_filter`
            // short-circuits a FILTRULE_CVS_IGNORE rule to `cvs_filter_list`
            // (exclude.c:1201-1206), so expand it to those default excludes
            // here rather than let its empty pattern match everything.
            // `filters::cvs_exclusion_rules` is the ONE owner of the list,
            // shared with the transfer-side expander in `generator::filters`.
            if wire_rule.cvs_exclude && wire_rule.pattern.is_empty() {
                return filters::cvs_exclusion_rules(false).collect::<Vec<_>>();
            }
            // Raw bytes, matching upstream's `char *pattern` model - a
            // non-UTF-8 daemon-config pattern byte matches itself.
            let mut pat =
                filters::path_pattern_bytes(std::path::Path::new(&wire_rule.pattern)).into_owned();
            if wire_rule.directory_only && pat.last() != Some(&b'/') {
                pat.push(b'/');
            }
            let anchor = |rule: FilterRule| {
                let rule = rule.with_negate(wire_rule.negate);
                if wire_rule.anchored {
                    rule.anchor_to_root()
                } else {
                    rule
                }
            };
            match wire_rule.rule_type {
                RuleType::Include => vec![anchor(FilterRule::include(pat))],
                RuleType::Exclude => vec![anchor(FilterRule::exclude(pat))],
                RuleType::Clear => vec![FilterRule::clear()],
                // Only include/exclude decide whether a NAME is hidden here:
                // upstream calls `check_filter` with `name_flags == 0`, and the
                // sided and per-directory kinds are answered elsewhere.
                _ => vec![],
            }
        })
        .collect();
    filters::FilterSet::from_rules(compiled).ok()
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
