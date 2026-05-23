//! SSH client config lookup for the `Compression` directive.
//!
//! Closes the SSC-3 gap: SSC-1 only inspects argv, so a user with
//! `Compression yes` set globally in `~/.ssh/config` or
//! `/etc/ssh/ssh_config` gets no warning when they also pass rsync's
//! `--compress`. SSC-5.b extends the parser to honour per-host `Host`
//! blocks (`Host web*.example.com`, `Host !banned.example.com *`) using
//! the connection target as the pattern-match input.
//!
//! # Scope
//!
//! Top-level directives, `Host` blocks (including glob and negation
//! tokens), and `Match` blocks whose conditions all pass are honoured.
//! Resolution mirrors OpenSSH's first-match-wins rule per directive: the
//! first matching `Compression` assignment in the scan wins for its
//! scope. `Match exec` is deferred per SSC-4.a; the rest of the SKIP
//! set (`canonical`, `final`, `tagged`) short-circuits the block.
//!
//! # Failure mode
//!
//! Parse errors and I/O errors never propagate. A malformed file emits
//! one `debug_log!` line and the caller falls back to the argv-only
//! answer. Hard-failing the transfer because a user's ssh_config has a
//! stray byte would be a worse outcome than missing a warning.
//!
//! # References
//!
//! - `docs/design/ssc-5-host-pattern-audit.md` - SSC-5 audit and fix
//!   shape that motivated this module's `Host`-pattern wiring.
//! - `docs/design/ssc-4a-match-conditions.md` - shared `Pattern` type
//!   and `MatchKind`-based case-folding policy (SSC-4.b).
//! - Memory note `project_ssh_compression_no_config_parse.md` - tracks
//!   the residual gaps closed by SSC-3..SSC-5.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use logging::debug_log;

