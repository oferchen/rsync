//! Numeric constants describing the rsync protocol range supported by this
//! implementation.

use ::core::ops::RangeInclusive;

/// Lowest protocol version supported by upstream rsync 3.4.1.
pub(crate) const OLDEST_SUPPORTED_PROTOCOL: u8 = 28;
/// Newest protocol version supported by upstream rsync 3.4.1.
pub(crate) const NEWEST_SUPPORTED_PROTOCOL: u8 = 32;
/// Protocol revision that introduced the binary negotiation handshake.
pub(crate) const FIRST_BINARY_NEGOTIATION_PROTOCOL: u8 = 30;
/// Highest protocol version upstream rsync 3.4.1 tolerates from a peer advertisement.
///
/// Mirrors `MAX_PROTOCOL_VERSION` from `rsync.h` so future protocol announcements that fall
/// within upstream's guard range are accepted and clamped to the newest supported revision.
pub const MAXIMUM_PROTOCOL_ADVERTISEMENT: u8 = 40;

/// Inclusive range of protocol versions that upstream rsync 3.4.1 understands.
pub(crate) const UPSTREAM_PROTOCOL_RANGE: RangeInclusive<u8> =
    OLDEST_SUPPORTED_PROTOCOL..=NEWEST_SUPPORTED_PROTOCOL;

/// Inclusive range of protocol versions supported by the Rust implementation.
///
/// The value stays in sync with [`super::ProtocolVersion::OLDEST`] and
/// [`super::ProtocolVersion::NEWEST`]. Compile-time guards in the surrounding
/// module assert the invariants when these constants drift.
pub const SUPPORTED_PROTOCOL_RANGE: RangeInclusive<u8> =
    OLDEST_SUPPORTED_PROTOCOL..=NEWEST_SUPPORTED_PROTOCOL;

/// Inclusive `(oldest, newest)` tuple describing the supported protocol span.
///
/// Diagnostics frequently surface the bounds explicitly. Publishing the tuple
/// keeps call sites aligned with [`SUPPORTED_PROTOCOL_RANGE`] without
/// duplicating literals.
pub const SUPPORTED_PROTOCOL_BOUNDS: (u8, u8) =
    (OLDEST_SUPPORTED_PROTOCOL, NEWEST_SUPPORTED_PROTOCOL);

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for protocol version constants
    #[test]
    fn oldest_supported_protocol_is_28() {
        assert_eq!(OLDEST_SUPPORTED_PROTOCOL, 28);
    }

    #[test]
    fn newest_supported_protocol_is_32() {
        assert_eq!(NEWEST_SUPPORTED_PROTOCOL, 32);
    }

    #[test]
    fn first_binary_negotiation_protocol_is_30() {
        assert_eq!(FIRST_BINARY_NEGOTIATION_PROTOCOL, 30);
    }

    #[test]
    fn maximum_protocol_advertisement_is_40() {
        assert_eq!(MAXIMUM_PROTOCOL_ADVERTISEMENT, 40);
    }

    // Tests for range consistency
    #[test]
    fn oldest_is_less_than_newest() {
        assert!(OLDEST_SUPPORTED_PROTOCOL < NEWEST_SUPPORTED_PROTOCOL);
    }

    #[test]
    fn binary_negotiation_is_within_supported_range() {
        assert!(FIRST_BINARY_NEGOTIATION_PROTOCOL >= OLDEST_SUPPORTED_PROTOCOL);
        assert!(FIRST_BINARY_NEGOTIATION_PROTOCOL <= NEWEST_SUPPORTED_PROTOCOL);
    }

    #[test]
    fn maximum_advertisement_is_above_newest() {
        assert!(MAXIMUM_PROTOCOL_ADVERTISEMENT > NEWEST_SUPPORTED_PROTOCOL);
    }

    // Tests for upstream_protocol_range
    #[test]
    fn upstream_protocol_range_start_is_oldest() {
        assert_eq!(*UPSTREAM_PROTOCOL_RANGE.start(), OLDEST_SUPPORTED_PROTOCOL);
    }

    #[test]
    fn upstream_protocol_range_end_is_newest() {
        assert_eq!(*UPSTREAM_PROTOCOL_RANGE.end(), NEWEST_SUPPORTED_PROTOCOL);
    }

    #[test]
    fn upstream_protocol_range_contains_binary_negotiation() {
        assert!(UPSTREAM_PROTOCOL_RANGE.contains(&FIRST_BINARY_NEGOTIATION_PROTOCOL));
    }

    // Tests for supported_protocol_range
    #[test]
    fn supported_protocol_range_start_is_oldest() {
        assert_eq!(*SUPPORTED_PROTOCOL_RANGE.start(), OLDEST_SUPPORTED_PROTOCOL);
    }

    #[test]
    fn supported_protocol_range_end_is_newest() {
        assert_eq!(*SUPPORTED_PROTOCOL_RANGE.end(), NEWEST_SUPPORTED_PROTOCOL);
    }

    #[test]
    fn supported_protocol_range_matches_upstream() {
        assert_eq!(SUPPORTED_PROTOCOL_RANGE, UPSTREAM_PROTOCOL_RANGE);
    }

    // Tests for supported_protocol_bounds
    #[test]
    fn supported_protocol_bounds_oldest() {
        assert_eq!(SUPPORTED_PROTOCOL_BOUNDS.0, OLDEST_SUPPORTED_PROTOCOL);
    }

    #[test]
    fn supported_protocol_bounds_newest() {
        assert_eq!(SUPPORTED_PROTOCOL_BOUNDS.1, NEWEST_SUPPORTED_PROTOCOL);
    }

    #[test]
    fn supported_protocol_bounds_matches_range() {
        assert_eq!(SUPPORTED_PROTOCOL_BOUNDS.0, *SUPPORTED_PROTOCOL_RANGE.start());
        assert_eq!(SUPPORTED_PROTOCOL_BOUNDS.1, *SUPPORTED_PROTOCOL_RANGE.end());
    }
}
