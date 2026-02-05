//! Protocol version 31 compatibility tests.
//!
//! These tests verify that protocol version 31 works correctly, including:
//! - Protocol version 31 handshake and negotiation
//! - All latest features introduced in v31
//! - Backward compatibility with older versions (v28-v30)
//! - Forward compatibility with newer versions (v32)
//!
//! # Protocol Version 31 Features
//!
//! Protocol 31 is notable for:
//! - **Safe file list always enabled**: Unlike v30 where safe file list can be negotiated,
//!   v31 mandates it unconditionally
//! - Uses binary negotiation (introduced in v30)
//! - Uses varint encoding
//! - Supports all v30 features: perishable modifier, sender/receiver modifiers, flist times
//!
//! # Upstream Reference
//!
//! Protocol 31 was introduced in rsync 3.1.x releases and is widely deployed.
//! It serves as a stable baseline for modern rsync implementations.

use crate::error::NegotiationError;
use crate::version::{ProtocolVersion, select_highest_mutual};

// ============================================================================
// Protocol 31 Handshake and Negotiation Tests
// ============================================================================

/// Verifies protocol 31 can be successfully negotiated when offered by peer.
#[test]
fn v31_handshake_succeeds_when_peer_offers_v31() {
    let result = select_highest_mutual([31]);
    assert!(result.is_ok(), "v31 negotiation should succeed");
    assert_eq!(result.unwrap(), ProtocolVersion::V31);
}

/// Verifies protocol 31 is selected when peer offers v31 along with older versions.
#[test]
fn v31_handshake_selects_v31_over_older_versions() {
    let result = select_highest_mutual([28, 29, 30, 31]).unwrap();
    assert_eq!(result, ProtocolVersion::V31, "should prefer v31 over older");
}

/// Verifies protocol 32 is selected when peer offers both v31 and v32.
#[test]
fn v31_handshake_upgrades_to_v32_when_available() {
    let result = select_highest_mutual([31, 32]).unwrap();
    assert_eq!(result, ProtocolVersion::V32, "should prefer v32 over v31");
}

/// Verifies protocol 31 negotiation from peer advertisement.
#[test]
fn v31_from_peer_advertisement_accepts_31() {
    let result = ProtocolVersion::from_peer_advertisement(31);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), ProtocolVersion::V31);
}

/// Verifies protocol 31 is the correct constant value.
#[test]
fn v31_constant_is_31() {
    assert_eq!(ProtocolVersion::V31.as_u8(), 31);
}

/// Verifies protocol 31 is within supported range.
#[test]
fn v31_is_within_supported_range() {
    assert!(ProtocolVersion::is_supported(31));
    assert!(ProtocolVersion::from_supported(31).is_some());
}

/// Verifies protocol 31 handshake with mixed valid and invalid versions.
#[test]
fn v31_handshake_ignores_invalid_versions() {
    // Mix of too old, valid, and too new (within clamping range)
    // Version 50 is above MAXIMUM_PROTOCOL_ADVERTISEMENT (40), so it causes an error
    // Use version 35 instead, which gets clamped to NEWEST (32)
    let result = select_highest_mutual([0, 27, 31, 35]).unwrap();
    assert_eq!(result, ProtocolVersion::NEWEST); // 35 clamps to 32 (NEWEST)
}

/// Verifies protocol 31 can be parsed from string.
#[test]
fn v31_parses_from_string() {
    let result: ProtocolVersion = "31".parse().unwrap();
    assert_eq!(result, ProtocolVersion::V31);
}

/// Verifies protocol 31 handshake with duplicate peer versions.
#[test]
fn v31_handshake_handles_duplicate_peer_versions() {
    let result = select_highest_mutual([31, 31, 31, 30]).unwrap();
    assert_eq!(result, ProtocolVersion::V31);
}

// ============================================================================
// Protocol 31 Feature Tests
// ============================================================================

