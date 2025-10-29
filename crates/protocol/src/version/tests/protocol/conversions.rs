use crate::error::NegotiationError;
use core::convert::TryFrom;
use core::num::{
    NonZeroI8, NonZeroI16, NonZeroI32, NonZeroI64, NonZeroI128, NonZeroIsize, NonZeroU8,
    NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU128, NonZeroUsize,
};
use std::collections::HashSet;

use super::super::{ProtocolVersion, SUPPORTED_PROTOCOLS};

#[test]
fn protocol_version_converts_to_wider_unsigned_primitives() {
    for version in ProtocolVersion::supported_versions_iter() {
        let expected = version.as_u8();
        assert_eq!(u16::from(version), u16::from(expected));
        assert_eq!(u32::from(version), u32::from(expected));
        assert_eq!(u64::from(version), u64::from(expected));
        assert_eq!(u128::from(version), u128::from(expected));
        assert_eq!(usize::from(version), usize::from(expected));
        assert_eq!(version.as_usize(), usize::from(expected));
    }
}

#[test]
fn protocol_version_converts_to_nonzero_wider_unsigned_primitives() {
    for version in ProtocolVersion::supported_versions_iter() {
        let expected = version.as_u8();
        assert_eq!(NonZeroU16::from(version).get(), u16::from(expected));
        assert_eq!(NonZeroU32::from(version).get(), u32::from(expected));
        assert_eq!(NonZeroU64::from(version).get(), u64::from(expected));
        assert_eq!(NonZeroU128::from(version).get(), u128::from(expected));
        assert_eq!(NonZeroUsize::from(version).get(), usize::from(expected));
    }
}

#[test]
fn rejects_out_of_range_non_zero_u8() {
    let value = NonZeroU8::new(27).expect("non-zero");
    let err = ProtocolVersion::try_from(value).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(27));
}

#[test]
fn from_supported_accepts_values_within_range() {
    let newest = ProtocolVersion::from_supported(ProtocolVersion::NEWEST.as_u8());
    assert_eq!(newest, Some(ProtocolVersion::NEWEST));

    let oldest = ProtocolVersion::from_supported(ProtocolVersion::OLDEST.as_u8());
    assert_eq!(oldest, Some(ProtocolVersion::OLDEST));
}

#[test]
fn from_supported_rejects_values_outside_range() {
    assert_eq!(ProtocolVersion::from_supported(0), None);
    assert_eq!(ProtocolVersion::from_supported(27), None);
    assert_eq!(ProtocolVersion::from_supported(33), None);
}

#[test]
fn from_supported_matches_supported_protocol_list() {
    let supported = ProtocolVersion::supported_protocol_numbers();

    for value in 0..=u8::MAX {
        let parsed = ProtocolVersion::from_supported(value);
        let expected = supported.contains(&value);

        assert_eq!(
            parsed.is_some(),
            expected,
            "value {value} should match membership"
        );

        if let Some(version) = parsed {
            assert_eq!(
                version.as_u8(),
                value,
                "returned version must preserve numeric value"
            );
        }
    }
}

#[test]
fn converts_from_non_zero_u8() {
    let value = NonZeroU8::new(31).expect("non-zero");
    let version = ProtocolVersion::try_from(value).expect("valid");
    assert_eq!(version.as_u8(), 31);
}

#[test]
fn converts_protocol_version_to_non_zero_u8() {
    let value = NonZeroU8::new(28).expect("non-zero");
    let version = ProtocolVersion::try_from(value).expect("valid");
    let round_trip: NonZeroU8 = version.into();
    assert_eq!(round_trip, value);
}

#[test]
fn exposes_non_zero_protocol_byte() {
    let version = ProtocolVersion::try_from(30).expect("valid");
    let non_zero = version.as_non_zero();
    assert_eq!(non_zero.get(), 30);

    let via_from: NonZeroU8 = version.into();
    assert_eq!(via_from, non_zero);
}

