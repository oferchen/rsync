//! Strongly typed rsync protocol version representation and helpers.

use ::core::convert::TryFrom;
use ::core::fmt;
use ::core::num::{
    NonZeroI8, NonZeroI16, NonZeroI32, NonZeroI64, NonZeroI128, NonZeroIsize, NonZeroU8,
    NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU128, NonZeroUsize,
};
use ::core::ops::RangeInclusive;
use ::core::str::FromStr;

use crate::error::NegotiationError;

use super::constants::{
    FIRST_BINARY_NEGOTIATION_PROTOCOL, MAXIMUM_PROTOCOL_ADVERTISEMENT, NEWEST_SUPPORTED_PROTOCOL,
    OLDEST_SUPPORTED_PROTOCOL, SUPPORTED_PROTOCOL_BOUNDS, SUPPORTED_PROTOCOL_RANGE,
    UPSTREAM_PROTOCOL_RANGE,
};
use super::iter::{SupportedProtocolNumbersIter, SupportedVersionsIter};
use super::parse::{ParseProtocolVersionError, ParseProtocolVersionErrorKind};

/// A single negotiated rsync protocol version.
#[doc(alias = "--protocol")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct ProtocolVersion(NonZeroU8);

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

/// Bitmask describing the protocol versions supported by the Rust implementation.
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

    /// Protocol version at which rsync switched from the legacy ASCII negotiation to the binary handshake.
    pub const BINARY_NEGOTIATION_INTRODUCED: ProtocolVersion =
        ProtocolVersion::new_const(FIRST_BINARY_NEGOTIATION_PROTOCOL);

    /// Protocol version 32, the newest revision advertised by upstream rsync 3.4.1.
    pub const V32: ProtocolVersion = ProtocolVersion::NEWEST;

    /// Protocol version 31, used by upstream rsync 3.1.x releases.
    pub const V31: ProtocolVersion = ProtocolVersion::new_const(31);

    /// Protocol version 30, the first release that adopted the binary negotiation handshake.
    pub const V30: ProtocolVersion = ProtocolVersion::new_const(30);

    /// Protocol version 29, the newest legacy `@RSYNCD:` ASCII negotiation revision.
    pub const V29: ProtocolVersion = ProtocolVersion::new_const(29);

    /// Protocol version 28, the oldest revision still supported for interoperability.
    pub const V28: ProtocolVersion = ProtocolVersion::OLDEST;

    /// Array of protocol versions supported by the Rust implementation, ordered from newest to oldest.
    pub const SUPPORTED_VERSIONS: [ProtocolVersion; SUPPORTED_PROTOCOL_COUNT] =
        SUPPORTED_PROTOCOL_VERSIONS;

    /// Returns a reference to the list of supported protocol versions in newest-to-oldest order.
    #[must_use]
    pub const fn supported_versions() -> &'static [ProtocolVersion] {
        &Self::SUPPORTED_VERSIONS
    }

    /// Returns the cached list of supported protocol versions as a fixed-size array reference.
    #[must_use]
    pub const fn supported_versions_array() -> &'static [ProtocolVersion; SUPPORTED_PROTOCOL_COUNT]
    {
        &Self::SUPPORTED_VERSIONS
    }

    /// Reports whether the provided numeric protocol identifier is supported by this implementation.
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

    /// Reports whether sessions negotiated at this protocol version use the binary framing introduced in protocol 30.
    #[must_use]
    pub const fn uses_binary_negotiation(self) -> bool {
        self.as_u8() >= Self::BINARY_NEGOTIATION_INTRODUCED.as_u8()
    }

    /// Reports whether this protocol version still relies on the legacy ASCII daemon negotiation.
    #[must_use]
    pub const fn uses_legacy_ascii_negotiation(self) -> bool {
        self.as_u8() < Self::BINARY_NEGOTIATION_INTRODUCED.as_u8()
    }

    // ========================================================================
    // Feature flag methods
    // ========================================================================
    // These methods provide semantic names for protocol version checks,
    // eliminating the need for scattered magic number comparisons like
    // `protocol.as_u8() >= 29`. They mirror the ProtocolCodec trait methods.

    /// Returns `true` if this protocol version uses variable-length integer encoding.
    ///
    /// - Protocol < 30: Uses fixed-size integers (4-byte, longint)
    /// - Protocol >= 30: Uses varint/varlong encoding
    ///
    /// This is the primary encoding boundary in the rsync protocol.
    #[must_use]
    pub const fn uses_varint_encoding(self) -> bool {
        self.as_u8() >= 30
    }

    /// Returns `true` if this protocol version uses legacy fixed-size encoding.
    ///
    /// Inverse of [`uses_varint_encoding`](Self::uses_varint_encoding).
    #[must_use]
    pub const fn uses_fixed_encoding(self) -> bool {
        self.as_u8() < 30
    }

    /// Returns `true` if this protocol version supports sender/receiver side modifiers (`s`, `r`).
    ///
    /// - Protocol < 29: Returns `false`
    /// - Protocol >= 29: Returns `true`
    ///
    /// # Upstream Reference
    ///
    /// `exclude.c:1567-1571` - Sender/receiver modifier support gated by protocol >= 29
    #[must_use]
    pub const fn supports_sender_receiver_modifiers(self) -> bool {
        self.as_u8() >= 29
    }

    /// Returns `true` if this protocol version supports the perishable modifier (`p`).
    ///
    /// - Protocol < 30: Returns `false`
    /// - Protocol >= 30: Returns `true`
    ///
    /// # Upstream Reference
    ///
    /// `exclude.c:1350` - `protocol_version >= 30 ? FILTRULE_PERISHABLE : 0`
    #[must_use]
    pub const fn supports_perishable_modifier(self) -> bool {
        self.as_u8() >= 30
    }

    /// Returns `true` if this protocol version uses old-style prefixes (protocol < 29).
    ///
    /// Old prefixes have restricted modifier support and different parsing rules.
    ///
    /// # Upstream Reference
    ///
    /// `exclude.c:1675` - `xflags = protocol_version >= 29 ? 0 : XFLG_OLD_PREFIXES`
    #[must_use]
    pub const fn uses_old_prefixes(self) -> bool {
        self.as_u8() < 29
    }

    /// Returns `true` if this protocol version supports file list timing statistics.
    ///
    /// - Protocol < 29: Returns `false` (no flist_buildtime/flist_xfertime)
    /// - Protocol >= 29: Returns `true`
    ///
    /// # Upstream Reference
    ///
    /// `main.c` - handle_stats() sends flist times only for protocol >= 29
    #[must_use]
    pub const fn supports_flist_times(self) -> bool {
        self.as_u8() >= 29
    }

    /// Returns `true` if this protocol version supports extended file flags.
    ///
    /// - Protocol < 28: Returns `false`
    /// - Protocol >= 28: Returns `true`
    ///
    /// Extended flags allow for more file attributes to be transmitted.
    #[must_use]
    pub const fn supports_extended_flags(self) -> bool {
        self.as_u8() >= 28
    }

    /// Returns `true` if this protocol version uses varint-encoded file list flags.
    ///
    /// - Protocol < 30: Uses 1-2 byte fixed flags
    /// - Protocol >= 30: Uses varint-encoded flags with COMPAT_VARINT_FLIST_FLAGS
    #[must_use]
    pub const fn uses_varint_flist_flags(self) -> bool {
        self.as_u8() >= 30
    }

    /// Returns `true` if this protocol version supports the safe file list mode.
    ///
    /// - Protocol < 30: Returns `false`
    /// - Protocol >= 30: Returns `true` (COMPAT_SAFE_FLIST may be negotiated)
    ///
    /// Note: Use [`safe_file_list_always_enabled`](Self::safe_file_list_always_enabled)
    /// to check if safe file list is mandatory (protocol >= 31).
    #[must_use]
    pub const fn uses_safe_file_list(self) -> bool {
        self.as_u8() >= 30
    }

    /// Returns `true` if safe file list mode is always enabled (protocol >= 31).
    ///
    /// - Protocol < 31: Returns `false` (requires COMPAT_SAFE_FLIST negotiation)
    /// - Protocol >= 31: Returns `true` (always enabled)
    ///
    /// Protocol 31+ unconditionally uses safe file list mode regardless of
    /// compatibility flags.
    #[must_use]
    pub const fn safe_file_list_always_enabled(self) -> bool {
        self.as_u8() >= 31
    }

    /// Returns the numeric protocol identifiers supported by this implementation in newest-to-oldest order.
    #[must_use]
    pub const fn supported_protocol_numbers() -> &'static [u8] {
        &SUPPORTED_PROTOCOLS
    }

    /// Returns the numeric protocol identifiers as a fixed-size array reference.
    #[must_use]
    pub const fn supported_protocol_numbers_array() -> &'static [u8; SUPPORTED_PROTOCOL_COUNT] {
        &SUPPORTED_PROTOCOLS
    }

    /// Returns a bitmap describing the protocol versions supported by this implementation.
    #[must_use]
    pub const fn supported_protocol_bitmap() -> u64 {
        SUPPORTED_PROTOCOL_BITMAP
    }

    /// Returns an iterator over the numeric protocol identifiers supported by this implementation.
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

    /// Returns the inclusive range of protocol versions supported by this implementation.
    #[must_use]
    pub const fn supported_range() -> RangeInclusive<u8> {
        SUPPORTED_PROTOCOL_RANGE
    }

    /// Returns the inclusive supported range as a tuple of `(oldest, newest)`.
    #[must_use]
    pub const fn supported_range_bounds() -> (u8, u8) {
        SUPPORTED_PROTOCOL_BOUNDS
    }

    /// Returns the oldest and newest supported protocol versions as strongly typed values.
    #[must_use]
    pub const fn supported_version_bounds() -> (ProtocolVersion, ProtocolVersion) {
        (Self::OLDEST, Self::NEWEST)
    }

    /// Returns the inclusive range of supported protocol versions using strongly typed values.
    #[must_use]
    pub const fn supported_version_range() -> RangeInclusive<ProtocolVersion> {
        Self::OLDEST..=Self::NEWEST
    }

    /// Returns an iterator over the supported protocol versions in newest-to-oldest order.
    #[must_use = "consume the iterator to inspect the supported protocol versions"]
    pub const fn supported_versions_iter() -> SupportedVersionsIter {
        SupportedVersionsIter::new(Self::supported_versions())
    }

    /// Returns the protocol version at the given index within the canonical newest-to-oldest list.
    #[must_use]
    pub const fn from_supported_index(index: usize) -> Option<Self> {
        if index < SUPPORTED_PROTOCOL_COUNT {
            Some(Self::SUPPORTED_VERSIONS[index])
        } else {
            None
        }
    }

    /// Attempts to construct a [`ProtocolVersion`] from a byte known to fall inside the supported range.
    #[must_use]
    pub const fn from_supported(value: u8) -> Option<Self> {
        if Self::is_supported_protocol_number(value) {
            Some(Self::new_const(value))
        } else {
            None
        }
    }

    /// Reports whether the provided version is supported by this implementation.
    #[must_use]
    #[inline]
    pub const fn is_supported(value: u8) -> bool {
        Self::is_supported_protocol_number(value)
    }

    /// Returns the zero-based offset from [`ProtocolVersion::OLDEST`] when iterating protocol versions in ascending order.
    #[must_use]
    #[inline]
    pub const fn offset_from_oldest(self) -> usize {
        (self.as_u8() - Self::OLDEST.as_u8()) as usize
    }

    /// Constructs a [`ProtocolVersion`] from its zero-based offset relative to [`ProtocolVersion::OLDEST`].
    #[must_use]
    pub const fn from_oldest_offset(offset: usize) -> Option<Self> {
        let oldest = Self::OLDEST.as_u8() as usize;
        let newest = Self::NEWEST.as_u8() as usize;

        match oldest.checked_add(offset) {
            Some(value) if value <= newest => Some(Self::new_const(value as u8)),
            _ => None,
        }
    }

    /// Returns the zero-based offset from [`ProtocolVersion::NEWEST`] when iterating protocol versions in descending order.
    #[must_use]
    #[inline]
    pub const fn offset_from_newest(self) -> usize {
        (Self::NEWEST.as_u8() - self.as_u8()) as usize
    }

    /// Constructs a [`ProtocolVersion`] from its zero-based offset relative to [`ProtocolVersion::NEWEST`].
    #[must_use]
    pub const fn from_newest_offset(offset: usize) -> Option<Self> {
        let newest = Self::NEWEST.as_u8() as usize;
        let oldest = Self::OLDEST.as_u8() as usize;
        let span = newest - oldest;

        if offset > span {
            return None;
        }

        Some(Self::new_const((newest - offset) as u8))
    }

    /// Returns the next newer protocol version within the supported range, if any.
    #[must_use]
    pub const fn next_newer(self) -> Option<Self> {
        if self.as_u8() >= Self::NEWEST.as_u8() {
            None
        } else {
            Some(Self::new_const(self.as_u8() + 1))
        }
    }

    /// Returns the next older protocol version within the supported range, if any.
    #[must_use]
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

    /// Converts a peer-advertised version into the negotiated protocol version.
    #[must_use = "the negotiated protocol version must be handled"]
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

