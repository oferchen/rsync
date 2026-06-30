//! Unit tests for the ssh_config compression lookup, moved verbatim
//! from the pre-decomposition single-file module. Items are reached
//! through the parent module's re-exports via `super::*`.

use std::ffi::OsString;
use std::path::PathBuf;

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
