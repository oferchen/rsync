//! Comprehensive protocol version selection and negotiation tests.
//!
//! This module provides exhaustive testing of protocol version negotiation,
//! covering:
//! - Version negotiation between client and server (protocol 27-32)
//! - Feature capability detection based on negotiated version
//! - Backwards compatibility with older rsync implementations
//! - Error handling for unsupported and malformed versions
//!
//! # Protocol Version Overview
//!
//! | Version | Features                                            | Negotiation |
//! |---------|-----------------------------------------------------|-------------|
//! | 27      | Unsupported (below minimum)                         | N/A         |
//! | 28      | Oldest supported, fixed encoding, old prefixes      | ASCII       |
//! | 29      | Sender/receiver modifiers, flist times              | ASCII       |
//! | 30      | Binary negotiation, varint, compatibility flags     | Binary      |
//! | 31      | Safe file list always enabled                       | Binary      |
//! | 32      | Current newest supported                            | Binary      |
//!
//! # Upstream Compatibility
//!
//! - rsync 2.6.4: Protocol 28
//! - rsync 3.0.x: Protocol 30
//! - rsync 3.1.x: Protocol 31
//! - rsync 3.4.x: Protocol 32

use protocol::{
    CompatibilityFlags, NegotiationError, ProtocolVersion, ProtocolVersionAdvertisement,
    SUPPORTED_PROTOCOLS, select_highest_mutual,
};

// ============================================================================
// Test Helper
// ============================================================================

/// Wrapper to implement ProtocolVersionAdvertisement for testing.
#[derive(Clone, Copy, Debug)]
struct VersionAd(u32);

impl ProtocolVersionAdvertisement for VersionAd {
    fn into_advertised_version(self) -> u32 {
        self.0
    }
}

// ============================================================================
// Protocol Version 27 Tests (Unsupported)
// ============================================================================

/// Test that protocol 27 is consistently rejected.
#[test]
fn version_27_always_rejected() {
    let result = select_highest_mutual([VersionAd(27)]);
    assert!(result.is_err(), "Protocol 27 must be rejected");

    match result.unwrap_err() {
        NegotiationError::UnsupportedVersion(v) => {
            assert_eq!(v, 27, "Error should contain version 27");
        }
        other => panic!("Expected UnsupportedVersion, got: {other:?}"),
    }
}

/// Test that offering both 27 and a supported version succeeds with supported version.
#[test]
fn version_27_with_supported_selects_supported() {
    // 27 + 28 -> select 28
    let result = select_highest_mutual([VersionAd(27), VersionAd(28)]).unwrap();
    assert_eq!(result.as_u8(), 28);

    // 27 + 30 -> select 30
    let result = select_highest_mutual([VersionAd(27), VersionAd(30)]).unwrap();
    assert_eq!(result.as_u8(), 30);

    // 27 + 32 -> select 32
    let result = select_highest_mutual([VersionAd(27), VersionAd(32)]).unwrap();
    assert_eq!(result.as_u8(), 32);
}

/// Test that multiple unsupported old versions report the oldest.
#[test]
fn multiple_old_versions_reports_oldest() {
    let result = select_highest_mutual([VersionAd(27), VersionAd(26), VersionAd(25)]);
    match result.unwrap_err() {
        NegotiationError::UnsupportedVersion(v) => {
            assert_eq!(v, 25, "Should report oldest rejected version");
        }
        other => panic!("Expected UnsupportedVersion, got: {other:?}"),
    }
}

// ============================================================================
// Protocol Version 28 Tests
// ============================================================================

/// Verify protocol 28 is the oldest supported version.
#[test]
fn version_28_is_oldest_supported() {
    assert_eq!(ProtocolVersion::OLDEST, ProtocolVersion::V28);
    assert_eq!(ProtocolVersion::OLDEST.as_u8(), 28);
}

/// Test negotiation to protocol 28 (minimal feature set).
#[test]
fn version_28_negotiation() {
    let result = select_highest_mutual([VersionAd(28)]).unwrap();
    assert_eq!(result, ProtocolVersion::V28);
}