impl From<ProtocolVersion> for u8 {
    #[inline]
    fn from(version: ProtocolVersion) -> Self {
        version.as_u8()
    }
}

impl From<ProtocolVersion> for NonZeroU8 {
    #[inline]
    fn from(version: ProtocolVersion) -> Self {
        version.as_non_zero()
    }
}

macro_rules! impl_from_protocol_version_for_unsigned {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl From<ProtocolVersion> for $ty {
                #[inline]
                fn from(version: ProtocolVersion) -> Self {
                    <$ty as From<u8>>::from(version.as_u8())
                }
            }
        )+
    };
}

impl_from_protocol_version_for_unsigned!(u16, u32, u64, u128, usize);

impl From<ProtocolVersion> for i8 {
    #[inline]
    fn from(version: ProtocolVersion) -> Self {
        i8::try_from(version.as_u8()).expect("protocol versions fit within i8")
    }
}

macro_rules! impl_from_protocol_version_for_signed {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl From<ProtocolVersion> for $ty {
                #[inline]
                fn from(version: ProtocolVersion) -> Self {
                    <$ty as From<u8>>::from(version.as_u8())
                }
            }
        )+
    };
}

impl_from_protocol_version_for_signed!(i16, i32, i64, i128, isize);

macro_rules! impl_from_protocol_version_for_nonzero_unsigned {
    ($($ty:ty => $base:ty),+ $(,)?) => {
        $(
            impl From<ProtocolVersion> for $ty {
                #[inline]
                fn from(version: ProtocolVersion) -> Self {
                    <$ty>::new(<$base as From<u8>>::from(version.as_u8()))
                        .expect("protocol versions are always non-zero")
                }
            }
        )+
    };
}