#[test]
fn compares_protocol_version_with_non_zero_u8() {
    let newest = NonZeroU8::new(ProtocolVersion::NEWEST.as_u8()).expect("non-zero");
    assert_eq!(ProtocolVersion::NEWEST, newest);
    assert_eq!(newest, ProtocolVersion::NEWEST);

    let oldest = NonZeroU8::new(ProtocolVersion::OLDEST.as_u8()).expect("non-zero");
    assert_ne!(ProtocolVersion::NEWEST, oldest);
    assert_ne!(oldest, ProtocolVersion::NEWEST);
}

#[test]
fn converts_protocol_version_to_u8() {
    let version = ProtocolVersion::try_from(32).expect("valid");
    let value: u8 = version.into();
    assert_eq!(value, 32);
}

#[test]
fn converts_protocol_version_to_signed_integers() {
    let newest = ProtocolVersion::NEWEST;
    let oldest = ProtocolVersion::OLDEST;

    let newest_i8: i8 = newest.into();
    assert_eq!(newest_i8, i8::try_from(newest.as_u8()).expect("fits in i8"));

    let newest_i16: i16 = newest.into();
    assert_eq!(newest_i16, i16::from(newest.as_u8()));

    let oldest_i32: i32 = oldest.into();
    assert_eq!(oldest_i32, i32::from(oldest.as_u8()));

    let newest_i64: i64 = newest.into();
    assert_eq!(newest_i64, i64::from(newest.as_u8()));

    let oldest_i128: i128 = oldest.into();
    assert_eq!(oldest_i128, i128::from(oldest.as_u8()));

    let newest_isize: isize = newest.into();
    assert_eq!(newest_isize, isize::from(newest.as_u8()));
}

#[test]
fn converts_protocol_version_to_non_zero_signed_integers() {
    let newest = ProtocolVersion::NEWEST;
    let oldest = ProtocolVersion::OLDEST;

    let newest_i8: NonZeroI8 = newest.into();
    assert_eq!(
        newest_i8.get(),
        i8::try_from(newest.as_u8()).expect("fits in i8")
    );

    let newest_i16: NonZeroI16 = newest.into();
    assert_eq!(newest_i16.get(), i16::from(newest.as_u8()));

    let oldest_i32: NonZeroI32 = oldest.into();
    assert_eq!(oldest_i32.get(), i32::from(oldest.as_u8()));

    let newest_i64: NonZeroI64 = newest.into();
    assert_eq!(newest_i64.get(), i64::from(newest.as_u8()));

    let oldest_i128: NonZeroI128 = oldest.into();
    assert_eq!(oldest_i128.get(), i128::from(oldest.as_u8()));

    let newest_isize: NonZeroIsize = newest.into();
    assert_eq!(newest_isize.get(), isize::from(newest.as_u8()));
}

#[test]
fn compares_directly_with_u8() {
    let version = ProtocolVersion::try_from(30).expect("valid");
    assert_eq!(version, 30);
    assert_eq!(30, version);
}

#[test]
fn compares_directly_with_non_zero_u8() {
    let version = ProtocolVersion::try_from(31).expect("valid");
    let non_zero = NonZeroU8::new(31).expect("non-zero");

    assert_eq!(version, non_zero);
    assert_eq!(non_zero, version);

    let different = NonZeroU8::new(ProtocolVersion::OLDEST.as_u8()).expect("non-zero");
    assert_ne!(version, different);
    assert_ne!(different, version);
}

#[test]
fn supported_versions_are_sorted_descending() {
    let mut sorted = SUPPORTED_PROTOCOLS;
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(sorted, SUPPORTED_PROTOCOLS);
}

#[test]
fn protocol_version_display_matches_numeric_value() {
    let version = ProtocolVersion::try_from(32).expect("valid");
    assert_eq!(version.to_string(), "32");
}

#[test]
fn protocol_versions_are_hashable() {
    let mut set = HashSet::new();
    assert!(set.insert(ProtocolVersion::NEWEST));
    assert!(set.contains(&ProtocolVersion::NEWEST));
    assert!(!set.insert(ProtocolVersion::NEWEST));
}