/// Verifies protocol 31 uses binary negotiation.
#[test]
fn v31_uses_binary_negotiation() {
    assert!(
        ProtocolVersion::V31.uses_binary_negotiation(),
        "v31 should use binary negotiation"
    );
    assert!(
        !ProtocolVersion::V31.uses_legacy_ascii_negotiation(),
        "v31 should not use legacy ASCII negotiation"
    );
}

/// Verifies protocol 31 uses varint encoding.
#[test]
fn v31_uses_varint_encoding() {
    assert!(
        ProtocolVersion::V31.uses_varint_encoding(),
        "v31 should use varint encoding"
    );
    assert!(
        !ProtocolVersion::V31.uses_fixed_encoding(),
        "v31 should not use fixed encoding"
    );
}

/// Verifies protocol 31 uses varint flist flags.
#[test]
fn v31_uses_varint_flist_flags() {
    assert!(
        ProtocolVersion::V31.uses_varint_flist_flags(),
        "v31 should use varint flist flags"
    );
}

/// Verifies protocol 31 supports safe file list.
#[test]
fn v31_uses_safe_file_list() {
    assert!(
        ProtocolVersion::V31.uses_safe_file_list(),
        "v31 should support safe file list"
    );
}

/// Verifies protocol 31 has safe file list ALWAYS enabled (key v31 feature).
#[test]
fn v31_safe_file_list_always_enabled() {
    assert!(
        ProtocolVersion::V31.safe_file_list_always_enabled(),
        "v31 key feature: safe file list is mandatory"
    );
}

/// Verifies protocol 31 supports sender/receiver modifiers.
#[test]
fn v31_supports_sender_receiver_modifiers() {
    assert!(
        ProtocolVersion::V31.supports_sender_receiver_modifiers(),
        "v31 should support sender/receiver modifiers"
    );
}

/// Verifies protocol 31 supports perishable modifier.
#[test]
fn v31_supports_perishable_modifier() {
    assert!(
        ProtocolVersion::V31.supports_perishable_modifier(),
        "v31 should support perishable modifier"
    );
}

/// Verifies protocol 31 supports flist times.
#[test]
fn v31_supports_flist_times() {
    assert!(
        ProtocolVersion::V31.supports_flist_times(),
        "v31 should support flist times"
    );
}

/// Verifies protocol 31 does not use old prefixes.
#[test]
fn v31_does_not_use_old_prefixes() {
    assert!(
        !ProtocolVersion::V31.uses_old_prefixes(),
        "v31 should use new prefixes"
    );
}

/// Verifies protocol 31 supports extended flags.
#[test]
fn v31_supports_extended_flags() {
    assert!(
        ProtocolVersion::V31.supports_extended_flags(),
        "v31 should support extended flags"
    );
}

/// Verifies protocol 31 has the complete feature set expected.
#[test]
fn v31_complete_feature_profile() {
    let v = ProtocolVersion::V31;

    // Binary negotiation features (v30+)
    assert!(v.uses_binary_negotiation());
    assert!(v.uses_varint_encoding());
    assert!(v.uses_varint_flist_flags());
    assert!(v.uses_safe_file_list());
    assert!(v.supports_perishable_modifier());

    // V31 specific: safe file list always enabled
    assert!(v.safe_file_list_always_enabled());

    // Features from v29+
    assert!(v.supports_sender_receiver_modifiers());
    assert!(v.supports_flist_times());
    assert!(!v.uses_old_prefixes());

    // Features from v28+
    assert!(v.supports_extended_flags());

    // Should NOT have legacy features
    assert!(!v.uses_legacy_ascii_negotiation());
    assert!(!v.uses_fixed_encoding());
}

// ============================================================================
// Backward Compatibility Tests (v31 with older versions)
// ============================================================================

/// Verifies v31 can downgrade to v30 when peer only supports v30.
#[test]
fn v31_downgrades_to_v30_for_older_peer() {
    let result = select_highest_mutual([30]).unwrap();
    assert_eq!(result, ProtocolVersion::V30);
}

