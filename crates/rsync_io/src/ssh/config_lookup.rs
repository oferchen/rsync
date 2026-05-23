//! SSH client config lookup for the `Compression` directive.
//!
//! Closes the SSC-3 gap: SSC-1 only inspects argv, so a user with
//! `Compression yes` set globally in `~/.ssh/config` or
//! `/etc/ssh/ssh_config` gets no warning when they also pass rsync's
//! `--compress`. This module reads those files and reports whether
//! `Compression yes` is in effect at top level or inside a `Host *`
//! block.
//!
//! # Scope
//!
//! Top-level directives (before any `Host` block) and directives under
//! `Host *` are honoured. Per-host blocks like `Host foo.example.com`
//! and `Match` blocks are intentionally not evaluated here: the warning
//! site does not know which host the user is about to connect to in a
//! form that can drive OpenSSH's full pattern-match semantics
//! (especially `Match exec`, which would have to run an external
//! command). Deferring these keeps the warning conservative and free of
//! false positives.
//!
//! # Failure mode
//!
//! Parse errors and I/O errors never propagate. A malformed file emits
//! one `debug_log!` line and the caller falls back to the argv-only
//! answer. Hard-failing the transfer because a user's ssh_config has a
//! stray byte would be a worse outcome than missing a warning.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use logging::debug_log;

/// Returns `true` when `~/.ssh/config` or `/etc/ssh/ssh_config`
/// configures `Compression yes` at top level or under `Host *`.
///
/// `options` is the SSH option argv; a `-F <file>` (or `-F<file>`)
/// override is honoured first when present and the file exists. After
/// the override, the lookup tries `~/.ssh/config`, then
/// `/etc/ssh/ssh_config`. The first existing file wins; later files in
/// the list are not consulted, matching OpenSSH's behaviour when `-F`
/// is supplied.
///
/// Returns `false` when:
/// - no candidate file exists,
/// - the chosen file does not contain a matching `Compression yes`,
/// - the chosen file fails to parse (a `debug_log!` line is emitted and
///   the function reports `false` rather than aborting the transfer).
pub(super) fn ssh_config_enables_compression(options: &[OsString]) -> bool {
    for candidate in candidate_paths(options) {
        if !candidate.is_file() {
            continue;
        }
        return read_and_check(&candidate);
    }
    false
}

/// Returns the ordered list of ssh_config paths the caller should
/// consult. Visible to tests so they can verify the precedence chain
/// without touching the real filesystem.
pub(super) fn candidate_paths(options: &[OsString]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(override_path) = extract_dash_f_path(options) {
        paths.push(override_path);
    }
    if let Some(home) = home_dir() {
        paths.push(home.join(".ssh").join("config"));
    }
    paths.push(PathBuf::from("/etc/ssh/ssh_config"));
    paths
}

/// Reads `path` and returns whether it enables compression. Parse and
/// I/O errors are converted to `false` with a single diagnostic line.
fn read_and_check(path: &Path) -> bool {
    match fs::read_to_string(path) {
        Ok(text) => parse_enables_compression(&text),
        Err(err) => {
            debug_log!(
                Io,
                1,
                "ssh_config compression detection: failed to read {}: {}",
                path.display(),
                err
            );
            false
        }
    }
}

