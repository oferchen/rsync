use std::collections::HashSet;
use std::num::{NonZeroI8, NonZeroU8, NonZeroU16};

use super::{ProtocolVersion, SUPPORTED_PROTOCOL_COUNT};

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
    let version = ProtocolVersion::from_peer_advertisement(35).unwrap();
    assert_eq!(version, ProtocolVersion::NEWEST);
}

#[test]
fn from_peer_advertisement_rejects_above_maximum() {
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

// ProtocolCapabilities tests

#[test]
fn capabilities_from_protocol_version() {
    use super::ProtocolCapabilities;

    let caps = ProtocolCapabilities::from(ProtocolVersion::V32);
    assert_eq!(caps.version(), ProtocolVersion::V32);
}

#[test]
fn capabilities_multiplex() {
    use super::ProtocolCapabilities;

    // All supported versions (28+) support multiplex I/O (requires >= 23).
    for v in ProtocolVersion::supported_versions() {
        let caps = ProtocolCapabilities::new(*v);
        assert!(caps.multiplex(), "v{} should support multiplex", v.as_u8());
    }
}

#[test]
fn capabilities_extended_flags() {
    use super::ProtocolCapabilities;

    // All supported versions (28+) support extended flags (requires >= 28).
    for v in ProtocolVersion::supported_versions() {
        let caps = ProtocolCapabilities::new(*v);
        assert!(
            caps.extended_flags(),
            "v{} should support extended flags",
            v.as_u8()
        );
    }
}

#[test]
fn capabilities_inline_hardlinks() {
    use super::ProtocolCapabilities;

    // Protocol 30+ supports inline hardlinks.
    let caps_32 = ProtocolCapabilities::new(ProtocolVersion::V32);
    assert!(caps_32.inline_hardlinks());

    let caps_30 = ProtocolCapabilities::new(ProtocolVersion::V30);
    assert!(caps_30.inline_hardlinks());

    let caps_29 = ProtocolCapabilities::new(ProtocolVersion::V29);
    assert!(!caps_29.inline_hardlinks());
}

#[test]
fn capabilities_preferred_compression() {
    use super::ProtocolCapabilities;
    use compress::strategy::ProtocolCompressionProfile;

    // The protocol crate's `zstd` feature is independent from the compress
    // crate's `zstd` feature, so the test must consult the same authoritative
    // source as `preferred_compression()` rather than duplicating the cfg
    // gate. upstream: compat.c:100-112 `valid_compressions_items[]`.
    for version in [
        ProtocolVersion::V32,
        ProtocolVersion::V31,
        ProtocolVersion::V30,
        ProtocolVersion::V28,
    ] {
        let caps = ProtocolCapabilities::new(version);
        let expected =
            ProtocolCompressionProfile::for_protocol(version.as_u8()).preferred_codec_name();
        assert_eq!(caps.preferred_compression(), expected);
    }

    // Protocol < 30 has no vstring negotiation; preferred codec is always
    // zlib regardless of any feature flag. upstream: compat.c:556-563.
    let caps_28 = ProtocolCapabilities::new(ProtocolVersion::V28);
    assert_eq!(caps_28.preferred_compression(), "zlib");
}

#[test]
fn capabilities_varint_encoding() {
    use super::ProtocolCapabilities;

    let caps_32 = ProtocolCapabilities::new(ProtocolVersion::V32);
    assert!(caps_32.varint_encoding());

    let caps_29 = ProtocolCapabilities::new(ProtocolVersion::V29);
    assert!(!caps_29.varint_encoding());
}

#[test]
fn capabilities_inc_recurse() {
    use super::ProtocolCapabilities;

    let caps_30 = ProtocolCapabilities::new(ProtocolVersion::V30);
    assert!(caps_30.inc_recurse());

    let caps_29 = ProtocolCapabilities::new(ProtocolVersion::V29);
    assert!(!caps_29.inc_recurse());
}

#[test]
fn capabilities_checksum_negotiation() {
    use super::ProtocolCapabilities;

    let caps_30 = ProtocolCapabilities::new(ProtocolVersion::V30);
    assert!(caps_30.checksum_negotiation());

    let caps_29 = ProtocolCapabilities::new(ProtocolVersion::V29);
    assert!(!caps_29.checksum_negotiation());
}

#[test]
fn capabilities_delete_stats() {
    use super::ProtocolCapabilities;

    let caps_31 = ProtocolCapabilities::new(ProtocolVersion::V31);
    assert!(caps_31.delete_stats());

    let caps_30 = ProtocolCapabilities::new(ProtocolVersion::V30);
    assert!(!caps_30.delete_stats());
}

#[test]
fn capabilities_equality() {
    use super::ProtocolCapabilities;

    let a = ProtocolCapabilities::new(ProtocolVersion::V32);
    let b = ProtocolCapabilities::from(ProtocolVersion::V32);
    assert_eq!(a, b);

    let c = ProtocolCapabilities::new(ProtocolVersion::V28);
    assert_ne!(a, c);
}

#[test]
fn capabilities_debug() {
    use super::ProtocolCapabilities;

    let caps = ProtocolCapabilities::new(ProtocolVersion::V32);
    let debug = format!("{caps:?}");
    assert!(debug.contains("ProtocolCapabilities"));
}
