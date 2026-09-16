//! Minimal `~/.ssh/config` parser for the embedded russh transport.
//!
//! Recognises the subset of OpenSSH client directives that the embedded
//! transport can act on: `Host`, `Hostname`, `User`, `Port`, `IdentityFile`,
//! `IdentitiesOnly`, `IdentityAgent`, `ConnectTimeout`. Unknown directives are skipped
//! silently. Wildcards in `Host` patterns follow OpenSSH semantics: `*`
//! matches any sequence, `?` matches any single character, `!pattern`
//! negates a match for that block.
//!
//! # The keyword set is not declared here
//!
//! Which spelling means which directive comes from
//! [`crate::ssh::config_options`], the one option table oc has, mirroring
//! upstream's single `keywords[]` scan (openssh/readconf.c:1194
//! `parse_token`). This module owns only what it DOES with a resolved
//! opcode - the `Host`-only scope and the refusal policy below - and it
//! shares the `Key Value` split, the yes/no parse and the glob matcher
//! with the compression reader (`crate::ssh::config_lookup`). Adding a
//! directive is one row in the table plus one arm here, and the other
//! reader cannot drift away from the spelling.
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
//! worse outcome. Four refusals are mirrored, each with upstream's own
//! wording, and each fires whether or not the surrounding `Host` block
//! matches - upstream parses every line and gates only the ASSIGNMENT on
//! `*activep`:
//!
//! * `invalid quotes` - a quote left open (openssh/readconf.c:1196-1199).
//! * `keyword host empty argument` - an empty `Host` token
//!   (openssh/readconf.c:1832-1836).
//! * `Missing argument.` / `missing argument.` / `missing time value.` -
//!   a directive whose value tokenised to nothing, e.g. `Port #comment`.
//!   The wording is upstream's own and splits by value SHAPE, not by
//!   keyword: single-token options say `Missing` (openssh/readconf.c:1364),
//!   multistate flags say `missing` (openssh/readconf.c:1108), and time
//!   values have their own line (openssh/readconf.c:1219). Measured
//!   against real `ssh -G` for every keyword below. The wording hangs off
//!   the option table's `ValueKind` so a keyword added later cannot pick
//!   the wrong one.
//! * `invalid time value.` - a `ConnectTimeout` value `convtime` rejects
//!   (openssh/readconf.c:1224-1227).
//!
//! The caller (`SshConfig::apply_ssh_config`) merges the resolved
//! directives into the existing config, with the rule that any value
//! already set on the URL wins over the config file.

use std::fs;
use std::path::{Path, PathBuf};

use super::error::SshError;
use crate::ssh::argv_split::argv_split;
use crate::ssh::config_files::{ConfigFile, check_default_user_config_perms};
use crate::ssh::config_options::{
    Opcode, glob_matches, parse_flag_value, parse_time_value, parse_token, split_directive,
};

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
    /// `ConnectTimeout` in whole seconds. `None` means no directive
    /// obtained a value - which is also what `ConnectTimeout none`
    /// resolves to, since upstream maps `none` onto the same -1 the unset
    /// slot holds (openssh/readconf.c:1222-1223, :2734).
    pub connect_timeout: Option<u32>,
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
    let mut resolved = ResolvedHost::default();
    if let Ok(text) = fs::read_to_string(path) {
        resolve_host_into(
            &mut resolved,
            &text,
            host_alias,
            &path.display().to_string(),
        )?;
    }
    Ok(resolved)
}