/// Parses `text` and returns `true` when `Compression yes` is in effect
/// at top level or under `Host *`.
///
/// Exposed to tests so they can assert behaviour without disk I/O.
pub(super) fn parse_enables_compression(text: &str) -> bool {
    let mut block = Block::TopLevel;
    let mut top_level: Option<bool> = None;
    let mut host_star: Option<bool> = None;

    for raw_line in text.lines() {
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = split_directive(line) else {
            debug_log!(
                Io,
                1,
                "ssh_config compression detection: skipping malformed line"
            );
            continue;
        };
        let key_lc = key.to_ascii_lowercase();
        match key_lc.as_str() {
            "host" => {
                block = if host_patterns_include_star(value) {
                    Block::HostStar
                } else {
                    Block::HostOther
                };
            }
            "match" => {
                block = Block::Match;
            }
            "compression" => {
                let parsed = parse_yes_no(value);
                match block {
                    Block::TopLevel if top_level.is_none() => top_level = parsed,
                    Block::HostStar if host_star.is_none() => host_star = parsed,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    top_level.unwrap_or(false) || host_star.unwrap_or(false)
}

/// Active config block while parsing.
#[derive(Copy, Clone, Eq, PartialEq)]
enum Block {
    TopLevel,
    HostStar,
    HostOther,
    Match,
}

/// A single token from an ssh_config `Match` pattern-list.
///
/// Stores the raw glob text (sans leading `!`) plus the negation flag.
/// Glob metacharacters `*` (any run) and `?` (one character) are honoured
/// at evaluation time by [`pattern_glob_matches`]. Mirrors OpenSSH's
/// `match_pattern_list` plus the embedded transport's
/// `host_matches_any_pattern` semantics.
//
// SSC-4.c will consume `Pattern` (and the rest of the Match-evaluator
// items below) from `parse_enables_compression`. Until that wiring
// lands the items are reachable only from tests, so suppress dead-code
// in non-test builds.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct Pattern {
    glob: String,
    negate: bool,
}

#[cfg_attr(not(test), allow(dead_code))]
impl Pattern {
    /// Builds a [`Pattern`] from a raw token. A leading `!` sets the
    /// negation flag; the remainder is stored verbatim as the glob text.
    pub(super) fn new(token: &str) -> Self {
        let (negate, glob) = token
            .strip_prefix('!')
            .map_or((false, token), |stripped| (true, stripped));
        Self {
            glob: glob.to_owned(),
            negate,
        }
    }

    /// Returns the stored glob text without the leading `!`.
    pub(super) fn glob(&self) -> &str {
        &self.glob
    }

    /// Returns `true` when this token is a negated pattern (`!glob`).
    pub(super) fn is_negated(&self) -> bool {
        self.negate
    }
}

#[cfg_attr(not(test), allow(dead_code))]
/// One condition from an ssh_config `Match` line.
///
/// The HONOR set selected in SSC-4.a: five variants covering `host`,
/// `originalhost`, `user`, `localuser`, and the argumentless `all`
/// sentinel. The SKIP / DEFER conditions (`canonical`, `final`,
/// `tagged`, `exec`) are not modelled here; the parser short-circuits
/// the whole block when it encounters one of them.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) enum MatchCondition {
    /// `Match host <patterns>`: target hostname after `Hostname`
    /// substitution and canonicalization. We never substitute, so this
    /// evaluates against [`MatchContext::host`] directly.
    Host(Vec<Pattern>),
    /// `Match originalhost <patterns>`: target hostname as given on the
    /// command line, before any substitution. Evaluates against
    /// [`MatchContext::original_host`].
    OriginalHost(Vec<Pattern>),
    /// `Match user <patterns>`: remote user (`-l`, `user@host`, or
    /// `User` directive). Evaluates against [`MatchContext::user`].
    User(Vec<Pattern>),
    /// `Match localuser <patterns>`: local user running ssh. Evaluates
    /// against [`MatchContext::local_user`].
    LocalUser(Vec<Pattern>),
    /// `Match all`: always matches; the conventional default-block
    /// sentinel.
    All,
}

#[cfg_attr(not(test), allow(dead_code))]
/// Connection context consulted when evaluating an ssh_config `Match`
/// line.
///
/// Lifetimes are external because every field is borrowed from the
/// caller's `SshCommand` or an env-lookup string.
#[derive(Debug, Clone, Copy)]
pub(super) struct MatchContext<'a> {
    /// Target host as passed to ssh on the command line, after
    /// canonicalization. oc-rsync never canonicalizes, so this equals
    /// `original_host` for our purposes.
    pub host: &'a str,
    /// Target host as given on the command line, before any
    /// `Hostname` substitution. Evaluated by `originalhost`.
    pub original_host: &'a str,
    /// Remote user. Empty string when the caller did not specify one;
    /// callers may pass the local user as a fallback to mirror
    /// OpenSSH's `User` default.
    pub user: &'a str,
    /// Local user running ssh. Sourced from `USER` on Unix and
    /// `USERNAME` on Windows when discovered via
    /// [`MatchContext::with_local_user_from_env`].
    pub local_user: &'a str,
}

#[cfg_attr(not(test), allow(dead_code))]
impl<'a> MatchContext<'a> {
    /// Builds a context with an explicit local-user string. Tests use
    /// this to avoid touching process environment.
    pub(super) fn new(
        host: &'a str,
        original_host: &'a str,
        user: &'a str,
        local_user: &'a str,
    ) -> Self {
        Self {
            host,
            original_host,
            user,
            local_user,
        }
    }

    /// Builds a context whose `local_user` is taken from the platform's
    /// canonical env var (`USER` on Unix, `USERNAME` on Windows). When
    /// the var is unset or non-UTF-8 the field is left as the empty
    /// string `""`, which never matches a non-empty pattern token.
    ///
    /// The returned context borrows from `local_user_buf`, so callers
    /// own the backing storage and the borrow checker can enforce the
    /// lifetime.
    pub(super) fn with_local_user_from_env(
        host: &'a str,
        original_host: &'a str,
        user: &'a str,
        local_user_buf: &'a mut String,
    ) -> Self {
        if let Some(value) = local_user_env() {
            *local_user_buf = value;
        }
        Self {
            host,
            original_host,
            user,
            local_user: local_user_buf.as_str(),
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
/// Returns the local username from `USER` (Unix) or `USERNAME`
/// (Windows). Returns `None` on platforms without either var or when
/// the value is empty.
fn local_user_env() -> Option<String> {
    #[cfg(unix)]
    let raw = std::env::var_os("USER");
    #[cfg(windows)]
    let raw = std::env::var_os("USERNAME");
    #[cfg(not(any(unix, windows)))]
    let raw: Option<std::ffi::OsString> = None;

    let value = raw?.to_string_lossy().into_owned();
    if value.is_empty() { None } else { Some(value) }
}

#[cfg_attr(not(test), allow(dead_code))]
/// Evaluates a parsed `Match` line against `ctx`.
///
/// Returns `true` only when *every* condition's pattern list resolves
/// to a match (logical AND across the line). Within one condition the
/// pattern list is OR-matched: any positive token that matches the
/// input passes, unless a negated token also matches, in which case
/// the whole condition fails (mirrors OpenSSH's `match_pattern_list`).
///
/// An empty `conditions` slice returns `false`. SSC-4.a chose this
/// conservative default so a malformed `Match` line with no recognised
/// tokens cannot activate a block.
pub(super) fn evaluate_match(conditions: &[MatchCondition], ctx: &MatchContext<'_>) -> bool {
    if conditions.is_empty() {
        return false;
    }
    conditions.iter().all(|cond| evaluate_condition(cond, ctx))
}

#[cfg_attr(not(test), allow(dead_code))]
/// Evaluates a single [`MatchCondition`] against the context's
/// corresponding field. Hostnames are compared case-insensitively;
/// usernames are compared case-sensitively on Unix and
/// case-insensitively on Windows, matching OpenSSH's behaviour.
fn evaluate_condition(condition: &MatchCondition, ctx: &MatchContext<'_>) -> bool {
    match condition {
        MatchCondition::All => true,
        MatchCondition::Host(patterns) => pattern_list_matches(patterns, ctx.host, MatchKind::Host),
        MatchCondition::OriginalHost(patterns) => {
            pattern_list_matches(patterns, ctx.original_host, MatchKind::Host)
        }
        MatchCondition::User(patterns) => pattern_list_matches(patterns, ctx.user, MatchKind::User),
        MatchCondition::LocalUser(patterns) => {
            pattern_list_matches(patterns, ctx.local_user, MatchKind::User)
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
/// Whether the input is a hostname or a username; controls case
/// folding per SSC-4.a.
#[derive(Copy, Clone)]
enum MatchKind {
    Host,
    User,
}

#[cfg_attr(not(test), allow(dead_code))]
/// Returns `true` when `input` matches the pattern list under OpenSSH's
/// OR-with-negation rule: any negated token that matches forces a
/// failure; otherwise at least one positive token must match. An empty
/// pattern list never matches.
fn pattern_list_matches(patterns: &[Pattern], input: &str, kind: MatchKind) -> bool {
    if patterns.is_empty() || input.is_empty() {
        return false;
    }
    let mut any_positive = false;
    for pattern in patterns {
        if pattern_glob_matches(pattern.glob(), input, kind) {
            if pattern.is_negated() {
                return false;
            }
            any_positive = true;
        }
    }
    any_positive
}

#[cfg_attr(not(test), allow(dead_code))]
/// Glob-matches `input` against `glob`, applying the case-folding rule
/// dictated by `kind`. Mirrors the embedded transport's `pattern_matches`
/// (`*` matches any run, `?` matches one character) with an added
/// case-folding step.
fn pattern_glob_matches(glob: &str, input: &str, kind: MatchKind) -> bool {
    if case_fold(kind) {
        let input_norm = input.to_ascii_lowercase();
        let glob_norm = glob.to_ascii_lowercase();
        glob_matches_bytes(input_norm.as_bytes(), glob_norm.as_bytes())
    } else {
        glob_matches_bytes(input.as_bytes(), glob.as_bytes())
    }
}

#[cfg_attr(not(test), allow(dead_code))]
/// Returns `true` when comparisons for `kind` should be ASCII
/// case-folded. Hostnames are always folded; usernames are folded only
/// on Windows, where account names are inherently case-insensitive.
fn case_fold(kind: MatchKind) -> bool {
    match kind {
        MatchKind::Host => true,
        MatchKind::User => cfg!(windows),
    }
}

#[cfg_attr(not(test), allow(dead_code))]
/// Byte-level glob matcher: `*` matches any run, `?` matches one byte.
/// No character classes; no extended globs. Equivalent to `fnmatch(3)`
/// without `FNM_PATHNAME`.
fn glob_matches_bytes(input: &[u8], glob: &[u8]) -> bool {
    if glob.is_empty() {
        return input.is_empty();
    }
    match glob[0] {
        b'*' => {
            if glob.len() == 1 {
                return true;
            }
            for i in 0..=input.len() {
                if glob_matches_bytes(&input[i..], &glob[1..]) {
                    return true;
                }
            }
            false
        }
        b'?' => !input.is_empty() && glob_matches_bytes(&input[1..], &glob[1..]),
        c => !input.is_empty() && input[0] == c && glob_matches_bytes(&input[1..], &glob[1..]),
    }
}

/// Returns `true` when any token in `patterns` is exactly `*`. Matches
/// OpenSSH's "wildcard everything" idiom, which is the only host
/// pattern we evaluate without runtime context.
fn host_patterns_include_star(patterns: &str) -> bool {
    patterns
        .split(|c: char| c.is_whitespace() || c == ',')
        .map(str::trim)
        .any(|tok| tok == "*")
}

/// Walks `options` looking for `-F file` (split across two args) or
/// `-Ffile` (concatenated). Returns the first occurrence as a
/// [`PathBuf`].
fn extract_dash_f_path(options: &[OsString]) -> Option<PathBuf> {
    let mut iter = options.iter();
    while let Some(opt) = iter.next() {
        if opt == OsStr::new("-F") {
            return iter.next().map(PathBuf::from);
        }
        let bytes = opt.to_string_lossy();
        if let Some(rest) = bytes.strip_prefix("-F")
            && !rest.is_empty()
        {
            return Some(PathBuf::from(rest));
        }
    }
    None
}

/// Strips a trailing `#...` comment from a config line.
fn strip_comment(line: &str) -> &str {
    line.find('#').map_or(line, |idx| &line[..idx])
}

/// Splits `Key Value` (or `Key=Value`) on the first whitespace or `=`
/// run. Returns `None` for keys with no value.
fn split_directive(line: &str) -> Option<(&str, &str)> {
    let (key, rest) = line.split_once(|c: char| c.is_whitespace() || c == '=')?;
    let value = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=');
    if value.is_empty() {
        return None;
    }
    Some((key, value))
}

/// Parses a yes/no value (case-insensitive). Returns `None` for any
/// other value so a typo cannot accidentally flip the bit.
fn parse_yes_no(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" => Some(true),
        "no" | "false" => Some(false),
        _ => None,
    }
}

/// Resolves the per-user home directory. Mirrors the helper in
/// `embedded::ssh_config` rather than reaching across module boundaries
/// because the embedded transport is gated behind a different feature.
fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_compression_yes_detected() {
        assert!(parse_enables_compression("Compression yes\n"));
    }

    #[test]
    fn host_star_compression_yes_detected() {
        let text = "Host *\n  Compression yes\n";
        assert!(parse_enables_compression(text));
    }

    #[test]
    fn per_host_compression_yes_ignored() {
        let text = "Host foo.example.com\n  Compression yes\n";
        assert!(!parse_enables_compression(text));
    }

    #[test]
    fn compression_no_returns_false() {
        assert!(!parse_enables_compression("Compression no\n"));
    }

    #[test]
    fn match_block_compression_yes_ignored() {
        let text = "Match host bar\n  Compression yes\n";
        assert!(!parse_enables_compression(text));
    }

    #[test]
    fn equals_separator_supported() {
        assert!(parse_enables_compression("Compression=yes\n"));
    }

    #[test]
    fn comments_stripped() {
        assert!(parse_enables_compression(
            "# header\nCompression yes # trailing\n"
        ));
    }

    #[test]
    fn extracts_split_dash_f() {
        let opts = vec![OsString::from("-F"), OsString::from("/tmp/custom")];
        assert_eq!(
            extract_dash_f_path(&opts),
            Some(PathBuf::from("/tmp/custom"))
        );
    }

    #[test]
    fn extracts_combined_dash_f() {
        let opts = vec![OsString::from("-F/tmp/custom")];
        assert_eq!(
            extract_dash_f_path(&opts),
            Some(PathBuf::from("/tmp/custom"))
        );
    }

    #[test]
    fn no_dash_f_returns_none() {
        let opts = vec![OsString::from("-oBatchMode=yes")];
        assert!(extract_dash_f_path(&opts).is_none());
    }

    fn ctx<'a>(host: &'a str, user: &'a str, local_user: &'a str) -> MatchContext<'a> {
        MatchContext::new(host, host, user, local_user)
    }

    fn patterns(tokens: &[&str]) -> Vec<Pattern> {
        tokens.iter().map(|t| Pattern::new(t)).collect()
    }

    #[test]
    fn pattern_strips_negation_prefix() {
        let pat = Pattern::new("!banned.example.com");
        assert!(pat.is_negated());
        assert_eq!(pat.glob(), "banned.example.com");
    }

    #[test]
    fn pattern_without_bang_is_positive() {
        let pat = Pattern::new("web*.example.com");
        assert!(!pat.is_negated());
        assert_eq!(pat.glob(), "web*.example.com");
    }

    #[test]
    fn evaluate_match_single_host_positive() {
        let cond = vec![MatchCondition::Host(patterns(&["web1.example.com"]))];
        assert!(evaluate_match(&cond, &ctx("web1.example.com", "", "")));
    }

    #[test]
    fn evaluate_match_single_host_negative() {
        let cond = vec![MatchCondition::Host(patterns(&["db.example.com"]))];
        assert!(!evaluate_match(&cond, &ctx("web1.example.com", "", "")));
    }

    #[test]
    fn evaluate_match_single_host_wildcard() {
        let cond = vec![MatchCondition::Host(patterns(&["*.example.com"]))];
        assert!(evaluate_match(&cond, &ctx("web1.example.com", "", "")));
        assert!(!evaluate_match(&cond, &ctx("db.internal", "", "")));
    }

    #[test]
    fn evaluate_match_single_host_question_mark() {
        let cond = vec![MatchCondition::Host(patterns(&["web?.example.com"]))];
        assert!(evaluate_match(&cond, &ctx("web1.example.com", "", "")));
        assert!(!evaluate_match(&cond, &ctx("web10.example.com", "", "")));
    }

    #[test]
    fn evaluate_match_host_case_insensitive() {
        let cond = vec![MatchCondition::Host(patterns(&["WEB1.EXAMPLE.COM"]))];
        assert!(evaluate_match(&cond, &ctx("web1.example.com", "", "")));
    }

    #[test]
    fn evaluate_match_host_negation() {
        let cond = vec![MatchCondition::Host(patterns(&[
            "*.example.com",
            "!banned.example.com",
        ]))];
        assert!(evaluate_match(&cond, &ctx("ok.example.com", "", "")));
        assert!(!evaluate_match(&cond, &ctx("banned.example.com", "", "")));
    }

    #[test]
    fn evaluate_match_host_and_user_and_chain() {
        let cond = vec![
            MatchCondition::Host(patterns(&["web*"])),
            MatchCondition::User(patterns(&["deploy"])),
        ];
        assert!(evaluate_match(&cond, &ctx("web1", "deploy", "")));
        assert!(!evaluate_match(&cond, &ctx("web1", "root", "")));
        assert!(!evaluate_match(&cond, &ctx("db1", "deploy", "")));
    }

    #[test]
    fn evaluate_match_or_within_condition_and_across() {
        let cond = vec![
            MatchCondition::Host(patterns(&["web*", "app*"])),
            MatchCondition::User(patterns(&["deploy", "ci"])),
        ];
        assert!(evaluate_match(&cond, &ctx("web1", "deploy", "")));
        assert!(evaluate_match(&cond, &ctx("app2", "ci", "")));
        assert!(!evaluate_match(&cond, &ctx("db1", "deploy", "")));
        assert!(!evaluate_match(&cond, &ctx("web1", "root", "")));
    }

    #[test]
    fn evaluate_match_originalhost_evaluates_against_original_host_field() {
        let cond = vec![MatchCondition::OriginalHost(patterns(&["web1"]))];
        let context = MatchContext::new("web1.canonical.example.com", "web1", "", "");
        assert!(evaluate_match(&cond, &context));
    }

    #[test]
    fn evaluate_match_localuser_evaluates_against_local_user_field() {
        let cond = vec![MatchCondition::LocalUser(patterns(&["ofer"]))];
        assert!(evaluate_match(&cond, &ctx("any", "", "ofer")));
        assert!(!evaluate_match(&cond, &ctx("any", "", "alice")));
    }

    #[test]
    fn evaluate_match_all_is_unconditional() {
        let cond = vec![MatchCondition::All];
        assert!(evaluate_match(&cond, &ctx("", "", "")));
        assert!(evaluate_match(
            &cond,
            &ctx("anything", "anyone", "anywhere")
        ));
    }

    #[test]
    fn evaluate_match_empty_condition_list_rejects() {
        assert!(!evaluate_match(&[], &ctx("web1", "deploy", "ofer")));
    }

    #[test]
    fn evaluate_match_empty_input_never_matches_non_empty_patterns() {
        let cond = vec![MatchCondition::User(patterns(&["deploy"]))];
        assert!(!evaluate_match(&cond, &ctx("web1", "", "ofer")));
    }

    #[test]
    fn evaluate_match_empty_pattern_list_rejects() {
        let cond = vec![MatchCondition::Host(Vec::new())];
        assert!(!evaluate_match(&cond, &ctx("web1", "", "")));
    }

    #[test]
    fn evaluate_match_negation_only_pattern_list_rejects() {
        let cond = vec![MatchCondition::Host(patterns(&["!banned"]))];
        assert!(!evaluate_match(&cond, &ctx("banned", "", "")));
        assert!(!evaluate_match(&cond, &ctx("ok", "", "")));
    }

    #[test]
    fn evaluate_match_and_chain_with_all_sentinel() {
        let cond = vec![
            MatchCondition::Host(patterns(&["web1"])),
            MatchCondition::All,
        ];
        assert!(evaluate_match(&cond, &ctx("web1", "", "")));
        assert!(!evaluate_match(&cond, &ctx("db1", "", "")));
    }

    #[cfg(not(windows))]
    #[test]
    fn user_pattern_case_sensitive_on_unix() {
        let cond = vec![MatchCondition::User(patterns(&["Deploy"]))];
        assert!(!evaluate_match(&cond, &ctx("web1", "deploy", "")));
        assert!(evaluate_match(&cond, &ctx("web1", "Deploy", "")));
    }

    #[cfg(windows)]
    #[test]
    fn user_pattern_case_insensitive_on_windows() {
        let cond = vec![MatchCondition::User(patterns(&["Deploy"]))];
        assert!(evaluate_match(&cond, &ctx("web1", "deploy", "")));
    }

    #[test]
    fn with_local_user_from_env_threads_fields_through() {
        let mut buf = String::from("fallback");
        let context = MatchContext::with_local_user_from_env("h", "orig", "u", &mut buf);
        assert_eq!(context.host, "h");
        assert_eq!(context.original_host, "orig");
        assert_eq!(context.user, "u");
        // local_user is either the env value or the fallback; never empty
        // because `fallback` is the seed and a missing env var leaves it
        // untouched.
        assert!(!context.local_user.is_empty());
    }
}
