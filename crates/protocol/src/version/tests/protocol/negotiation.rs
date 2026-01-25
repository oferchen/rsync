//! Protocol version negotiation tests.
//!
//! These tests verify rsync protocol version negotiation behavior, including:
//! - Version range boundaries (MIN_SUPPORTED=28 to MAX_SUPPORTED=32)
//! - Invalid version rejection
//! - Version feature flags across protocol versions
//!
//! # Protocol Version Overview
//!
//! The rsync protocol supports versions 28-32:
//! - **v28**: Oldest supported, legacy ASCII negotiation, fixed encoding
//! - **v29**: Last legacy ASCII negotiation version, adds sender/receiver modifiers
//! - **v30**: First binary negotiation, introduces varint encoding
//! - **v31**: Safe file list always enabled
//! - **v32**: Current newest supported version
//!
//! # Upstream Compatibility
//!
//! - Versions 33-40 are clamped to NEWEST (32) for forward compatibility
//! - Versions above 40 (MAXIMUM_PROTOCOL_ADVERTISEMENT) are rejected
//! - Versions below 28 are rejected as too old

use crate::error::NegotiationError;
use crate::version::{
    MAXIMUM_PROTOCOL_ADVERTISEMENT, ProtocolVersion, SUPPORTED_PROTOCOL_BOUNDS,
    SUPPORTED_PROTOCOL_RANGE, select_highest_mutual,
};

// ============================================================================
// Phase 2.15: Version Negotiation Min/Max Tests
// ============================================================================

/// Verifies MIN_SUPPORTED is protocol version 28.
#[test]
fn min_supported_version_is_28() {
    assert_eq!(ProtocolVersion::OLDEST.as_u8(), 28);
    assert_eq!(*SUPPORTED_PROTOCOL_RANGE.start(), 28);
    assert_eq!(SUPPORTED_PROTOCOL_BOUNDS.0, 28);
}

/// Verifies MAX_SUPPORTED is protocol version 32.
#[test]
fn max_supported_version_is_32() {
    assert_eq!(ProtocolVersion::NEWEST.as_u8(), 32);
    assert_eq!(*SUPPORTED_PROTOCOL_RANGE.end(), 32);
    assert_eq!(SUPPORTED_PROTOCOL_BOUNDS.1, 32);
}

/// Verifies the MAXIMUM_PROTOCOL_ADVERTISEMENT ceiling is 40.
#[test]
fn maximum_protocol_advertisement_is_40() {
    assert_eq!(MAXIMUM_PROTOCOL_ADVERTISEMENT, 40);
}

/// Verifies all versions within the supported range can be negotiated.
#[test]
fn negotiation_succeeds_for_all_supported_versions() {
    for version in 28..=32 {
        let result = select_highest_mutual([version]);
        assert!(result.is_ok(), "version {version} should be negotiable");
        assert_eq!(
            result.unwrap().as_u8(),
            version,
            "negotiated version should match input for {version}"
        );
    }
}

/// Verifies negotiation selects the highest mutual version.
#[test]
fn negotiation_selects_highest_mutual_version() {
    // When peer offers multiple supported versions, select highest
    let result = select_highest_mutual([28, 29, 30]).unwrap();
    assert_eq!(result.as_u8(), 30);

    let result = select_highest_mutual([28, 32]).unwrap();
    assert_eq!(result.as_u8(), 32);

    let result = select_highest_mutual([29, 31]).unwrap();
    assert_eq!(result.as_u8(), 31);
}

/// Verifies the version range boundaries are correctly handled.
#[test]
fn negotiation_boundary_versions() {
    // Minimum boundary
    let min_result = select_highest_mutual([28]);
    assert!(min_result.is_ok());
    assert_eq!(min_result.unwrap(), ProtocolVersion::OLDEST);

    // Maximum boundary
    let max_result = select_highest_mutual([32]);
    assert!(max_result.is_ok());
    assert_eq!(max_result.unwrap(), ProtocolVersion::NEWEST);
}

/// Verifies versions between NEWEST and MAXIMUM_PROTOCOL_ADVERTISEMENT are clamped.
#[test]
fn future_versions_clamp_to_newest() {
    // Versions 33-40 should clamp to NEWEST (32)
    for version in 33..=40 {
        let result = select_highest_mutual([version]);
        assert!(result.is_ok(), "version {version} should clamp to NEWEST");
        assert_eq!(
            result.unwrap(),
            ProtocolVersion::NEWEST,
            "version {version} should clamp to NEWEST"
        );
    }
}

