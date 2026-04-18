//! Strongly typed rsync protocol version representation and helpers.

mod capabilities;
mod conversions;

#[cfg(test)]
mod tests;

use ::core::num::NonZeroU8;
use ::core::ops::RangeInclusive;

use crate::error::NegotiationError;

use super::constants::{
    FIRST_BINARY_NEGOTIATION_PROTOCOL, MAXIMUM_PROTOCOL_ADVERTISEMENT, NEWEST_SUPPORTED_PROTOCOL,
    OLDEST_SUPPORTED_PROTOCOL, SUPPORTED_PROTOCOL_BOUNDS, SUPPORTED_PROTOCOL_RANGE,
};
use super::iter::{SupportedProtocolNumbersIter, SupportedVersionsIter};

/// A single negotiated rsync protocol version.
///
/// This type wraps a non-zero byte that identifies which revision of the rsync
/// wire protocol a session has agreed to use. The supported range is
/// [`V28`](Self::V28) through [`V32`](Self::V32), matching upstream rsync
/// 3.4.1. Protocol version 30 marks the boundary between the legacy ASCII
/// negotiation (`@RSYNCD:`) and the modern binary handshake.
///
/// # Constructing a Version
///
/// Use the named constants for well-known versions, [`from_supported`](Self::from_supported)
/// for runtime validation, or parse from a string via [`FromStr`](core::str::FromStr):
///
/// ```
/// use protocol::ProtocolVersion;
///
/// // Named constant
/// let v32 = ProtocolVersion::V32;
/// assert_eq!(v32.as_u8(), 32);
///
/// // Runtime validation
/// let v30 = ProtocolVersion::from_supported(30).expect("30 is supported");
/// assert!(v30.uses_binary_negotiation());
///
/// // String parsing (e.g. from CLI --protocol flag)
/// let parsed: ProtocolVersion = "31".parse().expect("valid version");
/// assert_eq!(parsed, ProtocolVersion::V31);
/// ```
///
/// # Feature Queries
///
/// The type exposes semantic predicates so callers can check protocol
/// capabilities without scattering magic-number comparisons:
///
/// ```
/// use protocol::ProtocolVersion;
///
/// let v = ProtocolVersion::V29;
/// assert!(v.uses_legacy_ascii_negotiation());
/// assert!(!v.uses_varint_encoding());
/// assert!(v.supports_flist_times());
/// ```
#[doc(alias = "--protocol")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct ProtocolVersion(NonZeroU8);

// Supported protocol declarations (macro-generated constants)

macro_rules! declare_supported_protocols {
    ($($ver:literal),+ $(,)?) => {
        /// Number of protocol versions supported by the Rust implementation.
        pub const SUPPORTED_PROTOCOL_COUNT: usize =
            declare_supported_protocols!(@len $($ver),+);

        /// Protocol versions supported by the Rust implementation, ordered from
        /// newest to oldest.
        pub const SUPPORTED_PROTOCOLS: [u8; SUPPORTED_PROTOCOL_COUNT] = [
            $($ver),+
        ];

        /// Strongly typed cache of supported protocol versions.
        const SUPPORTED_PROTOCOL_VERSIONS: [ProtocolVersion; SUPPORTED_PROTOCOL_COUNT] = [
            $(ProtocolVersion::new_const($ver)),+
        ];

        /// Comma-separated list of supported protocol versions ordered from
        /// newest to oldest.
        #[doc(alias = "--protocol")]
        pub const SUPPORTED_PROTOCOLS_DISPLAY: &str =
            declare_supported_protocols!(@stringify $($ver),+);
    };
    (@len $($ver:literal),+) => {
        <[()]>::len(&[$(declare_supported_protocols!(@unit $ver)),+])
    };
    (@unit $ver:literal) => { () };
    (@stringify $first:literal $(,$rest:literal)*) => {
        concat!(stringify!($first) $(, ", ", stringify!($rest))* )
    };
}

declare_supported_protocols!(32, 31, 30, 29, 28);

/// Bitmask describing the protocol versions supported by the Rust
/// implementation.
pub const SUPPORTED_PROTOCOL_BITMAP: u64 = {
    let mut bitmap = 0u64;
    let mut index = 0usize;

    while index < SUPPORTED_PROTOCOL_COUNT {
        let protocol = SUPPORTED_PROTOCOLS[index];
        bitmap |= 1u64 << protocol;
        index += 1;
    }

    bitmap
};

// Core impl block: constructors, accessors, supported-protocol queries

impl ProtocolVersion {
    pub(crate) const fn new_const(value: u8) -> Self {
        match NonZeroU8::new(value) {
            Some(v) => Self(v),
            None => panic!("protocol version must be non-zero"),
        }
    }

    /// The newest protocol version supported by upstream rsync 3.4.1.
    pub const NEWEST: ProtocolVersion = ProtocolVersion::new_const(NEWEST_SUPPORTED_PROTOCOL);

