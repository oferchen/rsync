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
//! Resolution mirrors OpenSSH's first-obtained-wins rule: one slot per
//! directive, claimed by the first assignment from an applying line
//! anywhere in the ordered scan, regardless of which scope kind the
//! line sits in (openssh/readconf.c:1229
//! `if (*activep && *intptr == -1)`).
//! SSC-4.c wires the `Match` evaluator into the parser, honouring
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
//! - [`paths`] - the local-user env lookup for `Match localuser`. The
//!   file load order itself (user file, system file, `-F` override)
//!   lives in [`crate::ssh::config_files`], shared with the embedded
//!   transport's reader.
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

use logging::debug_log;

use crate::ssh::config_files::{ConfigFile, check_default_user_config_perms, config_files};
use parser::CompressionScan;

// Test-only aliases so the moved test module can keep reaching every
// item through `super::*`, matching the pre-decomposition single-file
// layout. Gated on `cfg(test)` because the `ssh` module proper only
// consumes `ssh_config_enables_compression` and `MatchContext`.
#[cfg(test)]
use match_block::{MatchCondition, evaluate_match, match_line_applies};
// `pub(super)` rather than module-private: the `ssh -G` differential
// harness (`embedded::ssh_config_differential`) reads the compression
// decision straight out of the parser so its `compression` row is pinned
// against real ssh instead of against a unit test's own belief.
#[cfg(test)]
pub(super) use parser::parse_enables_compression;
#[cfg(test)]
use pattern::{Pattern, host_patterns_from_tokens, parse_pattern_list};

/// Returns `true` when `~/.ssh/config` or `/etc/ssh/ssh_config`
/// configures `Compression yes` for `ctx` at top level or under a
/// matching `Host` or `Match` block.
///
/// `options` is the SSH option argv; the file load order is the shared
/// [`config_files`] owner's - a `-F <file>` (or `-F<file>`) override is
/// read INSTEAD of `~/.ssh/config` and suppresses `/etc/ssh/ssh_config`
/// entirely, while without `-F` BOTH the user and system files are read
/// in that order into one claimed-slot scan, so first-obtained-wins
/// arbitrates per keyword across the file boundary
/// (openssh/ssh.c:561-592 `process_config_files()`).
///
/// `ctx` carries the connection context evaluated by SSC-4.b/SSC-5.b:
/// the destination host (used for `Host` blocks and `Match host`),
/// `originalhost`, remote user, and local user. When every field is
/// empty only top-level and `Host *` / `Match all` directives can fire.
///
/// Returns `false` when:
/// - no file in the load order exists,
/// - no read file contains a matching `Compression yes`,
/// - a file real `ssh` would refuse is hit - an untokenisable line, or a
///   group/world-writable default `~/.ssh/config`
///   (openssh/readconf.c:2579-2587) - because upstream aborts the whole
///   load there and no connection would exist to warn about. A
///   `debug_log!` line is emitted and the function reports `false`
///   rather than aborting the transfer.
pub(super) fn ssh_config_enables_compression(options: &[OsString], ctx: &MatchContext<'_>) -> bool {
    enables_compression_in(&config_files(options), ctx)
}

/// The multi-file scan behind [`ssh_config_enables_compression`]: one
/// [`CompressionScan`] threaded across `files` in order, so the
/// claimed-slot state - not file existence - decides which file's
/// directive wins. Takes the file list as a parameter so tests can
/// inject fixture paths instead of the host's real config files.
fn enables_compression_in(files: &[ConfigFile], ctx: &MatchContext<'_>) -> bool {
    let mut scan = CompressionScan::default();
    for file in files {
        // upstream: SSHCONF_CHECKPERM applies to the default user file
        // only (openssh/ssh.c:583) and its failure is fatal - the load
        // stops, so later files are not read either.
        if file.check_perm
            && let Err(reason) = check_default_user_config_perms(&file.path)
        {
            debug_log!(
                Io,
                1,
                "ssh_config compression detection: abandoning scan, ssh would refuse: {}",
                reason
            );
            return false;
        }
        // A missing or unreadable file is skipped: upstream ignores the
        // result of reading the default files (openssh/ssh.c:580-589
        // discards read_config_file's return in the no--F arm).
        let Ok(text) = std::fs::read_to_string(&file.path) else {
            continue;
        };
        if scan.scan(&text, ctx).is_err() {
            return false;
        }
    }
    scan.finish()
}
