//! Conversions between different representations of peer-advertised protocol
//! versions.

use ::core::num::{
    NonZeroI8, NonZeroI16, NonZeroI32, NonZeroI64, NonZeroI128, NonZeroIsize, NonZeroU8,
    NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU128, NonZeroUsize, Wrapping,
};

use super::ProtocolVersion;

/// Types that can be interpreted as peer-advertised protocol versions.
///
/// The negotiation helpers frequently operate on raw numeric identifiers while
/// higher layers may work with strongly typed wrappers. Providing this trait
/// keeps the conversion centralised and mirrors upstream rsync's tolerance for
/// future protocol numbers.
#[doc(hidden)]
pub trait ProtocolVersionAdvertisement {
    /// Returns the numeric representation expected by the negotiation logic.
    fn into_advertised_version(self) -> u32;
}

macro_rules! impl_protocol_version_advertisement {
    ($($ty:ty => $into:expr),+ $(,)?) => {
        $(
            impl ProtocolVersionAdvertisement for $ty {
                #[inline]
                fn into_advertised_version(self) -> u32 {
                    let convert = $into;
                    convert(self)
                }
            }

            impl ProtocolVersionAdvertisement for &$ty {
                #[inline]
                fn into_advertised_version(self) -> u32 {
                    let convert = $into;
                    convert(*self)
                }
            }

            impl ProtocolVersionAdvertisement for &mut $ty {
                #[inline]
                fn into_advertised_version(self) -> u32 {
                    let convert = $into;
                    convert(*self)
                }
            }
        )+
    };
}

impl_protocol_version_advertisement!(
    u8 => |value: u8| u32::from(value),
    NonZeroU8 => |value: NonZeroU8| u32::from(value.get()),
    ProtocolVersion => |value: ProtocolVersion| u32::from(value.as_u8()),
    u16 => |value: u16| u32::from(value),
    u32 => |value: u32| value,
    u64 => |value: u64| value.min(u64::from(u32::MAX)) as u32,
    u128 => |value: u128| value.min(u128::from(u32::MAX)) as u32,
    usize => |value: usize| {
        let cap = u32::MAX as usize;
        if value > cap {
            u32::MAX
        } else {
            value as u32
        }
    },
    NonZeroU16 => |value: NonZeroU16| u32::from(value.get()),
    NonZeroU32 => |value: NonZeroU32| value.get(),
    NonZeroU64 => |value: NonZeroU64| value.get().min(u64::from(u32::MAX)) as u32,
    NonZeroU128 => |value: NonZeroU128| value.get().min(u128::from(u32::MAX)) as u32,
    NonZeroUsize => |value: NonZeroUsize| {
        let cap = u32::MAX as usize;
        if value.get() > cap {
            u32::MAX
        } else {
            value.get() as u32
        }
    },
    Wrapping<u8> => |value: Wrapping<u8>| u32::from(value.0),
    Wrapping<u16> => |value: Wrapping<u16>| u32::from(value.0),
    Wrapping<u32> => |value: Wrapping<u32>| value.0,
    Wrapping<u64> => |value: Wrapping<u64>| value.0.min(u64::from(u32::MAX)) as u32,
    Wrapping<u128> => |value: Wrapping<u128>| value.0.min(u128::from(u32::MAX)) as u32,
    Wrapping<usize> => |value: Wrapping<usize>| {
        let cap = u32::MAX as usize;
        if value.0 > cap {
            u32::MAX
        } else {
            value.0 as u32
        }
    },
    i8 => |value: i8| u32::from((value.clamp(0, i8::MAX)) as u8),
    i16 => |value: i16| u32::from((value.clamp(0, i16::MAX)) as u16),
    i32 => |value: i32| value.clamp(0, i32::MAX) as u32,
    i64 => |value: i64| value.clamp(0, i64::from(u32::MAX)) as u32,
    i128 => |value: i128| value.clamp(0, i128::from(u32::MAX)) as u32,
    isize => |value: isize| {
        let clamped = (value as i128).clamp(0, i128::from(u32::MAX));
        clamped as u32
    },
    NonZeroI8 => |value: NonZeroI8| u32::from((value.get().clamp(0, i8::MAX)) as u8),
    NonZeroI16 => |value: NonZeroI16| u32::from((value.get().clamp(0, i16::MAX)) as u16),
    NonZeroI32 => |value: NonZeroI32| value.get().clamp(0, i32::MAX) as u32,
    NonZeroI64 => |value: NonZeroI64| value.get().clamp(0, i64::from(u32::MAX)) as u32,
    NonZeroI128 => |value: NonZeroI128| value.get().clamp(0, i128::from(u32::MAX)) as u32,
    NonZeroIsize => |value: NonZeroIsize| {
        let clamped = (value.get() as i128).clamp(0, i128::from(u32::MAX));
        clamped as u32
    },
    Wrapping<i8> => |value: Wrapping<i8>| u32::from((value.0.clamp(0, i8::MAX)) as u8),
    Wrapping<i16> => |value: Wrapping<i16>| u32::from((value.0.clamp(0, i16::MAX)) as u16),
    Wrapping<i32> => |value: Wrapping<i32>| value.0.clamp(0, i32::MAX) as u32,
    Wrapping<i64> => |value: Wrapping<i64>| value.0.clamp(0, i64::from(u32::MAX)) as u32,
    Wrapping<i128> => |value: Wrapping<i128>| value.0.clamp(0, i128::from(u32::MAX)) as u32,
    Wrapping<isize> => |value: Wrapping<isize>| {
        let clamped = (value.0 as i128).clamp(0, i128::from(u32::MAX));
        clamped as u32
    },
);