/// Verifies v31 can downgrade to v29 when peer only supports v29.
#[test]
fn v31_downgrades_to_v29_for_older_peer() {
    let result = select_highest_mutual([29]).unwrap();
    assert_eq!(result, ProtocolVersion::V29);
}

/// Verifies v31 can downgrade to v28 when peer only supports v28.
#[test]
fn v31_downgrades_to_v28_for_oldest_peer() {
    let result = select_highest_mutual([28]).unwrap();
    assert_eq!(result, ProtocolVersion::V28);
}

/// Verifies v31 selects highest common version when peer supports multiple older versions.
#[test]
fn v31_selects_highest_older_version_available() {
    let result = select_highest_mutual([28, 29, 30]).unwrap();
    assert_eq!(result, ProtocolVersion::V30, "should select v30 over v28/v29");
}

/// Verifies feature differences between v31 and v30.
#[test]
fn v31_vs_v30_feature_differences() {
    let v30 = ProtocolVersion::V30;
    let v31 = ProtocolVersion::V31;

    // Both use binary negotiation
    assert_eq!(v30.uses_binary_negotiation(), v31.uses_binary_negotiation());

    // Both use varint encoding
    assert_eq!(v30.uses_varint_encoding(), v31.uses_varint_encoding());

    // Both support safe file list
    assert_eq!(v30.uses_safe_file_list(), v31.uses_safe_file_list());

    // Key difference: v31 has safe file list ALWAYS enabled
    assert!(!v30.safe_file_list_always_enabled());
    assert!(v31.safe_file_list_always_enabled());
}

/// Verifies feature differences between v31 and v29.
#[test]
fn v31_vs_v29_feature_differences() {
    let v29 = ProtocolVersion::V29;
    let v31 = ProtocolVersion::V31;

    // v29 uses legacy negotiation, v31 uses binary
    assert!(v29.uses_legacy_ascii_negotiation());
    assert!(v31.uses_binary_negotiation());

    // v29 uses fixed encoding, v31 uses varint
    assert!(v29.uses_fixed_encoding());
    assert!(v31.uses_varint_encoding());

    // v29 doesn't support safe file list, v31 does (always)
    assert!(!v29.uses_safe_file_list());
    assert!(v31.uses_safe_file_list());
    assert!(v31.safe_file_list_always_enabled());

    // v29 doesn't support perishable modifier, v31 does
    assert!(!v29.supports_perishable_modifier());
    assert!(v31.supports_perishable_modifier());

    // Both support sender/receiver modifiers (v29+)
    assert_eq!(
        v29.supports_sender_receiver_modifiers(),
        v31.supports_sender_receiver_modifiers()
    );
}

/// Verifies feature differences between v31 and v28.
#[test]
fn v31_vs_v28_feature_differences() {
    let v28 = ProtocolVersion::V28;
    let v31 = ProtocolVersion::V31;

    // v28 uses legacy negotiation, v31 uses binary
    assert!(v28.uses_legacy_ascii_negotiation());
    assert!(v31.uses_binary_negotiation());

    // v28 uses fixed encoding, v31 uses varint
    assert!(v28.uses_fixed_encoding());
    assert!(v31.uses_varint_encoding());

    // v28 uses old prefixes, v31 uses new prefixes
    assert!(v28.uses_old_prefixes());
    assert!(!v31.uses_old_prefixes());

    // v28 doesn't support sender/receiver modifiers, v31 does
    assert!(!v28.supports_sender_receiver_modifiers());
    assert!(v31.supports_sender_receiver_modifiers());

    // v28 doesn't support flist times, v31 does
    assert!(!v28.supports_flist_times());
    assert!(v31.supports_flist_times());

    // Both support extended flags
    assert_eq!(v28.supports_extended_flags(), v31.supports_extended_flags());
}

