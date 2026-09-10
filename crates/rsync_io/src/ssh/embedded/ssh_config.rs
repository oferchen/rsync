//! Minimal `~/.ssh/config` parser for the embedded russh transport.
//!
//! Recognises the subset of OpenSSH client directives that the embedded
//! transport can act on: `Host`, `Hostname`, `User`, `Port`, `IdentityFile`,
//! `IdentitiesOnly`, `IdentityAgent`. Unknown directives are skipped
//! silently. Wildcards in `Host` patterns follow OpenSSH semantics: `*`
//! matches any sequence, `?` matches any single character, `!pattern`
//! negates a match for that block.
//!
//! # Tokenisation
//!
//! Every line's value half is split by [`argv_split`], the shared port of
//! upstream's `argv_split` - the same call readconf makes once per line at
//! openssh/readconf.c:1196. That is where quoting, backslash escapes and
//! `#`-comment termination are decided; this module only consumes the
//! resulting tokens. In particular a `#` is a comment **only at a token
//! boundary**, so `HostName x#y` resolves to `x#y`.
//!
//! # Failure mode
//!
//! A missing or unreadable file is still an empty result, but a file that
//! real `ssh` would REFUSE is now an error rather than silently-empty.
//! Upstream counts bad lines and then aborts the load outright
//! (openssh/readconf.c:2667), so the connection never happens; resolving
//! to different connection parameters than `ssh` would have used is the
//! worse outcome. Three refusals are mirrored, each with upstream's own
//! wording:
//!
//! * `invalid quotes` - a quote left open (openssh/readconf.c:1196-1199).
//! * `keyword host empty argument` - an empty `Host` token
//!   (openssh/readconf.c:1832-1836).
//! * `Missing argument.` / `missing argument.` - a directive whose value
//!   tokenised to nothing, e.g. `Port #comment`. The capitalisation is
//!   upstream's own and splits by arm: string options say `Missing`
//!   (openssh/readconf.c:1364), flag options say `missing`
//!   (openssh/readconf.c:1240). Measured against real `ssh -G`, both.
//!
//! The caller (`SshConfig::apply_ssh_config`) merges the resolved
//! directives into the existing config, with the rule that any value
//! already set on the URL wins over the config file.

use std::fs;
use std::path::{Path, PathBuf};

use super::error::SshError;
use crate::ssh::argv_split::argv_split;

/// Placeholder file name used when a caller supplies config text with no
/// path of its own. Upstream always has a real filename to name in a
/// refusal (openssh/readconf.c:1197 `"%s line %d: ..."`), so the
/// text-only entry point substitutes this rather than printing nothing.
#[cfg(test)]
const INLINE_CONFIG_NAME: &str = "<ssh_config>";

/// Resolved directives for a single host alias, merged in declaration order
/// across every matching `Host` block. Only the directives the embedded
/// russh transport understands are tracked.
#[derive(Debug, Default, Clone)]
pub(super) struct ResolvedHost {
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_files: Vec<PathBuf>,
    pub identities_only: Option<bool>,
    pub identity_agent: Option<String>,
}

/// Parses `path` and returns the directives that apply to `host_alias`.
///
/// Returns `Ok(ResolvedHost::default())` when the file cannot be read -
/// upstream treats an absent config the same way
/// (openssh/readconf.c:2632-2633 `if ((f = fopen(...)) == NULL) return 0`).
/// OpenSSH's first-match-wins precedence is honoured: once a directive is
/// set inside the first matching block, a later block cannot overwrite it.
/// Wildcards in `Host` patterns are expanded with [`pattern_matches`].
///
/// # Errors
///
/// [`SshError::SshConfig`] when a line is one upstream would refuse; see
/// the module docs for the three cases.
pub(super) fn resolve_host(path: &Path, host_alias: &str) -> Result<ResolvedHost, SshError> {
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(ResolvedHost::default());
    };
    resolve_host_in(&text, host_alias, &path.display().to_string())
}

/// Variant of [`resolve_host`] that takes the config text directly.
/// Exposed for unit tests and for the `ssh -G` differential harness so
/// neither has to touch the filesystem. Refusals name
/// [`INLINE_CONFIG_NAME`] instead of a real path.
///
/// # Errors
///
/// Same as [`resolve_host`].
#[cfg(test)]
pub(super) fn resolve_host_str(text: &str, host_alias: &str) -> Result<ResolvedHost, SshError> {
    resolve_host_in(text, host_alias, INLINE_CONFIG_NAME)
}