/// Returns `true` when `~/.ssh/config` or `/etc/ssh/ssh_config`
/// configures `Compression yes` for `target_host` at top level or under
/// a matching `Host` block.
///
/// `options` is the SSH option argv; a `-F <file>` (or `-F<file>`)
/// override is honoured first when present and the file exists. After
/// the override, the lookup tries `~/.ssh/config`, then
/// `/etc/ssh/ssh_config`. The first existing file wins; later files in
/// the list are not consulted, matching OpenSSH's behaviour when `-F`
/// is supplied.
///
/// `target_host` is the destination hostname taken from
/// `SshCommand::host`. SSC-5.b uses it to evaluate per-host `Host`
/// blocks (`Host web*.example.com`); when empty, only top-level and
/// `Host *` directives can fire.
///
/// Returns `false` when:
/// - no candidate file exists,
/// - the chosen file does not contain a matching `Compression yes`,
/// - the chosen file fails to parse (a `debug_log!` line is emitted and
///   the function reports `false` rather than aborting the transfer).
pub(super) fn ssh_config_enables_compression(options: &[OsString], target_host: &str) -> bool {
    for candidate in candidate_paths(options) {
        if !candidate.is_file() {
            continue;
        }
        return read_and_check(&candidate, target_host);
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

/// Reads `path` and returns whether it enables compression for
/// `target_host`. Parse and I/O errors are converted to `false` with a
/// single diagnostic line.
fn read_and_check(path: &Path, target_host: &str) -> bool {
    match fs::read_to_string(path) {
        Ok(text) => parse_enables_compression(&text, target_host),
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
/// for `target_host` at top level or under a matching `Host` block.
///
/// Per OpenSSH's first-match-wins rule (see SSC-4.a "First-match-wins
/// ordering" and SSC-5 audit findings G1/G2), each scope keeps its own
/// `Option<bool>` slot that records the first assignment encountered.
/// The final answer ORs the two scope slots: any scope whose first hit
/// was `Compression yes` flips the warning. A `Host` block contributes
/// only when `target_host` matches at least one positive pattern token
/// and no negated token (`pattern_list_matches`, sourced from SSC-4.b).
///
/// `target_host` may be empty; in that case only `Host *` patterns can
/// match (the empty-input guard in `pattern_list_matches` already
/// rejects non-empty patterns against an empty input).
///
/// Exposed to tests so they can assert behaviour without disk I/O.
pub(super) fn parse_enables_compression(text: &str, target_host: &str) -> bool {
    let mut block = Block::TopLevel;
    let mut top_level: Option<bool> = None;
    let mut host_block: Option<bool> = None;

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
                block = Block::Host(parse_pattern_list(value));
            }
            "match" => {
                block = Block::Match;
            }
            "compression" => {
                let parsed = parse_yes_no(value);
                match &block {
                    Block::TopLevel if top_level.is_none() => top_level = parsed,
                    Block::Host(patterns)
                        if host_block.is_none()
                            && pattern_list_matches(patterns, target_host, MatchKind::Host) =>
                    {
                        host_block = parsed;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    top_level.unwrap_or(false) || host_block.unwrap_or(false)
}

/// Active config block while parsing.
///
/// SSC-5.b replaced the prior `HostStar`/`HostOther` split with a
/// single `Host(Vec<Pattern>)` variant so the parser retains every
/// token from the `Host` line. The `Compression` arm consults the
/// shared SSC-4.b `pattern_list_matches` against the target host
/// instead of the old "`*` literal only" shortcut, closing audit gap
/// G1 (per-host blocks dropped) and G3 (matcher duplication).
#[derive(Clone, Eq, PartialEq)]
enum Block {
    TopLevel,
    Host(Vec<Pattern>),
    Match,
}

/// A single token from an ssh_config `Host` or `Match` pattern-list.
///
/// Stores the raw glob text (sans leading `!`) plus the negation flag.
/// Glob metacharacters `*` (any run) and `?` (one character) are honoured
/// at evaluation time by [`pattern_glob_matches`]. Mirrors OpenSSH's
/// `match_pattern_list` plus the embedded transport's
/// `host_matches_any_pattern` semantics, and is the single matcher
/// shared by `Host` blocks (SSC-5.b) and `Match` lines (SSC-4.b).
#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct Pattern {
    glob: String,
    negate: bool,
}

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

/// Tokenises a raw pattern-list (the value half of a `Host` line or a
/// `Match host`/`originalhost`/`user`/`localuser` condition) into
/// [`Pattern`] entries.
///
/// Tokens are split on whitespace or commas, matching OpenSSH's
/// `match_pattern_list`. Empty tokens are dropped. Used by both
/// `parse_enables_compression` (SSC-5.b) and future `Match` plumbing
/// (SSC-4.c) so the two callers share one tokeniser.
pub(super) fn parse_pattern_list(value: &str) -> Vec<Pattern> {
    value
        .split(|c: char| c.is_whitespace() || c == ',')
        .map(str::trim)
        .filter(|tok| !tok.is_empty())
        .map(Pattern::new)
        .collect()
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

/// Whether the input is a hostname or a username; controls case
/// folding per SSC-4.a.
#[derive(Copy, Clone)]
enum MatchKind {
    Host,
    User,
}

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

/// Returns `true` when comparisons for `kind` should be ASCII
/// case-folded. Hostnames are always folded; usernames are folded only
/// on Windows, where account names are inherently case-insensitive.
fn case_fold(kind: MatchKind) -> bool {
    match kind {
        MatchKind::Host => true,
        MatchKind::User => cfg!(windows),
    }
}

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
        assert!(parse_enables_compression(
            "Compression yes\n",
            "any.example.com"
        ));
    }

    #[test]
    fn top_level_compression_yes_detected_with_empty_host() {
        // Top-level scope must fire regardless of target host, including
        // the degenerate empty-string case used by callers that never
        // populate `SshCommand::host`.
        assert!(parse_enables_compression("Compression yes\n", ""));
    }

    #[test]
    fn host_star_compression_yes_detected() {
        let text = "Host *\n  Compression yes\n";
        assert!(parse_enables_compression(text, "any.example.com"));
    }

    #[test]
    fn host_star_block_off_with_top_level_still_fires() {
        // Top-level directive must still win regardless of any subsequent
        // host-specific block that does not match the target.
        let text = "Compression yes\nHost db*\n  Compression no\n";
        assert!(parse_enables_compression(text, "web1.example.com"));
    }

    #[test]
    fn per_host_literal_match_detected() {
        // SSC-5.b G1: literal `Host foo.example.com` blocks were dropped
        // pre-fix. The audit doc's first example asserts they now fire
        // when `target_host` matches the literal.
        let text = "Host web1.example.com\n  Compression yes\n";
        assert!(parse_enables_compression(text, "web1.example.com"));
    }

    #[test]
    fn per_host_glob_match_detected() {
        // SSC-5.b G1: glob tokens like `Host web*` resolve via the
        // shared SSC-4.b `pattern_glob_matches` matcher.
        let text = "Host web*\n  Compression yes\n";
        assert!(parse_enables_compression(text, "web1.example.com"));
    }

    #[test]
    fn per_host_glob_miss_returns_false() {
        // Negative case for the glob path: `Host db*` must not fire
        // when the target is `web1.example.com`.
        let text = "Host db*\n  Compression yes\n";
        assert!(!parse_enables_compression(text, "web1.example.com"));
    }

    #[test]
    fn per_host_negation_blocks_match() {
        // SSC-5.b G1: OpenSSH negation semantics. A bang-prefixed token
        // that matches forces the whole pattern-list to fail, even when
        // a positive token (`*`) would otherwise match.
        let text = "Host !banned.example.com *\n  Compression yes\n";
        assert!(!parse_enables_compression(text, "banned.example.com"));
    }

    #[test]
    fn per_host_negation_miss_keeps_positive_match() {
        // Negation only fires when the negated token itself matches.
        // For any other host, the positive `*` token wins.
        let text = "Host !banned.example.com *\n  Compression yes\n";
        assert!(parse_enables_compression(text, "ok.example.com"));
    }

    #[test]
    fn per_host_first_match_wins() {
        // OpenSSH first-match-wins within a scope: the first matching
        // `Compression` assignment in any `Host` block sticks, so a
        // later `Host *\n Compression yes` cannot override an earlier
        // matching `Host web1\n Compression no`.
        let text = "Host web1\n  Compression no\nHost *\n  Compression yes\n";
        assert!(!parse_enables_compression(text, "web1"));
    }

    #[test]
    fn per_host_compression_yes_ignored_when_target_does_not_match() {
        // Pre-SSC-5.b behaviour preserved for non-matching targets:
        // `Host foo` with target `bar` contributes nothing.
        let text = "Host foo.example.com\n  Compression yes\n";
        assert!(!parse_enables_compression(text, "bar.example.com"));
    }

    #[test]
    fn compression_no_returns_false() {
        assert!(!parse_enables_compression("Compression no\n", "any"));
    }

    #[test]
    fn match_block_compression_yes_ignored() {
        // SSC-5.b leaves `Match` blocks for SSC-4.c to wire; the
        // `Block::Match` arm still drops the directive here.
        let text = "Match host bar\n  Compression yes\n";
        assert!(!parse_enables_compression(text, "bar"));
    }

    #[test]
    fn equals_separator_supported() {
        assert!(parse_enables_compression("Compression=yes\n", "any"));
    }

    #[test]
    fn comments_stripped() {
        assert!(parse_enables_compression(
            "# header\nCompression yes # trailing\n",
            "any"
        ));
    }

    #[test]
    fn parse_pattern_list_splits_whitespace_and_commas() {
        let parsed = parse_pattern_list("web*,app*  !banned");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].glob(), "web*");
        assert!(!parsed[0].is_negated());
        assert_eq!(parsed[1].glob(), "app*");
        assert!(!parsed[1].is_negated());
        assert_eq!(parsed[2].glob(), "banned");
        assert!(parsed[2].is_negated());
    }

    #[test]
    fn parse_pattern_list_empty_input_yields_no_tokens() {
        assert!(parse_pattern_list("").is_empty());
        assert!(parse_pattern_list("   ,, , ").is_empty());
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