/// Verifies backward compatibility negotiation prefers v31 when available.
#[test]
fn v31_backward_compat_prefers_newest_mutual() {
    // Peer supports v28-v31
    let result = select_highest_mutual([28, 29, 30, 31]).unwrap();
    assert_eq!(result, ProtocolVersion::V31);

    // Peer supports v28, v30, v31 (missing v29)
    let result = select_highest_mutual([28, 30, 31]).unwrap();
    assert_eq!(result, ProtocolVersion::V31);
}

// ============================================================================
// Forward Compatibility Tests (v31 with newer versions)
// ============================================================================

/// Verifies v31 upgrades to v32 when peer supports v32.
#[test]
fn v31_upgrades_to_v32_when_peer_supports_it() {
    let result = select_highest_mutual([31, 32]).unwrap();
    assert_eq!(result, ProtocolVersion::V32);
}

/// Verifies v31 upgrades to v32 when peer offers future versions.
#[test]
fn v31_forward_compat_clamps_future_to_v32() {
    // Peer offers v31 and future versions (33-40)
    let result = select_highest_mutual([31, 35]).unwrap();
    assert_eq!(result, ProtocolVersion::V32, "future versions clamp to v32");
}

/// Verifies feature differences between v31 and v32.
#[test]
fn v31_vs_v32_feature_comparison() {
    let v31 = ProtocolVersion::V31;
    let v32 = ProtocolVersion::V32;

    // Both use binary negotiation
    assert_eq!(v31.uses_binary_negotiation(), v32.uses_binary_negotiation());

    // Both use varint encoding
    assert_eq!(v31.uses_varint_encoding(), v32.uses_varint_encoding());

    // Both have safe file list always enabled
    assert_eq!(
        v31.safe_file_list_always_enabled(),
        v32.safe_file_list_always_enabled()
    );

    // Both support all modern features
    assert_eq!(
        v31.supports_sender_receiver_modifiers(),
        v32.supports_sender_receiver_modifiers()
    );
    assert_eq!(
        v31.supports_perishable_modifier(),
        v32.supports_perishable_modifier()
    );
    assert_eq!(v31.supports_flist_times(), v32.supports_flist_times());

    // v31 and v32 have the same feature set
    // (v32 is a minor revision, no new features over v31 in this implementation)
}

/// Verifies v31 forward compatibility with future protocol versions.
#[test]
fn v31_forward_compat_with_future_versions() {
    // Peer offers v31, v32, and future version 35
    let result = select_highest_mutual([31, 32, 35]).unwrap();
    assert_eq!(result, ProtocolVersion::V32);

    // Peer offers only v31 and future version 38
    let result = select_highest_mutual([31, 38]).unwrap();
    assert_eq!(result, ProtocolVersion::V32, "clamps 38 to v32");
}

/// Verifies v31 rejects versions beyond maximum advertisement.
#[test]
fn v31_rejects_versions_beyond_maximum() {
    // Version 41 is beyond MAXIMUM_PROTOCOL_ADVERTISEMENT (40)
    let result = select_highest_mutual([31, 41]);
    assert!(result.is_err());
    match result.unwrap_err() {
        NegotiationError::UnsupportedVersion(v) => assert_eq!(v, 41),
        _ => panic!("expected UnsupportedVersion error"),
    }
}

// ============================================================================
// Protocol 31 Ordering and Comparison Tests
// ============================================================================

/// Verifies v31 ordering relative to other versions.
#[test]
fn v31_ordering() {
    assert!(ProtocolVersion::V31 > ProtocolVersion::V30);
    assert!(ProtocolVersion::V31 > ProtocolVersion::V29);
    assert!(ProtocolVersion::V31 > ProtocolVersion::V28);
    assert!(ProtocolVersion::V31 < ProtocolVersion::V32);
}

/// Verifies v31 equality comparisons.
#[test]
fn v31_equality() {
    assert_eq!(ProtocolVersion::V31, ProtocolVersion::V31);
    assert_ne!(ProtocolVersion::V31, ProtocolVersion::V30);
    assert_ne!(ProtocolVersion::V31, ProtocolVersion::V32);
}