/// Verifies mixed clamped and supported versions select NEWEST.
#[test]
fn mixed_clamped_and_supported_versions() {
    // Mix of supported and clamped versions
    let result = select_highest_mutual([28, 35]).unwrap();
    assert_eq!(result, ProtocolVersion::NEWEST);

    let result = select_highest_mutual([40, 29]).unwrap();
    assert_eq!(result, ProtocolVersion::NEWEST);
}

// ============================================================================
// Phase 2.16: Invalid Version Rejection Tests
// ============================================================================

/// Verifies version 0 is rejected as invalid.
#[test]
fn rejects_zero_version() {
    let result = select_highest_mutual([0_u32]);
    assert!(result.is_err());
    match result.unwrap_err() {
        NegotiationError::UnsupportedVersion(v) => assert_eq!(v, 0),
        _ => panic!("expected UnsupportedVersion error"),
    }
}

/// Verifies versions below minimum (28) are rejected.
#[test]
fn rejects_versions_below_minimum() {
    for version in 1_u8..28 {
        let result = select_highest_mutual([version]);
        assert!(result.is_err(), "version {version} should be rejected");
        match result.unwrap_err() {
            NegotiationError::UnsupportedVersion(v) => {
                assert_eq!(v, u32::from(version));
            }
            _ => panic!("expected UnsupportedVersion for version {version}"),
        }
    }
}

/// Verifies versions above MAXIMUM_PROTOCOL_ADVERTISEMENT are rejected.
#[test]
fn rejects_versions_above_maximum() {
    for version in [41_u32, 50, 100, 200, 255] {
        let result = select_highest_mutual([version]);
        assert!(result.is_err(), "version {version} should be rejected");
        match result.unwrap_err() {
            NegotiationError::UnsupportedVersion(v) => {
                assert_eq!(v, version);
            }
            _ => panic!("expected UnsupportedVersion for version {version}"),
        }
    }
}

/// Verifies empty version list results in NoMutualProtocol error.
#[test]
fn empty_version_list_returns_no_mutual_protocol() {
    let result = select_highest_mutual(Vec::<u8>::new());
    assert!(result.is_err());
    match result.unwrap_err() {
        NegotiationError::NoMutualProtocol { peer_versions } => {
            assert!(peer_versions.is_empty());
        }
        _ => panic!("expected NoMutualProtocol error"),
    }
}

/// Verifies only-too-old versions report the oldest rejected version.
#[test]
fn reports_oldest_rejected_version() {
    // When all versions are too old, report the oldest
    let result = select_highest_mutual([25_u8, 26, 27]);
    match result.unwrap_err() {
        NegotiationError::UnsupportedVersion(v) => {
            assert_eq!(v, 25, "should report oldest rejected version");
        }
        _ => panic!("expected UnsupportedVersion error"),
    }
}

/// Verifies mixed valid and invalid versions succeed with valid version.
#[test]
fn mixed_valid_and_invalid_versions_succeeds() {
    // Too old + valid
    let result = select_highest_mutual([27_u8, 30]).unwrap();
    assert_eq!(result.as_u8(), 30);

    // Zero + valid
    let result = select_highest_mutual([0_u32, 31]).unwrap();
    assert_eq!(result.as_u8(), 31);

    // Too old + clamped
    let result = select_highest_mutual([27_u8, 35]).unwrap();
    assert_eq!(result, ProtocolVersion::NEWEST);
}

/// Verifies boundary rejection at MIN-1 (version 27).
#[test]
fn rejects_version_27() {
    let result = select_highest_mutual([27_u8]);
    assert!(result.is_err());
    match result.unwrap_err() {
        NegotiationError::UnsupportedVersion(v) => assert_eq!(v, 27),
        _ => panic!("expected UnsupportedVersion error"),
    }
}

/// Verifies boundary rejection at MAX+1 above ceiling (version 41).
#[test]
fn rejects_version_41() {
    let result = select_highest_mutual([41_u32]);
    assert!(result.is_err());
    match result.unwrap_err() {
        NegotiationError::UnsupportedVersion(v) => assert_eq!(v, 41),
        _ => panic!("expected UnsupportedVersion error"),
    }
}