/// Verify protocol 28 feature capabilities.
#[test]
fn version_28_capabilities() {
    let v = ProtocolVersion::V28;

    // Negotiation style
    assert!(
        v.uses_legacy_ascii_negotiation(),
        "v28 uses ASCII negotiation"
    );
    assert!(
        !v.uses_binary_negotiation(),
        "v28 does not use binary negotiation"
    );

    // Encoding
    assert!(v.uses_fixed_encoding(), "v28 uses fixed encoding");
    assert!(!v.uses_varint_encoding(), "v28 does not use varint");

    // Features NOT available in v28
    assert!(
        !v.supports_sender_receiver_modifiers(),
        "v28 lacks s/r modifiers"
    );
    assert!(!v.supports_flist_times(), "v28 lacks flist times");
    assert!(
        !v.supports_perishable_modifier(),
        "v28 lacks perishable modifier"
    );

    // Features available in v28
    assert!(v.uses_old_prefixes(), "v28 uses old prefixes");
    assert!(v.supports_extended_flags(), "v28 supports extended flags");

    // Safe file list NOT available in v28
    assert!(!v.uses_safe_file_list(), "v28 lacks safe file list");
    assert!(
        !v.safe_file_list_always_enabled(),
        "v28 lacks always-on safe flist"
    );
}

// ============================================================================
// Protocol Version 29 Tests
// ============================================================================

/// Test negotiation to protocol 29.
#[test]
fn version_29_negotiation() {
    let result = select_highest_mutual([VersionAd(29)]).unwrap();
    assert_eq!(result, ProtocolVersion::V29);
}

/// Verify protocol 29 feature capabilities (last ASCII negotiation version).
#[test]
fn version_29_capabilities() {
    let v = ProtocolVersion::V29;

    // Negotiation style (still ASCII)
    assert!(
        v.uses_legacy_ascii_negotiation(),
        "v29 uses ASCII negotiation"
    );
    assert!(
        !v.uses_binary_negotiation(),
        "v29 does not use binary negotiation"
    );

    // Encoding (still fixed)
    assert!(v.uses_fixed_encoding(), "v29 uses fixed encoding");
    assert!(!v.uses_varint_encoding(), "v29 does not use varint");

    // Features added in v29
    assert!(
        v.supports_sender_receiver_modifiers(),
        "v29 adds s/r modifiers"
    );
    assert!(v.supports_flist_times(), "v29 adds flist times");

    // Features NOT available in v29
    assert!(
        !v.supports_perishable_modifier(),
        "v29 lacks perishable modifier"
    );
    assert!(!v.uses_safe_file_list(), "v29 lacks safe file list");

    // Old prefixes deprecated in v29
    assert!(!v.uses_old_prefixes(), "v29 uses new prefixes");
}

/// Test that v29 is the last version using ASCII negotiation.
#[test]
fn version_29_is_last_ascii_negotiation() {
    assert!(ProtocolVersion::V29.uses_legacy_ascii_negotiation());
    assert!(!ProtocolVersion::V30.uses_legacy_ascii_negotiation());

    // Verify the boundary
    let v29 = ProtocolVersion::V29;
    let v30 = ProtocolVersion::V30;

    assert!(v29.uses_legacy_ascii_negotiation());
    assert!(v30.uses_binary_negotiation());
    assert!(!v29.uses_binary_negotiation());
    assert!(!v30.uses_legacy_ascii_negotiation());
}

// ============================================================================
// Protocol Version 30 Tests
// ============================================================================

/// Test negotiation to protocol 30.
#[test]
fn version_30_negotiation() {
    let result = select_highest_mutual([VersionAd(30)]).unwrap();
    assert_eq!(result, ProtocolVersion::V30);
}

