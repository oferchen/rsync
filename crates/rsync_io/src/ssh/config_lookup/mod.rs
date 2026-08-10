//! SSH client config lookup for the `Compression` directive.
//!
//! Closes the SSC-3 gap: SSC-1 only inspects argv, so a user with
//! `Compression yes` set globally in `~/.ssh/config` or
//! `/etc/ssh/ssh_config` gets no warning when they also pass rsync's
//! `--compress`. SSC-5.b extends the parser to honour per-host `Host`
//! blocks (`Host web*.example.com`, `Host !banned.example.com *`) using
//! the connection target as the pattern-match input.
//!
//! # Scope
//!
//! Top-level directives, `Host` blocks (including glob and negation
//! tokens), and `Match` blocks whose conditions all pass are honoured.
//! Resolution mirrors OpenSSH's first-match-wins rule per directive: the
//! first matching `Compression` assignment in the scan wins for its
//! scope. SSC-4.c wires the `Match` evaluator into the parser, honouring
//! `host`, `originalhost`, `user`, `localuser`, and `all`. `Match exec`
//! is deliberately unsupported - executing arbitrary shell commands from
//! a passive config-lookup path is a security risk, and the
//! compression-detection use case does not need it. When a `Match exec`
//! block containing `Compression yes` is encountered, the parser emits
//! a user-visible warning explaining that the exec condition was not
//! evaluated and suggesting a workaround. The rest of the SKIP set
//! (`canonical`, `final`, `tagged`) likewise short-circuits the block.
//!
//! # Failure mode
//!
//! Parse errors and I/O errors never propagate. A malformed file emits
//! one `debug_log!` line and the caller falls back to the argv-only
//! answer. Hard-failing the transfer because a user's ssh_config has a
//! stray byte would be a worse outcome than missing a warning.
//!
//! # Module layout
//!
//! - [`paths`] - candidate file resolution, `-F` override, home/env.
//! - [`pattern`] - `Host`/`Match` pattern tokens and glob matching.
//! - [`match_block`] - `Match` condition model, context, evaluation.
//! - [`parser`] - the ssh_config scanner and compression decision.
//!
//! # References
//!
//! - `docs/design/ssc-5-host-pattern-audit.md` - SSC-5 audit and fix
//!   shape that motivated this module's `Host`-pattern wiring.
//! - `docs/design/ssc-4a-match-conditions.md` - shared `Pattern` type
//!   and `MatchKind`-based case-folding policy (SSC-4.b).
//! - Memory note `project_ssh_compression_no_config_parse.md` - tracks
//!   the residual gaps closed by SSC-3..SSC-5.

use std::ffi::OsString;

mod match_block;
mod parser;
mod paths;
mod pattern;

#[cfg(test)]
mod tests;

pub(super) use match_block::MatchContext;

use parser::read_and_check;
use paths::candidate_paths;

// Test-only aliases so the moved test module can keep reaching every
// item through `super::*`, matching the pre-decomposition single-file
// layout. Gated on `cfg(test)` because the `ssh` module proper only
// consumes `ssh_config_enables_compression` and `MatchContext`.
#[cfg(test)]
use match_block::{MatchCondition, evaluate_match, match_line_applies};
#[cfg(test)]
use parser::parse_enables_compression;
#[cfg(test)]
use paths::extract_dash_f_path;
#[cfg(test)]
use pattern::{Pattern, parse_pattern_list};

/// Returns `true` when `~/.ssh/config` or `/etc/ssh/ssh_config`
/// configures `Compression yes` for `ctx` at top level or under a
/// matching `Host` or `Match` block.
///
/// `options` is the SSH option argv; a `-F <file>` (or `-F<file>`)
/// override is honoured first when present and the file exists. After
/// the override, the lookup tries `~/.ssh/config`, then
/// `/etc/ssh/ssh_config`. The first existing file wins; later files in
/// the list are not consulted, matching OpenSSH's behaviour when `-F`
/// is supplied.
///
/// `ctx` carries the connection context evaluated by SSC-4.b/SSC-5.b:
/// the destination host (used for `Host` blocks and `Match host`),
/// `originalhost`, remote user, and local user. When every field is
/// empty only top-level and `Host *` / `Match all` directives can fire.
///
/// Returns `false` when:
/// - no candidate file exists,
/// - the chosen file does not contain a matching `Compression yes`,
/// - the chosen file fails to parse (a `debug_log!` line is emitted and
///   the function reports `false` rather than aborting the transfer).
pub(super) fn ssh_config_enables_compression(options: &[OsString], ctx: &MatchContext<'_>) -> bool {
    for candidate in candidate_paths(options) {
        if !candidate.is_file() {
            continue;
        }
        return read_and_check(&candidate, ctx);
    }
    false
}