/// Builds the refusal upstream would print for `path` line `line`.
fn refuse(path: &str, line: usize, reason: impl Into<String>) -> SshError {
    SshError::SshConfig {
        path: path.to_owned(),
        line,
        reason: reason.into(),
    }
}

/// The missing-argument wording for `keyword`.
///
/// Upstream splits by arm and the capitalisation differs: the string
/// options route through `parse_string` and print `Missing argument.`
/// (openssh/readconf.c:1364) while the yes/no flags route through
/// `parse_flag` and print `missing argument.` (openssh/readconf.c:1240).
/// Measured against real `ssh -G` for every keyword below rather than
/// inferred from the arm names.
fn missing_argument(keyword: &str) -> &'static str {
    match keyword {
        "identitiesonly" => "missing argument.",
        _ => "Missing argument.",
    }
}

fn resolve_host_in(text: &str, host_alias: &str, path: &str) -> Result<ResolvedHost, SshError> {
    let mut resolved = ResolvedHost::default();
    let mut in_matching_block = false;

    for (index, raw_line) in text.lines().enumerate() {
        // Upstream counts physical lines from 1 (openssh/readconf.c:2651-2654).
        let linenum = index + 1;
        // Trailing whitespace strip + leading whitespace skip
        // (openssh/readconf.c:1168-1172, :1178-1180). `\r` is in
        // upstream's WHITESPACE set (openssh/misc.c:464), so a CRLF file
        // needs no special case.
        let line = raw_line.trim();
        // A line whose keyword begins with `#` is a comment
        // (openssh/readconf.c:1181). This is the ONLY place a leading `#`
        // may be tested: mid-token a `#` is literal, and `argv_split`
        // owns that decision.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = split_directive(line) else {
            continue;
        };
        let key_lc = key.to_ascii_lowercase();

        // One tokenisation per line, exactly as upstream does it
        // (openssh/readconf.c:1196).
        let tokens = argv_split(value, true).map_err(|err| refuse(path, linenum, err.reason()))?;

        if key_lc == "host" {
            in_matching_block =
                host_matches_any_pattern(host_alias, &tokens).map_err(|EmptyHostToken| {
                    refuse(path, linenum, format!("keyword {key_lc} empty argument"))
                })?;
            continue;
        }
        if !in_matching_block {
            continue;
        }

        // Every directive below is single-valued, so it consumes exactly
        // one token - upstream's `arg = argv_next(&ac, &av)`. A `NULL`
        // there is fatal, which is how `Port #comment` is refused.
        let arg = match key_lc.as_str() {
            "hostname" | "user" | "port" | "identityfile" | "identitiesonly" | "identityagent" => {
                match tokens.first() {
                    Some(arg) => arg.as_str(),
                    None => return Err(refuse(path, linenum, missing_argument(&key_lc))),
                }
            }
            // Not a directive this parser acts on. Upstream would still
            // classify it, but oc's unknown-keyword policy is task 237j's
            // row, not this one.
            _ => continue,
        };

        match key_lc.as_str() {
            "hostname" => set_if_unset(&mut resolved.hostname, arg.to_owned()),
            "user" => set_if_unset(&mut resolved.user, arg.to_owned()),
            "port" => {
                if resolved.port.is_none()
                    && let Ok(parsed) = arg.parse::<u16>()
                {
                    resolved.port = Some(parsed);
                }
            }
            "identityfile" => {
                let expanded = expand_tilde(arg);
                if !resolved.identity_files.contains(&expanded) {
                    resolved.identity_files.push(expanded);
                }
            }
            "identitiesonly" => {
                if resolved.identities_only.is_none() {
                    resolved.identities_only = parse_yes_no(arg);
                }
            }
            "identityagent" => {
                set_if_unset(&mut resolved.identity_agent, expand_tilde_str(arg));
            }
            _ => unreachable!("the match above already narrowed the keyword set"),
        }
    }

    Ok(resolved)
}

/// Splits a `Key Value` directive on the first run of whitespace or
/// optional `=` separator. Returns `None` for lines that contain only the
/// key with no value.
///
/// The keyword half is upstream's `strdelim` (openssh/readconf.c:1176,
/// openssh/misc.c:469-507), a *different* splitter from `argv_split`; it
/// is the value half that this module then tokenises.
fn split_directive(line: &str) -> Option<(&str, &str)> {
    let (key, rest) = line.split_once(|c: char| c.is_whitespace() || c == '=')?;
    let value = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=');
    if value.is_empty() {
        return None;
    }
    Some((key, value))
}