/// Verify protocol 30 feature capabilities (first binary negotiation version).
#[test]
fn version_30_capabilities() {
    let v = ProtocolVersion::V30;

    // Binary negotiation introduced in v30
    assert!(
        !v.uses_legacy_ascii_negotiation(),
        "v30 uses binary negotiation"
    );
    assert!(v.uses_binary_negotiation(), "v30 uses binary negotiation");

    // Varint encoding introduced in v30
    assert!(!v.uses_fixed_encoding(), "v30 does not use fixed encoding");
    assert!(v.uses_varint_encoding(), "v30 uses varint encoding");

    // Features added in v30
    assert!(
        v.supports_perishable_modifier(),
        "v30 adds perishable modifier"
    );
    assert!(v.uses_safe_file_list(), "v30 adds safe file list");
    assert!(v.uses_varint_flist_flags(), "v30 adds varint flist flags");

    // Features carried forward from v29
    assert!(
        v.supports_sender_receiver_modifiers(),
        "v30 has s/r modifiers"
    );
    assert!(v.supports_flist_times(), "v30 has flist times");

    // Safe file list not always-on in v30
    assert!(
        !v.safe_file_list_always_enabled(),
        "v30 safe flist is optional"
    );
}

/// Test that v30 is the first version with binary negotiation.
#[test]
fn version_30_is_first_binary_negotiation() {
    assert_eq!(
        ProtocolVersion::BINARY_NEGOTIATION_INTRODUCED,
        ProtocolVersion::V30
    );
    assert!(ProtocolVersion::V30.uses_binary_negotiation());
    assert!(!ProtocolVersion::V29.uses_binary_negotiation());
}

/// Test compatibility flags are relevant for v30+.
#[test]
fn version_30_compatibility_flags() {
    // Compatibility flags encode/decode works for v30+
    let flags = CompatibilityFlags::INC_RECURSE
        | CompatibilityFlags::SAFE_FILE_LIST
        | CompatibilityFlags::SYMLINK_TIMES;

    let mut buf = Vec::new();
    flags.encode_to_vec(&mut buf).expect("encode");
    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).expect("decode");
    assert_eq!(decoded, flags);
}

// ============================================================================
// Protocol Version 31 Tests
// ============================================================================

/// Test negotiation to protocol 31.
#[test]
fn version_31_negotiation() {
    let result = select_highest_mutual([VersionAd(31)]).unwrap();
    assert_eq!(result, ProtocolVersion::V31);
}

/// Verify protocol 31 feature capabilities.
#[test]
fn version_31_capabilities() {
    let v = ProtocolVersion::V31;

    // Inherits from v30
    assert!(v.uses_binary_negotiation(), "v31 uses binary negotiation");
    assert!(v.uses_varint_encoding(), "v31 uses varint encoding");
    assert!(v.uses_safe_file_list(), "v31 has safe file list");

    // New in v31: safe file list always enabled
    assert!(
        v.safe_file_list_always_enabled(),
        "v31 safe flist always on"
    );
}

/// Test safe file list is always enabled in v31+.
#[test]
fn version_31_safe_file_list_always_enabled() {
    assert!(!ProtocolVersion::V30.safe_file_list_always_enabled());
    assert!(ProtocolVersion::V31.safe_file_list_always_enabled());
    assert!(ProtocolVersion::V32.safe_file_list_always_enabled());
}

// ============================================================================
// Protocol Version 32 Tests
// ============================================================================

/// Verify protocol 32 is the newest supported version.
#[test]
fn version_32_is_newest_supported() {
    assert_eq!(ProtocolVersion::NEWEST, ProtocolVersion::V32);
    assert_eq!(ProtocolVersion::NEWEST.as_u8(), 32);
}

/// Test negotiation to protocol 32.
#[test]
fn version_32_negotiation() {
    let result = select_highest_mutual([VersionAd(32)]).unwrap();
    assert_eq!(result, ProtocolVersion::V32);
    assert_eq!(result, ProtocolVersion::NEWEST);
}