/// Verifies v31 comparison with raw integers.
#[test]
fn v31_comparison_with_integers() {
    assert_eq!(ProtocolVersion::V31, 31u8);
    assert_eq!(31u8, ProtocolVersion::V31);
    assert_ne!(ProtocolVersion::V31, 30u8);
    assert_ne!(ProtocolVersion::V31, 32u8);
}

/// Verifies v31 is at the correct offset from oldest.
#[test]
fn v31_offset_from_oldest() {
    assert_eq!(ProtocolVersion::V31.offset_from_oldest(), 3);
}

/// Verifies v31 is at the correct offset from newest.
#[test]
fn v31_offset_from_newest() {
    assert_eq!(ProtocolVersion::V31.offset_from_newest(), 1);
}

/// Verifies v31 navigation to next newer version.
#[test]
fn v31_next_newer() {
    assert_eq!(
        ProtocolVersion::V31.next_newer(),
        Some(ProtocolVersion::V32)
    );
}

/// Verifies v31 navigation to next older version.
#[test]
fn v31_next_older() {
    assert_eq!(
        ProtocolVersion::V31.next_older(),
        Some(ProtocolVersion::V30)
    );
}

// ============================================================================
// Protocol 31 Conversion Tests
// ============================================================================

/// Verifies v31 converts to u8.
#[test]
fn v31_to_u8_conversion() {
    let byte: u8 = ProtocolVersion::V31.into();
    assert_eq!(byte, 31);
}

/// Verifies v31 converts to wider integer types.
#[test]
fn v31_to_wider_integer_conversions() {
    let u16_val: u16 = ProtocolVersion::V31.into();
    assert_eq!(u16_val, 31);

    let u32_val: u32 = ProtocolVersion::V31.into();
    assert_eq!(u32_val, 31);

    let u64_val: u64 = ProtocolVersion::V31.into();
    assert_eq!(u64_val, 31);
}

/// Verifies v31 converts from u8.
#[test]
fn v31_from_u8_conversion() {
    let version = ProtocolVersion::try_from(31u8).unwrap();
    assert_eq!(version, ProtocolVersion::V31);
}

/// Verifies v31 display formatting.
#[test]
fn v31_display_format() {
    assert_eq!(format!("{}", ProtocolVersion::V31), "31");
}

/// Verifies v31 debug formatting.
#[test]
fn v31_debug_format() {
    let debug = format!("{:?}", ProtocolVersion::V31);
    assert!(debug.contains("31"));
}

// ============================================================================
// Protocol 31 Consistency Tests
// ============================================================================

/// Verifies v31 appears in supported versions list.
#[test]
fn v31_in_supported_versions() {
    let versions = ProtocolVersion::supported_versions();
    assert!(
        versions.contains(&ProtocolVersion::V31),
        "v31 should be in supported versions"
    );
}

/// Verifies v31 appears in supported protocol numbers.
#[test]
fn v31_in_supported_protocol_numbers() {
    let numbers = ProtocolVersion::supported_protocol_numbers();
    assert!(
        numbers.contains(&31),
        "31 should be in supported protocol numbers"
    );
}

/// Verifies v31 bit is set in supported protocol bitmap.
#[test]
fn v31_in_protocol_bitmap() {
    let bitmap = ProtocolVersion::supported_protocol_bitmap();
    assert_ne!(
        bitmap & (1u64 << 31),
        0,
        "bit 31 should be set in bitmap"
    );
}

/// Verifies v31 is within supported range bounds.
#[test]
fn v31_within_range_bounds() {
    let (oldest, newest) = ProtocolVersion::supported_range_bounds();
    assert!(31 >= oldest, "v31 should be >= oldest supported");
    assert!(31 <= newest, "v31 should be <= newest supported");
}