/// A `Host` line carried a token that tokenised to the empty string.
///
/// A unit-like error rather than `()` so the call site reads as a named
/// condition and clippy's `result_unit_err` has nothing to complain about.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct EmptyHostToken;

/// Returns `true` when `host` matches any token in `tokens`, after
/// expanding wildcards and applying negations. A leading `!` on any token
/// negates and causes the whole line to fail to match.
///
/// Mirrors the `oHost` arm's own loop (openssh/readconf.c:1829-1858) step
/// for step, and the ORDER matters:
///
/// 1. `*activep = 0` before the loop (:1829) - a `Host` line with no
///    tokens at all leaves the block inactive.
/// 2. the empty-token guard (:1832-1836) runs FIRST, before negation is
///    stripped, so `Host a "" b` is refused even though `a` would match.
/// 3. a NEGATED match consumes the rest of the line and breaks
///    (:1845-1852), so a later empty token after it is never reached.
/// 4. a POSITIVE match does NOT break (:1854-1856): scanning continues so
///    a later negation can still kill the block.
///
/// # Errors
///
/// [`EmptyHostToken`] when a token is empty; the caller renders
/// upstream's `keyword host empty argument`.
fn host_matches_any_pattern(host: &str, tokens: &[String]) -> Result<bool, EmptyHostToken> {
    let mut any_positive_match = false;
    for token in tokens {
        if token.is_empty() {
            return Err(EmptyHostToken);
        }
        let (negate, pat) = token
            .strip_prefix('!')
            .map_or((false, token.as_str()), |stripped| (true, stripped));
        if pattern_matches(host, pat) {
            if negate {
                return Ok(false);
            }
            any_positive_match = true;
        }
    }
    Ok(any_positive_match)
}

/// Glob-matches `host` against `pattern`, where `*` matches any sequence
/// and `?` matches any single character. The implementation is a small
/// recursive descent that mirrors `fnmatch(3)` without character classes.
fn pattern_matches(host: &str, pattern: &str) -> bool {
    let host_bytes = host.as_bytes();
    let pat_bytes = pattern.as_bytes();
    fn matches(h: &[u8], p: &[u8]) -> bool {
        if p.is_empty() {
            return h.is_empty();
        }
        match p[0] {
            b'*' => {
                if p.len() == 1 {
                    return true;
                }
                for i in 0..=h.len() {
                    if matches(&h[i..], &p[1..]) {
                        return true;
                    }
                }
                false
            }
            b'?' => !h.is_empty() && matches(&h[1..], &p[1..]),
            c => !h.is_empty() && h[0] == c && matches(&h[1..], &p[1..]),
        }
    }
    matches(host_bytes, pat_bytes)
}

/// Expands a leading `~/` to the user's home directory. Returns the path
/// unchanged when expansion is not possible.
fn expand_tilde(path: &str) -> PathBuf {
    PathBuf::from(expand_tilde_str(path))
}

fn expand_tilde_str(path: &str) -> String {
    let home = home_dir();
    let home = home.as_ref().map(|h| h.to_string_lossy());
    expand_tilde_with(path, home.as_deref(), std::path::MAIN_SEPARATOR)
}

/// Expands a leading `~/` in `path` against `home`, emitting `sep` as the only
/// separator so the result never mixes `\` and `/`.
///
/// `home` and `sep` are parameters rather than reads of the environment and of
/// `MAIN_SEPARATOR` so that the Windows arm is exercisable from a unit test on
/// any host. Returns `path` unchanged when there is nothing to expand: no `~`
/// prefix, no home directory, or a `~user` form this parser does not resolve.
///
/// Mirrors OpenSSH's `tilde_expand` for the separator run that follows the
/// tilde - upstream: openssh/misc.c:1281 collapses it with
/// `copy += strspn(copy, "/")` before concatenating, which is why a value of
/// `~//probe` must still land under the home directory.
///
/// Two behaviours are Windows-specific, where OpenSSH assumes a single
/// separator character:
///
/// * `~\` is accepted as a tilde prefix when `sep` is `\`. This mirrors the
///   `WINDOWS` arm at openssh/misc.c:1284, which was added to `tilde_expand`
///   for exactly this reason.
/// * The remainder's separators are rewritten to `sep`. OpenSSH concatenates
///   with a literal `/` and leaves the remainder alone (openssh/misc.c:1315),
///   which on Windows yields a mixed path such as `C:\Users\x/.ssh/id_rsa`;
///   it gets away with it because the Win32 file APIs accept either character.
///   oc puts these paths in front of the user in diagnostics, so emitting one
///   separator kind is a deliberate oc divergence rather than a mirror.
fn expand_tilde_with(path: &str, home: Option<&str>, sep: char) -> String {
    let is_sep = |c: char| c == '/' || (sep == '\\' && c == '\\');
    let Some(rest) = path
        .strip_prefix('~')
        .and_then(|after| after.strip_prefix(is_sep))
    else {
        return path.to_owned();
    };
    let Some(home) = home else {
        return path.to_owned();
    };
    let rest = rest.trim_start_matches(is_sep);
    let home = home.trim_end_matches(is_sep);
    let mut out = String::with_capacity(home.len() + 1 + rest.len());
    out.push_str(home);
    out.push(sep);
    out.extend(rest.chars().map(|c| if is_sep(c) { sep } else { c }));
    out
}

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