/// Verify protocol 32 feature capabilities.
#[test]
fn version_32_capabilities() {
    let v = ProtocolVersion::V32;

    // All modern features enabled
    assert!(v.uses_binary_negotiation(), "v32 uses binary negotiation");
    assert!(v.uses_varint_encoding(), "v32 uses varint encoding");
    assert!(v.uses_safe_file_list(), "v32 has safe file list");
    assert!(
        v.safe_file_list_always_enabled(),
        "v32 safe flist always on"
    );
    assert!(v.uses_varint_flist_flags(), "v32 has varint flist flags");

    // All modifiers supported
    assert!(
        v.supports_sender_receiver_modifiers(),
        "v32 has s/r modifiers"
    );
    assert!(
        v.supports_perishable_modifier(),
        "v32 has perishable modifier"
    );

    // All features supported
    assert!(v.supports_extended_flags(), "v32 supports extended flags");
    assert!(v.supports_flist_times(), "v32 has flist times");
    assert!(!v.uses_old_prefixes(), "v32 uses new prefixes");
}

// ============================================================================
// Version Selection Algorithm Tests
// ============================================================================

/// Test that the highest mutual version is always selected.
#[test]
fn selects_highest_mutual_version() {
    // Single version
    for &version in &SUPPORTED_PROTOCOLS {
        let result = select_highest_mutual([VersionAd(u32::from(version))]).unwrap();
        assert_eq!(result.as_u8(), version);
    }

    // Multiple versions (ordered)
    let result = select_highest_mutual([VersionAd(28), VersionAd(29), VersionAd(30)]).unwrap();
    assert_eq!(result.as_u8(), 30);

    // Multiple versions (unordered)
    let result = select_highest_mutual([VersionAd(30), VersionAd(28), VersionAd(31)]).unwrap();
    assert_eq!(result.as_u8(), 31);
}

/// Test early termination when newest version is found.
#[test]
fn short_circuits_on_newest_version() {
    // When 32 is seen first, should return immediately
    let result = select_highest_mutual([VersionAd(32), VersionAd(28)]).unwrap();
    assert_eq!(result, ProtocolVersion::NEWEST);

    // When 32 is in the middle
    let result = select_highest_mutual([VersionAd(28), VersionAd(32), VersionAd(30)]).unwrap();
    assert_eq!(result, ProtocolVersion::NEWEST);

    // When 32 is at the end
    let result = select_highest_mutual([VersionAd(28), VersionAd(29), VersionAd(32)]).unwrap();
    assert_eq!(result, ProtocolVersion::NEWEST);
}

/// Test that duplicate versions don't affect selection.
#[test]
fn handles_duplicate_versions() {
    let result = select_highest_mutual([
        VersionAd(30),
        VersionAd(30),
        VersionAd(30),
        VersionAd(29),
        VersionAd(29),
    ])
    .unwrap();
    assert_eq!(result.as_u8(), 30);
}

/// Test version selection with sparse sets.
#[test]
fn handles_sparse_version_sets() {
    // Only oldest and newest
    let result = select_highest_mutual([VersionAd(28), VersionAd(32)]).unwrap();
    assert_eq!(result.as_u8(), 32);

    // Skip some versions
    let result = select_highest_mutual([VersionAd(28), VersionAd(31)]).unwrap();
    assert_eq!(result.as_u8(), 31);

    let result = select_highest_mutual([VersionAd(29), VersionAd(32)]).unwrap();
    assert_eq!(result.as_u8(), 32);
}

// ============================================================================
// Future Version Clamping Tests
// ============================================================================

/// Test that versions 33-40 are clamped to newest supported.
#[test]
fn clamps_future_versions_to_newest() {
    for future in 33..=40 {
        let result = select_highest_mutual([VersionAd(future)]).unwrap();
        assert_eq!(
            result,
            ProtocolVersion::NEWEST,
            "Version {future} should clamp to NEWEST"
        );
    }
}

/// Test mixed clamped and supported versions.
#[test]
fn mixed_clamped_and_supported_versions() {
    // Future + supported -> clamps to newest
    let result = select_highest_mutual([VersionAd(35), VersionAd(30)]).unwrap();
    assert_eq!(result, ProtocolVersion::NEWEST);

    // Multiple future versions
    let result = select_highest_mutual([VersionAd(33), VersionAd(35), VersionAd(40)]).unwrap();
    assert_eq!(result, ProtocolVersion::NEWEST);
}

