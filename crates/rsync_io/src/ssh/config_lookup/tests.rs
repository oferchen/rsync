//! Unit tests for the ssh_config compression lookup, moved verbatim
//! from the pre-decomposition single-file module. Items are reached
//! through the parent module's re-exports via `super::*`.

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
fn a_hash_inside_a_host_token_is_pattern_text() {
    // `argv_split` ends the line on a `#` only at a TOKEN BOUNDARY
    // (openssh/misc.c:2143-2144), so `Host x#y` is a three-character pattern.
    // This reader used to cut every line at the first `#`, which turned the
    // pattern into `x` and lost the block for the real alias.
    assert!(parse_enables_compression(
        "Host x#y\n  Compression yes\n",
        &ctx("x#y", "", "")
    ));

    // Non-vacuity: a `#` that STARTS a token DOES end the line, so the same
    // spelling with the `#` separated yields the single pattern `x` - which
    // the alias `x#y` does not match. Without this the first assertion would
    // also pass for a reader that never terminated on `#` at all.
    assert!(!parse_enables_compression(
        "Host x # y\n  Compression yes\n",
        &ctx("x#y", "", "")
    ));
    assert!(parse_enables_compression(
        "Host x # y\n  Compression yes\n",
        &ctx("x", "", "")
    ));
}

#[test]
fn a_value_keeps_a_hash_that_sits_inside_its_token() {
    // Same rule on the value half: `yes#no` is one token, not `yes`, so it
    // is not a recognised boolean and the bit must stay unset.
    assert!(!parse_enables_compression(
        "Host a\n  Compression yes#no\n",
        &ctx("a", "", "")
    ));

    // Non-vacuity companion: the same line with the `#` at a token boundary
    // IS `yes`, and the bit is set.
    assert!(parse_enables_compression(
        "Host a\n  Compression yes #no\n",
        &ctx("a", "", "")
    ));
}

#[test]
fn parse_host_pattern_list_keeps_a_comma_inside_one_token() {
    // A `Host` line is argv-tokenised: upstream splits it with
    // `argv_split` (openssh/misc.c:2130-2185), whose only separators are
    // `' '` and `'\t'` (openssh/misc.c:2141), then matches each token with
    // `match_pattern` (openssh/readconf.c:1844). The comma-splitting
    // `match_pattern_list` (openssh/match.c:143) is reachable only from
    // the `Match` criteria, so a comma here is pattern text.
    let parsed = host_patterns_from_tokens(&match_tokens("a,b\tc d"));
    assert_eq!(parsed.len(), 3);
    assert_eq!(parsed[0].glob(), "a,b");
    assert_eq!(parsed[1].glob(), "c");
    assert_eq!(parsed[2].glob(), "d");
}

#[test]
fn host_block_with_a_comma_matches_only_the_literal_alias() {
    // Measured on real `ssh -G -F fixture a`: `compression no`, i.e. the
    // block does not apply. The differential harness pins that half; this
    // pins the mirror image, which the oracle cannot answer because
    // `ssh -G a,b` refuses the alias with "hostname contains invalid
    // characters" and exits nonzero.
    let text = "Host a,b\n  Compression yes\n";
    assert!(!parse_enables_compression(text, &host_ctx("a")));
    assert!(!parse_enables_compression(text, &host_ctx("b")));
    assert!(parse_enables_compression(text, &host_ctx("a,b")));
}

#[test]
fn host_block_pattern_is_case_sensitive() {
    // `oHost` calls `match_pattern` directly
    // (openssh/readconf.c:1844) and `match_pattern` folds nothing -
    // `if (*pattern != '?' && *pattern != *s) return 0;`
    // (openssh/match.c:105-106). Measured on real `ssh -G`: alias `web1`
    // against `Host WEB1` reports `compression no`.
    let text = "Host WEB1\n  Compression yes\n";
    assert!(!parse_enables_compression(text, &host_ctx("web1")));
    assert!(parse_enables_compression(text, &host_ctx("WEB1")));
}