/// Verifies v31 from supported index lookup.
#[test]
fn v31_from_supported_index() {
    // v31 should be at index 1 (in newest-to-oldest order: 32, 31, 30, 29, 28)
    let version = ProtocolVersion::from_supported_index(1);
    assert_eq!(version, Some(ProtocolVersion::V31));
}

// ============================================================================
// Protocol 31 Error Handling Tests
// ============================================================================

/// Verifies appropriate error when v31 is not available.
#[test]
fn v31_error_when_peer_too_old() {
    // Peer only supports v27 (too old)
    let result = select_highest_mutual([27]);
    assert!(result.is_err());
    match result.unwrap_err() {
        NegotiationError::UnsupportedVersion(v) => assert_eq!(v, 27),
        _ => panic!("expected UnsupportedVersion error"),
    }
}

/// Verifies no mutual protocol error when peer offers nothing.
#[test]
fn v31_error_on_empty_peer_list() {
    let result = select_highest_mutual(Vec::<u8>::new());
    assert!(result.is_err());
    match result.unwrap_err() {
        NegotiationError::NoMutualProtocol { peer_versions } => {
            assert!(peer_versions.is_empty());
        }
        _ => panic!("expected NoMutualProtocol error"),
    }
}

// ============================================================================
// Protocol 31 Real-World Scenarios
// ============================================================================

/// Simulates typical v31 client connecting to v31 server.
#[test]
fn v31_typical_client_server_both_v31() {
    // Client supports v28-v32, server supports v28-v31
    let client_versions = [28, 29, 30, 31, 32];
    let server_versions = [28, 29, 30, 31];

    // Simulate server selecting highest mutual
    let negotiated = select_highest_mutual(client_versions).unwrap();
    assert_eq!(negotiated, ProtocolVersion::V32);

    // Simulate client selecting highest mutual from server's perspective
    let negotiated = select_highest_mutual(server_versions).unwrap();
    assert_eq!(negotiated, ProtocolVersion::V31);
}

/// Simulates v31 client connecting to modern v32 server.
#[test]
fn v31_client_to_v32_server() {
    // Client supports up to v31, server supports up to v32
    let client_versions = [28, 29, 30, 31];
    let _server_supports_v32 = [28, 29, 30, 31, 32];

    // Negotiation should settle on v31 (highest mutual)
    let negotiated = select_highest_mutual(client_versions).unwrap();
    assert_eq!(negotiated, ProtocolVersion::V31);
}

/// Simulates v31 client connecting to legacy v29 server.
#[test]
fn v31_client_to_legacy_v29_server() {
    // Client supports v28-v31, server only supports v28-v29
    let server_versions = [28, 29];

    // Should downgrade to v29
    let negotiated = select_highest_mutual(server_versions).unwrap();
    assert_eq!(negotiated, ProtocolVersion::V29);
}

/// Simulates v31 client connecting to v30 server.
#[test]
fn v31_client_to_v30_server() {
    // Server supports up to v30
    let server_versions = [28, 29, 30];

    // Should negotiate v30
    let negotiated = select_highest_mutual(server_versions).unwrap();
    assert_eq!(negotiated, ProtocolVersion::V30);
}

/// Simulates v31 deployment with mixed version infrastructure.
#[test]
fn v31_mixed_version_infrastructure() {
    // Various servers in infrastructure support different versions
    let old_server = [28, 29];
    let medium_server = [28, 29, 30];
    let modern_server = [28, 29, 30, 31];
    let latest_server = [28, 29, 30, 31, 32];

    // v31 client should adapt to each
    assert_eq!(select_highest_mutual(old_server).unwrap(), ProtocolVersion::V29);
    assert_eq!(select_highest_mutual(medium_server).unwrap(), ProtocolVersion::V30);
    assert_eq!(select_highest_mutual(modern_server).unwrap(), ProtocolVersion::V31);
    assert_eq!(select_highest_mutual(latest_server).unwrap(), ProtocolVersion::V32);
}
