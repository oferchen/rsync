//! `Match` block condition model, connection context, and evaluation.
//!
//! Holds [`MatchCondition`] (the HONOR set selected in SSC-4.a), the
//! [`MatchContext`] connection-context type consulted during matching,
//! and the evaluators ([`evaluate_match`], [`match_line_applies`]) that
//! mirror OpenSSH's first-match-wins / AND-across-conditions rules.

use super::paths::local_user_env;
use super::pattern::{MatchKind, Pattern, parse_pattern_list, pattern_list_matches};

/// One condition from an ssh_config `Match` line.
///
/// The HONOR set selected in SSC-4.a: five variants covering `host`,
/// `originalhost`, `user`, `localuser`, and the argumentless `all`
/// sentinel. The SKIP / DEFER conditions (`canonical`, `final`,
/// `tagged`, `exec`) are not modelled here; the parser short-circuits
/// the whole block when it encounters one of them.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(in crate::ssh) enum MatchCondition {
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
pub(in crate::ssh) struct MatchContext<'a> {
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
    pub(in crate::ssh) fn new(
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
    pub(in crate::ssh) fn with_local_user_from_env(
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
pub(in crate::ssh) fn evaluate_match(
    conditions: &[MatchCondition],
    ctx: &MatchContext<'_>,
) -> bool {
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
pub(super) fn match_line_applies(value: &str, ctx: &MatchContext<'_>, saw_exec: &mut bool) -> bool {
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