/// Test that versions above 40 are rejected.
#[test]
fn rejects_versions_above_maximum_advertisement() {
    let reject_versions = [41, 50, 100, 200, 255, 1000, u32::MAX];

    for version in reject_versions {
        let result = select_highest_mutual([VersionAd(version)]);
        assert!(result.is_err(), "Version {version} should be rejected");

        match result.unwrap_err() {
            NegotiationError::UnsupportedVersion(v) => {
                assert_eq!(v, version);
            }
            other => panic!("Expected UnsupportedVersion, got: {other:?}"),
        }
    }
}

// ============================================================================
// Backwards Compatibility Tests
// ============================================================================

/// Simulate negotiation with rsync 2.6.4 (protocol 28).
#[test]
fn backwards_compat_rsync_264() {
    // Old rsync would only advertise protocol 28
    let result = select_highest_mutual([VersionAd(28)]).unwrap();
    assert_eq!(result.as_u8(), 28);

    // Verify we can work with the minimal feature set
    assert!(result.uses_legacy_ascii_negotiation());
    assert!(result.uses_fixed_encoding());
}

/// Simulate negotiation with rsync 3.0.x (protocol 30).
#[test]
fn backwards_compat_rsync_30x() {
    let result = select_highest_mutual([VersionAd(30)]).unwrap();
    assert_eq!(result.as_u8(), 30);

    // First binary negotiation version
    assert!(result.uses_binary_negotiation());
    assert!(result.uses_varint_encoding());
}

/// Simulate negotiation with rsync 3.1.x (protocol 31).
#[test]
fn backwards_compat_rsync_31x() {
    let result = select_highest_mutual([VersionAd(31)]).unwrap();
    assert_eq!(result.as_u8(), 31);

    // Safe file list always enabled
    assert!(result.safe_file_list_always_enabled());
}

/// Simulate negotiation with rsync 3.4.x (protocol 32).
#[test]
fn backwards_compat_rsync_34x() {
    let result = select_highest_mutual([VersionAd(32)]).unwrap();
    assert_eq!(result.as_u8(), 32);
    assert_eq!(result, ProtocolVersion::NEWEST);
}

/// Test downgrade when peer uses older protocol.
#[test]
fn backwards_compat_downgrade() {
    // We support 32, peer supports 30 -> negotiate to 30
    let result = select_highest_mutual([VersionAd(30)]).unwrap();
    assert_eq!(result.as_u8(), 30);

    // Verify we can still communicate using v30 features
    assert!(result.uses_binary_negotiation());
}

// ============================================================================
// Error Handling Tests
// ============================================================================

/// Test empty version list produces NoMutualProtocol error.
#[test]
fn error_empty_version_list() {
    let result = select_highest_mutual::<Vec<VersionAd>, _>(vec![]);
    match result.unwrap_err() {
        NegotiationError::NoMutualProtocol { peer_versions } => {
            assert!(peer_versions.is_empty());
        }
        other => panic!("Expected NoMutualProtocol, got: {other:?}"),
    }
}

/// Test version 0 is rejected.
#[test]
fn error_version_zero() {
    let result = select_highest_mutual([VersionAd(0)]);
    match result.unwrap_err() {
        NegotiationError::UnsupportedVersion(v) => {
            assert_eq!(v, 0);
        }
        other => panic!("Expected UnsupportedVersion(0), got: {other:?}"),
    }
}

/// Test that zero with valid version succeeds.
#[test]
fn error_zero_with_valid_succeeds() {
    let result = select_highest_mutual([VersionAd(0), VersionAd(30)]).unwrap();
    assert_eq!(result.as_u8(), 30);
}

/// Test all-invalid versions produce appropriate error.
#[test]
fn error_all_invalid_versions() {
    // All too old
    let result = select_highest_mutual([VersionAd(25), VersionAd(26), VersionAd(27)]);
    match result.unwrap_err() {
        NegotiationError::UnsupportedVersion(v) => {
            assert_eq!(v, 25, "Should report oldest rejected");
        }
        other => panic!("Expected UnsupportedVersion, got: {other:?}"),
    }

    // All above maximum
    let result = select_highest_mutual([VersionAd(50), VersionAd(100)]);
    match result.unwrap_err() {
        NegotiationError::UnsupportedVersion(v) => {
            assert!(v == 50 || v == 100);
        }
        other => panic!("Expected UnsupportedVersion, got: {other:?}"),
    }
}

