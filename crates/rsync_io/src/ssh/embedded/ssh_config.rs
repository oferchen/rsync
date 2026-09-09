//! Minimal `~/.ssh/config` parser for the embedded russh transport.
//!
//! Recognises the subset of OpenSSH client directives that the embedded
//! transport can act on: `Host`, `Hostname`, `User`, `Port`, `IdentityFile`,
//! `IdentitiesOnly`, `IdentityAgent`. Unknown directives are skipped
//! silently. Wildcards in `Host` patterns follow OpenSSH semantics: `*`
//! matches any sequence, `?` matches any single character, `!pattern`
//! negates a match for that block.
//!
//! The parser is intentionally permissive: a malformed file produces an
//! empty result rather than an error, so a broken config never blocks a
//! transfer. The caller (`SshConfig::apply_ssh_config`) merges the
//! resolved directives into the existing config, with the rule that any
//! value already set on the URL wins over the config file.

use std::fs;
use std::path::{Path, PathBuf};

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
/// Returns `ResolvedHost::default()` when the file cannot be read or
/// contains no matching block. OpenSSH's first-match-wins precedence is
/// honoured: once a directive is set inside the first matching block, a
/// later block cannot overwrite it. Wildcards in `Host` patterns are
/// expanded with [`pattern_matches`].
pub(super) fn resolve_host(path: &Path, host_alias: &str) -> ResolvedHost {
    let Ok(text) = fs::read_to_string(path) else {
        return ResolvedHost::default();
    };
    resolve_host_str(&text, host_alias)
}

/// Variant of [`resolve_host`] that takes the config text directly.
/// Exposed for unit tests so they do not have to touch the filesystem.
pub(super) fn resolve_host_str(text: &str, host_alias: &str) -> ResolvedHost {
    let mut resolved = ResolvedHost::default();
    let mut in_matching_block = false;

    for raw_line in text.lines() {
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        let Some((key, value)) = split_directive(line) else {
            continue;
        };
        let key_lc = key.to_ascii_lowercase();

        if key_lc == "host" {
            in_matching_block = host_matches_any_pattern(host_alias, value);
            continue;
        }
        if !in_matching_block {
            continue;
        }

        match key_lc.as_str() {
            "hostname" => set_if_unset(&mut resolved.hostname, value.to_owned()),
            "user" => set_if_unset(&mut resolved.user, value.to_owned()),
            "port" => {
                if resolved.port.is_none()
                    && let Ok(parsed) = value.parse::<u16>()
                {
                    resolved.port = Some(parsed);
                }
            }
            "identityfile" => {
                let expanded = expand_tilde(value);
                if !resolved.identity_files.contains(&expanded) {
                    resolved.identity_files.push(expanded);
                }
            }
            "identitiesonly" => {
                if resolved.identities_only.is_none() {
                    resolved.identities_only = parse_yes_no(value);
                }
            }
            "identityagent" => {
                set_if_unset(&mut resolved.identity_agent, expand_tilde_str(value));
            }
            _ => {}
        }
    }

    resolved
}

/// Strips a trailing `#...` comment from a config line.
fn strip_comment(line: &str) -> &str {
    line.find('#').map_or(line, |idx| &line[..idx])
}

/// Splits a `Key Value` directive on the first run of whitespace or
/// optional `=` separator. Returns `None` for lines that contain only the
/// key with no value.
fn split_directive(line: &str) -> Option<(&str, &str)> {
    let (key, rest) = line.split_once(|c: char| c.is_whitespace() || c == '=')?;
    let value = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=');
    if value.is_empty() {
        return None;
    }
    Some((key, value))
}