#[test]
fn match_host_pattern_stays_case_insensitive() {
    // The control for the `Host` case fix. `Match host` and
    // `Match originalhost` go through `match_hostname`
    // (openssh/match.c:193-203), which lowercases the host and passes
    // `dolower=1`, so the fold is CORRECT here. If this reddens, the
    // shared `case_fold` policy was flipped instead of the `Host` call
    // site being given its own kind.
    let text = "Match host WEB1\n  Compression yes\n";
    assert!(parse_enables_compression(text, &host_ctx("web1")));
    let original = "Match originalhost WEB1\n  Compression yes\n";
    assert!(parse_enables_compression(original, &host_ctx("web1")));
}

#[test]
fn match_host_still_comma_splits_its_pattern_list() {
    // The control for the `Host` tokeniser split. `Match host` DOES
    // comma-split - `match_pattern_list` cuts each subpattern at a comma
    // (openssh/match.c:143) - so the very token that must not match under
    // `Host` must still match here. If this reddens, the shared `Match`
    // tokeniser was narrowed instead of the `Host` caller being given its
    // own.
    let text = "Match host a,b\n  Compression yes\n";
    assert!(parse_enables_compression(text, &host_ctx("a")));
    assert!(parse_enables_compression(text, &host_ctx("b")));
}

// The `-F` extractor's own tests moved with it to
// `crate::ssh::config_files`, the one owner of the file load order.

/// Materialises `text` as a config file and returns its [`ConfigFile`]
/// row for a composed-scan fixture.
fn fixture_file(dir: &tempfile::TempDir, name: &str, text: &str, check_perm: bool) -> ConfigFile {
    let path = dir.path().join(name);
    std::fs::write(&path, text).expect("write fixture");
    // `user_conf` only steers `Include` anchoring, which the compression
    // reader does not act on, so its value is immaterial here.
    ConfigFile {
        path,
        check_perm,
        user_conf: check_perm,
    }
}

// -- file load order (task 237e) --------------------------------------
//
// upstream: openssh/ssh.c:561-592 `process_config_files()` reads the
// user file, then the system file, into ONE options struct, so
// first-obtained-wins arbitrates per keyword ACROSS the file boundary.
// The system path is injected through the `ConfigFile` seam rather than
// read from the host's real `/etc/ssh/ssh_config`.

/// Cell (a): a keyword set in BOTH files resolves to the USER file's
/// value - in both directions, so this pins the order, not a constant.
#[test]
fn user_file_claims_the_slot_before_the_system_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let user = fixture_file(&dir, "user", "Compression no\n", false);
    let system = fixture_file(&dir, "system", "Compression yes\n", false);
    assert!(!enables_compression_in(
        &[user.clone(), system.clone()],
        &host_ctx("t")
    ));
    // Swap the CONTENTS: the first file still wins, so a composition
    // that read system-before-user reddens exactly one of the two.
    let user_yes = fixture_file(&dir, "user2", "Compression yes\n", false);
    let system_no = fixture_file(&dir, "system2", "Compression no\n", false);
    assert!(enables_compression_in(
        &[user_yes, system_no],
        &host_ctx("t")
    ));
}

/// Cell (b): a keyword ONLY in the system file applies - the cell that
/// was red before this change, when the first EXISTING file ended the
/// lookup and an existing user file hid the system file entirely.
#[test]
fn a_directive_only_in_the_system_file_applies() {
    let dir = tempfile::tempdir().expect("tempdir");
    // The user file EXISTS but does not claim the slot.
    let user = fixture_file(&dir, "user", "Host other\n  Port 2222\n", false);
    let system = fixture_file(&dir, "system", "Compression yes\n", false);
    assert!(enables_compression_in(&[user, system], &host_ctx("t")));
}

/// A missing user file is skipped, not fatal: the system file is still
/// read (openssh/ssh.c:580-589 discards the default reads' results).
#[test]
fn a_missing_user_file_still_reaches_the_system_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let user = ConfigFile {
        path: dir.path().join("nonexistent"),
        check_perm: true,
        user_conf: true,
    };
    let system = fixture_file(&dir, "system", "Compression yes\n", false);
    assert!(enables_compression_in(&[user, system], &host_ctx("t")));
}

