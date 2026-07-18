//! Error policy and exit-code constants for the delete emitter.
//!
//! Mirrors upstream rsync's continue-vs-abort behaviour around the delete
//! pass (`delete.c:178-207`) and surfaces the exit codes the caller maps
//! to upstream's `RERR_PARTIAL` / `RERR_VANISHED`.

/// Exit code for partial transfers caused by an I/O failure during the
/// delete pass. Mirrors upstream `errcode.h::RERR_PARTIAL` and
/// `core::exit_code::ExitCode::PartialTransfer`.
pub const EMITTER_PARTIAL_EXIT_CODE: i32 = 23;

/// Exit code reported when a destination entry vanished mid-pass. Mirrors
/// upstream `errcode.h::RERR_VANISHED` and `core::exit_code::ExitCode::Vanished`.
pub const EMITTER_VANISHED_EXIT_CODE: i32 = 24;

/// Upstream `IOERR_GENERAL`: the general-error bit the delete pass sets
/// for non-fatal failures other than a vanished destination entry.
pub(super) const IOERR_GENERAL: i32 = 1;

/// Sentinel bit set when the only failure observed was a vanished
/// destination entry (`io::ErrorKind::NotFound`). Distinct from
/// `IOERR_GENERAL` so the caller can map a vanished-only run to exit
/// code 24 instead of 23.
pub(super) const IOERR_VANISHED_ONLY: i32 = 1 << 1;

/// Policy controlling how the emitter reacts to per-entry I/O failures.
///
/// Mirrors upstream rsync's `--ignore-errors` and continue-on-error
/// behaviour (`delete.c:178-207`). The two booleans are orthogonal:
///
/// - `ignore_errors`: when `true`, non-fatal failures are logged but the
///   shared `io_error` flag is NOT set. Matches upstream `--ignore-errors`
///   which suppresses the `IOERR_GENERAL` bit so the run can still exit 0.
/// - `continue_on_error`: when `true`, non-fatal failures do not abort the
///   drain - the emitter records the error in `io_error` (unless
///   suppressed by `ignore_errors`) and moves on to the next entry. When
///   `false`, the first non-fatal failure also stops the drain.
///
/// Fatal classifications (see `DeleteEmitter::is_fatal_error`) always
/// abort the drain regardless of these flags so the caller can surface
/// the failure with a non-zero exit code.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct EmitterErrorPolicy {
    /// Suppress the `io_error` flag for non-fatal failures.
    pub ignore_errors: bool,
    /// Keep draining after a non-fatal failure.
    pub continue_on_error: bool,
}

impl Default for EmitterErrorPolicy {
    /// Upstream's default: surface non-fatal errors via `io_error` but
    /// keep going. Matches `delete.c:178-207`: errors flip the flag and
    /// the loop in `delete_dir_contents` continues to the next entry.
    fn default() -> Self {
        Self {
            ignore_errors: false,
            continue_on_error: true,
        }
    }
}