/// Verifies very large version values are rejected.
#[test]
fn rejects_very_large_versions() {
    let result = select_highest_mutual([u32::MAX]);
    assert!(result.is_err());
    match result.unwrap_err() {
        NegotiationError::UnsupportedVersion(v) => assert_eq!(v, u32::MAX),
        _ => panic!("expected UnsupportedVersion error"),
    }
}

// ============================================================================
// Version Feature Flag Tests
// ============================================================================

/// Verifies `uses_varint_encoding` boundary at protocol 30.
#[test]
fn uses_varint_encoding_boundary() {
    // Protocol < 30: fixed encoding
    assert!(!ProtocolVersion::V28.uses_varint_encoding());
    assert!(!ProtocolVersion::V29.uses_varint_encoding());

    // Protocol >= 30: varint encoding
    assert!(ProtocolVersion::V30.uses_varint_encoding());
    assert!(ProtocolVersion::V31.uses_varint_encoding());
    assert!(ProtocolVersion::V32.uses_varint_encoding());
}

/// Verifies `uses_fixed_encoding` is inverse of `uses_varint_encoding`.
#[test]
fn uses_fixed_encoding_is_inverse() {
    for version in ProtocolVersion::supported_versions() {
        assert_eq!(
            version.uses_fixed_encoding(),
            !version.uses_varint_encoding(),
            "uses_fixed_encoding must be inverse of uses_varint_encoding for v{}",
            version.as_u8()
        );
    }
}

/// Verifies `uses_binary_negotiation` boundary at protocol 30.
#[test]
fn uses_binary_negotiation_boundary() {
    // Protocol < 30: legacy ASCII negotiation
    assert!(!ProtocolVersion::V28.uses_binary_negotiation());
    assert!(!ProtocolVersion::V29.uses_binary_negotiation());

    // Protocol >= 30: binary negotiation
    assert!(ProtocolVersion::V30.uses_binary_negotiation());
    assert!(ProtocolVersion::V31.uses_binary_negotiation());
    assert!(ProtocolVersion::V32.uses_binary_negotiation());
}

/// Verifies `uses_legacy_ascii_negotiation` is inverse of `uses_binary_negotiation`.
#[test]
fn uses_legacy_ascii_negotiation_is_inverse() {
    for version in ProtocolVersion::supported_versions() {
        assert_eq!(
            version.uses_legacy_ascii_negotiation(),
            !version.uses_binary_negotiation(),
            "uses_legacy_ascii_negotiation must be inverse of uses_binary_negotiation for v{}",
            version.as_u8()
        );
    }
}

/// Verifies `supports_sender_receiver_modifiers` boundary at protocol 29.
#[test]
fn supports_sender_receiver_modifiers_boundary() {
    assert!(!ProtocolVersion::V28.supports_sender_receiver_modifiers());
    assert!(ProtocolVersion::V29.supports_sender_receiver_modifiers());
    assert!(ProtocolVersion::V30.supports_sender_receiver_modifiers());
    assert!(ProtocolVersion::V31.supports_sender_receiver_modifiers());
    assert!(ProtocolVersion::V32.supports_sender_receiver_modifiers());
}

/// Verifies `supports_perishable_modifier` boundary at protocol 30.
#[test]
fn supports_perishable_modifier_boundary() {
    assert!(!ProtocolVersion::V28.supports_perishable_modifier());
    assert!(!ProtocolVersion::V29.supports_perishable_modifier());
    assert!(ProtocolVersion::V30.supports_perishable_modifier());
    assert!(ProtocolVersion::V31.supports_perishable_modifier());
    assert!(ProtocolVersion::V32.supports_perishable_modifier());
}

/// Verifies `uses_old_prefixes` boundary at protocol 29.
#[test]
fn uses_old_prefixes_boundary() {
    assert!(ProtocolVersion::V28.uses_old_prefixes());
    assert!(!ProtocolVersion::V29.uses_old_prefixes());
    assert!(!ProtocolVersion::V30.uses_old_prefixes());
    assert!(!ProtocolVersion::V31.uses_old_prefixes());
    assert!(!ProtocolVersion::V32.uses_old_prefixes());
}

