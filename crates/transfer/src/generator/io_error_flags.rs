//! I/O error flags for file list building and transfer.
//!
//! Bitfield constants OR'd together to track error categories. Propagated to
//! the client summary and mapped to rsync exit codes via [`to_exit_code`].
//!
//! # Upstream Reference
//!
//! - `rsync.h:191-193` - `IOERR_GENERAL`, `IOERR_VANISHED`, `IOERR_DEL_LIMIT`

// The bit definitions and the peer-value mask live in `protocol` because they
// are wire values shared with the file-list and multiplex decoders; re-exported
// here so this module stays the generator's single view of `io_error`.
pub use protocol::{IOERR_DEL_LIMIT, IOERR_GENERAL, IOERR_VALID_MASK, IOERR_VANISHED};

/// Converts an accumulated `io_error` bitfield into the corresponding rsync
/// exit code. Returns 0 when no error bits are set.
///
/// # Precedence
///
/// Upstream evaluates three INDEPENDENT `if`s - not an `if`/`else if` chain -
/// and lets the last one that matches overwrite `exit_code`. Precedence is
/// therefore **GENERAL > VANISHED > DEL_LIMIT**, the reverse of the source
/// order. The assignment form below mirrors that structure directly so the
/// precedence cannot be misread as source order:
///
/// ```c
/// if (io_error & IOERR_DEL_LIMIT) exit_code = RERR_DEL_LIMIT;
/// if (io_error & IOERR_VANISHED)  exit_code = RERR_VANISHED;
/// if (io_error & IOERR_GENERAL || got_xfer_error) exit_code = RERR_PARTIAL;
/// ```
///
/// A `--max-delete` stop that also hit a permission-denied file is a *partial
/// transfer* (23), not a delete-limit stop (25): the caller needs to know data
/// was missed, which the delete limit alone does not imply.
///
/// `got_xfer_error` is not an `io_error` bit and is applied by the caller.
///
/// # Exit code mapping
///
/// | Highest-precedence bit set | Code | Upstream constant |
/// |----------------------------|------|-------------------|
/// | `IOERR_GENERAL` | 23 | `RERR_PARTIAL` |
/// | `IOERR_VANISHED` | 24 | `RERR_VANISHED` |
/// | `IOERR_DEL_LIMIT` | 25 | `RERR_DEL_LIMIT` |
/// | none | 0 | success |
///
/// # Upstream Reference
///
/// - `cleanup.c:210-218` - the three sequential assignments. Note the rule
///   lives here and NOT in `log.c:log_exit()`, which does no io_error mapping.
#[must_use]
pub const fn to_exit_code(io_error: i32) -> i32 {
    let mut exit_code = 0;
    if io_error & IOERR_DEL_LIMIT != 0 {
        exit_code = 25; // RERR_DEL_LIMIT
    }
    if io_error & IOERR_VANISHED != 0 {
        exit_code = 24; // RERR_VANISHED
    }
    if io_error & IOERR_GENERAL != 0 {
        exit_code = 23; // RERR_PARTIAL
    }
    exit_code
}

#[cfg(test)]
mod tests {
    use super::{IOERR_DEL_LIMIT, IOERR_GENERAL, IOERR_VANISHED, to_exit_code};

    /// The full truth table. Written out rather than derived so a future edit
    /// to the precedence has to change these numbers deliberately.
    ///
    /// upstream: `cleanup.c:210-218`.
    #[test]
    fn every_flag_combination_matches_upstream_precedence() {
        for (bits, expected) in [
            (0, 0),
            (IOERR_DEL_LIMIT, 25),
            (IOERR_VANISHED, 24),
            (IOERR_GENERAL, 23),
            (IOERR_DEL_LIMIT | IOERR_VANISHED, 24),
            (IOERR_DEL_LIMIT | IOERR_GENERAL, 23),
            (IOERR_VANISHED | IOERR_GENERAL, 23),
            (IOERR_DEL_LIMIT | IOERR_VANISHED | IOERR_GENERAL, 23),
        ] {
            assert_eq!(to_exit_code(bits), expected, "io_error bits {bits:#b}");
        }
    }

    /// The case that regressed: an `else if` chain lets DEL_LIMIT short-circuit
    /// and report 25, hiding the fact that data was actually missed. CI scripts
    /// branch on 23 to detect a partial transfer.
    ///
    /// upstream: `cleanup.c:216-217` - the GENERAL assignment runs last and
    /// overwrites the DEL_LIMIT one.
    #[test]
    fn general_error_outranks_delete_limit() {
        assert_eq!(to_exit_code(IOERR_DEL_LIMIT | IOERR_GENERAL), 23);
    }

    /// Sibling of the above: VANISHED also outranks DEL_LIMIT.
    ///
    /// upstream: `cleanup.c:214-215`.
    #[test]
    fn vanished_outranks_delete_limit() {
        assert_eq!(to_exit_code(IOERR_DEL_LIMIT | IOERR_VANISHED), 24);
    }
}
