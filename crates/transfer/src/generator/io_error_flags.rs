//! I/O error flags for file list building and transfer.
//!
//! Bitfield constants OR'd together to track error categories. Propagated to
//! the client summary and mapped to rsync exit codes via [`to_exit_code`].
//!
//! # Upstream Reference
//!
//! - `rsync.h:168-170` - `IOERR_GENERAL`, `IOERR_VANISHED`, `IOERR_DEL_LIMIT`

/// General I/O error occurred during file operations.
/// Must be 1 for backward compatibility with upstream rsync.
pub const IOERR_GENERAL: i32 = 1 << 0;
/// A file or directory vanished (was deleted) during the transfer.
pub const IOERR_VANISHED: i32 = 1 << 1;
/// Delete limit was exceeded during --delete operations.
pub const IOERR_DEL_LIMIT: i32 = 1 << 2;

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