/// Test error message content.
#[test]
fn error_message_includes_version_range() {
    let err = NegotiationError::UnsupportedVersion(27);
    let msg = err.to_string();

    // Should mention the version
    assert!(msg.contains("27"), "Error should mention version 27");

    // Should mention the valid range
    assert!(msg.contains("28"), "Error should mention min version 28");
    assert!(msg.contains("32"), "Error should mention max version 32");
}

// ============================================================================
// Feature Capability Matrix Tests
// ============================================================================

/// Comprehensive feature matrix test for all supported versions.
#[test]
fn feature_capability_matrix() {
    struct VersionFeatures {
        version: u8,
        binary_negotiation: bool,
        varint_encoding: bool,
        sender_receiver_modifiers: bool,
        perishable_modifier: bool,
        flist_times: bool,
        old_prefixes: bool,
        safe_file_list: bool,
        safe_file_list_always: bool,
        varint_flist_flags: bool,
    }

    let matrix = [
        VersionFeatures {
            version: 28,
            binary_negotiation: false,
            varint_encoding: false,
            sender_receiver_modifiers: false,
            perishable_modifier: false,
            flist_times: false,
            old_prefixes: true,
            safe_file_list: false,
            safe_file_list_always: false,
            varint_flist_flags: false,
        },
        VersionFeatures {
            version: 29,
            binary_negotiation: false,
            varint_encoding: false,
            sender_receiver_modifiers: true,
            perishable_modifier: false,
            flist_times: true,
            old_prefixes: false,
            safe_file_list: false,
            safe_file_list_always: false,
            varint_flist_flags: false,
        },
        VersionFeatures {
            version: 30,
            binary_negotiation: true,
            varint_encoding: true,
            sender_receiver_modifiers: true,
            perishable_modifier: true,
            flist_times: true,
            old_prefixes: false,
            safe_file_list: true,
            safe_file_list_always: false,
            varint_flist_flags: true,
        },
        VersionFeatures {
            version: 31,
            binary_negotiation: true,
            varint_encoding: true,
            sender_receiver_modifiers: true,
            perishable_modifier: true,
            flist_times: true,
            old_prefixes: false,
            safe_file_list: true,
            safe_file_list_always: true,
            varint_flist_flags: true,
        },
        VersionFeatures {
            version: 32,
            binary_negotiation: true,
            varint_encoding: true,
            sender_receiver_modifiers: true,
            perishable_modifier: true,
            flist_times: true,
            old_prefixes: false,
            safe_file_list: true,
            safe_file_list_always: true,
            varint_flist_flags: true,
        },
    ];

    for features in &matrix {
        let result = select_highest_mutual([VersionAd(u32::from(features.version))]).unwrap();
        let v = result;

        assert_eq!(
            v.uses_binary_negotiation(),
            features.binary_negotiation,
            "v{} binary_negotiation",
            features.version
        );
        assert_eq!(
            v.uses_varint_encoding(),
            features.varint_encoding,
            "v{} varint_encoding",
            features.version
        );
        assert_eq!(
            v.supports_sender_receiver_modifiers(),
            features.sender_receiver_modifiers,
            "v{} sender_receiver_modifiers",
            features.version
        );
        assert_eq!(
            v.supports_perishable_modifier(),
            features.perishable_modifier,
            "v{} perishable_modifier",
            features.version
        );
        assert_eq!(
            v.supports_flist_times(),
            features.flist_times,
            "v{} flist_times",
            features.version
        );
        assert_eq!(
            v.uses_old_prefixes(),
            features.old_prefixes,
            "v{} old_prefixes",
            features.version
        );
        assert_eq!(
            v.uses_safe_file_list(),
            features.safe_file_list,
            "v{} safe_file_list",
            features.version
        );
        assert_eq!(
            v.safe_file_list_always_enabled(),
            features.safe_file_list_always,
            "v{} safe_file_list_always",
            features.version
        );
        assert_eq!(
            v.uses_varint_flist_flags(),
            features.varint_flist_flags,
            "v{} varint_flist_flags",
            features.version
        );
    }
}

