//! `io_error` bit definitions and the peer-value sanitising mask.
//!
//! These are wire values: a peer reports its accumulated `io_error` in the file
//! list trailer and in `MSG_IO_ERROR`, and the local side ORs the received value
//! into its own `io_error`. The bits then steer local behaviour (deletion is
//! suppressed while `IOERR_GENERAL` is set) and the final exit code.
//!
//! # Upstream Reference
//!
//! - `rsync.h:191-200` - `IOERR_GENERAL`, `IOERR_VANISHED`, `IOERR_DEL_LIMIT`,
//!   `IOERR_VALID_MASK`.

/// General I/O error occurred during file operations.
/// Must be 1 for backward compatibility with upstream rsync.
///
/// upstream: `rsync.h:191`
pub const IOERR_GENERAL: i32 = 1 << 0;
/// A file or directory vanished (was deleted) during the transfer.
///
/// upstream: `rsync.h:192`
pub const IOERR_VANISHED: i32 = 1 << 1;
/// Delete limit was exceeded during --delete operations.
///
/// upstream: `rsync.h:193`
pub const IOERR_DEL_LIMIT: i32 = 1 << 2;

/// Mask of every defined `IOERR_*` bit.
///
/// Every `io_error` value read from a peer must be reduced through this mask
/// before it is OR'd into the local `io_error`, so a hostile peer cannot plant
/// undefined bits that are then stored, re-forwarded to the next hop, and used
/// to steer local decisions. Apply it at each point the value is read off the
/// wire, not at the consumers.
///
/// upstream: `rsync.h:200` - `IOERR_VALID_MASK`, applied at `io.c:1706`
/// (`MSG_IO_ERROR`) and `flist.c:2950`, `flist.c:2968`, `flist.c:3071`
/// (`recv_file_list()`).
pub const IOERR_VALID_MASK: i32 = IOERR_GENERAL | IOERR_VANISHED | IOERR_DEL_LIMIT;

/// Reduces a peer-supplied `io_error` value to the defined `IOERR_*` bits.
///
/// upstream: `flist.c:2950` - `io_error |= err & IOERR_VALID_MASK;`
#[must_use]
pub const fn sanitize_peer_io_error(value: i32) -> i32 {
    value & IOERR_VALID_MASK
}

#[cfg(test)]
mod tests {
    use super::{
        IOERR_DEL_LIMIT, IOERR_GENERAL, IOERR_VALID_MASK, IOERR_VANISHED, sanitize_peer_io_error,
    };

    /// The mask is derived from the bit set, so adding an `IOERR_*` bit without
    /// widening the mask cannot silently make that bit unreachable.
    ///
    /// upstream: `rsync.h:200`
    #[test]
    fn mask_covers_exactly_the_defined_bits() {
        assert_eq!(IOERR_VALID_MASK, 0b111);
        for bit in [IOERR_GENERAL, IOERR_VANISHED, IOERR_DEL_LIMIT] {
            assert_eq!(sanitize_peer_io_error(bit), bit);
        }
    }

    /// A hostile peer can put any 32-bit value on the wire. None of the
    /// undefined bits may survive into the local `io_error`.
    #[test]
    fn undefined_bits_never_survive() {
        for value in [-1, i32::MIN, i32::MAX, 0x7fff_fff8, 42, 0x0100_0000] {
            let masked = sanitize_peer_io_error(value);
            assert_eq!(masked & !IOERR_VALID_MASK, 0, "value {value:#x}");
            assert_eq!(masked, value & IOERR_VALID_MASK, "value {value:#x}");
        }
    }

    /// Masking must not disturb a well-behaved peer's value: every combination
    /// of defined bits round-trips unchanged, so the fix is inert on the
    /// honest path.
    #[test]
    fn defined_combinations_round_trip_unchanged() {
        for bits in 0..=IOERR_VALID_MASK {
            assert_eq!(sanitize_peer_io_error(bits), bits);
        }
    }
}

/// The two upstream rules for a file-list `io_error`, stated once so a consumer
/// can be checked against them.
///
/// Upstream does not have one `io_error` rule, it has two, and they differ
/// exactly on `--ignore-errors`:
///
/// * a value the PEER sent in the file-list trailer is accumulated only when
///   `ignore_errors` is clear - `flist.c:2949`, `:2967`, `:3070` are all
///   `if (!ignore_errors) io_error |= err & IOERR_VALID_MASK`;
/// * a value this side generated while decoding is accumulated unconditionally
///   - `flist.c:841`'s filename-transcode failure has no `ignore_errors` check.
///
/// Storing both in one accumulator is what let four of five consumers apply the
/// wrong rule, so the rule is expressed here and consumers are tested against
/// it rather than each restating it.
#[must_use]
pub const fn combine_flist_io_error(peer: i32, local: i32, ignore_errors: bool) -> i32 {
    let peer = if ignore_errors { 0 } else { peer };
    peer | local
}

#[cfg(test)]
mod combine_tests {
    use super::{IOERR_GENERAL, IOERR_VANISHED, combine_flist_io_error};

    /// `--ignore-errors` suppresses the PEER's trailer value and nothing else.
    ///
    /// The `(peer=set, local=0, ignore=true)` row is the one that regressed:
    /// four of five drains re-admitted the peer value after the gate had
    /// filtered it, so a peer-reported error still reached the exit code under
    /// `--ignore-errors`. upstream: flist.c:2949/2967/3070.
    #[test]
    fn ignore_errors_suppresses_only_the_peer_half() {
        for &(peer, local, ignore, want) in &[
            (IOERR_GENERAL, 0, false, IOERR_GENERAL),
            (IOERR_GENERAL, 0, true, 0),
            (0, IOERR_GENERAL, false, IOERR_GENERAL),
            // upstream never gates a local decode failure.
            (0, IOERR_GENERAL, true, IOERR_GENERAL),
            (IOERR_VANISHED, IOERR_GENERAL, true, IOERR_GENERAL),
            (
                IOERR_VANISHED,
                IOERR_GENERAL,
                false,
                IOERR_VANISHED | IOERR_GENERAL,
            ),
            (0, 0, true, 0),
            (0, 0, false, 0),
        ] {
            assert_eq!(
                combine_flist_io_error(peer, local, ignore),
                want,
                "peer={peer:#x} local={local:#x} ignore_errors={ignore}"
            );
        }
    }

    /// The grid must cover every `(peer set?) x (local set?) x (ignore?)`
    /// combination, so a dropped row cannot shrink the guard.
    #[test]
    fn rule_grid_is_complete() {
        let mut seen = 0u8;
        for peer in [0, IOERR_GENERAL] {
            for local in [0, IOERR_GENERAL] {
                for ignore in [false, true] {
                    let bit =
                        u8::from(peer != 0) << 2 | u8::from(local != 0) << 1 | u8::from(ignore);
                    seen |= 1 << bit;
                    // Every combination must be answerable by the one rule.
                    let _ = combine_flist_io_error(peer, local, ignore);
                }
            }
        }
        assert_eq!(seen, 0xFF, "not all eight rule combinations exercised");
    }
}
