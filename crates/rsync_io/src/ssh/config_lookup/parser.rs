//! The ssh_config scanner driving compression detection.
//!
//! Reads a config file, walks its lines in order tracking the active
//! [`Block`], and applies OpenSSH's first-obtained-wins rule - one slot
//! per option, claimed by the first assignment from an applying line -
//! then exposes the decision through [`parse_enables_compression`].
//!
//! Every line-level primitive is borrowed rather than owned here: the
//! keyword lookup, the `Key Value` split and the yes/no parse all come
//! from [`crate::ssh::config_options`], and values are tokenised by the
//! shared [`argv_split`] owner. The connection-configuring reader
//! (`crate::ssh::embedded::ssh_config`) consumes the same four, so the
//! two cannot disagree about what a keyword is called, where a token
//! ends, or what counts as `yes`.

use std::path::Path;

use logging::debug_log;

use crate::ssh::argv_split::argv_split;
use crate::ssh::config_options::{Opcode, parse_flag_value, parse_token, split_directive};

use super::match_block::{MatchContext, match_line_applies};
use super::pattern::{MatchKind, Pattern, host_patterns_from_tokens, pattern_list_matches};

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
/// Per OpenSSH's first-obtained-wins rule the option keeps ONE slot,
/// claimed by the first assignment made from an active line: upstream
/// sets an option only while it is still unset
/// (openssh/readconf.c:1229 `if (*activep && *intptr == -1)`), over a
/// single ordered pass of the file. Whether a line is active is decided
/// by the block it sits in - top level always applies; a `Host` block
/// applies when `ctx.host` matches at least one positive pattern token
/// and no negated token (`pattern_list_matches`, sourced from SSC-4.b);
/// a `Match` block applies when every condition on its header line
/// evaluates true against `ctx`, with SKIP/DEFER conditions
/// (`canonical`, `final`, `tagged`, `exec`) rendering the whole block
/// inert per SSC-4.a. The per-option claim itself is the option table's
/// [`ResolutionPolicy`](crate::ssh::config_options::ResolutionPolicy):
/// `Compression` is a `FirstObtained` row.
///
/// `ctx`'s fields may be empty; in that case only `Host *` and
/// `Match all` (or other patterns that tolerate empty input) can match.
///
/// When a `Match exec` block containing `Compression yes` is
/// encountered, a user-visible warning is emitted explaining that the
/// exec condition was not evaluated and suggesting a workaround (move
/// the directive to a `Host` or `Match host` block, or pass
/// `-e "ssh -C"` explicitly). The warning fires only when the skipped
/// directive could still have claimed the slot - once an earlier active
/// line has claimed it, an evaluated exec block could not have changed
/// the answer either, so no warning is owed.
///
/// Exposed to tests so they can assert behaviour without disk I/O.
pub(in crate::ssh) fn parse_enables_compression(text: &str, ctx: &MatchContext<'_>) -> bool {
    let mut block = Block::TopLevel;
    let mut compression: Option<bool> = None;
    let mut exec_block_has_compression = false;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        // The ONLY place a leading `#` is a comment marker: upstream tests
        // the first non-blank character of the line and skips
        // (openssh/readconf.c:1181). Anywhere else a `#` is handled by
        // `argv_split`'s token-boundary rule below, not by cutting the
        // string here - `HostName x#y` is a hostname containing a `#`.
        if line.is_empty() || line.starts_with('#') {
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
        // A value real `ssh` cannot tokenise aborts its whole config load
        // (openssh/readconf.c:1196-1199 -> :2667), so there is no
        // connection left to warn about. Abandoning the scan reports "no
        // compression", which is this reader's documented degraded answer
        // - it must not fail, and it must not guess.
        let Ok(tokens) = argv_split(value, true) else {
            debug_log!(
                Io,
                1,
                "ssh_config compression detection: abandoning scan, ssh would refuse this file"
            );
            return false;
        };
        // One keyword lookup against the shared option table, exactly as
        // upstream resolves the opcode once per line before the switch
        // (openssh/readconf.c:1194 `parse_token`).
        let opcode = parse_token(key);
        match opcode {
            Opcode::Host => {
                block = Block::Host(host_patterns_from_tokens(&tokens));
            }
            Opcode::Match => {
                let mut saw_exec = false;
                let applies = match_line_applies(&tokens, ctx, &mut saw_exec);
                block = if saw_exec {
                    Block::MatchExecSkipped
                } else {
                    Block::MatchEvaluated(applies)
                };
            }
            Opcode::Compression => {
                let parsed = tokens
                    .first()
                    .map(String::as_str)
                    .and_then(parse_flag_value);
                // The activity gate: upstream's `*activep`, decided by
                // the enclosing block for every line alike
                // (openssh/readconf.c:1829 for `Host`, :1864 for
                // `Match`).
                let active = match &block {
                    Block::TopLevel => true,
                    Block::Host(patterns) => {
                        pattern_list_matches(patterns, ctx.host, MatchKind::HostBlock)
                    }
                    Block::MatchEvaluated(applies) => *applies,
                    Block::MatchExecSkipped => {
                        // Never active - the condition was not evaluated.
                        // But a `Compression yes` here could have claimed
                        // a still-unset slot had real ssh evaluated the
                        // block, which is exactly when the warning below
                        // is owed.
                        if parsed == Some(true) && compression.is_none() {
                            exec_block_has_compression = true;
                        }
                        false
                    }
                };
                // ONE slot per option, claimed by the first assignment
                // from an active line: upstream assigns only while the
                // option is unset (openssh/readconf.c:1229
                // `if (*activep && *intptr == -1)`). The claim rule is
                // the table's per-option `ResolutionPolicy` row.
                if active && opcode.resolution_policy().may_assign(compression.is_some()) {
                    compression = parsed;
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

    compression.unwrap_or(false)
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