/// Resolves `host_alias` across the ordered config-file load order,
/// threading ONE [`ResolvedHost`] through every file so the per-keyword
/// claim state - first-obtained for the scalar slots, accumulating for
/// `IdentityFile` - carries across the file boundary, exactly as
/// upstream reads the user file and then the system file into the same
/// options struct (openssh/ssh.c:561-592 `process_config_files()`).
/// `Host` block state, by contrast, is per file: each scan starts back
/// at the top level (openssh/readconf.c `read_config_file_depth()`
/// re-initialises `active`).
///
/// A file flagged `check_perm` (the default `~/.ssh/config`,
/// openssh/ssh.c:583) is refused when group/world-writable or owned by
/// neither root nor the caller; a missing or unreadable file is skipped.
///
/// # Errors
///
/// [`SshError::SshConfigPermissions`] when a `check_perm` file fails
/// upstream's owner/permission check (openssh/readconf.c:2579-2587);
/// [`SshError::SshConfig`] when a line is one upstream would refuse.
/// Either aborts the load - later files are not read.
pub(super) fn resolve_host_files(
    files: &[ConfigFile],
    host_alias: &str,
) -> Result<ResolvedHost, SshError> {
    let mut resolved = ResolvedHost::default();
    for file in files {
        if file.check_perm && check_default_user_config_perms(&file.path).is_err() {
            return Err(SshError::SshConfigPermissions {
                path: file.path.display().to_string(),
            });
        }
        let Ok(text) = fs::read_to_string(&file.path) else {
            continue;
        };
        resolve_host_into(
            &mut resolved,
            &text,
            host_alias,
            &file.path.display().to_string(),
        )?;
    }
    Ok(resolved)
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
    let mut resolved = ResolvedHost::default();
    resolve_host_into(&mut resolved, text, host_alias, INLINE_CONFIG_NAME)?;
    Ok(resolved)
}

/// Builds the refusal upstream would print for `path` line `line`.
fn refuse(path: &str, line: usize, reason: impl Into<String>) -> SshError {
    SshError::SshConfig {
        path: path.to_owned(),
        line,
        reason: reason.into(),
    }
}

/// Scans one file's text into `resolved`, claiming slots per the option
/// table's `ResolutionPolicy` - which is what lets a later file's scan
/// continue the same state. `Host` block activity is local to the call.
fn resolve_host_into(
    resolved: &mut ResolvedHost,
    text: &str,
    host_alias: &str,
    path: &str,
) -> Result<(), SshError> {
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
        // One keyword lookup against the shared option table, exactly
        // where upstream resolves the opcode for the line
        // (openssh/readconf.c:1194 `parse_token`).
        let opcode = parse_token(key);

        // One tokenisation per line, exactly as upstream does it
        // (openssh/readconf.c:1196).
        let tokens =
            argv_split(value, true).map_err(|err| refuse(path, linenum, err.to_string()))?;

        if opcode == Opcode::Host {
            in_matching_block =
                host_matches_any_pattern(host_alias, &tokens).map_err(|EmptyHostToken| {
                    // Upstream interpolates the LOWERCASED keyword it
                    // matched on (openssh/readconf.c:1184, :1833), not the
                    // spelling in the file.
                    refuse(path, linenum, "keyword host empty argument")
                })?;
            continue;
        }

        // Every directive below is single-valued, so it consumes exactly
        // one token - upstream's `arg = argv_next(&ac, &av)`. A `NULL`
        // there is fatal, which is how `Port #comment` is refused. The
        // refusal fires BEFORE the block-activity gate: upstream
        // processes every line and `*activep` gates only the assignment
        // (e.g. openssh/readconf.c:1229 `if (*activep && *intptr == -1)`),
        // so a bad line inside a NON-matching `Host` block still aborts
        // the load. Measured on `ssh -G`: `Host other` + `Port #x` is
        // `Missing argument.` even for an alias `other` never matches.
        //
        // The opcode set is this reader's, not the table's: `Compression`
        // is in the table with the same shape but belongs to the other
        // reader, which never refuses. Widening the refusal to every known
        // keyword is upstream's behaviour and a separate row.
        let arg = match opcode {
            Opcode::Hostname
            | Opcode::User
            | Opcode::Port
            | Opcode::IdentityFile
            | Opcode::IdentitiesOnly
            | Opcode::IdentityAgent
            | Opcode::ConnectTimeout => {
                let Some(arg) = tokens.first() else {
                    let Some(reason) = opcode.missing_argument() else {
                        continue;
                    };
                    return Err(refuse(path, linenum, reason));
                };
                arg.as_str()
            }
            // Not a directive this parser acts on. Upstream would still
            // classify it, but oc's unknown-keyword policy is task 237j's
            // row, not this one.
            _ => continue,
        };

        // Value validation that upstream performs before the `*activep`
        // check, so it too refuses from an inactive block.
        let connect_timeout = match opcode {
            Opcode::ConnectTimeout => {
                parse_connect_timeout(arg).map_err(|reason| refuse(path, linenum, reason))?
            }
            _ => None,
        };

        if !in_matching_block {
            continue;
        }

        match opcode {
            Opcode::Hostname => set_if_unset(&mut resolved.hostname, arg.to_owned()),
            Opcode::User => set_if_unset(&mut resolved.user, arg.to_owned()),
            Opcode::Port => {
                if resolved.port.is_none()
                    && let Ok(parsed) = arg.parse::<u16>()
                {
                    resolved.port = Some(parsed);
                }
            }
            Opcode::IdentityFile => {
                let expanded = expand_tilde(arg);
                if !resolved.identity_files.contains(&expanded) {
                    resolved.identity_files.push(expanded);
                }
            }
            Opcode::IdentitiesOnly => {
                if resolved.identities_only.is_none() {
                    resolved.identities_only = parse_flag_value(arg);
                }
            }
            Opcode::IdentityAgent => {
                set_if_unset(&mut resolved.identity_agent, expand_tilde_str(arg));
            }
            Opcode::ConnectTimeout => {
                // `None` here is `ConnectTimeout none`: upstream writes -1,
                // the unset sentinel, so the slot stays claimable and a
                // LATER numeric directive still wins (measured on
                // `ssh -G`: `none` then `5` dumps 5). A plain
                // first-obtained-wins that let `none` claim the slot
                // would diverge exactly there.
                if let Some(secs) = connect_timeout {
                    set_if_unset(&mut resolved.connect_timeout, secs);
                }
            }
            _ => unreachable!("the match above already narrowed the opcode set"),
        }
    }

    Ok(())
}