/// Test feature flags are mutually exclusive where appropriate.
#[test]
fn feature_mutual_exclusivity() {
    for version in ProtocolVersion::supported_versions() {
        // Binary and ASCII negotiation are mutually exclusive
        assert_ne!(
            version.uses_binary_negotiation(),
            version.uses_legacy_ascii_negotiation(),
            "v{}: binary and ASCII negotiation must be mutually exclusive",
            version.as_u8()
        );

        // Varint and fixed encoding are mutually exclusive
        assert_ne!(
            version.uses_varint_encoding(),
            version.uses_fixed_encoding(),
            "v{}: varint and fixed encoding must be mutually exclusive",
            version.as_u8()
        );
    }
}

/// Test that all supported versions have extended flags.
#[test]
fn all_versions_support_extended_flags() {
    for version in ProtocolVersion::supported_versions() {
        assert!(
            version.supports_extended_flags(),
            "v{} should support extended flags",
            version.as_u8()
        );
    }
}

// ============================================================================
// Protocol Version Navigation Tests
// ============================================================================

/// Test navigation between protocol versions.
#[test]
fn version_navigation_next_newer() {
    assert_eq!(
        ProtocolVersion::V28.next_newer(),
        Some(ProtocolVersion::V29)
    );
    assert_eq!(
        ProtocolVersion::V29.next_newer(),
        Some(ProtocolVersion::V30)
    );
    assert_eq!(
        ProtocolVersion::V30.next_newer(),
        Some(ProtocolVersion::V31)
    );
    assert_eq!(
        ProtocolVersion::V31.next_newer(),
        Some(ProtocolVersion::V32)
    );
    assert_eq!(ProtocolVersion::V32.next_newer(), None);
}

/// Test navigation between protocol versions (older).
#[test]
fn version_navigation_next_older() {
    assert_eq!(
        ProtocolVersion::V32.next_older(),
        Some(ProtocolVersion::V31)
    );
    assert_eq!(
        ProtocolVersion::V31.next_older(),
        Some(ProtocolVersion::V30)
    );
    assert_eq!(
        ProtocolVersion::V30.next_older(),
        Some(ProtocolVersion::V29)
    );
    assert_eq!(
        ProtocolVersion::V29.next_older(),
        Some(ProtocolVersion::V28)
    );
    assert_eq!(ProtocolVersion::V28.next_older(), None);
}

/// Test offset calculations.
#[test]
fn version_offset_calculations() {
    // Offset from oldest
    assert_eq!(ProtocolVersion::V28.offset_from_oldest(), 0);
    assert_eq!(ProtocolVersion::V29.offset_from_oldest(), 1);
    assert_eq!(ProtocolVersion::V30.offset_from_oldest(), 2);
    assert_eq!(ProtocolVersion::V31.offset_from_oldest(), 3);
    assert_eq!(ProtocolVersion::V32.offset_from_oldest(), 4);

    // Offset from newest
    assert_eq!(ProtocolVersion::V32.offset_from_newest(), 0);
    assert_eq!(ProtocolVersion::V31.offset_from_newest(), 1);
    assert_eq!(ProtocolVersion::V30.offset_from_newest(), 2);
    assert_eq!(ProtocolVersion::V29.offset_from_newest(), 3);
    assert_eq!(ProtocolVersion::V28.offset_from_newest(), 4);
}

/// Test roundtrip via offset.
#[test]
fn version_offset_roundtrip() {
    for version in ProtocolVersion::supported_versions() {
        // Round-trip via oldest offset
        let offset = version.offset_from_oldest();
        let recovered = ProtocolVersion::from_oldest_offset(offset).unwrap();
        assert_eq!(*version, recovered);

        // Round-trip via newest offset
        let offset = version.offset_from_newest();
        let recovered = ProtocolVersion::from_newest_offset(offset).unwrap();
        assert_eq!(*version, recovered);
    }
}

