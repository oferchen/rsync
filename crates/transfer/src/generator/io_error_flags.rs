//! I/O error flags for file list building and transfer.
//!
//! Bitfield constants OR'd together to track error categories. Propagated to
//! the client summary and mapped to rsync exit codes via [`to_exit_code`].
//!
//! # Upstream Reference
//!
//! - `rsync.h:168-170` - `IOERR_GENERAL`, `IOERR_VANISHED`, `IOERR_DEL_LIMIT`

// The bit definitions and the peer-value mask live in `protocol` because they
// are wire values shared with the file-list and multiplex decoders; re-exported
// here so this module stays the generator's single view of `io_error`.
pub use protocol::{IOERR_DEL_LIMIT, IOERR_GENERAL, IOERR_VALID_MASK, IOERR_VANISHED};

/// Converts an accumulated `io_error` bitfield into the corresponding rsync
/// exit code.
///
/// Mirrors upstream `log.c` - `log_exit()` which maps the io_error flags to
/// `RERR_*` exit codes. Returns 0 when no error bits are set.
///
/// # Exit code mapping
///
/// | Condition | Code | Upstream constant |
/// |-----------|------|-------------------|
/// | `IOERR_DEL_LIMIT` set | 25 | `RERR_DEL_LIMIT` |
/// | `IOERR_VANISHED` set (only) | 24 | `RERR_VANISHED` |
/// | `IOERR_GENERAL` set | 23 | `RERR_PARTIAL` |
/// | No bits set | 0 | success |
#[must_use]
pub const fn to_exit_code(io_error: i32) -> i32 {
    if io_error & IOERR_DEL_LIMIT != 0 {
        25 // RERR_DEL_LIMIT
    } else if io_error & IOERR_GENERAL != 0 {
        23 // RERR_PARTIAL
    } else if io_error & IOERR_VANISHED != 0 {
        24 // RERR_VANISHED
    } else {
        0
    }
}