/// Parses a `ConnectTimeout` value per the `parse_time` arm
/// (openssh/readconf.c:1215-1233). `Ok(None)` is the case-SENSITIVE
/// literal `none` (strcmp at openssh/readconf.c:1222), which upstream maps
/// onto the unset sentinel; everything else must satisfy `convtime` or the
/// load is refused with upstream's own wording. An empty token shares the
/// missing-value wording (`!arg || *arg == '\0'`, openssh/readconf.c:1218).
fn parse_connect_timeout(arg: &str) -> Result<Option<u32>, &'static str> {
    if arg.is_empty() {
        return Err("missing time value.");
    }
    if arg == "none" {
        return Ok(None);
    }
    match parse_time_value(arg) {
        Some(secs) => Ok(Some(secs)),
        None => Err("invalid time value."),
    }
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

/// Glob-matches `host` against `pattern` with no case folding, which is
/// what the `oHost` arm does (openssh/readconf.c:1844 calls `match_pattern`
/// directly). The glob itself is the shared `match_pattern` port.
fn pattern_matches(host: &str, pattern: &str) -> bool {
    glob_matches(host.as_bytes(), pattern.as_bytes())
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

    // -- ConnectTimeout ---------------------------------------------------
    //
    // Every cell below was measured against real `ssh -G` (OpenSSH_10.3p1)
    // before being written down; the differential harness pins the
    // oracle-reachable half live.

    #[test]
    fn connect_timeout_resolves_in_seconds() {
        assert_eq!(
            resolve("Host a\n  ConnectTimeout 7\n", "a").connect_timeout,
            Some(7)
        );
        // convtime's qualifier forms reach the directive.
        assert_eq!(
            resolve("Host a\n  ConnectTimeout 1m30s\n", "a").connect_timeout,
            Some(90)
        );
        // 0 is a VALUE, not unset (upstream dumps `connecttimeout 0`).
        assert_eq!(
            resolve("Host a\n  ConnectTimeout 0\n", "a").connect_timeout,
            Some(0)
        );
    }

    /// First-obtained-wins, the property the whole 237d row hangs on.
    /// Oracle: `7` then `9` dumps `connecttimeout 7`.
    #[test]
    fn connect_timeout_first_obtained_wins() {
        let text = "Host a\n  ConnectTimeout 7\n  ConnectTimeout 9\n";
        assert_eq!(resolve(text, "a").connect_timeout, Some(7));
    }

    /// The `none` SENTINEL quirk: upstream's `none` writes -1, which IS
    /// the unset sentinel, so `none` never claims the first-obtained slot
    /// and a LATER value still wins (openssh/readconf.c:1222, :1229).
    /// Oracle: `none` then `5` dumps `connecttimeout 5`; `5` then `none`
    /// dumps 5; `none` alone dumps `connecttimeout none`.
    #[test]
    fn connect_timeout_none_is_the_unset_sentinel_not_a_value() {
        assert_eq!(
            resolve("Host a\n  ConnectTimeout none\n  ConnectTimeout 5\n", "a").connect_timeout,
            Some(5)
        );
        assert_eq!(
            resolve("Host a\n  ConnectTimeout 5\n  ConnectTimeout none\n", "a").connect_timeout,
            Some(5)
        );
        assert_eq!(
            resolve("Host a\n  ConnectTimeout none\n", "a").connect_timeout,
            None
        );
    }

    /// A value under a NON-matching `Host` block does not apply.
    /// Oracle: `Host other` + `ConnectTimeout 9` dumps `connecttimeout
    /// none` for alias `t`.
    #[test]
    fn connect_timeout_is_host_scoped() {
        let text = "Host other\n  ConnectTimeout 9\nHost t\n  Port 2222\n";
        let resolved = resolve(text, "t");
        assert_eq!(resolved.connect_timeout, None);
        assert_eq!(resolved.port, Some(2222));
    }

    /// The refusals, upstream's own wording (openssh/readconf.c:1218-1227):
    /// a missing or empty value says `missing time value.`, anything
    /// convtime rejects says `invalid time value.` - including `NONE`,
    /// because the `none` strcmp is case-SENSITIVE.
    #[test]
    fn connect_timeout_refusals_use_upstreams_wording() {
        for (text, reason) in [
            ("Host a\n  ConnectTimeout #x\n", "missing time value."),
            ("Host a\n  ConnectTimeout \"\"\n", "missing time value."),
            ("Host a\n  ConnectTimeout bogus\n", "invalid time value."),
            ("Host a\n  ConnectTimeout -5\n", "invalid time value."),
            ("Host a\n  ConnectTimeout NONE\n", "invalid time value."),
        ] {
            assert_eq!(
                refusal(text, "a"),
                format!("<ssh_config> line 2: {reason}"),
                "fixture {text:?}"
            );
        }
    }

    /// The exposure-surfaced defect this change fixes: upstream refuses a
    /// bad line from a NON-matching `Host` block too, because `*activep`
    /// gates only the assignment (openssh/readconf.c:1229) while the parse
    /// runs for every line. oc used to skip inactive blocks wholesale.
    /// Oracle: both fixtures exit 255 for alias `t`, refusing line 2.
    #[test]
    fn a_bad_value_in_a_non_matching_block_still_refuses() {
        assert_eq!(
            refusal("Host other\n  ConnectTimeout bogus\nHost t\n", "t"),
            "<ssh_config> line 2: invalid time value."
        );
        // The hoisted gate covers the OTHER value options' missing-arg
        // refusal too - the same upstream rule, same measurement.
        assert_eq!(
            refusal("Host other\n  Port #x\nHost t\n", "t"),
            "<ssh_config> line 2: Missing argument."
        );
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

    // -- file load order (task 237e) ----------------------------------
    //
    // upstream: openssh/ssh.c:561-592 `process_config_files()` reads the
    // user file, then the system file, into ONE options struct. The
    // system path is injected through the `ConfigFile` seam rather than
    // read from the host's real `/etc/ssh/ssh_config`.

    /// Materialises `text` as a config file and returns its
    /// [`ConfigFile`] row for a composed-resolve fixture.
    fn fixture_file(
        dir: &tempfile::TempDir,
        name: &str,
        text: &str,
        check_perm: bool,
    ) -> ConfigFile {
        let path = dir.path().join(name);
        std::fs::write(&path, text).expect("write fixture");
        ConfigFile { path, check_perm }
    }

    /// Cell (a): a scalar keyword set in BOTH files keeps the USER
    /// file's value - the claim state crosses the file boundary, so the
    /// system file cannot re-claim a taken slot
    /// (openssh/readconf.c:1229 over openssh/ssh.c:571-589).
    #[test]
    fn user_file_claims_scalar_slots_before_the_system_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = fixture_file(&dir, "user", "Host t\n  Port 1111\n  User first\n", false);
        let system = fixture_file(
            &dir,
            "system",
            "Host *\n  Port 2222\n  User second\n  HostName sys.example.com\n",
            false,
        );
        let resolved = resolve_host_files(&[user, system], "t").expect("accepted");
        assert_eq!(resolved.port, Some(1111));
        assert_eq!(resolved.user.as_deref(), Some("first"));
        // A slot the user file left unclaimed IS the system file's to
        // claim - cell (b) for this reader, red before this change.
        assert_eq!(resolved.hostname.as_deref(), Some("sys.example.com"));
    }

    /// `IdentityFile` keeps its `Accumulate` policy across the boundary:
    /// both files' identities land, in load order
    /// (openssh/readconf.c:1394 `add_identity_file` has no unset test).
    #[test]
    fn identity_files_accumulate_across_both_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = fixture_file(&dir, "user", "Host t\n  IdentityFile /user/key\n", false);
        let system = fixture_file(
            &dir,
            "system",
            "Host *\n  IdentityFile /system/key\n",
            false,
        );
        let resolved = resolve_host_files(&[user, system], "t").expect("accepted");
        assert_eq!(
            resolved.identity_files,
            vec![PathBuf::from("/user/key"), PathBuf::from("/system/key")]
        );
    }

    /// A missing user file is skipped and the system file still applies
    /// (openssh/ssh.c:580-589 discards the default reads' results).
    #[test]
    fn a_missing_user_file_still_reaches_the_system_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = ConfigFile {
            path: dir.path().join("nonexistent"),
            check_perm: true,
        };
        let system = fixture_file(&dir, "system", "Host *\n  Port 2222\n", false);
        let resolved = resolve_host_files(&[user, system], "t").expect("accepted");
        assert_eq!(resolved.port, Some(2222));
    }

    /// A refusal in the user file aborts the whole load: the error names
    /// the USER file even though the system file carries the same defect
    /// on a different line, proving the system file was never scanned
    /// (openssh/readconf.c:2667 fatals before the second read).
    #[test]
    fn a_refused_user_file_stops_before_the_system_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = fixture_file(&dir, "user", "Host \"broken\n", false);
        let system = fixture_file(&dir, "system", "ok line\nHost \"broken\n", false);
        let err = resolve_host_files(&[user.clone(), system], "t").expect_err("refused");
        assert_eq!(
            err.to_string(),
            format!("{} line 1: invalid quotes", user.path.display())
        );
    }

    /// Cell (d), embedded half: the world-writable DEFAULT user file is
    /// refused with upstream's fatal wording, while the SAME mode on an
    /// explicit (`-F`-shaped, `check_perm: false`) file is accepted -
    /// CHECKPERM's scope is the flag, not the file
    /// (openssh/ssh.c:571-583, openssh/readconf.c:2579-2587).
    #[cfg(unix)]
    #[test]
    fn checkperm_refuses_the_default_user_file_but_not_an_explicit_one() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let mut file = fixture_file(&dir, "config", "Host t\n  Port 2222\n", true);
        std::fs::set_permissions(&file.path, std::fs::Permissions::from_mode(0o666))
            .expect("chmod");
        let err = resolve_host_files(&[file.clone()], "t").expect_err("refused");
        assert_eq!(
            err.to_string(),
            format!("Bad owner or permissions on {}", file.path.display())
        );
        file.check_perm = false;
        let resolved = resolve_host_files(&[file], "t").expect("explicit file accepted");
        assert_eq!(resolved.port, Some(2222));
    }
}
