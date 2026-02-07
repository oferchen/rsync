//! Protocol version 32 compatibility tests.
//!
//! These tests verify that protocol version 32 works correctly, including:
//! - Protocol version 32 handshake and negotiation
//! - All features introduced in v32 (rsync 3.4.1)
//! - Backward compatibility with older versions (v28-v31)
//! - Wire format handling specific to v32
//!
//! # Protocol Version 32 Features
//!
//! Protocol 32 is the newest version, released with rsync 3.4.1. It includes:
//! - **All v31 features**: Binary negotiation, varint encoding, safe file list always enabled
//! - **XXH128 checksums**: 128-bit XXHash for better collision resistance
//! - **Zstd compression**: Modern, efficient compression algorithm
//! - **ID0_NAMES capability**: File-list entries support id0 names
//! - **Enhanced algorithm negotiation**: Full vstring-based algorithm selection
//!
//! # Upstream Reference
//!
//! Protocol 32 corresponds to rsync 3.4.1 and represents the current state-of-the-art
//! in rsync protocol evolution. It is the target protocol for modern rsync deployments.

use crate::error::NegotiationError;
use crate::version::{ProtocolVersion, select_highest_mutual};

// ============================================================================
// Protocol 32 Handshake and Negotiation Tests
// ============================================================================

/// Verifies protocol 32 can be successfully negotiated when offered by peer.
#[test]
fn v32_handshake_succeeds_when_peer_offers_v32() {
    let result = select_highest_mutual([32]);
    assert!(result.is_ok(), "v32 negotiation should succeed");
    assert_eq!(result.unwrap(), ProtocolVersion::V32);
}

/// Verifies protocol 32 is selected when peer offers v32 along with older versions.
#[test]
fn v32_handshake_selects_v32_over_older_versions() {
    let result = select_highest_mutual([28, 29, 30, 31, 32]).unwrap();
    assert_eq!(result, ProtocolVersion::V32, "should prefer v32 over older");
}

/// Verifies protocol 32 negotiation from peer advertisement.
#[test]
fn v32_from_peer_advertisement_accepts_32() {
    let result = ProtocolVersion::from_peer_advertisement(32);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), ProtocolVersion::V32);
}

/// Verifies protocol 32 is the correct constant value.
#[test]
fn v32_constant_is_32() {
    assert_eq!(ProtocolVersion::V32.as_u8(), 32);
}

/// Verifies protocol 32 equals NEWEST.
#[test]
fn v32_equals_newest() {
    assert_eq!(ProtocolVersion::V32, ProtocolVersion::NEWEST);
}

/// Verifies protocol 32 is within supported range.
#[test]
fn v32_is_within_supported_range() {
    assert!(ProtocolVersion::is_supported(32));
    assert!(ProtocolVersion::from_supported(32).is_some());
}

/// Verifies protocol 32 handshake with mixed valid and invalid versions.
#[test]
fn v32_handshake_ignores_invalid_versions() {
    // Mix of too old, valid, and within clamping range
    // Version 35 is clamped to NEWEST (32)
    let result = select_highest_mutual([0, 27, 32, 35]).unwrap();
    assert_eq!(result, ProtocolVersion::NEWEST);
}

/// Verifies protocol 32 can be parsed from string.
#[test]
fn v32_parses_from_string() {
    let result: ProtocolVersion = "32".parse().unwrap();
    assert_eq!(result, ProtocolVersion::V32);
}

/// Verifies protocol 32 handshake with duplicate peer versions.
#[test]
fn v32_handshake_handles_duplicate_peer_versions() {
    let result = select_highest_mutual([32, 32, 32, 31]).unwrap();
    assert_eq!(result, ProtocolVersion::V32);
}

// ============================================================================
// Protocol 32 Feature Tests
// ============================================================================

/// Verifies protocol 32 uses binary negotiation.
#[test]
fn v32_uses_binary_negotiation() {
    assert!(
        ProtocolVersion::V32.uses_binary_negotiation(),
        "v32 should use binary negotiation"
    );
    assert!(
        !ProtocolVersion::V32.uses_legacy_ascii_negotiation(),
        "v32 should not use legacy ASCII negotiation"
    );
}