/// Verifies `supports_flist_times` boundary at protocol 29.
#[test]
fn supports_flist_times_boundary() {
    assert!(!ProtocolVersion::V28.supports_flist_times());
    assert!(ProtocolVersion::V29.supports_flist_times());
    assert!(ProtocolVersion::V30.supports_flist_times());
    assert!(ProtocolVersion::V31.supports_flist_times());
    assert!(ProtocolVersion::V32.supports_flist_times());
}

/// Verifies `supports_extended_flags` for all supported versions (28+).
#[test]
fn supports_extended_flags_all_versions() {
    for version in ProtocolVersion::supported_versions() {
        assert!(
            version.supports_extended_flags(),
            "v{} should support extended flags",
            version.as_u8()
        );
    }
}

/// Verifies `uses_varint_flist_flags` boundary at protocol 30.
#[test]
fn uses_varint_flist_flags_boundary() {
    assert!(!ProtocolVersion::V28.uses_varint_flist_flags());
    assert!(!ProtocolVersion::V29.uses_varint_flist_flags());
    assert!(ProtocolVersion::V30.uses_varint_flist_flags());
    assert!(ProtocolVersion::V31.uses_varint_flist_flags());
    assert!(ProtocolVersion::V32.uses_varint_flist_flags());
}

/// Verifies `uses_safe_file_list` boundary at protocol 30.
#[test]
fn uses_safe_file_list_boundary() {
    assert!(!ProtocolVersion::V28.uses_safe_file_list());
    assert!(!ProtocolVersion::V29.uses_safe_file_list());
    assert!(ProtocolVersion::V30.uses_safe_file_list());
    assert!(ProtocolVersion::V31.uses_safe_file_list());
    assert!(ProtocolVersion::V32.uses_safe_file_list());
}

/// Verifies `safe_file_list_always_enabled` boundary at protocol 31.
#[test]
fn safe_file_list_always_enabled_boundary() {
    assert!(!ProtocolVersion::V28.safe_file_list_always_enabled());
    assert!(!ProtocolVersion::V29.safe_file_list_always_enabled());
    assert!(!ProtocolVersion::V30.safe_file_list_always_enabled());
    assert!(ProtocolVersion::V31.safe_file_list_always_enabled());
    assert!(ProtocolVersion::V32.safe_file_list_always_enabled());
}

// ============================================================================
// Feature Flag Consistency Tests
// ============================================================================

/// Verifies feature flags are consistent with negotiation style.
///
/// All binary negotiation versions (>= 30) should have:
/// - varint encoding
/// - varint flist flags
/// - safe file list
/// - perishable modifier support
#[test]
fn feature_flags_consistent_with_negotiation_style() {
    for version in ProtocolVersion::supported_versions() {
        if version.uses_binary_negotiation() {
            assert!(
                version.uses_varint_encoding(),
                "binary negotiation v{} should use varint encoding",
                version.as_u8()
            );
            assert!(
                version.uses_varint_flist_flags(),
                "binary negotiation v{} should use varint flist flags",
                version.as_u8()
            );
            assert!(
                version.uses_safe_file_list(),
                "binary negotiation v{} should use safe file list",
                version.as_u8()
            );
            assert!(
                version.supports_perishable_modifier(),
                "binary negotiation v{} should support perishable modifier",
                version.as_u8()
            );
        } else {
            assert!(
                version.uses_fixed_encoding(),
                "legacy negotiation v{} should use fixed encoding",
                version.as_u8()
            );
            assert!(
                !version.uses_varint_flist_flags(),
                "legacy negotiation v{} should not use varint flist flags",
                version.as_u8()
            );
            assert!(
                !version.uses_safe_file_list(),
                "legacy negotiation v{} should not use safe file list",
                version.as_u8()
            );
            assert!(
                !version.supports_perishable_modifier(),
                "legacy negotiation v{} should not support perishable modifier",
                version.as_u8()
            );
        }
    }
}