fn set_if_unset<T>(slot: &mut Option<T>, value: T) {
    if slot.is_none() {
        *slot = Some(value);
    }
}

fn parse_yes_no(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" => Some(true),
        "no" | "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolve, asserting the config was accepted. Most cells below are
    /// about which value wins, not about refusal.
    fn resolve(text: &str, alias: &str) -> ResolvedHost {
        resolve_host_str(text, alias).expect("config accepted")
    }

    /// The `path line N: reason` text of a refusal, for the cells that
    /// assert on upstream's wording.
    fn refusal(text: &str, alias: &str) -> String {
        match resolve_host_str(text, alias) {
            Ok(resolved) => panic!("expected a refusal, resolved {resolved:?}"),
            Err(err) => err.to_string(),
        }
    }

    #[test]
    fn missing_file_returns_default() {
        let resolved =
            resolve_host(Path::new("/nonexistent/ssh/config"), "anything").expect("absent is Ok");
        assert!(resolved.hostname.is_none());
        assert!(resolved.user.is_none());
        assert!(resolved.port.is_none());
        assert!(resolved.identity_files.is_empty());
    }

    #[test]
    fn simple_host_block_applies() {
        let text = "Host example\n  HostName 1.2.3.4\n  User deploy\n  Port 2222\n";
        let resolved = resolve(text, "example");
        assert_eq!(resolved.hostname.as_deref(), Some("1.2.3.4"));
        assert_eq!(resolved.user.as_deref(), Some("deploy"));
        assert_eq!(resolved.port, Some(2222));
    }

    #[test]
    fn non_matching_block_is_ignored() {
        let text = "Host other\n  HostName ignored\n";
        assert!(resolve(text, "example").hostname.is_none());
    }

    #[test]
    fn wildcard_star_matches() {
        let text = "Host *.example.com\n  User wild\n";
        assert_eq!(
            resolve(text, "alpha.example.com").user.as_deref(),
            Some("wild")
        );
    }

    #[test]
    fn first_match_wins() {
        let text = "Host example\n  User first\nHost *\n  User second\n";
        assert_eq!(resolve(text, "example").user.as_deref(), Some("first"));
    }

    #[test]
    fn comments_and_blank_lines_skipped() {
        let text = "# top comment\n\nHost example  # inline\n  User u # trail\n";
        assert_eq!(resolve(text, "example").user.as_deref(), Some("u"));
    }

    #[test]
    fn equals_separator_supported() {
        let text = "Host example\n  Port=4242\n";
        assert_eq!(resolve(text, "example").port, Some(4242));
    }

    #[test]
    fn negation_disables_block() {
        let text = "Host *.example.com !banned.example.com\n  User u\n";
        assert!(resolve(text, "banned.example.com").user.is_none());
        assert_eq!(resolve(text, "ok.example.com").user.as_deref(), Some("u"));
    }

    /// A comma inside a `Host` token is pattern text, not a separator.
    ///
    /// Upstream tokenises the line with `argv_split`, whose separators are
    /// `' '` and `'\t'` only (openssh/misc.c:2141), and matches each token
    /// with `match_pattern` (openssh/readconf.c:1844). Measured on real
    /// `ssh -G -F fixture a`: `port 22`, i.e. no match. The differential
    /// harness pins that half against the oracle; this pins the mirror
    /// image, which the oracle cannot answer because `ssh -G a,b` refuses
    /// the alias as an invalid hostname.
    #[test]
    fn comma_in_a_host_pattern_is_literal_not_a_separator() {
        let text = "Host a,b\n  Port 2222\n";
        assert!(resolve(text, "a").port.is_none());
        assert!(resolve(text, "b").port.is_none());
        assert_eq!(resolve(text, "a,b").port, Some(2222));
    }

    /// A TAB separates `Host` tokens exactly as a space does
    /// (openssh/misc.c:2141), so narrowing the separator set to space and
    /// TAB must not have dropped the TAB.
    #[test]
    fn tab_separates_host_patterns() {
        let text = "Host first\tsecond\n  Port 2222\n";
        assert_eq!(resolve(text, "first").port, Some(2222));
        assert_eq!(resolve(text, "second").port, Some(2222));
    }

    #[test]
    fn identities_only_yes_is_recognised() {
        let text = "Host example\n  IdentitiesOnly yes\n";
        assert_eq!(resolve(text, "example").identities_only, Some(true));
    }

    #[test]
    fn identity_files_collected_in_order() {
        let text = "Host example\n  IdentityFile /a\n  IdentityFile /b\n";
        assert_eq!(
            resolve(text, "example").identity_files,
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }

    #[test]
    fn invalid_port_is_ignored() {
        let text = "Host example\n  Port notanumber\n";
        assert!(resolve(text, "example").port.is_none());
    }

    // -- argv_split boundary behaviour, as seen through the directives --
    //
    // The tokeniser itself is unit-tested in `crate::ssh::argv_split`;
    // these cells prove the parser CONSUMES those tokens rather than
    // re-deriving its own. Each was measured against real `ssh -G`
    // (OpenSSH_10.3p1) before being written down.

    /// A `#` inside a token is pattern text, so the alias that matches is
    /// `a#b` and not `a`. Before the tokeniser landed, a whole-line
    /// comment strip cut the value to `a` and inverted both rows.
    /// Oracle: `Host a#b` + alias `a#b` gives `port 2222`; alias `a` gives
    /// `port 22`.
    #[test]
    fn a_hash_inside_a_host_token_is_pattern_text() {
        let text = "Host a#b\n  Port 2222\n";
        assert_eq!(resolve(text, "a#b").port, Some(2222));
        assert!(resolve(text, "a").port.is_none());
    }

    /// The non-vacuity companion: at a token boundary the `#` really does
    /// end the line, so the trailing token is not a pattern.
    /// Oracle: `Host a #b` + alias `a` gives `port 2222`.
    #[test]
    fn a_hash_at_a_token_boundary_still_ends_the_host_line() {
        let text = "Host a #b\n  Port 2222\n";
        assert_eq!(resolve(text, "a").port, Some(2222));
        assert!(resolve(text, "b").port.is_none());
    }

    /// Quotes are consumed, not matched: `Host "a"` matches the alias `a`.
    /// Oracle: `port 2222`. Without the tokeniser oc compared the alias
    /// against the literal `"a"` and declined.
    #[test]
    fn quotes_around_a_host_token_are_stripped() {
        assert_eq!(resolve("Host \"a\"\n  Port 2222\n", "a").port, Some(2222));
        assert_eq!(resolve("Host 'a'\n  Port 2222\n", "a").port, Some(2222));
    }

    /// A quoted wildcard is still a wildcard once the quotes are gone.
    /// Oracle: `Host "*"` + alias `x` gives `port 2222`.
    #[test]
    fn a_quoted_wildcard_still_globs() {
        assert_eq!(resolve("Host \"*\"\n  Port 2222\n", "x").port, Some(2222));
    }

    /// A quote may open and close mid-token, splicing the halves.
    /// Oracle: `Host "a"b` + alias `ab` gives `port 2222`.
    #[test]
    fn a_mid_token_quote_splices_the_host_pattern() {
        assert_eq!(resolve("Host \"a\"b\n  Port 2222\n", "ab").port, Some(2222));
    }

    /// Quotes group a space into ONE pattern, so `Host "web 1" other`
    /// carries two tokens and `web` matches neither.
    /// Oracle: alias `web` gives `port 22`; alias `other` gives 2222.
    /// (`ssh -G 'web 1'` refuses the alias itself, so the grouped token is
    /// asserted here rather than differentially - the same oracle boundary
    /// task 1204 recorded for `a,b`.)
    #[test]
    fn quotes_group_a_space_into_one_host_pattern() {
        let text = "Host \"web 1\" other\n  Port 2222\n";
        assert!(resolve(text, "web").port.is_none());
        assert!(resolve(text, "1").port.is_none());
        assert_eq!(resolve(text, "other").port, Some(2222));
        assert_eq!(resolve(text, "web 1").port, Some(2222));
    }

    /// Non-vacuity companion for the row above: strip the quotes and the same
    /// line is THREE patterns, so `web` matches on its own. Without this the
    /// quoted assertions would also hold for a tokeniser that dropped the
    /// whole line.
    /// Oracle: `Host web 1 other` + alias `web` gives `port 2222`, where the
    /// quoted spelling gives `port 22`.
    #[test]
    fn a_bare_space_still_separates_two_host_patterns() {
        let text = "Host web 1 other\n  Port 2222\n";
        assert_eq!(resolve(text, "web").port, Some(2222));
        assert_eq!(resolve(text, "1").port, Some(2222));
        assert_eq!(resolve(text, "other").port, Some(2222));
    }

    /// An escaped quote is ordinary text, so the token keeps the `"`.
    /// Oracle: `Host \"a` + alias `a` gives `port 22`.
    #[test]
    fn an_escaped_quote_stays_in_the_host_pattern() {
        let text = "Host \\\"a\n  Port 2222\n";
        assert!(resolve(text, "a").port.is_none());
        assert_eq!(resolve(text, "\"a").port, Some(2222));
    }

    /// A directive VALUE keeps its `#` too - this is the row that reaches
    /// every keyword, not just `Host`.
    /// Oracle: `HostName x#y` dumps `hostname x#y`; `IdentityFile /k#1`
    /// dumps `identityfile /k#1`.
    #[test]
    fn a_hash_inside_a_directive_value_is_literal() {
        let text = "Host a\n  HostName x#y\n  IdentityFile /k#1\n";
        let resolved = resolve(text, "a");
        assert_eq!(resolved.hostname.as_deref(), Some("x#y"));
        assert_eq!(identity_file_strings(&resolved), vec!["/k#1".to_owned()]);
    }

    /// A quoted directive value keeps its embedded space and loses the
    /// quotes. Oracle: `HostName "x y"` dumps `hostname x y`.
    #[test]
    fn a_quoted_directive_value_keeps_its_space_and_loses_the_quotes() {
        let text = "Host a\n  HostName \"x y\"\n";
        assert_eq!(resolve(text, "a").hostname.as_deref(), Some("x y"));
    }

    /// An unrecognised escape keeps BOTH characters
    /// (openssh/misc.c:2161-2164). Oracle: `HostName x\ny` dumps
    /// `hostname x\ny`, and `HostName "x\ y"` dumps `hostname x\ y`
    /// because the space escape is quote-gated.
    #[test]
    fn an_unrecognised_escape_survives_into_the_value() {
        assert_eq!(
            resolve("Host a\n  HostName x\\ny\n", "a")
                .hostname
                .as_deref(),
            Some("x\\ny")
        );
        assert_eq!(
            resolve("Host a\n  HostName \"x\\ y\"\n", "a")
                .hostname
                .as_deref(),
            Some("x\\ y")
        );
    }

    // -- the refusals ---------------------------------------------------

    /// The empty-token hard error, upstream's own wording.
    /// Oracle: `ssh -G` exits 255 with
    /// `<file> line 1: keyword host empty argument`
    /// (openssh/readconf.c:1832-1836), measured for the middle, first and
    /// last positions and for both quote characters.
    #[test]
    fn an_empty_host_token_is_refused_in_every_position() {
        for text in [
            "Host a \"\" b\n  Port 2222\n",
            "Host \"\" a\n  Port 2222\n",
            "Host a \"\"\n  Port 2222\n",
            "Host a '' b\n  Port 2222\n",
        ] {
            assert_eq!(
                refusal(text, "a"),
                "<ssh_config> line 1: keyword host empty argument",
                "fixture {text:?}"
            );
        }
    }

    /// The non-vacuity companion: the same shapes WITHOUT an empty token
    /// resolve. Without this, a parser that refused every quoted `Host`
    /// line would satisfy the cell above.
    #[test]
    fn a_host_line_with_no_empty_token_is_accepted() {
        assert_eq!(
            resolve("Host a \"b\" c\n  Port 2222\n", "a").port,
            Some(2222)
        );
        assert_eq!(
            resolve("Host a \"b\" c\n  Port 2222\n", "b").port,
            Some(2222)
        );
    }

    /// A NEGATED match consumes the rest of the line, so an empty token
    /// after it is never reached (openssh/readconf.c:1845-1852). This is
    /// the ordering cell: a guard hoisted ahead of the loop would refuse
    /// here and diverge.
    #[test]
    fn an_empty_token_after_a_negated_match_is_not_reached() {
        let text = "Host !a \"\"\n  Port 2222\n";
        assert!(resolve(text, "a").port.is_none());
        // For an alias the negation does NOT match, the loop runs on and
        // the empty token IS refused.
        assert_eq!(
            refusal(text, "z"),
            "<ssh_config> line 1: keyword host empty argument"
        );
    }

    /// An unterminated quote refuses the line, with upstream's wording.
    /// Oracle: `ssh -G` exits 255 with `<file> line 1: invalid quotes`
    /// (openssh/readconf.c:1196-1199).
    #[test]
    fn an_unterminated_quote_is_refused() {
        assert_eq!(
            refusal("Host \"a b\n  Port 2222\n", "a"),
            "<ssh_config> line 1: invalid quotes"
        );
        // A quote on a VALUE line refuses too, and carries that line's
        // number - proving the refusal is per-line, not Host-specific.
        assert_eq!(
            refusal("Host a\n  HostName \"x\n", "a"),
            "<ssh_config> line 2: invalid quotes"
        );
    }

    /// A directive whose value tokenises to nothing is the
    /// `argv_next` returned NULL arm. Oracle-measured per keyword: the
    /// string options capitalise it (openssh/readconf.c:1364) and the
    /// yes/no flags do not (openssh/readconf.c:1240).
    #[test]
    fn a_value_that_is_only_a_comment_is_refused() {
        for (keyword, reason) in [
            ("HostName", "Missing argument."),
            ("User", "Missing argument."),
            ("Port", "Missing argument."),
            ("IdentityFile", "Missing argument."),
            ("IdentityAgent", "Missing argument."),
            ("IdentitiesOnly", "missing argument."),
        ] {
            let text = format!("Host a\n  {keyword} #x\n");
            assert_eq!(
                refusal(&text, "a"),
                format!("<ssh_config> line 2: {reason}"),
                "keyword {keyword}"
            );
        }
    }

    /// The non-vacuity companion for the cell above: the same directives
    /// with a real value are accepted, and a comment AFTER the value is
    /// not a missing argument.
    #[test]
    fn a_value_followed_by_a_comment_is_not_missing() {
        let text = "Host a\n  HostName real #note\n  Port 2222 #note\n";
        let resolved = resolve(text, "a");
        assert_eq!(resolved.hostname.as_deref(), Some("real"));
        assert_eq!(resolved.port, Some(2222));
    }

    /// A `Host` line whose value is only a comment leaves the block
    /// INACTIVE rather than refusing - upstream clears `*activep` before
    /// the loop and the loop then runs zero times
    /// (openssh/readconf.c:1829-1831). Oracle: `port 22`, exit 0.
    #[test]
    fn a_host_line_that_is_only_a_comment_deactivates_without_refusing() {
        let text = "Host a\n  Port 2222\nHost #x\n  Port 3333\n";
        assert_eq!(resolve(text, "a").port, Some(2222));
        assert!(resolve("Host #x\n  Port 3333\n", "a").port.is_none());
    }

    /// A refusal from a real file names that file, not the inline
    /// placeholder - the path really is threaded through.
    #[test]
    fn a_refusal_names_the_file_it_came_from() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ssh_config");
        std::fs::write(&path, "Host a \"\" b\n").expect("write fixture");
        let err = resolve_host(&path, "a").expect_err("refused");
        assert_eq!(
            err.to_string(),
            format!("{} line 1: keyword host empty argument", path.display())
        );
    }

    /// Renders the resolved identity files as strings. `PathBuf` equality
    /// compares components, so it collapses separator runs and treats `/` as a
    /// separator on Windows - both of which would hide exactly the defects the
    /// pins below exist to catch.
    fn identity_file_strings(resolved: &ResolvedHost) -> Vec<String> {
        resolved
            .identity_files
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    /// Before the fix the expansion went through `PathBuf::join`, whose
    /// absolute-path rule replaced the accumulated path outright and dropped
    /// the home directory, yielding `/probe`.
    /// upstream: openssh/misc.c:1281 collapses the run instead.
    #[test]
    fn tilde_slash_run_keeps_home_prefix() {
        let home = home_dir().expect("HOME or USERPROFILE must be set");
        let text = "Host example\n  IdentityFile ~//probe\n";
        let resolved = resolve(text, "example");
        let expected = format!(
            "{}{}probe",
            home.to_string_lossy(),
            std::path::MAIN_SEPARATOR
        );
        assert_eq!(identity_file_strings(&resolved), vec![expected]);
    }

    #[test]
    fn identity_file_tilde_expands_through_the_directive() {
        let home = home_dir().expect("HOME or USERPROFILE must be set");
        let text = "Host example\n  IdentityFile ~/.ssh/id_probe\n";
        let resolved = resolve(text, "example");
        let sep = std::path::MAIN_SEPARATOR;
        let expected = format!("{}{sep}.ssh{sep}id_probe", home.to_string_lossy());
        assert_eq!(identity_file_strings(&resolved), vec![expected]);
    }

    #[test]
    fn identity_agent_tilde_expands_through_the_directive() {
        let home = home_dir().expect("HOME or USERPROFILE must be set");
        let text = "Host example\n  IdentityAgent ~/agent.sock\n";
        let resolved = resolve(text, "example");
        let expected = format!(
            "{}{}agent.sock",
            home.to_string_lossy(),
            std::path::MAIN_SEPARATOR
        );
        assert_eq!(resolved.identity_agent.as_deref(), Some(expected.as_str()));
    }

    /// The reported defect: a `\`-separated home joined to a `/`-separated
    /// remainder. Pinned by value with `sep` supplied explicitly, because the
    /// Windows join character is not observable through `PathBuf` on a Unix
    /// host.
    #[test]
    fn windows_expansion_emits_no_forward_slash() {
        let out = expand_tilde_with("~/.ssh/id_probe", Some(r"C:\Users\x"), '\\');
        assert_eq!(out, r"C:\Users\x\.ssh\id_probe");
        assert!(!out.contains('/'), "mixed separators in {out}");
    }

    /// upstream: openssh/misc.c:1284 - the Win32 fork's `WINDOWS` arm accepts a
    /// backslash directly after the tilde.
    #[test]
    fn windows_accepts_backslash_after_tilde() {
        let out = expand_tilde_with(r"~\.ssh\id_probe", Some(r"C:\Users\x"), '\\');
        assert_eq!(out, r"C:\Users\x\.ssh\id_probe");
    }

    /// On Unix a backslash is an ordinary filename character and must survive
    /// expansion untouched, so the rewrite above is strictly a Windows arm.
    #[test]
    fn unix_expansion_preserves_literal_backslash() {
        let out = expand_tilde_with(r"~/od\d", Some("/home/u"), '/');
        assert_eq!(out, r"/home/u/od\d");
        assert_eq!(expand_tilde_with(r"~\od", Some("/home/u"), '/'), r"~\od");
    }

    /// upstream: openssh/misc.c:1281 `copy += strspn(copy, "/")`.
    #[test]
    fn separator_run_after_tilde_is_collapsed() {
        assert_eq!(
            expand_tilde_with("~//probe", Some("/home/u"), '/'),
            "/home/u/probe"
        );
        assert_eq!(
            expand_tilde_with(r"~\\probe", Some(r"C:\Users\x"), '\\'),
            r"C:\Users\x\probe"
        );
    }

    /// upstream: openssh/misc.c:1309 - the trailing separator on the home
    /// directory is not doubled.
    #[test]
    fn trailing_separator_on_home_is_not_doubled() {
        assert_eq!(
            expand_tilde_with("~/probe", Some("/home/u/"), '/'),
            "/home/u/probe"
        );
        assert_eq!(
            expand_tilde_with("~/probe", Some(r"C:\Users\x\"), '\\'),
            r"C:\Users\x\probe"
        );
    }

    #[test]
    fn unexpandable_tilde_forms_are_left_alone() {
        assert_eq!(expand_tilde_with("~", Some("/home/u"), '/'), "~");
        assert_eq!(
            expand_tilde_with("~other/k", Some("/home/u"), '/'),
            "~other/k"
        );
        assert_eq!(expand_tilde_with("~/k", None, '/'), "~/k");
        assert_eq!(expand_tilde_with("/abs/k", Some("/home/u"), '/'), "/abs/k");
    }

    /// The live wrapper must pick the platform's own separator; this is what
    /// carries the by-value pins above onto the real path.
    #[test]
    fn live_wrapper_uses_the_platform_separator() {
        let home = home_dir().expect("HOME or USERPROFILE must be set");
        let expanded = expand_tilde_str("~/a/b");
        let expected = format!(
            "{}{}a{}b",
            home.to_string_lossy(),
            std::path::MAIN_SEPARATOR,
            std::path::MAIN_SEPARATOR
        );
        assert_eq!(expanded, expected);
    }

    #[test]
    fn pattern_question_mark_matches_single_char() {
        assert!(pattern_matches("a", "?"));
        assert!(!pattern_matches("ab", "?"));
        assert!(pattern_matches("ab", "??"));
    }
}