// ============================================================================
// Protocol Version Ordering Tests
// ============================================================================

/// Test that protocol versions are properly ordered.
#[test]
fn version_ordering() {
    assert!(ProtocolVersion::V28 < ProtocolVersion::V29);
    assert!(ProtocolVersion::V29 < ProtocolVersion::V30);
    assert!(ProtocolVersion::V30 < ProtocolVersion::V31);
    assert!(ProtocolVersion::V31 < ProtocolVersion::V32);

    // Transitive property
    assert!(ProtocolVersion::V28 < ProtocolVersion::V32);
    assert!(ProtocolVersion::OLDEST < ProtocolVersion::NEWEST);
}

/// Test version equality.
#[test]
fn version_equality() {
    assert_eq!(ProtocolVersion::V28, ProtocolVersion::V28);
    assert_eq!(ProtocolVersion::V32, ProtocolVersion::NEWEST);
    assert_eq!(ProtocolVersion::V28, ProtocolVersion::OLDEST);

    // Partial equality with u8
    assert!(ProtocolVersion::V30 == 30_u8);
    assert!(30_u8 == ProtocolVersion::V30);
}

// ============================================================================
// Exhaustive Pairwise Negotiation Tests
// ============================================================================

/// Test all pairwise combinations of supported versions.
#[test]
fn exhaustive_pairwise_negotiation() {
    let supported: Vec<u8> = SUPPORTED_PROTOCOLS.to_vec();

    for &v1 in &supported {
        for &v2 in &supported {
            let result =
                select_highest_mutual([VersionAd(u32::from(v1)), VersionAd(u32::from(v2))])
                    .expect("pairwise negotiation should succeed");

            let expected = std::cmp::max(v1, v2);
            assert_eq!(
                result.as_u8(),
                expected,
                "Negotiating [{v1}, {v2}] should select {expected}"
            );
        }
    }
}

/// Test all triple combinations of supported versions.
#[test]
fn exhaustive_triple_negotiation() {
    let supported: Vec<u8> = SUPPORTED_PROTOCOLS.to_vec();

    for &v1 in &supported {
        for &v2 in &supported {
            for &v3 in &supported {
                let result = select_highest_mutual([
                    VersionAd(u32::from(v1)),
                    VersionAd(u32::from(v2)),
                    VersionAd(u32::from(v3)),
                ])
                .expect("triple negotiation should succeed");

                let expected = std::cmp::max(v1, std::cmp::max(v2, v3));
                assert_eq!(
                    result.as_u8(),
                    expected,
                    "Negotiating [{v1}, {v2}, {v3}] should select {expected}"
                );
            }
        }
    }
}

// ============================================================================
// Protocol Version Bitmap Tests
// ============================================================================

/// Test that the supported protocol bitmap is correct.
#[test]
fn supported_protocol_bitmap() {
    let bitmap = ProtocolVersion::supported_protocol_bitmap();

    // Verify supported versions have bits set
    for version in 28..=32 {
        let mask = 1u64 << version;
        assert_ne!(bitmap & mask, 0, "Bit for v{version} should be set");
    }

    // Verify unsupported versions don't have bits set
    for version in 0..28 {
        let mask = 1u64 << version;
        assert_eq!(bitmap & mask, 0, "Bit for v{version} should not be set");
    }

    for version in 33..64 {
        let mask = 1u64 << version;
        assert_eq!(bitmap & mask, 0, "Bit for v{version} should not be set");
    }
}

/// Test is_supported_protocol_number for all u8 values.
#[test]
fn is_supported_protocol_number_comprehensive() {
    for value in 0u8..=255 {
        let expected = (28..=32).contains(&value);
        assert_eq!(
            ProtocolVersion::is_supported_protocol_number(value),
            expected,
            "is_supported_protocol_number({value}) should be {expected}"
        );
    }
}