/// Verifies version 28 has the expected feature set (minimal features).
#[test]
fn version_28_feature_profile() {
    let v = ProtocolVersion::V28;
    assert!(!v.uses_varint_encoding(), "v28 uses fixed encoding");
    assert!(v.uses_fixed_encoding(), "v28 uses fixed encoding");
    assert!(
        v.uses_legacy_ascii_negotiation(),
        "v28 uses legacy negotiation"
    );
    assert!(
        !v.uses_binary_negotiation(),
        "v28 does not use binary negotiation"
    );
    assert!(
        !v.supports_sender_receiver_modifiers(),
        "v28 lacks s/r modifiers"
    );
    assert!(
        !v.supports_perishable_modifier(),
        "v28 lacks perishable modifier"
    );
    assert!(!v.supports_flist_times(), "v28 lacks flist times");
    assert!(v.uses_old_prefixes(), "v28 uses old prefixes");
    assert!(v.supports_extended_flags(), "v28 supports extended flags");
    assert!(!v.uses_varint_flist_flags(), "v28 lacks varint flist flags");
    assert!(!v.uses_safe_file_list(), "v28 lacks safe file list");
    assert!(
        !v.safe_file_list_always_enabled(),
        "v28 lacks always-on safe file list"
    );
}

/// Verifies version 29 has the expected feature set (adds modifiers and flist times).
#[test]
fn version_29_feature_profile() {
    let v = ProtocolVersion::V29;
    assert!(!v.uses_varint_encoding(), "v29 uses fixed encoding");
    assert!(v.uses_fixed_encoding(), "v29 uses fixed encoding");
    assert!(
        v.uses_legacy_ascii_negotiation(),
        "v29 uses legacy negotiation"
    );
    assert!(
        !v.uses_binary_negotiation(),
        "v29 does not use binary negotiation"
    );
    assert!(
        v.supports_sender_receiver_modifiers(),
        "v29 adds s/r modifiers"
    );
    assert!(
        !v.supports_perishable_modifier(),
        "v29 lacks perishable modifier"
    );
    assert!(v.supports_flist_times(), "v29 adds flist times");
    assert!(!v.uses_old_prefixes(), "v29 uses new prefixes");
    assert!(v.supports_extended_flags(), "v29 supports extended flags");
    assert!(!v.uses_varint_flist_flags(), "v29 lacks varint flist flags");
    assert!(!v.uses_safe_file_list(), "v29 lacks safe file list");
    assert!(
        !v.safe_file_list_always_enabled(),
        "v29 lacks always-on safe file list"
    );
}

/// Verifies version 30 has the expected feature set (first binary negotiation).
#[test]
fn version_30_feature_profile() {
    let v = ProtocolVersion::V30;
    assert!(v.uses_varint_encoding(), "v30 uses varint encoding");
    assert!(!v.uses_fixed_encoding(), "v30 does not use fixed encoding");
    assert!(
        !v.uses_legacy_ascii_negotiation(),
        "v30 does not use legacy negotiation"
    );
    assert!(v.uses_binary_negotiation(), "v30 uses binary negotiation");
    assert!(
        v.supports_sender_receiver_modifiers(),
        "v30 has s/r modifiers"
    );
    assert!(
        v.supports_perishable_modifier(),
        "v30 adds perishable modifier"
    );
    assert!(v.supports_flist_times(), "v30 has flist times");
    assert!(!v.uses_old_prefixes(), "v30 uses new prefixes");
    assert!(v.supports_extended_flags(), "v30 supports extended flags");
    assert!(v.uses_varint_flist_flags(), "v30 adds varint flist flags");
    assert!(v.uses_safe_file_list(), "v30 adds safe file list");
    assert!(
        !v.safe_file_list_always_enabled(),
        "v30 lacks always-on safe file list"
    );
}

/// Verifies version 31 has the expected feature set (safe file list always enabled).
#[test]
fn version_31_feature_profile() {
    let v = ProtocolVersion::V31;
    assert!(v.uses_varint_encoding(), "v31 uses varint encoding");
    assert!(!v.uses_fixed_encoding(), "v31 does not use fixed encoding");
    assert!(
        !v.uses_legacy_ascii_negotiation(),
        "v31 does not use legacy negotiation"
    );
    assert!(v.uses_binary_negotiation(), "v31 uses binary negotiation");
    assert!(
        v.supports_sender_receiver_modifiers(),
        "v31 has s/r modifiers"
    );
    assert!(
        v.supports_perishable_modifier(),
        "v31 has perishable modifier"
    );
    assert!(v.supports_flist_times(), "v31 has flist times");
    assert!(!v.uses_old_prefixes(), "v31 uses new prefixes");
    assert!(v.supports_extended_flags(), "v31 supports extended flags");
    assert!(v.uses_varint_flist_flags(), "v31 has varint flist flags");
    assert!(v.uses_safe_file_list(), "v31 has safe file list");
    assert!(
        v.safe_file_list_always_enabled(),
        "v31 adds always-on safe file list"
    );
}

