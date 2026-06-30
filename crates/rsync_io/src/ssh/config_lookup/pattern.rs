//! Host/user pattern matching shared by `Host` blocks and `Match`
//! lines.
//!
//! Holds the [`Pattern`] token type, the tokeniser
//! ([`parse_pattern_list`]), and the byte-level glob matcher with the
//! case-folding policy selected by [`MatchKind`]. Mirrors OpenSSH's
//! `match_pattern_list` semantics.

/// A single token from an ssh_config `Host` or `Match` pattern-list.
///
/// Stores the raw glob text (sans leading `!`) plus the negation flag.
/// Glob metacharacters `*` (any run) and `?` (one character) are honoured
/// at evaluation time by [`pattern_glob_matches`]. Mirrors OpenSSH's
/// `match_pattern_list` plus the embedded transport's
/// `host_matches_any_pattern` semantics, and is the single matcher
/// shared by `Host` blocks (SSC-5.b) and `Match` lines (SSC-4.b).
#[derive(Debug, Clone, Eq, PartialEq)]
pub(in crate::ssh) struct Pattern {
    glob: String,
    negate: bool,
}

impl Pattern {
    /// Builds a [`Pattern`] from a raw token. A leading `!` sets the
    /// negation flag; the remainder is stored verbatim as the glob text.
    pub(in crate::ssh) fn new(token: &str) -> Self {
        let (negate, glob) = token
            .strip_prefix('!')
            .map_or((false, token), |stripped| (true, stripped));
        Self {
            glob: glob.to_owned(),
            negate,
        }
    }

    /// Returns the stored glob text without the leading `!`.
    pub(in crate::ssh) fn glob(&self) -> &str {
        &self.glob
    }

    /// Returns `true` when this token is a negated pattern (`!glob`).
    pub(in crate::ssh) fn is_negated(&self) -> bool {
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
pub(in crate::ssh) fn parse_pattern_list(value: &str) -> Vec<Pattern> {
    value
        .split(|c: char| c.is_whitespace() || c == ',')
        .map(str::trim)
        .filter(|tok| !tok.is_empty())
        .map(Pattern::new)
        .collect()
}

/// Whether the input is a hostname or a username; controls case
/// folding per SSC-4.a.
#[derive(Copy, Clone)]
pub(super) enum MatchKind {
    Host,
    User,
}

/// Returns `true` when `input` matches the pattern list under OpenSSH's
/// OR-with-negation rule: any negated token that matches forces a
/// failure; otherwise at least one positive token must match. An empty
/// pattern list never matches.
pub(super) fn pattern_list_matches(patterns: &[Pattern], input: &str, kind: MatchKind) -> bool {
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
