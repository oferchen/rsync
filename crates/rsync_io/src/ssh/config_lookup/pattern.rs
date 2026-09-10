//! Host/user pattern matching shared by `Host` blocks and `Match`
//! lines.
//!
//! Holds the [`Pattern`] token type, the two tokenisers
//! ([`parse_host_pattern_list`] for `Host` lines,
//! [`parse_pattern_list`] for `Match` criteria), and the byte-level glob
//! matcher with the case-folding policy selected by [`MatchKind`].
//! The separator set and the case-folding rule both differ per keyword,
//! so neither is inferred from "it is a hostname".

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

/// Tokenises a `Match host`/`originalhost`/`user`/`localuser` condition
/// argument into [`Pattern`] entries.
///
/// Tokens are split on whitespace **or commas**: the `Match` criteria are
/// pattern-lists, and `match_pattern_list` cuts each subpattern at a comma
/// (openssh/match.c:143). Empty tokens are dropped.
///
/// ⚠ Not for `Host` lines - those are argv-tokenised and a comma is
/// ordinary pattern text there. Use [`parse_host_pattern_list`].
pub(in crate::ssh) fn parse_pattern_list(value: &str) -> Vec<Pattern> {
    split_tokens(value, |c| c.is_whitespace() || c == ',')
}

/// Builds the `Host` pattern list from already-split `argv_split` tokens.
///
/// Takes tokens rather than the raw value because upstream tokenises the
/// line exactly once, at openssh/readconf.c:1196, and the `oHost` arm then
/// consumes that same vector through `argv_next`
/// (openssh/readconf.c:1831-1858). Re-splitting the value here would be a
/// second tokeniser that could disagree with the first about quotes,
/// escapes and `#`.
///
/// Each token becomes one pattern verbatim: the `oHost` arm matches with
/// `match_pattern` directly (openssh/readconf.c:1844) and never reaches
/// `match_pattern_list`, so a comma is ordinary pattern text and
/// `Host a,b` does not match the alias `a`. That is why this is a
/// separate entry point rather than a widened [`parse_pattern_list`] -
/// the four `Match` criteria that share the tokeniser need the comma
/// split that `Host` must not have.
///
/// An empty token is kept rather than dropped. Upstream REFUSES the line
/// outright (openssh/readconf.c:1832-1836), but that refusal is ordered
/// *inside* the match loop, after a negated token that already matched has
/// broken out - so `Host !a ""` is accepted for the alias `a` and refused
/// for any other (measured against `ssh -G`). This reader cannot express
/// that ordering because it builds the whole list before matching, and its
/// documented contract is never to fail. An empty pattern matches only the
/// empty host, so keeping it is inert here; the refusal lives in the
/// reader that configures the connection (`embedded::ssh_config`).
pub(in crate::ssh) fn host_patterns_from_tokens(tokens: &[String]) -> Vec<Pattern> {
    tokens.iter().map(|token| Pattern::new(token)).collect()
}

/// Splits `value` on `is_separator`, dropping empty tokens.
fn split_tokens(value: &str, is_separator: impl Fn(char) -> bool) -> Vec<Pattern> {
    value
        .split(is_separator)
        .map(str::trim)
        .filter(|tok| !tok.is_empty())
        .map(Pattern::new)
        .collect()
}

/// Which keyword's pattern is being matched; controls case folding.
///
/// Split by keyword rather than by "hostname vs username": the two
/// host-carrying keywords differ from each other, so folding cannot be
/// inferred from the input being a hostname.
#[derive(Copy, Clone)]
pub(super) enum MatchKind {
    /// A `Host` block pattern: compared byte-exactly. The `oHost` arm
    /// calls `match_pattern` directly (openssh/readconf.c:1844), and
    /// `match_pattern` folds nothing -
    /// `if (*pattern != '?' && *pattern != *s) return 0;`
    /// (openssh/match.c:105-106).
    HostBlock,
    /// A `Match host` / `Match originalhost` argument: compared
    /// case-INSENSITIVELY. These route through `match_hostname`
    /// (openssh/match.c:193-203), which lowercases the host and passes
    /// `dolower=1` into `match_pattern_list`.
    MatchHost,
    /// A `Match user` / `Match localuser` argument.
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
/// case-folded. `Host` block patterns are never folded; `Match host`
/// patterns always are; usernames are folded only on Windows, where
/// account names are inherently case-insensitive.
fn case_fold(kind: MatchKind) -> bool {
    match kind {
        MatchKind::HostBlock => false,
        MatchKind::MatchHost => true,
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