impl_from_protocol_version_for_nonzero_unsigned!(
    NonZeroU16 => u16,
    NonZeroU32 => u32,
    NonZeroU64 => u64,
    NonZeroU128 => u128,
    NonZeroUsize => usize,
);

impl From<ProtocolVersion> for NonZeroI8 {
    #[inline]
    fn from(version: ProtocolVersion) -> Self {
        NonZeroI8::new(i8::try_from(version.as_u8()).expect("protocol versions fit within i8"))
            .expect("protocol versions are always non-zero")
    }
}

macro_rules! impl_from_protocol_version_for_nonzero_signed {
    ($($ty:ty => $base:ty),+ $(,)?) => {
        $(
            impl From<ProtocolVersion> for $ty {
                #[inline]
                fn from(version: ProtocolVersion) -> Self {
                    <$ty>::new(<$base as From<u8>>::from(version.as_u8()))
                        .expect("protocol versions are always non-zero")
                }
            }
        )+
    };
}

impl_from_protocol_version_for_nonzero_signed!(
    NonZeroI16 => i16,
    NonZeroI32 => i32,
    NonZeroI64 => i64,
    NonZeroI128 => i128,
    NonZeroIsize => isize,
);

impl TryFrom<u8> for ProtocolVersion {
    type Error = NegotiationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if !UPSTREAM_PROTOCOL_RANGE.contains(&value) {
            return Err(NegotiationError::UnsupportedVersion(u32::from(value)));
        }

