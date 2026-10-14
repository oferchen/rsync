//! Error policy and exit-code constants for the delete emitter.
//!
//! Mirrors upstream rsync's continue-vs-abort behaviour around the delete
//! pass (`delete.c:delete_dir_contents()` / `delete_item()`) and surfaces
//! the exit codes the caller maps
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
///
/// Re-exported from the one definition in `protocol::io_error` rather than
/// restated, so the bit cannot drift from the value the wire decoders use.
pub(super) use protocol::IOERR_GENERAL;

/// Bit set when the only failure observed was a vanished destination entry
/// (`io::ErrorKind::NotFound`), letting the caller map a vanished-only run to
/// exit code 24 instead of 23. This is upstream's `IOERR_VANISHED`; the local
/// name records that the delete pass uses it as a sole-cause sentinel.
pub(super) use protocol::IOERR_VANISHED as IOERR_VANISHED_ONLY;

/// Policy controlling how the emitter reacts to per-entry I/O failures.
///
/// Mirrors upstream rsync's `--ignore-errors` and continue-on-error
/// behaviour. The two booleans are orthogonal:
///
/// - `ignore_errors`: when `true`, non-fatal failures are logged but the
///   shared `io_error` flag is NOT set. Upstream `--ignore-errors` instead
///   keeps `io_error` but stops it from gating deletion
///   (`generator.c:304 delete_in_dir()`) and from reaching the wire
///   (`flist.c:3068 send_file_list()`).
/// - `continue_on_error`: when `true`, non-fatal failures do not abort the
///   drain - the emitter records the error in `io_error` (unless
///   suppressed by `ignore_errors`) and moves on to the next entry. When
///   `false`, the first non-fatal failure also stops the drain.
///
/// The emitter never aborts the pass on a per-entry errno on its own
/// (mirroring upstream `delete.c:141-164`, where the
/// `delete_dir_contents()` loop moves on after a failed `delete_item()`);
/// only `continue_on_error: false`
/// turns the first failure into an aborting error.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct EmitterErrorPolicy {
    /// Suppress the `io_error` flag for non-fatal failures.
    pub ignore_errors: bool,
    /// Keep draining after a non-fatal failure.
    pub continue_on_error: bool,
}

impl Default for EmitterErrorPolicy {
    /// Upstream's default: surface non-fatal errors via `io_error` but
    /// keep going. Matches `delete.c:141-164`: a failed `delete_item()` is
    /// reported and the `delete_dir_contents()` loop continues to the next
    /// entry.
    fn default() -> Self {
        Self {
            ignore_errors: false,
            continue_on_error: true,
        }
    }
}