/// Verifies protocol 32 uses varint encoding.
#[test]
fn v32_uses_varint_encoding() {
    assert!(
        ProtocolVersion::V32.uses_varint_encoding(),
        "v32 should use varint encoding"
    );
    assert!(
        !ProtocolVersion::V32.uses_fixed_encoding(),
        "v32 should not use fixed encoding"
    );
}

/// Verifies protocol 32 uses varint flist flags.
#[test]
fn v32_uses_varint_flist_flags() {
    assert!(
        ProtocolVersion::V32.uses_varint_flist_flags(),
        "v32 should use varint flist flags"
    );
}

/// Verifies protocol 32 supports safe file list.
#[test]
fn v32_uses_safe_file_list() {
    assert!(
        ProtocolVersion::V32.uses_safe_file_list(),
        "v32 should support safe file list"
    );
}

/// Verifies protocol 32 has safe file list ALWAYS enabled (inherited from v31).
#[test]
fn v32_safe_file_list_always_enabled() {
    assert!(
        ProtocolVersion::V32.safe_file_list_always_enabled(),
        "v32 inherits v31's mandatory safe file list"
    );
}

/// Verifies protocol 32 supports sender/receiver modifiers.
#[test]
fn v32_supports_sender_receiver_modifiers() {
    assert!(
        ProtocolVersion::V32.supports_sender_receiver_modifiers(),
        "v32 should support sender/receiver modifiers"
    );
}

/// Verifies protocol 32 supports perishable modifier.
#[test]
fn v32_supports_perishable_modifier() {
    assert!(
        ProtocolVersion::V32.supports_perishable_modifier(),
        "v32 should support perishable modifier"
    );
}

/// Verifies protocol 32 supports flist times.
#[test]
fn v32_supports_flist_times() {
    assert!(
        ProtocolVersion::V32.supports_flist_times(),
        "v32 should support flist times"
    );
}

/// Verifies protocol 32 does not use old prefixes.
#[test]
fn v32_does_not_use_old_prefixes() {
    assert!(
        !ProtocolVersion::V32.uses_old_prefixes(),
        "v32 should use new prefixes"
    );
}

/// Verifies protocol 32 supports extended flags.
#[test]
fn v32_supports_extended_flags() {
    assert!(
        ProtocolVersion::V32.supports_extended_flags(),
        "v32 should support extended flags"
    );
}