    /// The oldest protocol version supported by upstream rsync 3.4.1.
    pub const OLDEST: ProtocolVersion = ProtocolVersion::new_const(OLDEST_SUPPORTED_PROTOCOL);

    /// Protocol version at which rsync switched from the legacy ASCII
    /// negotiation to the binary handshake.
    pub const BINARY_NEGOTIATION_INTRODUCED: ProtocolVersion =
        ProtocolVersion::new_const(FIRST_BINARY_NEGOTIATION_PROTOCOL);

    /// Protocol version 32, the newest revision advertised by upstream rsync
    /// 3.4.1.
    pub const V32: ProtocolVersion = ProtocolVersion::NEWEST;

    /// Protocol version 31, used by upstream rsync 3.1.x releases.
    pub const V31: ProtocolVersion = ProtocolVersion::new_const(31);

    /// Protocol version 30, the first release that adopted the binary
    /// negotiation handshake.
    pub const V30: ProtocolVersion = ProtocolVersion::new_const(30);

    /// Protocol version 29, the newest legacy `@RSYNCD:` ASCII negotiation
    /// revision.
    pub const V29: ProtocolVersion = ProtocolVersion::new_const(29);

    /// Protocol version 28, the oldest revision still supported for
    /// interoperability.
    pub const V28: ProtocolVersion = ProtocolVersion::OLDEST;

    /// Array of protocol versions supported by the Rust implementation,
    /// ordered from newest to oldest.
    pub const SUPPORTED_VERSIONS: [ProtocolVersion; SUPPORTED_PROTOCOL_COUNT] =
        SUPPORTED_PROTOCOL_VERSIONS;