        Ok(Self::from_supported(value).expect("values within the upstream range are supported"))
    }
}

impl TryFrom<NonZeroU8> for ProtocolVersion {
    type Error = NegotiationError;

    fn try_from(value: NonZeroU8) -> Result<Self, Self::Error> {
        <ProtocolVersion as TryFrom<u8>>::try_from(value.get())
    }
}

impl FromStr for ProtocolVersion {
    type Err = ParseProtocolVersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim_matches(|c: char| c.is_ascii_whitespace());
        if trimmed.is_empty() {
            return Err(ParseProtocolVersionError::new(
                ParseProtocolVersionErrorKind::Empty,
            ));
        }

        let mut digits = trimmed;
        let mut saw_negative = false;

        if let Some(first) = digits.as_bytes().first().copied() {
            match first {
                b'+' => {
                    digits = &digits[1..];
                }
                b'-' => {
                    saw_negative = true;
                    digits = &digits[1..];
                }
                _ => {}
            }
        }

        if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
            return Err(ParseProtocolVersionError::new(
                ParseProtocolVersionErrorKind::InvalidDigit,
            ));
        }

        if saw_negative {
            return Err(ParseProtocolVersionError::new(
                ParseProtocolVersionErrorKind::Negative,
            ));
        }

        let value: u16 = digits
            .parse()
            .map_err(|_| ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Overflow))?;

        if value > u16::from(u8::MAX) {
            return Err(ParseProtocolVersionError::new(
                ParseProtocolVersionErrorKind::Overflow,
            ));
        }

        let byte = value as u8;
        ProtocolVersion::from_supported(byte).ok_or_else(|| {
            ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::UnsupportedRange(byte))
        })
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_u8())
    }
}

impl PartialEq<u8> for ProtocolVersion {
    fn eq(&self, other: &u8) -> bool {
        self.as_u8() == *other
    }
}

impl PartialEq<ProtocolVersion> for u8 {
    fn eq(&self, other: &ProtocolVersion) -> bool {
        *self == other.as_u8()
    }
}

impl PartialEq<NonZeroU8> for ProtocolVersion {
    fn eq(&self, other: &NonZeroU8) -> bool {
        self.as_non_zero() == *other
    }
}