/// Returns `true` when `host` matches any pattern in `patterns`, after
/// expanding wildcards and applying negations. OpenSSH treats space- or
/// comma-separated tokens as alternatives within a single `Host` line; a
/// leading `!` on any token negates and causes the whole line to fail to
/// match.
fn host_matches_any_pattern(host: &str, patterns: &str) -> bool {
    let mut any_positive_match = false;
    for raw in patterns.split(|c: char| c.is_whitespace() || c == ',') {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        let (negate, pat) = token
            .strip_prefix('!')
            .map_or((false, token), |stripped| (true, stripped));
        if pattern_matches(host, pat) {
            if negate {
                return false;
            }
            any_positive_match = true;
        }
    }
    any_positive_match
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

    #[test]
    fn missing_file_returns_default() {
        let resolved = resolve_host(Path::new("/nonexistent/ssh/config"), "anything");
        assert!(resolved.hostname.is_none());
        assert!(resolved.user.is_none());
        assert!(resolved.port.is_none());
        assert!(resolved.identity_files.is_empty());
    }

    #[test]
    fn simple_host_block_applies() {
        let text = "Host example\n  HostName 1.2.3.4\n  User deploy\n  Port 2222\n";
        let resolved = resolve_host_str(text, "example");
        assert_eq!(resolved.hostname.as_deref(), Some("1.2.3.4"));
        assert_eq!(resolved.user.as_deref(), Some("deploy"));
        assert_eq!(resolved.port, Some(2222));
    }

    #[test]
    fn non_matching_block_is_ignored() {
        let text = "Host other\n  HostName ignored\n";
        let resolved = resolve_host_str(text, "example");
        assert!(resolved.hostname.is_none());
    }

    #[test]
    fn wildcard_star_matches() {
        let text = "Host *.example.com\n  User wild\n";
        let resolved = resolve_host_str(text, "alpha.example.com");
        assert_eq!(resolved.user.as_deref(), Some("wild"));
    }

    #[test]
    fn first_match_wins() {
        let text = "Host example\n  User first\nHost *\n  User second\n";
        let resolved = resolve_host_str(text, "example");
        assert_eq!(resolved.user.as_deref(), Some("first"));
    }

    #[test]
    fn comments_and_blank_lines_skipped() {
        let text = "# top comment\n\nHost example  # inline\n  User u # trail\n";
        let resolved = resolve_host_str(text, "example");
        assert_eq!(resolved.user.as_deref(), Some("u"));
    }

    #[test]
    fn equals_separator_supported() {
        let text = "Host example\n  Port=4242\n";
        let resolved = resolve_host_str(text, "example");
        assert_eq!(resolved.port, Some(4242));
    }

    #[test]
    fn negation_disables_block() {
        let text = "Host *.example.com !banned.example.com\n  User u\n";
        let resolved = resolve_host_str(text, "banned.example.com");
        assert!(resolved.user.is_none());
        let resolved_ok = resolve_host_str(text, "ok.example.com");
        assert_eq!(resolved_ok.user.as_deref(), Some("u"));
    }

    #[test]
    fn identities_only_yes_is_recognised() {
        let text = "Host example\n  IdentitiesOnly yes\n";
        let resolved = resolve_host_str(text, "example");
        assert_eq!(resolved.identities_only, Some(true));
    }

    #[test]
    fn identity_files_collected_in_order() {
        let text = "Host example\n  IdentityFile /a\n  IdentityFile /b\n";
        let resolved = resolve_host_str(text, "example");
        assert_eq!(
            resolved.identity_files,
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }

    #[test]
    fn invalid_port_is_ignored() {
        let text = "Host example\n  Port notanumber\n";
        let resolved = resolve_host_str(text, "example");
        assert!(resolved.port.is_none());
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
        let resolved = resolve_host_str(text, "example");
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
        let resolved = resolve_host_str(text, "example");
        let sep = std::path::MAIN_SEPARATOR;
        let expected = format!("{}{sep}.ssh{sep}id_probe", home.to_string_lossy());
        assert_eq!(identity_file_strings(&resolved), vec![expected]);
    }

    #[test]
    fn identity_agent_tilde_expands_through_the_directive() {
        let home = home_dir().expect("HOME or USERPROFILE must be set");
        let text = "Host example\n  IdentityAgent ~/agent.sock\n";
        let resolved = resolve_host_str(text, "example");
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
