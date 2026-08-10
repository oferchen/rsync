//! The ssh_config scanner driving compression detection.
//!
//! Reads a config file, walks its lines tracking the active [`Block`],
//! and applies OpenSSH's first-match-wins rule per scope. Holds the
//! line-level helpers ([`strip_comment`], [`split_directive`],
//! [`parse_yes_no`]) and the public [`parse_enables_compression`]
//! entry consumed by tests and the lookup driver.

use std::path::Path;

use logging::debug_log;

use super::match_block::{MatchContext, match_line_applies};
use super::pattern::{MatchKind, Pattern, parse_pattern_list, pattern_list_matches};

/// Reads `path` and returns whether it enables compression for `ctx`.
/// Parse and I/O errors are converted to `false` with a single
/// diagnostic line.
pub(super) fn read_and_check(path: &Path, ctx: &MatchContext<'_>) -> bool {
    match std::fs::read_to_string(path) {
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
pub(in crate::ssh) fn parse_enables_compression(text: &str, ctx: &MatchContext<'_>) -> bool {
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
