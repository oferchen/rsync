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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u8_converts_correctly() {
        assert_eq!(42_u8.into_advertised_version(), 42);
        assert_eq!(u8::MAX.into_advertised_version(), 255);
    }

    #[test]
    fn u16_converts_correctly() {
        assert_eq!(1000_u16.into_advertised_version(), 1000);
    }

    #[test]
    fn u32_passes_through() {
        assert_eq!(12345_u32.into_advertised_version(), 12345);
        assert_eq!(u32::MAX.into_advertised_version(), u32::MAX);
    }

    #[test]
    fn u64_clamps_to_u32_max() {
        assert_eq!(100_u64.into_advertised_version(), 100);
        assert_eq!(u64::MAX.into_advertised_version(), u32::MAX);
    }

    #[test]
    fn u128_clamps_to_u32_max() {
        assert_eq!(100_u128.into_advertised_version(), 100);
        assert_eq!(u128::MAX.into_advertised_version(), u32::MAX);
    }

    #[test]
    fn usize_clamps_properly() {
        assert_eq!(100_usize.into_advertised_version(), 100);
    }

    #[test]
    fn i8_clamps_negative_to_zero() {
        assert_eq!((-5_i8).into_advertised_version(), 0);
        assert_eq!(50_i8.into_advertised_version(), 50);
    }

    #[test]
    fn i16_clamps_negative_to_zero() {
        assert_eq!((-100_i16).into_advertised_version(), 0);
        assert_eq!(100_i16.into_advertised_version(), 100);
    }

    #[test]
    fn i32_clamps_negative_to_zero() {
        assert_eq!((-1000_i32).into_advertised_version(), 0);
        assert_eq!(1000_i32.into_advertised_version(), 1000);
    }

    #[test]
    fn i64_clamps_properly() {
        assert_eq!((-1_i64).into_advertised_version(), 0);
        assert_eq!(i64::MAX.into_advertised_version(), u32::MAX);
    }

    #[test]
    fn protocol_version_converts() {
        let version = ProtocolVersion::V31;
        assert_eq!(version.into_advertised_version(), 31);
    }

    #[test]
    fn nonzero_u8_converts() {
        let value = NonZeroU8::new(30).unwrap();
        assert_eq!(value.into_advertised_version(), 30);
    }

    #[test]
    fn nonzero_i32_clamps_properly() {
        let positive = NonZeroI32::new(100).unwrap();
        assert_eq!(positive.into_advertised_version(), 100);

        let negative = NonZeroI32::new(-100).unwrap();
        assert_eq!(negative.into_advertised_version(), 0);
    }

    #[test]
    fn wrapping_u32_extracts_value() {
        let value = Wrapping(12345_u32);
        assert_eq!(value.into_advertised_version(), 12345);
    }

    #[test]
    fn wrapping_i32_clamps_negative() {
        let negative = Wrapping(-50_i32);
        assert_eq!(negative.into_advertised_version(), 0);
    }

    #[test]
    fn reference_types_work() {
        let value = 42_u32;
        assert_eq!((&value).into_advertised_version(), 42);
        let mut mutable = 100_u32;
        assert_eq!((&mut mutable).into_advertised_version(), 100);
    }
}
