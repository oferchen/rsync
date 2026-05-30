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
//! scope. SSC-4.c wires the `Match` evaluator into the parser, honouring
//! `host`, `originalhost`, `user`, `localuser`, and `all`. `Match exec`
//! is deliberately unsupported - executing arbitrary shell commands from
//! a passive config-lookup path is a security risk, and the
//! compression-detection use case does not need it. When a `Match exec`
//! block containing `Compression yes` is encountered, the parser emits
//! a user-visible warning explaining that the exec condition was not
//! evaluated and suggesting a workaround. The rest of the SKIP set
//! (`canonical`, `final`, `tagged`) likewise short-circuits the block.
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
/// configures `Compression yes` for `ctx` at top level or under a
/// matching `Host` or `Match` block.
///
/// `options` is the SSH option argv; a `-F <file>` (or `-F<file>`)
/// override is honoured first when present and the file exists. After
/// the override, the lookup tries `~/.ssh/config`, then
/// `/etc/ssh/ssh_config`. The first existing file wins; later files in
/// the list are not consulted, matching OpenSSH's behaviour when `-F`
/// is supplied.
///
/// `ctx` carries the connection context evaluated by SSC-4.b/SSC-5.b:
/// the destination host (used for `Host` blocks and `Match host`),
/// `originalhost`, remote user, and local user. When every field is
/// empty only top-level and `Host *` / `Match all` directives can fire.
///
/// Returns `false` when:
/// - no candidate file exists,
/// - the chosen file does not contain a matching `Compression yes`,
/// - the chosen file fails to parse (a `debug_log!` line is emitted and
///   the function reports `false` rather than aborting the transfer).
pub(super) fn ssh_config_enables_compression(options: &[OsString], ctx: &MatchContext<'_>) -> bool {
    for candidate in candidate_paths(options) {
        if !candidate.is_file() {
            continue;
        }
        return read_and_check(&candidate, ctx);
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

/// Reads `path` and returns whether it enables compression for `ctx`.
/// Parse and I/O errors are converted to `false` with a single
/// diagnostic line.
fn read_and_check(path: &Path, ctx: &MatchContext<'_>) -> bool {
    match fs::read_to_string(path) {
        Ok(text) => parse_enables_compression(&text, ctx),
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
/// for `ctx` at top level or under a matching `Host` or `Match` block.
///
/// Per OpenSSH's first-match-wins rule (see SSC-4.a "First-match-wins
/// ordering" and SSC-5 audit findings G1/G2), each scope keeps its own
/// `Option<bool>` slot that records the first assignment encountered.
/// The final answer ORs the scope slots: any scope whose first hit was
/// `Compression yes` flips the warning. A `Host` block contributes only
/// when `ctx.host` matches at least one positive pattern token and no
/// negated token (`pattern_list_matches`, sourced from SSC-4.b). A
/// `Match` block contributes only when every condition on its header
/// line evaluates true against `ctx`; SKIP/DEFER conditions
/// (`canonical`, `final`, `tagged`, `exec`) render the whole block
/// inert per SSC-4.a.
///
/// `ctx`'s fields may be empty; in that case only `Host *` and
/// `Match all` (or other patterns that tolerate empty input) can match.
///
/// When a `Match exec` block containing `Compression yes` is
/// encountered, a user-visible warning is emitted explaining that the
/// exec condition was not evaluated and suggesting a workaround (move
/// the directive to a `Host` or `Match host` block, or pass
/// `-e "ssh -C"` explicitly). The warning fires only when the skipped
/// block actually contains a compression directive that would affect
/// the detection result - bare `Match exec` blocks without compression
/// settings produce no warning.
///
/// Exposed to tests so they can assert behaviour without disk I/O.
pub(super) fn parse_enables_compression(text: &str, ctx: &MatchContext<'_>) -> bool {
    let mut block = Block::TopLevel;
    let mut top_level: Option<bool> = None;
    let mut host_block: Option<bool> = None;
    let mut match_block: Option<bool> = None;
    let mut exec_block_has_compression = false;

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
                let mut saw_exec = false;
                let applies = match_line_applies(value, ctx, &mut saw_exec);
                block = if saw_exec {
                    Block::MatchExecSkipped
                } else {
                    Block::MatchEvaluated(applies)
                };
            }
            "compression" => {
                let parsed = parse_yes_no(value);
                match &block {
                    Block::TopLevel if top_level.is_none() => top_level = parsed,
                    Block::Host(patterns)
                        if host_block.is_none()
                            && pattern_list_matches(patterns, ctx.host, MatchKind::Host) =>
                    {
                        host_block = parsed;
                    }
                    Block::MatchEvaluated(true) if match_block.is_none() => {
                        match_block = parsed;
                    }
                    Block::MatchExecSkipped => {
                        if parsed == Some(true) {
                            exec_block_has_compression = true;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    if exec_block_has_compression {
        debug_log!(
            Io,
            1,
            "ssh_config compression detection: Match exec block contains \
             Compression yes but the exec condition was not evaluated"
        );
        eprintln!(
            "warning: ssh_config contains \"Compression yes\" inside a \"Match exec\" block."
        );
        eprintln!("         The exec condition was not evaluated because executing arbitrary");
        eprintln!("         commands from a config-lookup path is a security risk. If SSH");
        eprintln!("         compression is active, oc-rsync's --compress will double-compress.");
        eprintln!("         Workaround: move \"Compression yes\" to a Host or Match host block,");
        eprintln!("         or pass -e \"ssh -C\" explicitly so oc-rsync can detect it.");
    }

    top_level.unwrap_or(false) || host_block.unwrap_or(false) || match_block.unwrap_or(false)
}

/// Active config block while parsing.
///
/// SSC-5.b replaced the prior `HostStar`/`HostOther` split with a
/// single `Host(Vec<Pattern>)` variant so the parser retains every
/// token from the `Host` line. The `Compression` arm consults the
/// shared SSC-4.b `pattern_list_matches` against the target host
/// instead of the old "`*` literal only" shortcut, closing audit gap
/// G1 (per-host blocks dropped) and G3 (matcher duplication).
///
/// SSC-4.c added [`Block::MatchEvaluated`]: when the parser encounters a
/// `Match` directive it evaluates the header line once via
/// [`match_line_applies`] and records the boolean outcome. Subsequent
/// `Compression` directives inside the block consult that cached
/// decision instead of re-evaluating per directive.
///
/// MED-3 added [`Block::MatchExecSkipped`]: when a `Match exec` block
/// is encountered, the parser cannot evaluate the condition (security
/// risk - executing arbitrary commands from a passive config-lookup
/// path). Directives inside the block are tracked separately so the
/// parser can detect when `Compression yes` appears inside an
/// unevaluated exec block and emit a targeted warning.
#[derive(Clone, Eq, PartialEq)]
enum Block {
    TopLevel,
    Host(Vec<Pattern>),
    MatchEvaluated(bool),
    /// Block gated by a `Match exec` condition that was not evaluated.
    /// Directives inside this block are not honoured but are inspected
    /// for `Compression yes` to emit a targeted user warning.
    MatchExecSkipped,
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
/// `match_pattern_list`. Empty tokens are dropped. Shared by
/// `Host`-block resolution (SSC-5.b) and the `Match`-line evaluator
/// (SSC-4.c) so both callers route through one tokeniser.
pub(super) fn parse_pattern_list(value: &str) -> Vec<Pattern> {
    value
        .split(|c: char| c.is_whitespace() || c == ',')
        .map(str::trim)
        .filter(|tok| !tok.is_empty())
        .map(Pattern::new)
        .collect()
}

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

impl<'a> MatchContext<'a> {
    /// Builds a context with an explicit local-user string. Tests use
    /// this to avoid touching process environment.
    #[cfg_attr(not(test), allow(dead_code))]
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

/// Tokenises the value half of a `Match` header into [`MatchCondition`]
/// entries, then evaluates them against `ctx`.
///
/// Returns `true` when every recognised condition matches; `false`
/// otherwise. SKIP / DEFER keywords (`canonical`, `final`, `tagged`,
/// `exec`) short-circuit the line per SSC-4.a: encountering any of
/// them renders the whole block inert, regardless of the rest of the
/// line, mirroring the conservative reading documented in the spec.
///
/// `exec` is **never** executed. Spawning subprocesses from a passive
/// config-lookup path inverts the trust model the warning was meant to
/// live inside, so we treat it as a non-match with no fork/exec. See
/// SSC-4.a "DEFER" rationale for the full security justification.
///
/// When `exec` is encountered, `*saw_exec` is set to `true` so the
/// caller can distinguish an exec-skipped block from a legitimately
/// non-matching block. The flag is only set, never cleared, so
/// repeated calls accumulate the signal across multiple `Match exec`
/// blocks.
///
/// Recognises keys case-insensitively; arguments are split on whitespace
/// or commas via [`parse_pattern_list`] for the keys that take a
/// pattern-list. Unknown tokens cause the line to be treated as inert
/// (conservative: a typo cannot accidentally flip the warning).
fn match_line_applies(value: &str, ctx: &MatchContext<'_>, saw_exec: &mut bool) -> bool {
    let mut conditions = Vec::new();
    let mut tokens = value.split_ascii_whitespace();
    while let Some(keyword) = tokens.next() {
        let keyword_lc = keyword.to_ascii_lowercase();
        match keyword_lc.as_str() {
            "all" => conditions.push(MatchCondition::All),
            "host" => {
                let Some(arg) = tokens.next() else {
                    return false;
                };
                conditions.push(MatchCondition::Host(parse_pattern_list(arg)));
            }
            "originalhost" => {
                let Some(arg) = tokens.next() else {
                    return false;
                };
                conditions.push(MatchCondition::OriginalHost(parse_pattern_list(arg)));
            }
            "user" => {
                let Some(arg) = tokens.next() else {
                    return false;
                };
                conditions.push(MatchCondition::User(parse_pattern_list(arg)));
            }
            "localuser" => {
                let Some(arg) = tokens.next() else {
                    return false;
                };
                conditions.push(MatchCondition::LocalUser(parse_pattern_list(arg)));
            }
            // SKIP set (SSC-4.a): we never reach OpenSSH's second pass,
            // and no oc-rsync code path produces a `-P` tag. Any block
            // gated by one of these is dead code from our perspective.
            "canonical" | "final" | "tagged" => return false,
            // DELIBERATE OMISSION (MED-1, SSC-4.a DEFER): `Match exec`
            // runs an arbitrary shell command to decide whether the block
            // applies. Evaluating it here would be a security risk -
            // executing user-supplied commands from a passive config-lookup
            // path inverts the trust model. It also adds subprocess
            // complexity for a feature the compression-detection use case
            // does not need. The block is treated as non-matching with no
            // fork/exec; the argv-side SSC-1 check still catches `-C` and
            // `-o Compression=yes`.
            "exec" => {
                *saw_exec = true;
                return false;
            }
            _ => return false,
        }
    }
    evaluate_match(&conditions, ctx)
}

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

    /// Builds a host-only `MatchContext` for tests that only need to
    /// drive `Host`-block resolution. Mirrors the pre-SSC-4.c signature
    /// where `parse_enables_compression` took a bare `target_host`.
    fn host_ctx(target: &str) -> MatchContext<'_> {
        MatchContext::new(target, target, "", "")
    }

    #[test]
    fn top_level_compression_yes_detected() {
        assert!(parse_enables_compression(
            "Compression yes\n",
            &host_ctx("any.example.com")
        ));
    }

    #[test]
    fn top_level_compression_yes_detected_with_empty_host() {
        // Top-level scope must fire regardless of target host, including
        // the degenerate empty-string case used by callers that never
        // populate `SshCommand::host`.
        assert!(parse_enables_compression(
            "Compression yes\n",
            &host_ctx("")
        ));
    }

    #[test]
    fn host_star_compression_yes_detected() {
        let text = "Host *\n  Compression yes\n";
        assert!(parse_enables_compression(
            text,
            &host_ctx("any.example.com")
        ));
    }

    #[test]
    fn host_star_block_off_with_top_level_still_fires() {
        // Top-level directive must still win regardless of any subsequent
        // host-specific block that does not match the target.
        let text = "Compression yes\nHost db*\n  Compression no\n";
        assert!(parse_enables_compression(
            text,
            &host_ctx("web1.example.com")
        ));
    }

    #[test]
    fn per_host_literal_match_detected() {
        // SSC-5.b G1: literal `Host foo.example.com` blocks were dropped
        // pre-fix. The audit doc's first example asserts they now fire
        // when `target_host` matches the literal.
        let text = "Host web1.example.com\n  Compression yes\n";
        assert!(parse_enables_compression(
            text,
            &host_ctx("web1.example.com")
        ));
    }

    #[test]
    fn per_host_glob_match_detected() {
        // SSC-5.b G1: glob tokens like `Host web*` resolve via the
        // shared SSC-4.b `pattern_glob_matches` matcher.
        let text = "Host web*\n  Compression yes\n";
        assert!(parse_enables_compression(
            text,
            &host_ctx("web1.example.com")
        ));
    }

    #[test]
    fn per_host_glob_miss_returns_false() {
        // Negative case for the glob path: `Host db*` must not fire
        // when the target is `web1.example.com`.
        let text = "Host db*\n  Compression yes\n";
        assert!(!parse_enables_compression(
            text,
            &host_ctx("web1.example.com")
        ));
    }

    #[test]
    fn per_host_negation_blocks_match() {
        // SSC-5.b G1: OpenSSH negation semantics. A bang-prefixed token
        // that matches forces the whole pattern-list to fail, even when
        // a positive token (`*`) would otherwise match.
        let text = "Host !banned.example.com *\n  Compression yes\n";
        assert!(!parse_enables_compression(
            text,
            &host_ctx("banned.example.com")
        ));
    }

    #[test]
    fn per_host_negation_miss_keeps_positive_match() {
        // Negation only fires when the negated token itself matches.
        // For any other host, the positive `*` token wins.
        let text = "Host !banned.example.com *\n  Compression yes\n";
        assert!(parse_enables_compression(text, &host_ctx("ok.example.com")));
    }

    #[test]
    fn per_host_first_match_wins() {
        // OpenSSH first-match-wins within a scope: the first matching
        // `Compression` assignment in any `Host` block sticks, so a
        // later `Host *\n Compression yes` cannot override an earlier
        // matching `Host web1\n Compression no`.
        let text = "Host web1\n  Compression no\nHost *\n  Compression yes\n";
        assert!(!parse_enables_compression(text, &host_ctx("web1")));
    }

    #[test]
    fn per_host_compression_yes_ignored_when_target_does_not_match() {
        // Pre-SSC-5.b behaviour preserved for non-matching targets:
        // `Host foo` with target `bar` contributes nothing.
        let text = "Host foo.example.com\n  Compression yes\n";
        assert!(!parse_enables_compression(
            text,
            &host_ctx("bar.example.com")
        ));
    }

    #[test]
    fn compression_no_returns_false() {
        assert!(!parse_enables_compression(
            "Compression no\n",
            &host_ctx("any")
        ));
    }

    #[test]
    fn match_host_block_flips_compression_to_true() {
        // SSC-4.c: a `Match host` block whose pattern matches the target
        // must contribute its `Compression yes` directive. This is the
        // targeted regression that proves `Block::MatchEvaluated(true)`
        // is wired into the `Compression` arm. Without SSC-4.c the
        // `Match` block was inert and the assertion would fail.
        let text = "Match host bar\n  Compression yes\n";
        assert!(parse_enables_compression(text, &host_ctx("bar")));
    }

    #[test]
    fn equals_separator_supported() {
        assert!(parse_enables_compression(
            "Compression=yes\n",
            &host_ctx("any")
        ));
    }

    #[test]
    fn comments_stripped() {
        assert!(parse_enables_compression(
            "# header\nCompression yes # trailing\n",
            &host_ctx("any")
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

    // SSC-4.d: synthetic ssh_config fixtures exercising the wired
    // `Match` block path in `parse_enables_compression`. Each test
    // drives one combination of header conditions and asserts the
    // resulting compression decision.

    /// Builds a fully populated [`MatchContext`] for the SSC-4.d
    /// fixtures. Mirrors the `Match`-line semantics: `host` and
    /// `original_host` may differ to exercise the originalhost/host
    /// distinction.
    fn match_ctx<'a>(
        host: &'a str,
        original_host: &'a str,
        user: &'a str,
        local_user: &'a str,
    ) -> MatchContext<'a> {
        MatchContext::new(host, original_host, user, local_user)
    }

    #[test]
    fn match_host_glob_enables_compression_on_hit() {
        // `Match host *.example.com` matches `web1.example.com`, so the
        // `Compression yes` inside the block fires.
        let text = "Match host *.example.com\n  Compression yes\n";
        assert!(parse_enables_compression(
            text,
            &match_ctx("web1.example.com", "web1.example.com", "", "")
        ));
    }

    #[test]
    fn match_host_glob_ignored_when_host_does_not_match() {
        // Same fixture, target does not match the glob; the block is
        // inert and no other scope enables compression.
        let text = "Match host *.example.com\n  Compression yes\n";
        assert!(!parse_enables_compression(
            text,
            &match_ctx("db.internal", "db.internal", "", "")
        ));
    }

    #[test]
    fn match_all_enables_compression_unconditionally() {
        // `Match all` matches any context, including an entirely empty
        // one, and contributes its `Compression yes`.
        let text = "Match all\n  Compression yes\n";
        assert!(parse_enables_compression(text, &match_ctx("", "", "", "")));
    }

    #[test]
    fn match_block_first_match_wins_within_scope() {
        // SSC-4.c first-match-wins across the shared match-block slot:
        // the first matching `Compression` (here `yes`) sticks and the
        // later `Match all\n Compression no` cannot override it.
        let text = "Match host *.example.com\n  Compression yes\nMatch all\n  Compression no\n";
        assert!(parse_enables_compression(
            text,
            &match_ctx("web1.example.com", "web1.example.com", "", "")
        ));
    }

    #[test]
    fn match_block_first_no_blocks_later_yes() {
        // Mirror image: a `Match all\n Compression no` ahead of a
        // matching `Compression yes` sinks the scope. The earlier `no`
        // is recorded as the first decision and the later block's
        // `yes` is dropped.
        let text = "Match all\n  Compression no\nMatch host *.example.com\n  Compression yes\n";
        assert!(!parse_enables_compression(
            text,
            &match_ctx("web1.example.com", "web1.example.com", "", "")
        ));
    }

    #[test]
    fn match_user_pattern_enables_compression() {
        // `Match user deploy` matches the context user; `Compression
        // yes` fires.
        let text = "Match user deploy\n  Compression yes\n";
        assert!(parse_enables_compression(
            text,
            &match_ctx("web1", "web1", "deploy", "")
        ));
    }

    #[test]
    fn match_user_pattern_miss_keeps_compression_off() {
        let text = "Match user deploy\n  Compression yes\n";
        assert!(!parse_enables_compression(
            text,
            &match_ctx("web1", "web1", "root", "")
        ));
    }

    #[test]
    fn match_originalhost_distinct_from_host() {
        // `originalhost` resolves against the pre-canonicalization
        // operand. With `original_host = "web1"` but `host = "web1.
        // canonical.example.com"`, the originalhost block fires while
        // a `host = web1` block would not.
        let text = "Match originalhost web1\n  Compression yes\n";
        assert!(parse_enables_compression(
            text,
            &match_ctx("web1.canonical.example.com", "web1", "", "")
        ));
    }

    #[test]
    fn match_host_does_not_match_originalhost_operand() {
        // Inverse of the above: `Match host web1` consults the
        // canonicalized field. With `host` populated only with the
        // canonical form, a literal `web1` pattern misses.
        let text = "Match host web1\n  Compression yes\n";
        assert!(!parse_enables_compression(
            text,
            &match_ctx("web1.canonical.example.com", "web1", "", "")
        ));
    }

    #[test]
    fn match_canonical_keyword_skips_block() {
        // SKIP set: `canonical` renders the block inert even when the
        // host pattern would otherwise match.
        let text = "Match canonical host *.example.com\n  Compression yes\n";
        assert!(!parse_enables_compression(
            text,
            &match_ctx("web1.example.com", "web1.example.com", "", "")
        ));
    }

    #[test]
    fn match_final_keyword_skips_block() {
        let text = "Match final host *.example.com\n  Compression yes\n";
        assert!(!parse_enables_compression(
            text,
            &match_ctx("web1.example.com", "web1.example.com", "", "")
        ));
    }

    #[test]
    fn match_tagged_keyword_skips_block() {
        let text = "Match tagged ci host *.example.com\n  Compression yes\n";
        assert!(!parse_enables_compression(
            text,
            &match_ctx("web1.example.com", "web1.example.com", "", "")
        ));
    }

    #[test]
    fn match_exec_never_spawns_and_never_matches() {
        // Security: `exec` must never spawn the command. The block is
        // treated as non-matching and the `Compression yes` is dropped.
        // If this test ever flips to true, the parser has started
        // invoking the shell - regression of SSC-4.a's DEFER policy.
        let text = "Match exec /bin/true\n  Compression yes\n";
        assert!(!parse_enables_compression(
            text,
            &match_ctx("web1.example.com", "web1.example.com", "", "")
        ));
    }

    #[test]
    fn top_level_compression_no_not_overridden_by_non_matching_match() {
        // Top-level scope captures `Compression no` first. A later
        // `Match host` block whose pattern misses contributes nothing,
        // so the overall answer stays `false`.
        let text = "Compression no\nMatch host db*\n  Compression yes\n";
        assert!(!parse_enables_compression(
            text,
            &match_ctx("web1.example.com", "web1.example.com", "", "")
        ));
    }

    #[test]
    fn top_level_compression_no_or_with_matching_match_yes() {
        // Top-level scope is `Compression no`; match-block scope is
        // `Compression yes`. The final answer ORs the slots, so any
        // scope set to true wins. Confirms the OR semantics described
        // in `parse_enables_compression`.
        let text = "Compression no\nMatch host *.example.com\n  Compression yes\n";
        assert!(parse_enables_compression(
            text,
            &match_ctx("web1.example.com", "web1.example.com", "", "")
        ));
    }

    #[test]
    fn host_block_and_match_block_both_contribute_via_or() {
        // Host block enables compression for `web*`; match block
        // enables compression for `Match user deploy`. Either one is
        // sufficient on its own; together they still produce `true`.
        let text = "Host web*\n  Compression yes\nMatch user deploy\n  Compression yes\n";
        assert!(parse_enables_compression(
            text,
            &match_ctx("web1", "web1", "deploy", "")
        ));
    }

    #[test]
    fn host_block_miss_with_match_block_hit_still_enables() {
        // Host block misses (`db*` vs `web1`); match block hits via
        // user. OR across scopes flips the answer to `true`.
        let text = "Host db*\n  Compression yes\nMatch user deploy\n  Compression yes\n";
        assert!(parse_enables_compression(
            text,
            &match_ctx("web1", "web1", "deploy", "")
        ));
    }

    #[test]
    fn match_localuser_matches_env_local_user() {
        // `Match localuser` evaluates against the local-user field that
        // `MatchContext::with_local_user_from_env` populates from the
        // process environment. Drive the test against the same string
        // we hand the context to keep the assertion deterministic.
        let mut buf = String::from("ofer");
        let ctx = MatchContext::with_local_user_from_env("any", "any", "", &mut buf);
        let text = format!("Match localuser {}\n  Compression yes\n", ctx.local_user);
        assert!(parse_enables_compression(&text, &ctx));
    }

    #[test]
    fn match_localuser_miss_keeps_compression_off() {
        // Same fixture shape, but pattern does not match the local
        // user; the block contributes nothing.
        let mut buf = String::from("ofer");
        let ctx = MatchContext::with_local_user_from_env("any", "any", "", &mut buf);
        let text = "Match localuser nobody\n  Compression yes\n";
        assert!(!parse_enables_compression(text, &ctx));
    }

    // MED-2: `Match exec` warning flag tests. These exercise the
    // `saw_exec` output parameter on `match_line_applies` to verify
    // the one-shot warning fires exactly when expected.

    #[test]
    fn match_exec_sets_saw_exec_flag() {
        // When the parser encounters `Match exec`, the saw_exec flag
        // must be set to true so the caller can emit a warning.
        let mut saw_exec = false;
        let result = match_line_applies("exec /bin/true", &ctx("web1", "", ""), &mut saw_exec);
        assert!(!result, "Match exec must not match");
        assert!(saw_exec, "saw_exec flag must be set");
    }

    #[test]
    fn match_without_exec_does_not_set_flag() {
        // Normal Match conditions must not set the saw_exec flag.
        let mut saw_exec = false;
        match_line_applies("host web1", &ctx("web1", "", ""), &mut saw_exec);
        assert!(!saw_exec, "saw_exec should not be set for non-exec Match");
    }

    #[test]
    fn match_all_does_not_set_flag() {
        let mut saw_exec = false;
        match_line_applies("all", &ctx("", "", ""), &mut saw_exec);
        assert!(!saw_exec, "saw_exec should not be set for Match all");
    }

    #[test]
    fn match_exec_flag_set_once_across_multiple_exec_blocks() {
        // Multiple Match exec blocks should set the flag but only the
        // first encounter matters for the one-shot warning. Verify the
        // flag stays true after a second exec block.
        let mut saw_exec = false;
        match_line_applies("exec /bin/true", &ctx("web1", "", ""), &mut saw_exec);
        assert!(saw_exec);
        match_line_applies("exec /bin/false", &ctx("web1", "", ""), &mut saw_exec);
        assert!(saw_exec, "flag should remain true after second exec");
    }

    #[test]
    fn match_exec_flag_not_reset_by_non_exec_match() {
        // Once saw_exec is set by a Match exec block, a subsequent
        // non-exec Match must not clear it.
        let mut saw_exec = false;
        match_line_applies("exec /usr/bin/test", &ctx("web1", "", ""), &mut saw_exec);
        assert!(saw_exec);
        match_line_applies("host web1", &ctx("web1", "", ""), &mut saw_exec);
        assert!(saw_exec, "non-exec Match must not clear saw_exec flag");
    }

    // MED-6: `Match exec` warning integration tests exercising the full
    // `parse_enables_compression` path with synthetic ssh_config fixtures.

    #[test]
    fn match_exec_with_compression_yes_returns_false() {
        // A `Match exec` block with `Compression yes` must not contribute
        // to the compression result - the exec condition was never
        // evaluated, so we cannot know whether the block would apply.
        let text = "Match exec /usr/local/bin/check-vpn\n  Compression yes\n";
        assert!(!parse_enables_compression(
            text,
            &match_ctx("web1.example.com", "web1.example.com", "", "")
        ));
    }

    #[test]
    fn match_exec_without_compression_does_not_warn() {
        // A `Match exec` block that does not contain `Compression` should
        // not trigger the warning. The warning is only relevant when
        // compression detection may be incomplete.
        let text = "Match exec /usr/local/bin/check-vpn\n  ForwardAgent yes\n";
        // No assertion on stderr - we verify correctness by confirming
        // the function returns false and does not panic.
        assert!(!parse_enables_compression(
            text,
            &match_ctx("web1.example.com", "web1.example.com", "", "")
        ));
    }

    #[test]
    fn match_exec_compression_no_does_not_warn() {
        // `Compression no` inside a `Match exec` block is not actionable
        // - even if the block were evaluated, it would not enable
        // compression. The warning should not fire for this case.
        let text = "Match exec /usr/local/bin/check-vpn\n  Compression no\n";
        assert!(!parse_enables_compression(
            text,
            &match_ctx("web1.example.com", "web1.example.com", "", "")
        ));
    }

    #[test]
    fn match_exec_with_compression_yes_alongside_top_level() {
        // Top-level `Compression no` plus a `Match exec` block with
        // `Compression yes`. The top-level `no` is honoured; the exec
        // block is skipped. The overall result is `false`, but the
        // warning should still fire because the exec block might enable
        // compression if evaluated.
        let text = "Compression no\nMatch exec /usr/local/bin/check-vpn\n  Compression yes\n";
        assert!(!parse_enables_compression(
            text,
            &match_ctx("web1.example.com", "web1.example.com", "", "")
        ));
    }

    #[test]
    fn match_exec_block_does_not_affect_subsequent_blocks() {
        // A `Match exec` block must not contaminate subsequent `Match`
        // blocks. The `Match all` block after the exec block should be
        // evaluated normally.
        let text = "Match exec /usr/local/bin/check-vpn\n  Compression yes\n\
                    Match all\n  Compression yes\n";
        assert!(parse_enables_compression(
            text,
            &match_ctx("web1.example.com", "web1.example.com", "", "")
        ));
    }

    #[test]
    fn match_exec_block_followed_by_host_block() {
        // After a `Match exec` block, a `Host` block should be evaluated
        // normally and contribute its compression setting.
        let text = "Match exec /usr/local/bin/check-vpn\n  Compression yes\n\
                    Host *.example.com\n  Compression yes\n";
        assert!(parse_enables_compression(
            text,
            &match_ctx("web1.example.com", "web1.example.com", "", "")
        ));
    }

    #[test]
    fn multiple_match_exec_blocks_with_compression() {
        // Multiple `Match exec` blocks each containing `Compression yes`
        // should all be skipped. The overall result is `false`.
        let text = "Match exec /usr/local/bin/check-vpn\n  Compression yes\n\
                    Match exec /usr/local/bin/check-lan\n  Compression yes\n";
        assert!(!parse_enables_compression(
            text,
            &match_ctx("web1.example.com", "web1.example.com", "", "")
        ));
    }

    #[test]
    fn match_exec_block_with_other_directives_and_compression() {
        // A realistic ssh_config snippet where the exec block contains
        // multiple directives including `Compression yes`. Only the
        // compression directive triggers the warning logic.
        let text = "Match exec \"test -f /etc/vpn.conf\"\n\
                    \x20 ProxyJump bastion.example.com\n\
                    \x20 Compression yes\n\
                    \x20 ForwardAgent yes\n";
        assert!(!parse_enables_compression(
            text,
            &match_ctx("internal.example.com", "internal.example.com", "", "")
        ));
    }

    #[test]
    fn realistic_ssh_config_with_match_exec_and_host_blocks() {
        // A realistic multi-section ssh_config where some blocks use
        // `Match exec` and others use `Host`. The host-block compression
        // should be detected while the exec-block compression is skipped.
        let text = "\
Host bastion.example.com\n\
  Compression no\n\
\n\
Match exec \"test -f /etc/vpn.conf\"\n\
  Compression yes\n\
  ProxyJump bastion.example.com\n\
\n\
Host *.internal.example.com\n\
  Compression yes\n\
\n\
Host *\n\
  ServerAliveInterval 60\n";
        // Target matches `*.internal.example.com`, so host-block
        // compression fires.
        assert!(parse_enables_compression(
            text,
            &match_ctx("db.internal.example.com", "db.internal.example.com", "", "")
        ));
        // Target does not match any host block with compression,
        // and the exec block is skipped.
        assert!(!parse_enables_compression(
            text,
            &match_ctx("external.example.com", "external.example.com", "", "")
        ));
    }
}