/// Verifies version 32 has the expected feature set (current newest).
#[test]
fn version_32_feature_profile() {
    let v = ProtocolVersion::V32;
    assert!(v.uses_varint_encoding(), "v32 uses varint encoding");
    assert!(!v.uses_fixed_encoding(), "v32 does not use fixed encoding");
    assert!(
        !v.uses_legacy_ascii_negotiation(),
        "v32 does not use legacy negotiation"
    );
    assert!(v.uses_binary_negotiation(), "v32 uses binary negotiation");
    assert!(
        v.supports_sender_receiver_modifiers(),
        "v32 has s/r modifiers"
    );
    assert!(
        v.supports_perishable_modifier(),
        "v32 has perishable modifier"
    );
    assert!(v.supports_flist_times(), "v32 has flist times");
    assert!(!v.uses_old_prefixes(), "v32 uses new prefixes");
    assert!(v.supports_extended_flags(), "v32 supports extended flags");
    assert!(v.uses_varint_flist_flags(), "v32 has varint flist flags");
    assert!(v.uses_safe_file_list(), "v32 has safe file list");
    assert!(
        v.safe_file_list_always_enabled(),
        "v32 has always-on safe file list"
    );
}

// ============================================================================
// Error Message Tests
// ============================================================================

/// Verifies `UnsupportedVersion` error message includes version range.
#[test]
fn unsupported_version_error_includes_range() {
    let err = NegotiationError::UnsupportedVersion(27);
    let msg = err.to_string();

    assert!(msg.contains("27"), "error should mention version 27");
    assert!(msg.contains("28"), "error should mention min version 28");
    assert!(msg.contains("32"), "error should mention max version 32");
}

/// Verifies `NoMutualProtocol` error message includes peer versions.
#[test]
fn no_mutual_protocol_error_includes_peer_versions() {
    let err = NegotiationError::NoMutualProtocol {
        peer_versions: vec![],
    };
    let msg = err.to_string();

    assert!(msg.contains("peer offered"), "error should mention peer");
    assert!(
        msg.contains("we support"),
        "error should mention our support"
    );
}

// ============================================================================
// from_peer_advertisement Tests
// ============================================================================

/// Verifies `from_peer_advertisement` accepts valid versions.
#[test]
fn from_peer_advertisement_accepts_valid() {
    for version in 28..=32 {
        let result = ProtocolVersion::from_peer_advertisement(version);
        assert!(result.is_ok(), "version {version} should be accepted");
        assert_eq!(result.unwrap().as_u8(), version as u8);
    }
}

/// Verifies `from_peer_advertisement` clamps future versions.
#[test]
fn from_peer_advertisement_clamps_future() {
    for version in 33..=40 {
        let result = ProtocolVersion::from_peer_advertisement(version);
        assert!(result.is_ok(), "version {version} should clamp");
        assert_eq!(result.unwrap(), ProtocolVersion::NEWEST);
    }
}

/// Verifies `from_peer_advertisement` rejects zero.
#[test]
fn from_peer_advertisement_rejects_zero() {
    let result = ProtocolVersion::from_peer_advertisement(0);
    assert!(result.is_err());
    match result.unwrap_err() {
        NegotiationError::UnsupportedVersion(v) => assert_eq!(v, 0),
        _ => panic!("expected UnsupportedVersion"),
    }
}

/// Verifies `from_peer_advertisement` rejects too-old versions.
#[test]
fn from_peer_advertisement_rejects_too_old() {
    for version in 1..28 {
        let result = ProtocolVersion::from_peer_advertisement(version);
        assert!(result.is_err(), "version {version} should be rejected");
    }
}

/// Verifies `from_peer_advertisement` rejects versions above ceiling.
#[test]
fn from_peer_advertisement_rejects_above_ceiling() {
    for version in [41_u32, 50, 100, 255, u32::MAX] {
        let result = ProtocolVersion::from_peer_advertisement(version);
        assert!(result.is_err(), "version {version} should be rejected");
    }
}
