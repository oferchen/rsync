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
