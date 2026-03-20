//! Trait implementations for converting between [`ProtocolVersion`] and
//! numeric types, string parsing, and equality comparisons.

use ::core::convert::TryFrom;
use ::core::fmt;
use ::core::num::{
    NonZeroI8, NonZeroI16, NonZeroI32, NonZeroI64, NonZeroI128, NonZeroIsize, NonZeroU8,
    NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU128, NonZeroUsize,
};
use ::core::str::FromStr;

use crate::error::NegotiationError;

use super::super::constants::UPSTREAM_PROTOCOL_RANGE;
use super::super::parse::{ParseProtocolVersionError, ParseProtocolVersionErrorKind};
use super::ProtocolVersion;

// ---------------------------------------------------------------------------
// From<ProtocolVersion> for primitive types
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// TryFrom implementations
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// FromStr
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_u8())
    }
}

// ---------------------------------------------------------------------------
// PartialEq cross-type comparisons
// ---------------------------------------------------------------------------

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
