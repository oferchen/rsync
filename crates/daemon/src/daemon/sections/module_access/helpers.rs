// Helpers for module access - logging, sanitization, bandwidth formatting, filter rules, and utilities.

/// Applies the module-specific bandwidth directives to the active limiter.
///
/// The helper mirrors upstream rsync's precedence rules: a module `bwlimit`
/// directive overrides the daemon-wide limit with the strictest rate while
/// honouring explicitly configured bursts. When a module omits the directive
/// the limiter remains in the state established by the daemon scope, ensuring
/// clients observe inherited throttling exactly as the C implementation does.
/// The function returns the [`LimiterChange`] reported by
/// [`apply_effective_limit`], allowing callers and tests to verify whether the
/// limiter configuration changed as a result of the module overrides.
pub(crate) fn apply_module_bandwidth_limit(
    limiter: &mut Option<BandwidthLimiter>,
    module_limit: Option<NonZeroU64>,
    module_limit_specified: bool,
    module_limit_configured: bool,
    module_burst: Option<NonZeroU64>,
    module_burst_specified: bool,
) -> LimiterChange {
    if module_limit_configured && module_limit.is_none() {
        let burst_only_override =
            module_burst_specified && module_burst.is_some() && limiter.is_some();
        if !burst_only_override {
            return if limiter.take().is_some() {
                LimiterChange::Disabled
            } else {
                LimiterChange::Unchanged
            };
        }
    }

    let limit_specified =
        module_limit_specified || (module_limit_configured && module_limit.is_some());
    let burst_specified =
        module_burst_specified && (module_limit_configured || module_limit_specified);

    BandwidthLimitComponents::new_with_flags(
        module_limit,
        module_burst,
        limit_specified,
        burst_specified,
    )
    .apply_to_limiter(limiter)
}

/// Opens or creates a log file and wraps it in a shared message sink.
///
/// The log file is opened in append mode, creating it if it doesn't exist.
/// Returns a thread-safe [`SharedLogSink`] for concurrent logging.
pub(crate) fn open_log_sink(path: &Path, brand: Brand) -> Result<SharedLogSink, DaemonError> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| log_file_error(path, error))?;
    Ok(Arc::new(Mutex::new(MessageSink::with_brand(file, brand))))
}