    /// Returns a reference to the list of supported protocol versions in
    /// newest-to-oldest order.
    #[must_use]
    pub const fn supported_versions() -> &'static [ProtocolVersion] {
        &Self::SUPPORTED_VERSIONS
    }

    /// Returns the cached list of supported protocol versions as a fixed-size
    /// array reference.
    #[must_use]
    pub const fn supported_versions_array() -> &'static [ProtocolVersion; SUPPORTED_PROTOCOL_COUNT]
    {
        &Self::SUPPORTED_VERSIONS
    }

    /// Reports whether the provided numeric protocol identifier is supported
    /// by this implementation.
    #[must_use]
    pub const fn is_supported_protocol_number(value: u8) -> bool {
        if value < Self::OLDEST.as_u8() || value > Self::NEWEST.as_u8() {
            return false;
        }

        if value as u32 >= u64::BITS {
            return false;
        }

        (SUPPORTED_PROTOCOL_BITMAP & (1u64 << value)) != 0
    }

    /// Returns the numeric protocol identifiers supported by this
    /// implementation in newest-to-oldest order.
    #[must_use]
    pub const fn supported_protocol_numbers() -> &'static [u8] {
        &SUPPORTED_PROTOCOLS
    }

    /// Returns the numeric protocol identifiers as a fixed-size array
    /// reference.
    #[must_use]
    pub const fn supported_protocol_numbers_array() -> &'static [u8; SUPPORTED_PROTOCOL_COUNT] {
        &SUPPORTED_PROTOCOLS
    }

    /// Returns a bitmap describing the protocol versions supported by this
    /// implementation.
    #[must_use]
    pub const fn supported_protocol_bitmap() -> u64 {
        SUPPORTED_PROTOCOL_BITMAP
    }

    /// Returns an iterator over the numeric protocol identifiers supported by
    /// this implementation.
    #[must_use = "consume the iterator to inspect the supported protocol numbers"]
    pub const fn supported_protocol_numbers_iter() -> SupportedProtocolNumbersIter {
        SupportedProtocolNumbersIter::new(Self::supported_protocol_numbers())
    }

    /// Returns the comma-separated list of supported protocol versions.
    #[doc(alias = "--protocol")]
    #[must_use]
    pub const fn supported_protocol_numbers_display() -> &'static str {
        SUPPORTED_PROTOCOLS_DISPLAY
    }

    /// Returns the inclusive range of protocol versions supported by this
    /// implementation.
    #[must_use]
    pub const fn supported_range() -> RangeInclusive<u8> {
        SUPPORTED_PROTOCOL_RANGE
    }

    /// Returns the inclusive supported range as a tuple of `(oldest, newest)`.
    #[must_use]
    pub const fn supported_range_bounds() -> (u8, u8) {
        SUPPORTED_PROTOCOL_BOUNDS
    }

    /// Returns the oldest and newest supported protocol versions as strongly
    /// typed values.
    #[must_use]
    pub const fn supported_version_bounds() -> (ProtocolVersion, ProtocolVersion) {
        (Self::OLDEST, Self::NEWEST)
    }

    /// Returns the inclusive range of supported protocol versions using
    /// strongly typed values.
    #[must_use]
    pub const fn supported_version_range() -> RangeInclusive<ProtocolVersion> {
        Self::OLDEST..=Self::NEWEST
    }

    /// Returns an iterator over the supported protocol versions in
    /// newest-to-oldest order.
    #[must_use = "consume the iterator to inspect the supported protocol versions"]
    pub const fn supported_versions_iter() -> SupportedVersionsIter {
        SupportedVersionsIter::new(Self::supported_versions())
    }

    /// Returns the protocol version at the given index within the canonical
    /// newest-to-oldest list.
    pub const fn from_supported_index(index: usize) -> Option<Self> {
        if index < SUPPORTED_PROTOCOL_COUNT {
            Some(Self::SUPPORTED_VERSIONS[index])
        } else {
            None
        }
    }

    /// Attempts to construct a [`ProtocolVersion`] from a byte known to fall
    /// inside the supported range.
    pub const fn from_supported(value: u8) -> Option<Self> {
        if Self::is_supported_protocol_number(value) {
            Some(Self::new_const(value))
        } else {
            None
        }
    }

    /// Reports whether the provided version is supported by this
    /// implementation.
    #[must_use]
    #[inline]
    pub const fn is_supported(value: u8) -> bool {
        Self::is_supported_protocol_number(value)
    }

    /// Returns the zero-based offset from [`ProtocolVersion::OLDEST`] when
    /// iterating protocol versions in ascending order.
    #[must_use]
    #[inline]
    pub const fn offset_from_oldest(self) -> usize {
        (self.as_u8() - Self::OLDEST.as_u8()) as usize
    }

    /// Constructs a [`ProtocolVersion`] from its zero-based offset relative to
    /// [`ProtocolVersion::OLDEST`].
    pub const fn from_oldest_offset(offset: usize) -> Option<Self> {
        let oldest = Self::OLDEST.as_u8() as usize;
        let newest = Self::NEWEST.as_u8() as usize;

        match oldest.checked_add(offset) {
            Some(value) if value <= newest => Some(Self::new_const(value as u8)),
            _ => None,
        }
    }

    /// Returns the zero-based offset from [`ProtocolVersion::NEWEST`] when
    /// iterating protocol versions in descending order.
    #[must_use]
    #[inline]
    pub const fn offset_from_newest(self) -> usize {
        (Self::NEWEST.as_u8() - self.as_u8()) as usize
    }

    /// Constructs a [`ProtocolVersion`] from its zero-based offset relative to
    /// [`ProtocolVersion::NEWEST`].
    pub const fn from_newest_offset(offset: usize) -> Option<Self> {
        let newest = Self::NEWEST.as_u8() as usize;
        let oldest = Self::OLDEST.as_u8() as usize;
        let span = newest - oldest;

        if offset > span {
            return None;
        }

        Some(Self::new_const((newest - offset) as u8))
    }

    /// Returns the next newer protocol version within the supported range, if
    /// any.
    pub const fn next_newer(self) -> Option<Self> {
        if self.as_u8() >= Self::NEWEST.as_u8() {
            None
        } else {
            Some(Self::new_const(self.as_u8() + 1))
        }
    }

    /// Returns the next older protocol version within the supported range, if
    /// any.
    pub const fn next_older(self) -> Option<Self> {
        if self.as_u8() <= Self::OLDEST.as_u8() {
            None
        } else {
            Some(Self::new_const(self.as_u8() - 1))
        }
    }

    /// Returns the raw numeric value represented by this version.
    #[must_use]
    #[inline]
    pub const fn as_u8(self) -> u8 {
        self.0.get()
    }

    /// Returns the numeric protocol value as a [`usize`].
    #[must_use]
    #[inline]
    pub const fn as_usize(self) -> usize {
        self.as_u8() as usize
    }

    /// Returns the non-zero byte representation used in protocol negotiation.
    #[must_use]
    #[inline]
    pub const fn as_non_zero(self) -> NonZeroU8 {
        self.0
    }

    /// Converts a peer-advertised version into the negotiated protocol
    /// version.
    #[inline]
    pub fn from_peer_advertisement(value: u32) -> Result<Self, NegotiationError> {
        if value == 0 {
            return Err(NegotiationError::UnsupportedVersion(value));
        }

        if value > u32::from(MAXIMUM_PROTOCOL_ADVERTISEMENT) {
            return Err(NegotiationError::UnsupportedVersion(value));
        }

        let clamped = if value > u32::from(Self::NEWEST.as_u8()) {
            Self::NEWEST.as_u8()
        } else {
            value as u8
        };

        if clamped < Self::OLDEST.as_u8() {
            return Err(NegotiationError::UnsupportedVersion(value));
        }

        match NonZeroU8::new(clamped) {
            Some(non_zero) => Ok(Self(non_zero)),
            None => Err(NegotiationError::UnsupportedVersion(value)),
        }
    }
}
