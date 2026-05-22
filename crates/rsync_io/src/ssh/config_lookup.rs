//! SSH client config lookup for the `Compression` directive.
//!
//! Closes the SSC-3 gap: SSC-1 only inspects argv, so a user with
//! `Compression yes` set globally in `~/.ssh/config` or
//! `/etc/ssh/ssh_config` gets no warning when they also pass rsync's
//! `--compress`. This module reads those files and reports whether
//! `Compression yes` is in effect at top level or inside a `Host *`
//! block.
//!
//! # Scope
//!
//! Top-level directives (before any `Host` block) and directives under
//! `Host *` are honoured. Per-host blocks like `Host foo.example.com`
//! and `Match` blocks are intentionally not evaluated here: the warning
//! site does not know which host the user is about to connect to in a
//! form that can drive OpenSSH's full pattern-match semantics
//! (especially `Match exec`, which would have to run an external
//! command). Deferring these keeps the warning conservative and free of
//! false positives.
//!
//! # Failure mode
//!
//! Parse errors and I/O errors never propagate. A malformed file emits
//! one `debug_log!` line and the caller falls back to the argv-only
//! answer. Hard-failing the transfer because a user's ssh_config has a
//! stray byte would be a worse outcome than missing a warning.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use logging::debug_log;

/// Returns `true` when `~/.ssh/config` or `/etc/ssh/ssh_config`
/// configures `Compression yes` at top level or under `Host *`.
///
/// `options` is the SSH option argv; a `-F <file>` (or `-F<file>`)
/// override is honoured first when present and the file exists. After
/// the override, the lookup tries `~/.ssh/config`, then
/// `/etc/ssh/ssh_config`. The first existing file wins; later files in
/// the list are not consulted, matching OpenSSH's behaviour when `-F`
/// is supplied.
///
/// Returns `false` when:
/// - no candidate file exists,
/// - the chosen file does not contain a matching `Compression yes`,
/// - the chosen file fails to parse (a `debug_log!` line is emitted and
///   the function reports `false` rather than aborting the transfer).
pub(super) fn ssh_config_enables_compression(options: &[OsString]) -> bool {
    for candidate in candidate_paths(options) {
        if !candidate.is_file() {
            continue;
        }
        return read_and_check(&candidate);
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

/// Reads `path` and returns whether it enables compression. Parse and
/// I/O errors are converted to `false` with a single diagnostic line.
fn read_and_check(path: &Path) -> bool {
    match fs::read_to_string(path) {
        Ok(text) => parse_enables_compression(&text),
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
/// at top level or under `Host *`.
///
/// Exposed to tests so they can assert behaviour without disk I/O.
pub(super) fn parse_enables_compression(text: &str) -> bool {
    let mut block = Block::TopLevel;
    let mut top_level: Option<bool> = None;
    let mut host_star: Option<bool> = None;

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
                block = if host_patterns_include_star(value) {
                    Block::HostStar
                } else {
                    Block::HostOther
                };
            }
            "match" => {
                block = Block::Match;
            }
            "compression" => {
                let parsed = parse_yes_no(value);
                match block {
                    Block::TopLevel if top_level.is_none() => top_level = parsed,
                    Block::HostStar if host_star.is_none() => host_star = parsed,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    top_level.unwrap_or(false) || host_star.unwrap_or(false)
}

/// Active config block while parsing.
#[derive(Copy, Clone, Eq, PartialEq)]
enum Block {
    TopLevel,
    HostStar,
    HostOther,
    Match,
}

/// Returns `true` when any token in `patterns` is exactly `*`. Matches
/// OpenSSH's "wildcard everything" idiom, which is the only host
/// pattern we evaluate without runtime context.
fn host_patterns_include_star(patterns: &str) -> bool {
    patterns
        .split(|c: char| c.is_whitespace() || c == ',')
        .map(str::trim)
        .any(|tok| tok == "*")
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
        assert!(parse_enables_compression("Compression yes\n"));
    }

    #[test]
    fn host_star_compression_yes_detected() {
        let text = "Host *\n  Compression yes\n";
        assert!(parse_enables_compression(text));
    }

    #[test]
    fn per_host_compression_yes_ignored() {
        let text = "Host foo.example.com\n  Compression yes\n";
        assert!(!parse_enables_compression(text));
    }

    #[test]
    fn compression_no_returns_false() {
        assert!(!parse_enables_compression("Compression no\n"));
    }

    #[test]
    fn match_block_compression_yes_ignored() {
        let text = "Match host bar\n  Compression yes\n";
        assert!(!parse_enables_compression(text));
    }

    #[test]
    fn equals_separator_supported() {
        assert!(parse_enables_compression("Compression=yes\n"));
    }

    #[test]
    fn comments_stripped() {
        assert!(parse_enables_compression(
            "# header\nCompression yes # trailing\n"
        ));
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
}