impl PartialEq<ProtocolVersion> for NonZeroU8 {
    fn eq(&self, other: &ProtocolVersion) -> bool {
        *self == other.as_non_zero()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_is_32() {
        assert_eq!(ProtocolVersion::NEWEST.as_u8(), 32);
    }

    #[test]
    fn oldest_is_28() {
        assert_eq!(ProtocolVersion::OLDEST.as_u8(), 28);
    }

    #[test]
    fn v32_equals_newest() {
        assert_eq!(ProtocolVersion::V32, ProtocolVersion::NEWEST);
    }

    #[test]
    fn v28_equals_oldest() {
        assert_eq!(ProtocolVersion::V28, ProtocolVersion::OLDEST);
    }

    #[test]
    fn supported_protocol_count_is_5() {
        assert_eq!(SUPPORTED_PROTOCOL_COUNT, 5);
    }

    #[test]
    fn supported_protocols_are_in_descending_order() {
        let protocols = ProtocolVersion::supported_protocol_numbers();
        for window in protocols.windows(2) {
            assert!(
                window[0] > window[1],
                "protocols should be in descending order"
            );
        }
    }

    #[test]
    fn is_supported_protocol_number_returns_true_for_valid() {
        for version in [28, 29, 30, 31, 32] {
            assert!(
                ProtocolVersion::is_supported_protocol_number(version),
                "version {version} should be supported"
            );
        }
    }

    #[test]
    fn is_supported_protocol_number_returns_false_for_invalid() {
        for version in [0, 1, 27, 33, 100, 255] {
            assert!(
                !ProtocolVersion::is_supported_protocol_number(version),
                "version {version} should not be supported"
            );
        }
    }

    #[test]
    fn uses_binary_negotiation_for_30_and_above() {
        assert!(ProtocolVersion::V30.uses_binary_negotiation());
        assert!(ProtocolVersion::V31.uses_binary_negotiation());
        assert!(ProtocolVersion::V32.uses_binary_negotiation());
    }

    #[test]
    fn uses_legacy_ascii_negotiation_for_29_and_below() {
        assert!(ProtocolVersion::V28.uses_legacy_ascii_negotiation());
        assert!(ProtocolVersion::V29.uses_legacy_ascii_negotiation());
    }

    // ------------------------------------------------------------------------
    // Feature flag method tests
    // ------------------------------------------------------------------------

    #[test]
    fn uses_varint_encoding_boundary_at_30() {
        assert!(!ProtocolVersion::V28.uses_varint_encoding());
        assert!(!ProtocolVersion::V29.uses_varint_encoding());
        assert!(ProtocolVersion::V30.uses_varint_encoding());
        assert!(ProtocolVersion::V31.uses_varint_encoding());
        assert!(ProtocolVersion::V32.uses_varint_encoding());
    }

    #[test]
    fn uses_fixed_encoding_is_inverse_of_varint() {
        for version in ProtocolVersion::supported_versions() {
            assert_eq!(
                version.uses_fixed_encoding(),
                !version.uses_varint_encoding(),
                "uses_fixed_encoding should be inverse of uses_varint_encoding for {version}"
            );
        }
    }

    #[test]
    fn supports_sender_receiver_modifiers_boundary_at_29() {
        assert!(!ProtocolVersion::V28.supports_sender_receiver_modifiers());
        assert!(ProtocolVersion::V29.supports_sender_receiver_modifiers());
        assert!(ProtocolVersion::V30.supports_sender_receiver_modifiers());
        assert!(ProtocolVersion::V31.supports_sender_receiver_modifiers());
        assert!(ProtocolVersion::V32.supports_sender_receiver_modifiers());
    }

    #[test]
    fn supports_perishable_modifier_boundary_at_30() {
        assert!(!ProtocolVersion::V28.supports_perishable_modifier());
        assert!(!ProtocolVersion::V29.supports_perishable_modifier());
        assert!(ProtocolVersion::V30.supports_perishable_modifier());
        assert!(ProtocolVersion::V31.supports_perishable_modifier());
        assert!(ProtocolVersion::V32.supports_perishable_modifier());
    }

    #[test]
    fn uses_old_prefixes_boundary_at_29() {
        assert!(ProtocolVersion::V28.uses_old_prefixes());
        assert!(!ProtocolVersion::V29.uses_old_prefixes());
        assert!(!ProtocolVersion::V30.uses_old_prefixes());
        assert!(!ProtocolVersion::V31.uses_old_prefixes());
        assert!(!ProtocolVersion::V32.uses_old_prefixes());
    }

    #[test]
    fn supports_flist_times_boundary_at_29() {
        assert!(!ProtocolVersion::V28.supports_flist_times());
        assert!(ProtocolVersion::V29.supports_flist_times());
        assert!(ProtocolVersion::V30.supports_flist_times());
        assert!(ProtocolVersion::V31.supports_flist_times());
        assert!(ProtocolVersion::V32.supports_flist_times());
    }

    #[test]
    fn supports_extended_flags_for_all_supported_versions() {
        // All supported versions (28+) support extended flags
        for version in ProtocolVersion::supported_versions() {
            assert!(
                version.supports_extended_flags(),
                "version {version} should support extended flags"
            );
        }
    }

    #[test]
    fn uses_varint_flist_flags_boundary_at_30() {
        assert!(!ProtocolVersion::V28.uses_varint_flist_flags());
        assert!(!ProtocolVersion::V29.uses_varint_flist_flags());
        assert!(ProtocolVersion::V30.uses_varint_flist_flags());
        assert!(ProtocolVersion::V31.uses_varint_flist_flags());
        assert!(ProtocolVersion::V32.uses_varint_flist_flags());
    }

    #[test]
    fn uses_safe_file_list_boundary_at_30() {
        assert!(!ProtocolVersion::V28.uses_safe_file_list());
        assert!(!ProtocolVersion::V29.uses_safe_file_list());
        assert!(ProtocolVersion::V30.uses_safe_file_list());
        assert!(ProtocolVersion::V31.uses_safe_file_list());
        assert!(ProtocolVersion::V32.uses_safe_file_list());
    }

    #[test]
    fn safe_file_list_always_enabled_boundary_at_31() {
        assert!(!ProtocolVersion::V28.safe_file_list_always_enabled());
        assert!(!ProtocolVersion::V29.safe_file_list_always_enabled());
        assert!(!ProtocolVersion::V30.safe_file_list_always_enabled());
        assert!(ProtocolVersion::V31.safe_file_list_always_enabled());
        assert!(ProtocolVersion::V32.safe_file_list_always_enabled());
    }

    #[test]
    fn feature_flags_consistent_with_binary_negotiation() {
        // All versions using binary negotiation (>= 30) should have:
        // - varint encoding
        // - varint flist flags
        // - safe file list
        // - perishable modifier support
        for version in ProtocolVersion::supported_versions() {
            if version.uses_binary_negotiation() {
                assert!(version.uses_varint_encoding());
                assert!(version.uses_varint_flist_flags());
                assert!(version.uses_safe_file_list());
                assert!(version.supports_perishable_modifier());
            } else {
                assert!(version.uses_fixed_encoding());
                assert!(!version.uses_varint_flist_flags());
                assert!(!version.uses_safe_file_list());
                assert!(!version.supports_perishable_modifier());
            }
        }
    }

    #[test]
    fn from_supported_returns_some_for_valid() {
        assert_eq!(
            ProtocolVersion::from_supported(30),
            Some(ProtocolVersion::V30)
        );
    }

    #[test]
    fn from_supported_returns_none_for_invalid() {
        assert!(ProtocolVersion::from_supported(27).is_none());
        assert!(ProtocolVersion::from_supported(33).is_none());
    }

    #[test]
    fn from_supported_index_returns_newest_at_zero() {
        assert_eq!(
            ProtocolVersion::from_supported_index(0),
            Some(ProtocolVersion::NEWEST)
        );
    }

    #[test]
    fn from_supported_index_returns_none_for_out_of_bounds() {
        assert!(ProtocolVersion::from_supported_index(100).is_none());
    }

    #[test]
    fn offset_from_oldest_correct() {
        assert_eq!(ProtocolVersion::V28.offset_from_oldest(), 0);
        assert_eq!(ProtocolVersion::V29.offset_from_oldest(), 1);
        assert_eq!(ProtocolVersion::V32.offset_from_oldest(), 4);
    }

    #[test]
    fn offset_from_newest_correct() {
        assert_eq!(ProtocolVersion::V32.offset_from_newest(), 0);
        assert_eq!(ProtocolVersion::V31.offset_from_newest(), 1);
        assert_eq!(ProtocolVersion::V28.offset_from_newest(), 4);
    }

    #[test]
    fn from_oldest_offset_roundtrip() {
        for version in ProtocolVersion::supported_versions() {
            let offset = version.offset_from_oldest();
            assert_eq!(ProtocolVersion::from_oldest_offset(offset), Some(*version));
        }
    }

    #[test]
    fn from_newest_offset_roundtrip() {
        for version in ProtocolVersion::supported_versions() {
            let offset = version.offset_from_newest();
            assert_eq!(ProtocolVersion::from_newest_offset(offset), Some(*version));
        }
    }

    #[test]
    fn from_oldest_offset_returns_none_for_out_of_range() {
        assert!(ProtocolVersion::from_oldest_offset(100).is_none());
    }

    #[test]
    fn from_newest_offset_returns_none_for_out_of_range() {
        assert!(ProtocolVersion::from_newest_offset(100).is_none());
    }

    #[test]
    fn next_newer_returns_next_version() {
        assert_eq!(
            ProtocolVersion::V28.next_newer(),
            Some(ProtocolVersion::V29)
        );
        assert_eq!(
            ProtocolVersion::V31.next_newer(),
            Some(ProtocolVersion::V32)
        );
    }

    #[test]
    fn next_newer_returns_none_at_newest() {
        assert!(ProtocolVersion::NEWEST.next_newer().is_none());
    }

    #[test]
    fn next_older_returns_previous_version() {
        assert_eq!(
            ProtocolVersion::V32.next_older(),
            Some(ProtocolVersion::V31)
        );
        assert_eq!(
            ProtocolVersion::V29.next_older(),
            Some(ProtocolVersion::V28)
        );
    }

    #[test]
    fn next_older_returns_none_at_oldest() {
        assert!(ProtocolVersion::OLDEST.next_older().is_none());
    }

    #[test]
    fn as_u8_returns_correct_value() {
        assert_eq!(ProtocolVersion::V30.as_u8(), 30);
    }

    #[test]
    fn as_usize_returns_correct_value() {
        assert_eq!(ProtocolVersion::V30.as_usize(), 30);
    }

    #[test]
    fn as_non_zero_returns_non_zero() {
        let nz = ProtocolVersion::V30.as_non_zero();
        assert_eq!(nz.get(), 30);
    }

    #[test]
    fn from_peer_advertisement_accepts_valid_version() {
        let version = ProtocolVersion::from_peer_advertisement(30).unwrap();
        assert_eq!(version.as_u8(), 30);
    }

    #[test]
    fn from_peer_advertisement_clamps_to_newest() {
        // Values between NEWEST (32) and MAXIMUM_PROTOCOL_ADVERTISEMENT (40) are clamped
        let version = ProtocolVersion::from_peer_advertisement(35).unwrap();
        assert_eq!(version, ProtocolVersion::NEWEST);
    }

    #[test]
    fn from_peer_advertisement_rejects_above_maximum() {
        // Values above MAXIMUM_PROTOCOL_ADVERTISEMENT (40) are rejected
        assert!(ProtocolVersion::from_peer_advertisement(50).is_err());
    }

    #[test]
    fn from_peer_advertisement_rejects_zero() {
        assert!(ProtocolVersion::from_peer_advertisement(0).is_err());
    }

    #[test]
    fn from_peer_advertisement_rejects_too_old() {
        assert!(ProtocolVersion::from_peer_advertisement(27).is_err());
    }

    #[test]
    fn try_from_u8_accepts_valid() {
        let version = ProtocolVersion::try_from(30_u8).unwrap();
        assert_eq!(version.as_u8(), 30);
    }

    #[test]
    fn try_from_u8_rejects_invalid() {
        assert!(ProtocolVersion::try_from(0_u8).is_err());
        assert!(ProtocolVersion::try_from(27_u8).is_err());
    }

    #[test]
    fn try_from_non_zero_u8_accepts_valid() {
        let nz = NonZeroU8::new(30).unwrap();
        let version = ProtocolVersion::try_from(nz).unwrap();
        assert_eq!(version.as_u8(), 30);
    }

    #[test]
    fn from_str_parses_valid_version() {
        let version: ProtocolVersion = "30".parse().unwrap();
        assert_eq!(version.as_u8(), 30);
    }

    #[test]
    fn from_str_handles_whitespace() {
        let version: ProtocolVersion = "  31  ".parse().unwrap();
        assert_eq!(version.as_u8(), 31);
    }

    #[test]
    fn from_str_handles_plus_prefix() {
        let version: ProtocolVersion = "+30".parse().unwrap();
        assert_eq!(version.as_u8(), 30);
    }

    #[test]
    fn from_str_rejects_negative() {
        let result: Result<ProtocolVersion, _> = "-30".parse();
        assert!(result.is_err());
    }

    #[test]
    fn from_str_rejects_empty() {
        let result: Result<ProtocolVersion, _> = "".parse();
        assert!(result.is_err());
    }

    #[test]
    fn from_str_rejects_invalid_digit() {
        let result: Result<ProtocolVersion, _> = "3x".parse();
        assert!(result.is_err());
    }

    #[test]
    fn from_str_rejects_overflow() {
        let result: Result<ProtocolVersion, _> = "999999".parse();
        assert!(result.is_err());
    }

    #[test]
    fn from_str_rejects_unsupported_range() {
        let result: Result<ProtocolVersion, _> = "27".parse();
        assert!(result.is_err());
    }

    #[test]
    fn display_shows_numeric_value() {
        assert_eq!(format!("{}", ProtocolVersion::V30), "30");
    }

    #[test]
    fn partial_eq_with_u8() {
        assert!(ProtocolVersion::V30 == 30_u8);
        assert!(30_u8 == ProtocolVersion::V30);
        assert!(ProtocolVersion::V30 != 31_u8);
    }

    #[test]
    fn partial_eq_with_non_zero_u8() {
        let nz = NonZeroU8::new(30).unwrap();
        assert!(ProtocolVersion::V30 == nz);
        assert!(nz == ProtocolVersion::V30);
    }

    #[test]
    fn from_u8_conversion() {
        let version = ProtocolVersion::V30;
        let byte: u8 = version.into();
        assert_eq!(byte, 30);
    }

    #[test]
    fn from_u16_conversion() {
        let version = ProtocolVersion::V30;
        let value: u16 = version.into();
        assert_eq!(value, 30);
    }

    #[test]
    fn from_u32_conversion() {
        let version = ProtocolVersion::V30;
        let value: u32 = version.into();
        assert_eq!(value, 30);
    }

    #[test]
    fn from_i8_conversion() {
        let version = ProtocolVersion::V30;
        let value: i8 = version.into();
        assert_eq!(value, 30);
    }

    #[test]
    fn from_i32_conversion() {
        let version = ProtocolVersion::V30;
        let value: i32 = version.into();
        assert_eq!(value, 30);
    }

    #[test]
    fn from_non_zero_u16_conversion() {
        let version = ProtocolVersion::V30;
        let value: NonZeroU16 = version.into();
        assert_eq!(value.get(), 30);
    }

    #[test]
    fn from_non_zero_i8_conversion() {
        let version = ProtocolVersion::V30;
        let value: NonZeroI8 = version.into();
        assert_eq!(value.get(), 30);
    }

    #[test]
    fn supported_versions_iter_yields_all() {
        let versions: Vec<_> = ProtocolVersion::supported_versions_iter().collect();
        assert_eq!(versions.len(), SUPPORTED_PROTOCOL_COUNT);
        assert_eq!(versions[0], ProtocolVersion::NEWEST);
    }

    #[test]
    fn supported_protocol_numbers_iter_yields_all() {
        let numbers: Vec<_> = ProtocolVersion::supported_protocol_numbers_iter().collect();
        assert_eq!(numbers.len(), SUPPORTED_PROTOCOL_COUNT);
        assert_eq!(numbers[0], 32);
    }

    #[test]
    fn supported_range_bounds_correct() {
        let (oldest, newest) = ProtocolVersion::supported_range_bounds();
        assert_eq!(oldest, 28);
        assert_eq!(newest, 32);
    }

    #[test]
    fn supported_version_bounds_correct() {
        let (oldest, newest) = ProtocolVersion::supported_version_bounds();
        assert_eq!(oldest, ProtocolVersion::V28);
        assert_eq!(newest, ProtocolVersion::V32);
    }

    #[test]
    fn supported_protocol_bitmap_has_correct_bits() {
        let bitmap = ProtocolVersion::supported_protocol_bitmap();
        for version in [28, 29, 30, 31, 32] {
            assert!(
                (bitmap & (1u64 << version)) != 0,
                "bit for version {version} should be set"
            );
        }
        assert!(
            (bitmap & (1u64 << 27)) == 0,
            "bit for version 27 should not be set"
        );
        assert!(
            (bitmap & (1u64 << 33)) == 0,
            "bit for version 33 should not be set"
        );
    }

    #[test]
    fn ord_impl_orders_by_version_number() {
        assert!(ProtocolVersion::V28 < ProtocolVersion::V29);
        assert!(ProtocolVersion::V30 < ProtocolVersion::V31);
        assert!(ProtocolVersion::V31 < ProtocolVersion::V32);
    }

    #[test]
    fn hash_impl_is_consistent() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ProtocolVersion::V30);
        assert!(set.contains(&ProtocolVersion::V30));
        assert!(!set.contains(&ProtocolVersion::V31));
    }

    #[test]
    fn clone_produces_equal_value() {
        let v = ProtocolVersion::V30;
        let cloned = v;
        assert_eq!(v, cloned);
    }

    #[test]
    fn copy_trait_works() {
        let v = ProtocolVersion::V30;
        let copied = v;
        assert_eq!(v, copied);
    }

    #[test]
    fn supported_protocols_display_is_not_empty() {
        let display = ProtocolVersion::supported_protocol_numbers_display();
        assert!(!display.is_empty());
        assert!(display.contains("32"));
        assert!(display.contains("28"));
    }
}