/// Creates a [`DaemonError`] for log file open failures.
fn log_file_error(path: &Path, error: io::Error) -> DaemonError {
    DaemonError::new(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        rsync_error!(
            FEATURE_UNAVAILABLE_EXIT_CODE,
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
fn log_message(log: &SharedLogSink, message: &Message) {
    if let Ok(mut sink) = log.lock()
        && sink.write(message).is_ok()
    {
        let _ = sink.flush();
    }
}

/// Formats a host for logging, using the IP address as fallback.
fn format_host(host: Option<&str>, fallback: IpAddr) -> String {
    host.map_or_else(|| fallback.to_string(), str::to_string)
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

/// Formats a bandwidth rate in human-readable units (bytes/s, KiB/s, etc.).
///
/// Chooses the largest unit that divides evenly into the rate, falling back
/// to raw bytes/s for values that don't align to a power-of-1024 boundary.
pub(crate) fn format_bandwidth_rate(value: NonZeroU64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;
    const PIB: u64 = TIB * 1024;

    let bytes = value.get();
    if bytes.is_multiple_of(PIB) {
        let rate = bytes / PIB;
        format!("{rate} PiB/s")
    } else if bytes.is_multiple_of(TIB) {
        let rate = bytes / TIB;
        format!("{rate} TiB/s")
    } else if bytes.is_multiple_of(GIB) {
        let rate = bytes / GIB;
        format!("{rate} GiB/s")
    } else if bytes.is_multiple_of(MIB) {
        let rate = bytes / MIB;
        format!("{rate} MiB/s")
    } else if bytes.is_multiple_of(KIB) {
        let rate = bytes / KIB;
        format!("{rate} KiB/s")
    } else {
        format!("{bytes} bytes/s")
    }
}

/// Parses the daemon `dont compress` parameter into a `SkipCompressList`.
///
/// The daemon format uses space-separated glob-style suffixes (e.g., `"*.gz *.zip *.jpg"`).
/// Each suffix is stripped of its `*.` prefix and converted to the slash-separated
/// format that `SkipCompressList::parse` expects. Bare suffixes without `*.` prefix
/// are also accepted.
///
/// Returns `None` if the input is empty or contains no valid suffixes.
///
/// # Upstream Reference
///
/// - `loadparm.c` - `dont compress` parameter, space-separated globs
/// - `exclude.c:set_dont_compress_re()` - converts to regex for per-file matching
fn parse_daemon_dont_compress(value: &str) -> Option<SkipCompressList> {
    let suffixes: Vec<&str> = value
        .split_whitespace()
        .filter_map(|token| {
            // Strip `*.` prefix used in daemon config notation
            if let Some(suffix) = token.strip_prefix("*.") {
                if !suffix.is_empty() {
                    return Some(suffix);
                }
            }
            // Accept bare suffixes without glob prefix
            let bare = token.trim_start_matches('.');
            if !bare.is_empty() { Some(bare) } else { None }
        })
        .collect();

    if suffixes.is_empty() {
        return None;
    }

    let spec = suffixes.join("/");
    SkipCompressList::parse(&spec).ok()
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
/// The order matches upstream: filter, include, exclude, include_from, exclude_from.
fn build_daemon_filter_rules(
    module: &ModuleRuntime,
) -> Result<Vec<FilterRuleWireFormat>, io::Error> {
    let mut rules = Vec::new();

    // 1. filter rules - full filter syntax (e.g., "- *.tmp", "+ *.rs")
    // upstream: parse_filter_str(&daemon_filter_list, lp_filter(i), rule, FILTRULE_WORD_SPLIT)
    // Each element in the Vec is one complete filter rule from a `filter =` line.
    for filter_str in &module.filter {
        if let Some(rule) = parse_daemon_filter_token(filter_str.trim()) {
            rules.push(rule);
        }
    }

    // 2. include rules - bare patterns, word-split on whitespace
    // upstream: parse_filter_str(&daemon_filter_list, lp_include(i), rule,
    //           FILTRULE_INCLUDE | FILTRULE_WORD_SPLIT)
    for include_str in &module.include {
        for pattern in include_str.split_whitespace() {
            rules.push(build_pattern_rule(pattern, true));
        }
    }

    // 3. exclude rules - bare patterns, word-split on whitespace
    // upstream: parse_filter_str(&daemon_filter_list, lp_exclude(i), rule, FILTRULE_WORD_SPLIT)
    for exclude_str in &module.exclude {
        for pattern in exclude_str.split_whitespace() {
            rules.push(build_pattern_rule(pattern, false));
        }
    }

    // 4. include_from - read patterns from file, one per line
    // upstream: parse_filter_file(&daemon_filter_list, lp_include_from(i), rule, 0)
    if let Some(ref path) = module.include_from {
        let patterns = read_patterns_from_file(path)?;
        for pattern in patterns {
            rules.push(build_pattern_rule(&pattern, true));
        }
    }

    // 5. exclude_from - read patterns from file, one per line
    // upstream: parse_filter_file(&daemon_filter_list, lp_exclude_from(i), rule, 0)
    if let Some(ref path) = module.exclude_from {
        let patterns = read_patterns_from_file(path)?;
        for pattern in patterns {
            rules.push(build_pattern_rule(&pattern, false));
        }
    }

    Ok(rules)
}

/// Reads patterns from a file, one per line.
///
/// Skips empty lines and comment lines (starting with `#` or `;`).
/// This matches upstream rsync's `parse_filter_file()` behavior for
/// `exclude_from` and `include_from` daemon parameters.
fn read_patterns_from_file(path: &Path) -> Result<Vec<String>, io::Error> {
    let content = fs::read_to_string(path).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("failed to read filter file '{}': {e}", path.display()),
        )
    })?;

    let patterns = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with(';'))
        .map(str::to_string)
        .collect();

    Ok(patterns)
}

/// Parses a single daemon filter token in filter rule syntax.
///
/// Supports short-form prefixes: `+` (include), `-` (exclude).
/// The pattern follows the prefix after optional whitespace.
/// Returns `None` for unrecognised tokens (silently skipped, matching
/// upstream's lenient parsing of daemon filter strings).
fn parse_daemon_filter_token(token: &str) -> Option<FilterRuleWireFormat> {
    if let Some(pattern) = token.strip_prefix("+ ").or_else(|| token.strip_prefix('+')) {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return None;
        }
        Some(build_pattern_rule(pattern, true))
    } else if let Some(pattern) = token.strip_prefix("- ").or_else(|| token.strip_prefix('-')) {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return None;
        }
        Some(build_pattern_rule(pattern, false))
    } else {
        // Bare pattern defaults to exclude (upstream behaviour)
        if token.is_empty() {
            return None;
        }
        Some(build_pattern_rule(token, false))
    }
}

/// Constructs a `FilterRuleWireFormat` from a pattern string.
///
/// Handles anchored patterns (leading `/`) and directory-only patterns
/// (trailing `/`) matching upstream rsync's pattern interpretation.
/// The pattern is preserved as-is in the wire format - the `anchored` and
/// `directory_only` flags are set for metadata but the pattern itself retains
/// its original form.
fn build_pattern_rule(pattern: &str, is_include: bool) -> FilterRuleWireFormat {
    let anchored = pattern.starts_with('/');
    let directory_only = pattern.ends_with('/');

    if is_include {
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