/// Block state does NOT cross the file boundary: a `Host` block left
/// open at the end of the user file must not swallow the system file's
/// top-level directives, because `read_config_file` starts every file
/// back at the always-active top level (openssh/readconf.c
/// `read_config_file_depth()` re-initialises `active` per file).
#[test]
fn a_host_block_does_not_extend_into_the_next_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let user = fixture_file(&dir, "user", "Host nevermatches\n  Port 2222\n", false);
    let system = fixture_file(&dir, "system", "Compression yes\n", false);
    assert!(enables_compression_in(&[user, system], &host_ctx("t")));
}

/// A refusal in the user file aborts the WHOLE load: the system file is
/// never read (openssh/readconf.c:2667 fatals before ssh.c's second
/// read_config_file call), so its `Compression yes` must not apply.
#[test]
fn a_refused_user_file_stops_the_scan_before_the_system_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let user = fixture_file(&dir, "user", "Host \"broken\n", false);
    let system = fixture_file(&dir, "system", "Compression yes\n", false);
    assert!(!enables_compression_in(&[user, system], &host_ctx("t")));
}

/// Cell (d), lookup half: the CHECKPERM gate rides on the
/// `check_perm` flag - the same world-writable file is refused as the
/// default user config but accepted as an explicit (`-F`-shaped) file
/// (openssh/ssh.c:571-583: only the default user path passes
/// SSHCONF_CHECKPERM).
#[cfg(unix)]
#[test]
fn checkperm_scope_follows_the_flag_not_the_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let mut file = fixture_file(&dir, "config", "Compression yes\n", true);
    std::fs::set_permissions(&file.path, std::fs::Permissions::from_mode(0o666)).expect("chmod");
    // As the default user file: refused, and the refusal kills the
    // whole load - the system file behind it is not consulted.
    let system = fixture_file(&dir, "system", "Compression yes\n", false);
    assert!(!enables_compression_in(
        &[file.clone(), system],
        &host_ctx("t")
    ));
    // The SAME file as an explicit -F file: accepted.
    file.check_perm = false;
    assert!(enables_compression_in(&[file], &host_ctx("t")));
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
    // First-obtained-wins: the first matching `Compression` (here
    // `yes`) claims the one slot and the later `Match all\n
    // Compression no` cannot override it.
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
fn top_level_no_first_obtained_blocks_matching_match_yes() {
    // First-obtained-wins across SCOPES, not per scope: upstream keeps
    // ONE slot per option, assigned only while unset
    // (openssh/readconf.c:1229 `if (*activep && *intptr == -1)`), so
    // the top-level `no` claims the slot and the later matching
    // `Match` block's `yes` is ignored. Oracle (OpenSSH_10.3p1):
    // `compression no`. The pre-fix per-scope OR answered `true` here.
    let text = "Compression no\nMatch host *.example.com\n  Compression yes\n";
    assert!(!parse_enables_compression(
        text,
        &match_ctx("web1.example.com", "web1.example.com", "", "")
    ));
}

#[test]
fn top_level_no_first_obtained_blocks_matching_host_yes() {
    // Task 1221's measured divergence, now inverted into the pin:
    // `Compression no` at top level, then a matching `Host` block's
    // `yes`. Oracle (OpenSSH_10.3p1): `compression no`; the pre-fix
    // three-slot OR answered `true`.
    let text = "Compression no\nHost t\n  Compression yes\n";
    assert!(!parse_enables_compression(text, &host_ctx("t")));
}

#[test]
fn host_no_first_obtained_blocks_matching_match_yes() {
    // Host-block scope obtains `no` first; a later matching `Match`
    // block cannot flip it. Oracle (OpenSSH_10.3p1): `compression no`.
    let text = "Host t\n  Compression no\nMatch host t\n  Compression yes\n";
    assert!(!parse_enables_compression(text, &host_ctx("t")));
}

#[test]
fn match_no_first_obtained_blocks_matching_host_yes() {
    // Match-block scope obtains `no` first; a later matching `Host`
    // block cannot flip it. Oracle (OpenSSH_10.3p1): `compression no`.
    let text = "Match host t\n  Compression no\nHost t\n  Compression yes\n";
    assert!(!parse_enables_compression(text, &host_ctx("t")));
}