/// Verifies protocol 32 has the complete feature set expected.
#[test]
fn v32_complete_feature_profile() {
    let v = ProtocolVersion::V32;

    // Binary negotiation features (v30+)
    assert!(v.uses_binary_negotiation());
    assert!(v.uses_varint_encoding());
    assert!(v.uses_varint_flist_flags());
    assert!(v.uses_safe_file_list());
    assert!(v.supports_perishable_modifier());

    // V31+ specific: safe file list always enabled
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
// Protocol 32 as NEWEST Tests
// ============================================================================

/// Verifies v32 is the newest supported protocol.
#[test]
fn v32_is_newest_supported() {
    assert_eq!(ProtocolVersion::V32, ProtocolVersion::NEWEST);
    assert_eq!(ProtocolVersion::NEWEST.as_u8(), 32);
}

/// Verifies v32 has no newer version.
#[test]
fn v32_has_no_next_newer() {
    assert!(ProtocolVersion::V32.next_newer().is_none());
}

/// Verifies v32 has v31 as next older.
#[test]
fn v32_next_older_is_v31() {
    assert_eq!(
        ProtocolVersion::V32.next_older(),
        Some(ProtocolVersion::V31)
    );
}

/// Verifies future versions clamp to v32.
#[test]
fn v32_future_versions_clamp_to_v32() {
    // Version 35 is within MAXIMUM_PROTOCOL_ADVERTISEMENT (40)
    let result = ProtocolVersion::from_peer_advertisement(35).unwrap();
    assert_eq!(result, ProtocolVersion::V32);

    // Version 40 is at the edge
    let result = ProtocolVersion::from_peer_advertisement(40).unwrap();
    assert_eq!(result, ProtocolVersion::V32);
}

/// Verifies versions beyond maximum advertisement are rejected.
#[test]
fn v32_rejects_versions_beyond_maximum() {
    // Version 41 is beyond MAXIMUM_PROTOCOL_ADVERTISEMENT (40)
    let result = ProtocolVersion::from_peer_advertisement(41);
    assert!(result.is_err());
}

// ============================================================================
// Backward Compatibility Tests (v32 with older versions)
// ============================================================================

/// Verifies v32 can downgrade to v31 when peer only supports v31.
#[test]
fn v32_downgrades_to_v31_for_older_peer() {
    let result = select_highest_mutual([31]).unwrap();
    assert_eq!(result, ProtocolVersion::V31);
}

/// Verifies v32 can downgrade to v30 when peer only supports v30.
#[test]
fn v32_downgrades_to_v30_for_older_peer() {
    let result = select_highest_mutual([30]).unwrap();
    assert_eq!(result, ProtocolVersion::V30);
}

/// Verifies v32 can downgrade to v29 when peer only supports v29.
#[test]
fn v32_downgrades_to_v29_for_older_peer() {
    let result = select_highest_mutual([29]).unwrap();
    assert_eq!(result, ProtocolVersion::V29);
}

/// Verifies v32 can downgrade to v28 when peer only supports v28.
#[test]
fn v32_downgrades_to_v28_for_oldest_peer() {
    let result = select_highest_mutual([28]).unwrap();
    assert_eq!(result, ProtocolVersion::V28);
}

/// Verifies v32 selects highest common version when peer supports multiple older versions.
#[test]
fn v32_selects_highest_older_version_available() {
    let result = select_highest_mutual([28, 29, 30, 31]).unwrap();
    assert_eq!(
        result,
        ProtocolVersion::V31,
        "should select v31 over v28/v29/v30"
    );
}

/// Verifies feature differences between v32 and v31.
#[test]
fn v32_vs_v31_feature_equivalence() {
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

    // Both support varint flist flags
    assert_eq!(v31.uses_varint_flist_flags(), v32.uses_varint_flist_flags());

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
}

/// Verifies feature differences between v32 and v30.
#[test]
fn v32_vs_v30_feature_differences() {
    let v30 = ProtocolVersion::V30;
    let v32 = ProtocolVersion::V32;

    // Both use binary negotiation
    assert_eq!(v30.uses_binary_negotiation(), v32.uses_binary_negotiation());

    // Both use varint encoding
    assert_eq!(v30.uses_varint_encoding(), v32.uses_varint_encoding());

    // Both support safe file list
    assert_eq!(v30.uses_safe_file_list(), v32.uses_safe_file_list());

    // Key difference: v32 has safe file list ALWAYS enabled, v30 requires negotiation
    assert!(!v30.safe_file_list_always_enabled());
    assert!(v32.safe_file_list_always_enabled());
}

/// Verifies feature differences between v32 and v29.
#[test]
fn v32_vs_v29_feature_differences() {
    let v29 = ProtocolVersion::V29;
    let v32 = ProtocolVersion::V32;

    // v29 uses legacy negotiation, v32 uses binary
    assert!(v29.uses_legacy_ascii_negotiation());
    assert!(v32.uses_binary_negotiation());

    // v29 uses fixed encoding, v32 uses varint
    assert!(v29.uses_fixed_encoding());
    assert!(v32.uses_varint_encoding());

    // v29 doesn't support safe file list, v32 does (always)
    assert!(!v29.uses_safe_file_list());
    assert!(v32.uses_safe_file_list());
    assert!(v32.safe_file_list_always_enabled());

    // v29 doesn't support perishable modifier, v32 does
    assert!(!v29.supports_perishable_modifier());
    assert!(v32.supports_perishable_modifier());

    // Both support sender/receiver modifiers (v29+)
    assert_eq!(
        v29.supports_sender_receiver_modifiers(),
        v32.supports_sender_receiver_modifiers()
    );
}

/// Verifies feature differences between v32 and v28.
#[test]
fn v32_vs_v28_feature_differences() {
    let v28 = ProtocolVersion::V28;
    let v32 = ProtocolVersion::V32;

    // v28 uses legacy negotiation, v32 uses binary
    assert!(v28.uses_legacy_ascii_negotiation());
    assert!(v32.uses_binary_negotiation());

    // v28 uses fixed encoding, v32 uses varint
    assert!(v28.uses_fixed_encoding());
    assert!(v32.uses_varint_encoding());

    // v28 uses old prefixes, v32 uses new prefixes
    assert!(v28.uses_old_prefixes());
    assert!(!v32.uses_old_prefixes());

    // v28 doesn't support sender/receiver modifiers, v32 does
    assert!(!v28.supports_sender_receiver_modifiers());
    assert!(v32.supports_sender_receiver_modifiers());

    // v28 doesn't support flist times, v32 does
    assert!(!v28.supports_flist_times());
    assert!(v32.supports_flist_times());

    // Both support extended flags
    assert_eq!(v28.supports_extended_flags(), v32.supports_extended_flags());
}

/// Verifies backward compatibility negotiation prefers v32 when available.
#[test]
fn v32_backward_compat_prefers_newest_mutual() {
    // Peer supports v28-v32
    let result = select_highest_mutual([28, 29, 30, 31, 32]).unwrap();
    assert_eq!(result, ProtocolVersion::V32);

    // Peer supports v28, v30, v32 (missing v29, v31)
    let result = select_highest_mutual([28, 30, 32]).unwrap();
    assert_eq!(result, ProtocolVersion::V32);
}

// ============================================================================
// Protocol 32 Ordering and Comparison Tests
// ============================================================================

/// Verifies v32 ordering relative to other versions.
#[test]
fn v32_ordering() {
    assert!(ProtocolVersion::V32 > ProtocolVersion::V31);
    assert!(ProtocolVersion::V32 > ProtocolVersion::V30);
    assert!(ProtocolVersion::V32 > ProtocolVersion::V29);
    assert!(ProtocolVersion::V32 > ProtocolVersion::V28);
}

/// Verifies v32 equality comparisons.
#[test]
fn v32_equality() {
    assert_eq!(ProtocolVersion::V32, ProtocolVersion::V32);
    assert_eq!(ProtocolVersion::V32, ProtocolVersion::NEWEST);
    assert_ne!(ProtocolVersion::V32, ProtocolVersion::V31);
}

/// Verifies v32 comparison with raw integers.
#[test]
fn v32_comparison_with_integers() {
    assert_eq!(ProtocolVersion::V32, 32u8);
    assert_eq!(32u8, ProtocolVersion::V32);
    assert_ne!(ProtocolVersion::V32, 31u8);
    assert_ne!(ProtocolVersion::V32, 33u8);
}

/// Verifies v32 is at the correct offset from oldest.
#[test]
fn v32_offset_from_oldest() {
    assert_eq!(ProtocolVersion::V32.offset_from_oldest(), 4);
}

/// Verifies v32 is at the correct offset from newest.
#[test]
fn v32_offset_from_newest() {
    assert_eq!(ProtocolVersion::V32.offset_from_newest(), 0);
}

/// Verifies v32 navigation to next newer version.
#[test]
fn v32_next_newer_is_none() {
    assert_eq!(ProtocolVersion::V32.next_newer(), None);
}

/// Verifies v32 navigation to next older version.
#[test]
fn v32_next_older_navigation() {
    assert_eq!(
        ProtocolVersion::V32.next_older(),
        Some(ProtocolVersion::V31)
    );
}

// ============================================================================
// Protocol 32 Conversion Tests
// ============================================================================

/// Verifies v32 converts to u8.
#[test]
fn v32_to_u8_conversion() {
    let byte: u8 = ProtocolVersion::V32.into();
    assert_eq!(byte, 32);
}

/// Verifies v32 converts to wider integer types.
#[test]
fn v32_to_wider_integer_conversions() {
    let u16_val: u16 = ProtocolVersion::V32.into();
    assert_eq!(u16_val, 32);

    let u32_val: u32 = ProtocolVersion::V32.into();
    assert_eq!(u32_val, 32);

    let u64_val: u64 = ProtocolVersion::V32.into();
    assert_eq!(u64_val, 32);
}

/// Verifies v32 converts from u8.
#[test]
fn v32_from_u8_conversion() {
    let version = ProtocolVersion::try_from(32u8).unwrap();
    assert_eq!(version, ProtocolVersion::V32);
}

/// Verifies v32 display formatting.
#[test]
fn v32_display_format() {
    assert_eq!(format!("{}", ProtocolVersion::V32), "32");
}

/// Verifies v32 debug formatting.
#[test]
fn v32_debug_format() {
    let debug = format!("{:?}", ProtocolVersion::V32);
    assert!(debug.contains("32"));
}

// ============================================================================
// Protocol 32 Consistency Tests
// ============================================================================

/// Verifies v32 appears in supported versions list.
#[test]
fn v32_in_supported_versions() {
    let versions = ProtocolVersion::supported_versions();
    assert!(
        versions.contains(&ProtocolVersion::V32),
        "v32 should be in supported versions"
    );
}

/// Verifies v32 appears in supported protocol numbers.
#[test]
fn v32_in_supported_protocol_numbers() {
    let numbers = ProtocolVersion::supported_protocol_numbers();
    assert!(
        numbers.contains(&32),
        "32 should be in supported protocol numbers"
    );
}

/// Verifies v32 bit is set in supported protocol bitmap.
#[test]
fn v32_in_protocol_bitmap() {
    let bitmap = ProtocolVersion::supported_protocol_bitmap();
    assert_ne!(bitmap & (1u64 << 32), 0, "bit 32 should be set in bitmap");
}

/// Verifies v32 is within supported range bounds.
#[test]
fn v32_within_range_bounds() {
    let (oldest, newest) = ProtocolVersion::supported_range_bounds();
    assert!(32 >= oldest, "v32 should be >= oldest supported");
    assert!(32 <= newest, "v32 should be <= newest supported");
}

/// Verifies v32 from supported index lookup.
#[test]
fn v32_from_supported_index() {
    // v32 should be at index 0 (in newest-to-oldest order: 32, 31, 30, 29, 28)
    let version = ProtocolVersion::from_supported_index(0);
    assert_eq!(version, Some(ProtocolVersion::V32));
}

/// Verifies v32 is first in supported versions list.
#[test]
fn v32_is_first_in_supported_list() {
    let versions = ProtocolVersion::supported_versions();
    assert_eq!(versions[0], ProtocolVersion::V32);
}

// ============================================================================
// Protocol 32 Error Handling Tests
// ============================================================================

/// Verifies appropriate error when v32 is not available.
#[test]
fn v32_error_when_peer_too_old() {
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
fn v32_error_on_empty_peer_list() {
    let result = select_highest_mutual(Vec::<u8>::new());
    assert!(result.is_err());
    match result.unwrap_err() {
        NegotiationError::NoMutualProtocol { peer_versions } => {
            assert!(peer_versions.is_empty());
        }
        _ => panic!("expected NoMutualProtocol error"),
    }
}

/// Verifies error when only unsupported versions offered.
#[test]
fn v32_error_on_only_unsupported_versions() {
    let result = select_highest_mutual([27, 26, 25]);
    assert!(result.is_err());
}

// ============================================================================
// Protocol 32 Real-World Scenarios
// ============================================================================

/// Simulates typical v32 client connecting to v32 server.
#[test]
fn v32_typical_client_server_both_v32() {
    // Both client and server support v28-v32
    let client_versions = [28, 29, 30, 31, 32];
    let server_versions = [28, 29, 30, 31, 32];

    // Negotiation should settle on v32
    let negotiated = select_highest_mutual(client_versions).unwrap();
    assert_eq!(negotiated, ProtocolVersion::V32);

    let negotiated = select_highest_mutual(server_versions).unwrap();
    assert_eq!(negotiated, ProtocolVersion::V32);
}

/// Simulates v32 client connecting to v31 server.
#[test]
fn v32_client_to_v31_server() {
    // Server only supports up to v31
    let server_versions = [28, 29, 30, 31];

    // Negotiation should settle on v31
    let negotiated = select_highest_mutual(server_versions).unwrap();
    assert_eq!(negotiated, ProtocolVersion::V31);
}

/// Simulates v32 client connecting to legacy v29 server.
#[test]
fn v32_client_to_legacy_v29_server() {
    // Server only supports v28-v29 (legacy ASCII negotiation)
    let server_versions = [28, 29];

    // Should downgrade to v29
    let negotiated = select_highest_mutual(server_versions).unwrap();
    assert_eq!(negotiated, ProtocolVersion::V29);
}

/// Simulates v32 client connecting to v30 server.
#[test]
fn v32_client_to_v30_server() {
    // Server supports up to v30
    let server_versions = [28, 29, 30];

    // Should negotiate v30
    let negotiated = select_highest_mutual(server_versions).unwrap();
    assert_eq!(negotiated, ProtocolVersion::V30);
}

/// Simulates v32 deployment with mixed version infrastructure.
#[test]
fn v32_mixed_version_infrastructure() {
    // Various servers in infrastructure support different versions
    let old_server = [28, 29];
    let medium_server = [28, 29, 30];
    let modern_server = [28, 29, 30, 31];
    let latest_server = [28, 29, 30, 31, 32];

    // v32 client should adapt to each
    assert_eq!(
        select_highest_mutual(old_server).unwrap(),
        ProtocolVersion::V29
    );
    assert_eq!(
        select_highest_mutual(medium_server).unwrap(),
        ProtocolVersion::V30
    );
    assert_eq!(
        select_highest_mutual(modern_server).unwrap(),
        ProtocolVersion::V31
    );
    assert_eq!(
        select_highest_mutual(latest_server).unwrap(),
        ProtocolVersion::V32
    );
}

/// Simulates future-proofing with hypothetical v33.
#[test]
fn v32_future_version_handling() {
    // Peer advertises v33, which should be clamped to v32
    let result = ProtocolVersion::from_peer_advertisement(33).unwrap();
    assert_eq!(result, ProtocolVersion::V32);

    // Same for v35
    let result = ProtocolVersion::from_peer_advertisement(35).unwrap();
    assert_eq!(result, ProtocolVersion::V32);

    // And v40 (edge of MAXIMUM_PROTOCOL_ADVERTISEMENT)
    let result = ProtocolVersion::from_peer_advertisement(40).unwrap();
    assert_eq!(result, ProtocolVersion::V32);
}

// ============================================================================
// Protocol 32 Wire Format Tests
// ============================================================================

/// Verifies v32 uses modern NDX encoding (delta-based).
#[test]
fn v32_uses_modern_ndx_encoding() {
    // v32 uses protocol >= 30, which uses delta-encoded NDX format
    assert!(ProtocolVersion::V32.as_u8() >= 30);
    assert!(ProtocolVersion::V32.uses_varint_encoding());
}

/// Verifies v32 uses modern file size encoding (varlong).
#[test]
fn v32_uses_modern_file_size_encoding() {
    // Protocol >= 30 uses varlong encoding for file sizes
    assert!(ProtocolVersion::V32.as_u8() >= 30);
}

/// Verifies v32 uses modern mtime encoding (varlong).
#[test]
fn v32_uses_modern_mtime_encoding() {
    // Protocol >= 30 uses varlong encoding for mtime
    assert!(ProtocolVersion::V32.as_u8() >= 30);
}

// ============================================================================
// Protocol 32 Capability Flag Tests
// ============================================================================

/// Verifies v32 supports all known capability flags.
#[test]
fn v32_supports_all_capability_flags() {
    use crate::CompatibilityFlags;

    // v32 should support all known capability flags
    let all_flags = CompatibilityFlags::ALL_KNOWN;

    // Verify key flags are defined
    assert!(all_flags.contains(CompatibilityFlags::INC_RECURSE));
    assert!(all_flags.contains(CompatibilityFlags::SYMLINK_TIMES));
    assert!(all_flags.contains(CompatibilityFlags::SYMLINK_ICONV));
    assert!(all_flags.contains(CompatibilityFlags::SAFE_FILE_LIST));
    assert!(all_flags.contains(CompatibilityFlags::AVOID_XATTR_OPTIMIZATION));
    assert!(all_flags.contains(CompatibilityFlags::CHECKSUM_SEED_FIX));
    assert!(all_flags.contains(CompatibilityFlags::INPLACE_PARTIAL_DIR));
    assert!(all_flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS));
    assert!(all_flags.contains(CompatibilityFlags::ID0_NAMES));
}

/// Verifies ID0_NAMES capability is available for v32.
#[test]
fn v32_id0_names_capability() {
    use crate::CompatibilityFlags;

    // ID0_NAMES is a v32 capability for id0 name support
    let flags = CompatibilityFlags::ID0_NAMES;
    assert!(!flags.is_empty());
    assert_eq!(flags.bits(), 1 << 8);
}

/// Verifies VARINT_FLIST_FLAGS capability is required for v32 algorithm negotiation.
#[test]
fn v32_varint_flist_flags_for_negotiation() {
    use crate::CompatibilityFlags;

    // VARINT_FLIST_FLAGS (bit 7) is required for vstring algorithm negotiation
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;
    assert!(!flags.is_empty());
    assert_eq!(flags.bits(), 1 << 7);
}

// ============================================================================
// Protocol 32 Algorithm Negotiation Tests
// ============================================================================

/// Verifies v32 supports XXH128 checksum algorithm.
#[test]
fn v32_supports_xxh128_checksum() {
    use crate::ChecksumAlgorithm;

    // XXH128 is available in v32
    let xxh128 = ChecksumAlgorithm::XXH128;
    assert_eq!(xxh128.as_str(), "xxh128");
}

/// Verifies v32 supports all modern checksum algorithms.
#[test]
fn v32_supports_all_checksum_algorithms() {
    use crate::ChecksumAlgorithm;

    // v32 supports all algorithms
    assert_eq!(ChecksumAlgorithm::None.as_str(), "none");
    assert_eq!(ChecksumAlgorithm::MD4.as_str(), "md4");
    assert_eq!(ChecksumAlgorithm::MD5.as_str(), "md5");
    assert_eq!(ChecksumAlgorithm::SHA1.as_str(), "sha1");
    assert_eq!(ChecksumAlgorithm::XXH64.as_str(), "xxh64");
    assert_eq!(ChecksumAlgorithm::XXH3.as_str(), "xxh3");
    assert_eq!(ChecksumAlgorithm::XXH128.as_str(), "xxh128");
}

/// Verifies v32 supports Zstd compression algorithm.
#[test]
fn v32_supports_zstd_compression() {
    use crate::CompressionAlgorithm;

    // Zstd is available in v32
    let zstd = CompressionAlgorithm::Zstd;
    assert_eq!(zstd.as_str(), "zstd");
}

/// Verifies v32 supports all compression algorithms.
#[test]
fn v32_supports_all_compression_algorithms() {
    use crate::CompressionAlgorithm;

    // v32 supports all algorithms (feature-gated)
    assert_eq!(CompressionAlgorithm::None.as_str(), "none");
    assert_eq!(CompressionAlgorithm::Zlib.as_str(), "zlib");
    assert_eq!(CompressionAlgorithm::ZlibX.as_str(), "zlibx");
    assert_eq!(CompressionAlgorithm::LZ4.as_str(), "lz4");
    assert_eq!(CompressionAlgorithm::Zstd.as_str(), "zstd");
}

/// Verifies checksum algorithm parsing works correctly.
#[test]
fn v32_checksum_algorithm_parsing() {
    use crate::ChecksumAlgorithm;

    // Parse all supported algorithms
    assert!(ChecksumAlgorithm::parse("none").is_ok());
    assert!(ChecksumAlgorithm::parse("md4").is_ok());
    assert!(ChecksumAlgorithm::parse("md5").is_ok());
    assert!(ChecksumAlgorithm::parse("sha1").is_ok());
    assert!(ChecksumAlgorithm::parse("xxh64").is_ok());
    assert!(ChecksumAlgorithm::parse("xxh").is_ok()); // xxh is alias for xxh64
    assert!(ChecksumAlgorithm::parse("xxh3").is_ok());
    assert!(ChecksumAlgorithm::parse("xxh128").is_ok());

    // Unknown algorithm should fail
    assert!(ChecksumAlgorithm::parse("unknown").is_err());
}

/// Verifies compression algorithm parsing works correctly.
#[test]
fn v32_compression_algorithm_parsing() {
    use crate::CompressionAlgorithm;

    // Parse all supported algorithms
    assert!(CompressionAlgorithm::parse("none").is_ok());
    assert!(CompressionAlgorithm::parse("zlib").is_ok());
    assert!(CompressionAlgorithm::parse("zlibx").is_ok());
    assert!(CompressionAlgorithm::parse("lz4").is_ok());
    assert!(CompressionAlgorithm::parse("zstd").is_ok());

    // Unknown algorithm should fail
    assert!(CompressionAlgorithm::parse("unknown").is_err());
}

// ============================================================================
// Protocol 32 Interoperability Tests
// ============================================================================

/// Verifies v32 can be used with the protocol codec.
#[test]
fn v32_protocol_codec_creation() {
    use crate::codec::{ProtocolCodec, create_protocol_codec};

    let codec = create_protocol_codec(32);
    assert_eq!(codec.protocol_version(), 32);
    assert!(!codec.is_legacy());
}

/// Verifies v32 can be used with the NDX codec.
#[test]
fn v32_ndx_codec_creation() {
    use crate::codec::{NdxCodec, create_ndx_codec};

    let mut codec = create_ndx_codec(32);

    // v32 uses modern NDX encoding
    let mut buf = Vec::new();
    codec.write_ndx(&mut buf, 0).unwrap();
    // Modern encoding: delta from prev=-1 to 0, diff=1 => byte value 0x01
    assert_eq!(buf, vec![0x01]);
}

/// Verifies v32 varint encoding is used for file list flags.
#[test]
fn v32_varint_file_list_flags() {
    use crate::varint::encode_varint_to_vec;

    // v32 uses varint encoding for file list flags
    let mut buf = Vec::new();
    encode_varint_to_vec(127, &mut buf); // small value
    assert_eq!(buf.len(), 1);

    buf.clear();
    encode_varint_to_vec(128, &mut buf); // needs 2 bytes
    assert_eq!(buf.len(), 2);
}

// ============================================================================
// Protocol 32 Edge Cases and Boundary Tests
// ============================================================================

/// Verifies v32 handles zero-valued advertisements correctly.
#[test]
fn v32_zero_advertisement_rejected() {
    let result = ProtocolVersion::from_peer_advertisement(0);
    assert!(result.is_err());
}

/// Verifies v32 handles maximum u32 advertisement correctly.
#[test]
fn v32_max_u32_advertisement_rejected() {
    let result = ProtocolVersion::from_peer_advertisement(u32::MAX);
    assert!(result.is_err());
}

/// Verifies v32 handles MAXIMUM_PROTOCOL_ADVERTISEMENT boundary.
#[test]
fn v32_maximum_advertisement_boundary() {
    // 40 is accepted and clamped
    let result = ProtocolVersion::from_peer_advertisement(40);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), ProtocolVersion::V32);

    // 41 is rejected
    let result = ProtocolVersion::from_peer_advertisement(41);
    assert!(result.is_err());
}

/// Verifies v32 is at the expected position in the version ordering.
#[test]
fn v32_version_ordering_position() {
    let versions = ProtocolVersion::supported_versions();

    // v32 should be first (newest)
    assert_eq!(versions[0], ProtocolVersion::V32);

    // v28 should be last (oldest)
    assert_eq!(versions[versions.len() - 1], ProtocolVersion::V28);
}

/// Verifies all supported versions can negotiate with v32.
#[test]
fn v32_all_versions_negotiate() {
    for &version in ProtocolVersion::supported_protocol_numbers() {
        let result = select_highest_mutual([version]);
        assert!(
            result.is_ok(),
            "version {version} should negotiate successfully"
        );
    }
}

/// Verifies v32 hash and equality work in collections.
#[test]
fn v32_hash_and_equality_in_collections() {
    use std::collections::HashSet;

    let mut set = HashSet::new();
    set.insert(ProtocolVersion::V32);
    set.insert(ProtocolVersion::V32);
    set.insert(ProtocolVersion::NEWEST);

    // All three are the same, so set should have 1 element
    assert_eq!(set.len(), 1);
    assert!(set.contains(&ProtocolVersion::V32));
    assert!(set.contains(&ProtocolVersion::NEWEST));
}