#[test]
fn cross_scope_yes_first_obtained_still_fires() {
    // Non-vacuity control for the four cells above: a `yes` obtained
    // first survives a later matching `no`, so the engine is
    // first-obtained-wins and not "any `no` wins" or "last wins".
    // Oracle (OpenSSH_10.3p1): `compression yes`.
    let text = "Compression yes\nHost t\n  Compression no\n";
    assert!(parse_enables_compression(text, &host_ctx("t")));
}

#[test]
fn host_block_yes_then_match_block_yes_still_fires() {
    // Host block enables compression for `web*` and claims the slot;
    // the later matching `Match user deploy` block changes nothing.
    let text = "Host web*\n  Compression yes\nMatch user deploy\n  Compression yes\n";
    assert!(parse_enables_compression(
        text,
        &match_ctx("web1", "web1", "deploy", "")
    ));
}

#[test]
fn host_block_miss_with_match_block_hit_still_enables() {
    // Host block misses (`db*` vs `web1`), so it never claims the
    // slot; the matching `Match user` block's `yes` is the first
    // obtained value.
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
// Tokenises a `Match` header value the way the production caller does,
// so the tests exercise the same `argv_split` boundaries as the parser.
fn match_tokens(value: &str) -> Vec<String> {
    crate::ssh::argv_split::argv_split(value, true).expect("fixture tokenises")
}

// `saw_exec` output parameter on `match_line_applies` to verify
// the one-shot warning fires exactly when expected.

#[test]
fn match_exec_sets_saw_exec_flag() {
    // When the parser encounters `Match exec`, the saw_exec flag
    // must be set to true so the caller can emit a warning.
    let mut saw_exec = false;
    let result = match_line_applies(
        &match_tokens("exec /bin/true"),
        &ctx("web1", "", ""),
        &mut saw_exec,
    );
    assert!(!result, "Match exec must not match");
    assert!(saw_exec, "saw_exec flag must be set");
}

#[test]
fn match_without_exec_does_not_set_flag() {
    // Normal Match conditions must not set the saw_exec flag.
    let mut saw_exec = false;
    match_line_applies(
        &match_tokens("host web1"),
        &ctx("web1", "", ""),
        &mut saw_exec,
    );
    assert!(!saw_exec, "saw_exec should not be set for non-exec Match");
}

#[test]
fn match_all_does_not_set_flag() {
    let mut saw_exec = false;
    match_line_applies(&match_tokens("all"), &ctx("", "", ""), &mut saw_exec);
    assert!(!saw_exec, "saw_exec should not be set for Match all");
}

#[test]
fn match_exec_flag_set_once_across_multiple_exec_blocks() {
    // Multiple Match exec blocks should set the flag but only the
    // first encounter matters for the one-shot warning. Verify the
    // flag stays true after a second exec block.
    let mut saw_exec = false;
    match_line_applies(
        &match_tokens("exec /bin/true"),
        &ctx("web1", "", ""),
        &mut saw_exec,
    );
    assert!(saw_exec);
    match_line_applies(
        &match_tokens("exec /bin/false"),
        &ctx("web1", "", ""),
        &mut saw_exec,
    );
    assert!(saw_exec, "flag should remain true after second exec");
}

#[test]
fn match_exec_flag_not_reset_by_non_exec_match() {
    // Once saw_exec is set by a Match exec block, a subsequent
    // non-exec Match must not clear it.
    let mut saw_exec = false;
    match_line_applies(
        &match_tokens("exec /usr/bin/test"),
        &ctx("web1", "", ""),
        &mut saw_exec,
    );
    assert!(saw_exec);
    match_line_applies(
        &match_tokens("host web1"),
        &ctx("web1", "", ""),
        &mut saw_exec,
    );
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
    // `Compression yes`. The top-level `no` claims the slot first, so
    // the overall result is `false` - and because the slot was already
    // claimed, an evaluated exec block could not have changed the
    // answer either, so no warning is owed.
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
