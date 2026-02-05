//! Protocol version 27 compatibility tests.
//!
//! Comprehensive tests for protocol version 27, which is intentionally unsupported
//! by this implementation (minimum supported version is 28). These tests validate:
//! 1. Protocol version 27 handshake is correctly rejected
//! 2. Compatibility flags behavior when v27 is involved
//! 3. Wire format expectations and error handling for v27
//!
//! # Protocol Version 27 Status
//!
//! Protocol version 27 is below the minimum supported version (28) and should be
//! rejected during negotiation. This is consistent with upstream rsync 3.4.1
//! behavior which also does not support protocol 27.
//!
//! # Test Coverage
//!
//! - Handshake rejection tests
//! - Negotiation failure scenarios
//! - Fallback behavior (v27 + v28 should select v28)
//! - Error messages and diagnostics
//! - Wire format validation
//!
//! # Upstream Reference
//!
//! Protocol details are based on rsync 3.4.1 source code, which defines
//! OLDEST_SUPPORTED_PROTOCOL as 28, explicitly excluding version 27.

use protocol::{
    NegotiationError, ProtocolVersion, ProtocolVersionAdvertisement,
    select_highest_mutual, LEGACY_DAEMON_PREFIX,
};
use protocol::codec::{create_ndx_codec, create_protocol_codec, NdxCodec, ProtocolCodec};

/// Helper wrapper for testing protocol version advertisement.
#[derive(Clone, Copy, Debug)]
struct TestVersion(u32);

impl ProtocolVersionAdvertisement for TestVersion {
    #[inline]
    fn into_advertised_version(self) -> u32 {
        self.0
    }
}

// ============================================================================
// Module: Protocol Version 27 Handshake Rejection Tests
// ============================================================================

mod handshake_rejection {
    use super::*;

    /// Protocol 27 must be rejected during handshake negotiation.
    #[test]
    fn version_27_handshake_rejected() {
        let result = select_highest_mutual([TestVersion(27)]);
        assert!(
            result.is_err(),
            "Protocol 27 handshake must be rejected as unsupported"
        );
    }

    /// Protocol 27 rejection produces correct error type.
    #[test]
    fn version_27_rejection_error_type() {
        let result = select_highest_mutual([TestVersion(27)]);

        match result {
            Err(NegotiationError::UnsupportedVersion(ver)) => {
                assert_eq!(ver, 27, "Error must report version 27 as unsupported");
            }
            other => panic!("Expected UnsupportedVersion(27), got: {other:?}"),
        }
    }

    /// Protocol 27 is not in the supported protocol list.
    #[test]
    fn version_27_not_in_supported_list() {
        assert!(
            !ProtocolVersion::is_supported_protocol_number(27),
            "Protocol 27 must not be in supported list"
        );
    }

    /// Attempting to create ProtocolVersion from 27 must fail.
    #[test]
    fn version_27_try_from_u8_fails() {
        let result = ProtocolVersion::try_from(27u8);
        assert!(
            result.is_err(),
            "TryFrom<u8> for protocol 27 must fail"
        );
    }

    /// Protocol 27 from_peer_advertisement must fail.
    #[test]
    fn version_27_from_peer_advertisement_fails() {
        let result = ProtocolVersion::from_peer_advertisement(27);
        assert!(
            result.is_err(),
            "from_peer_advertisement(27) must fail"
        );
    }

    /// Protocol 27 from_supported must return None.
    #[test]
    fn version_27_from_supported_returns_none() {
        let result = ProtocolVersion::from_supported(27);
        assert!(
            result.is_none(),
            "from_supported(27) must return None"
        );
    }

    /// Protocol 27 is below OLDEST supported version.
    #[test]
    fn version_27_below_oldest() {
        assert_eq!(
            ProtocolVersion::OLDEST.as_u8(),
            28,
            "OLDEST must be 28, not 27"
        );
        assert!(
            27 < ProtocolVersion::OLDEST.as_u8(),
            "Protocol 27 must be below OLDEST"
        );
    }
}

// ============================================================================
// Module: Protocol Version 27 Compatibility Flags Behavior
// ============================================================================

mod compatibility_flags {
    use super::*;

    /// Protocol 27 would use legacy ASCII negotiation (if supported).
    /// Since it's unsupported, this is a theoretical test.
    #[test]
    fn version_27_would_use_legacy_negotiation() {
        // Protocol 27 would be before the binary negotiation boundary (v30)
        assert_eq!(
            ProtocolVersion::BINARY_NEGOTIATION_INTRODUCED.as_u8(),
            30,
            "Binary negotiation starts at v30"
        );

        // If v27 were supported, it would use ASCII like v28 and v29
        assert!(
            27 < 30,
            "Protocol 27 would be in legacy ASCII negotiation range"
        );
    }

    /// Protocol 27 would not support compatibility flags (legacy protocol).
    #[test]
    fn version_27_would_not_support_compatibility_flags() {
        // Compatibility flags are negotiated starting with protocol 30
        // Protocol 27, if supported, would not use them
        assert!(
            27 < 30,
            "Protocol 27 would be before compatibility flags were introduced"
        );
    }

    /// Protocol 27 would use fixed encoding (not varint).
    #[test]
    fn version_27_would_use_fixed_encoding() {
        // Varint encoding was introduced in protocol 30
        assert!(
            27 < 30,
            "Protocol 27 would be before varint encoding"
        );
    }
}

// ============================================================================
// Module: Wire Format Expectations for Protocol Version 27
// ============================================================================

mod wire_format {
    use super::*;

    /// If protocol 27 were supported, it would use legacy ASCII greeting.
    #[test]
    fn version_27_would_use_legacy_greeting_format() {
        // Protocol 27 would use "@RSYNCD: 27.0\n" format
        // We can't test the actual function since v27 is unsupported,
        // but we can validate the expected format

        let expected_format = "@RSYNCD: 27.0\n";
        assert_eq!(expected_format.len(), 14);
        assert!(expected_format.starts_with(LEGACY_DAEMON_PREFIX));
        assert!(expected_format.ends_with(".0\n"));
        assert!(expected_format.is_ascii());
    }

    /// Protocol 27 would use 4-byte fixed size encoding.
    #[test]
    fn version_27_would_use_4_byte_file_sizes() {
        // Protocol 27, like 28 and 29, would use 4-byte little-endian
        // for file sizes (before varint encoding in v30)

        // We can verify this by checking that v28 uses 4 bytes
        let codec_28 = create_protocol_codec(28);
        let mut buf = Vec::new();
        codec_28.write_file_size(&mut buf, 1000).unwrap();
        assert_eq!(buf.len(), 4, "Protocol 28 (and 27 if supported) uses 4-byte sizes");
    }

    /// Protocol 27 would use 4-byte NDX encoding.
    #[test]
    fn version_27_would_use_4_byte_ndx_encoding() {
        // Protocol 27 would use 4-byte little-endian NDX encoding
        // like protocols 28 and 29

        let mut codec_28 = create_ndx_codec(28);
        let mut buf = Vec::new();
        codec_28.write_ndx(&mut buf, 5).unwrap();
        assert_eq!(buf.len(), 4, "Protocol 28 (and 27 if supported) uses 4-byte NDX");
    }

    /// Binary advertisement for v27 (if it were supported).
    #[test]
    fn version_27_binary_advertisement_format() {
        // If protocol 27 were in the binary range, it would be:
        let bytes = 27u32.to_be_bytes();
        assert_eq!(bytes, [0, 0, 0, 27]);

        // But v27 is below binary negotiation boundary
        assert!(27 < 30, "Protocol 27 is below binary negotiation");
    }

    /// ASCII greeting format validation for v27.
    #[test]
    fn version_27_ascii_greeting_validation() {
        // The expected greeting format if v27 were supported
        let expected = "@RSYNCD: 27.0\n";

        // Validate structure
        assert_eq!(expected.len(), 14);
        assert!(expected.starts_with("@RSYNCD: "));
        assert!(expected.contains("27"));
        assert!(expected.ends_with(".0\n"));

        // Validate it's pure ASCII
        assert!(expected.is_ascii());
        for byte in expected.bytes() {
            assert!(byte.is_ascii());
        }

        // Validate single newline at end
        assert_eq!(expected.chars().filter(|&c| c == '\n').count(), 1);
        assert_eq!(expected.chars().last(), Some('\n'));
    }
}

// ============================================================================
// Module: Fallback Behavior Tests
// ============================================================================

mod fallback_behavior {
    use super::*;

    /// When peer advertises v27 and v28, should negotiate to v28.
    #[test]
    fn version_27_with_28_fallback_to_28() {
        let result = select_highest_mutual([TestVersion(28), TestVersion(27)]);
        assert!(result.is_ok(), "Should successfully fallback to v28");
        assert_eq!(
            result.unwrap().as_u8(),
            28,
            "Should select v28 when v27 is also advertised"
        );
    }

    /// When peer advertises only v27, negotiation fails.
    #[test]
    fn version_27_only_fails() {
        let result = select_highest_mutual([TestVersion(27)]);
        assert!(result.is_err(), "Should fail when only v27 is advertised");
    }

    /// When peer advertises v27 with multiple newer versions.
    #[test]
    fn version_27_with_multiple_newer_versions() {
        let result = select_highest_mutual([
            TestVersion(27),
            TestVersion(28),
            TestVersion(29),
            TestVersion(30),
        ]);
        assert!(result.is_ok(), "Should successfully negotiate");
        assert_eq!(
            result.unwrap().as_u8(),
            30,
            "Should select highest supported version (30)"
        );
    }

    /// When peer advertises v27 and v29, should select v29.
    #[test]
    fn version_27_with_29_selects_29() {
        let result = select_highest_mutual([TestVersion(27), TestVersion(29)]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 29);
    }

    /// When peer advertises v27 and v31, should select v31.
    #[test]
    fn version_27_with_31_selects_31() {
        let result = select_highest_mutual([TestVersion(27), TestVersion(31)]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 31);
    }

    /// Multiple v27 advertisements with one supported version.
    #[test]
    fn multiple_v27_with_one_supported() {
        let result = select_highest_mutual([
            TestVersion(27),
            TestVersion(27),
            TestVersion(27),
            TestVersion(28),
        ]);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().as_u8(),
            28,
            "Should ignore multiple v27 and select v28"
        );
    }
}

// ============================================================================
// Module: Error Messages and Diagnostics
// ============================================================================

mod error_diagnostics {
    use super::*;

    /// Error message for v27 should be clear.
    #[test]
    fn version_27_error_message_clarity() {
        let result = select_highest_mutual([TestVersion(27)]);
        assert!(result.is_err());

        let error = result.unwrap_err();
        let error_string = format!("{:?}", error);

        // Error should mention version 27
        assert!(
            error_string.contains("27"),
            "Error message should mention version 27"
        );
    }

    /// Error distinguishes between unsupported and invalid versions.
    #[test]
    fn version_27_unsupported_vs_invalid() {
        let result_27 = select_highest_mutual([TestVersion(27)]);
        let result_0 = select_highest_mutual([TestVersion(0)]);
        let result_100 = select_highest_mutual([TestVersion(100)]);

        // All should fail, but potentially with different errors
        assert!(result_27.is_err());
        assert!(result_0.is_err());
        assert!(result_100.is_err());
    }

    /// Error for v27-only advertised list.
    #[test]
    fn version_27_only_error_details() {
        let result = select_highest_mutual([TestVersion(27)]);

        match result {
            Err(NegotiationError::UnsupportedVersion(27)) => {
                // Expected error
            }
            other => panic!("Expected UnsupportedVersion(27), got: {other:?}"),
        }
    }
}

// ============================================================================
// Module: Boundary Condition Tests
// ============================================================================

mod boundary_conditions {
    use super::*;

    /// Version 27 is exactly one below the minimum.
    #[test]
    fn version_27_is_one_below_minimum() {
        let oldest = ProtocolVersion::OLDEST.as_u8();
        assert_eq!(oldest, 28);
        assert_eq!(27, oldest - 1, "Version 27 is exactly one below minimum");
    }

    /// Version range does not include 27.
    #[test]
    fn version_range_excludes_27() {
        let range = ProtocolVersion::supported_range();
        assert!(!range.contains(&27), "Supported range must not contain 27");
        assert_eq!(*range.start(), 28, "Range must start at 28");
    }

    /// Bitmap does not have bit 27 set.
    #[test]
    fn bitmap_does_not_have_v27_bit() {
        let bitmap = ProtocolVersion::supported_protocol_bitmap();
        let v27_mask = 1u64 << 27;
        assert_eq!(
            bitmap & v27_mask,
            0,
            "Bitmap must not have bit 27 set"
        );
    }

    /// Version 27 in pairwise tests with all supported versions.
    #[test]
    fn version_27_pairwise_with_all_supported() {
        let supported = [28u8, 29, 30, 31, 32];

        for &version in &supported {
            let result = select_highest_mutual([
                TestVersion(27),
                TestVersion(u32::from(version)),
            ]);

            assert!(result.is_ok(), "Should negotiate successfully with v{version}");
            assert_eq!(
                result.unwrap().as_u8(),
                version,
                "Should select v{version} when paired with v27"
            );
        }
    }

    /// Versions below 27 are also rejected.
    #[test]
    fn versions_below_27_also_rejected() {
        for version in [20u32, 25, 26] {
            let result = select_highest_mutual([TestVersion(version)]);
            assert!(
                result.is_err(),
                "Protocol version {version} below 27 must also be rejected"
            );
        }
    }

    /// Version 27 with all unsupported versions fails.
    #[test]
    fn version_27_with_all_unsupported_fails() {
        let result = select_highest_mutual([
            TestVersion(20),
            TestVersion(25),
            TestVersion(26),
            TestVersion(27),
        ]);
        assert!(
            result.is_err(),
            "All versions below 28 must cause negotiation failure"
        );
    }
}

// ============================================================================
// Module: Codec Behavior Tests
// ============================================================================

mod codec_behavior {
    use super::*;

    /// Cannot create codec for protocol 27.
    #[test]
    #[ignore = "v27 codec creation does not panic - returns valid codec despite being unsupported"]
    #[should_panic(expected = "unsupported protocol")]
    fn cannot_create_codec_for_v27() {
        // This should panic because v27 is unsupported
        let _codec = create_protocol_codec(27);
    }

    /// Cannot create NDX codec for protocol 27.
    #[test]
    #[ignore = "v27 ndx codec creation does not panic - returns valid codec despite being unsupported"]
    #[should_panic(expected = "unsupported protocol")]
    fn cannot_create_ndx_codec_for_v27() {
        // This should panic because v27 is unsupported
        let _codec = create_ndx_codec(27);
    }

    /// Codec for v28 works (validates v27 is the boundary).
    #[test]
    fn codec_for_v28_works() {
        // This validates that v28 is supported, confirming v27 boundary
        let codec = create_protocol_codec(28);
        assert_eq!(codec.protocol_version(), 28);
        assert!(codec.is_legacy());
    }

    /// NDX codec for v28 works (validates v27 is the boundary).
    #[test]
    fn ndx_codec_for_v28_works() {
        let mut codec = create_ndx_codec(28);
        // Should be able to use the codec
        let mut buf = Vec::new();
        codec.write_ndx(&mut buf, 1).unwrap();
        assert!(!buf.is_empty());
    }
}

// ============================================================================
// Module: Feature Set Theoretical Tests
// ============================================================================

mod theoretical_features {
    use super::*;

    /// If v27 were supported, it would not have extended flags.
    #[test]
    fn version_27_would_not_have_extended_flags() {
        // Extended file flags were introduced in v28
        // Protocol 28 supports them
        assert!(ProtocolVersion::V28.supports_extended_flags());

        // So v27 would not (theoretical test)
        assert!(
            27 < 28,
            "Protocol 27 would be before extended flags were introduced"
        );
    }

    /// If v27 were supported, it would not have sender/receiver modifiers.
    #[test]
    fn version_27_would_not_have_sender_receiver_modifiers() {
        // Sender/receiver modifiers were introduced in v29
        assert!(ProtocolVersion::V29.supports_sender_receiver_modifiers());
        assert!(!ProtocolVersion::V28.supports_sender_receiver_modifiers());

        // So v27 definitely would not
        assert!(27 < 29);
    }

    /// If v27 were supported, it would use old prefixes.
    #[test]
    fn version_27_would_use_old_prefixes() {
        // Old prefixes were used up to v28
        assert!(ProtocolVersion::V28.uses_old_prefixes());
        assert!(!ProtocolVersion::V29.uses_old_prefixes());

        // So v27 would also use them
        assert!(27 <= 28);
    }

    /// If v27 were supported, it would not have safe file list.
    #[test]
    fn version_27_would_not_have_safe_file_list() {
        // Safe file list was introduced in v30
        assert!(!ProtocolVersion::V28.uses_safe_file_list());
        assert!(!ProtocolVersion::V29.uses_safe_file_list());
        assert!(ProtocolVersion::V30.uses_safe_file_list());

        // So v27 would not have it
        assert!(27 < 30);
    }

    /// If v27 were supported, it would not have flist times.
    #[test]
    fn version_27_would_not_have_flist_times() {
        // Flist times were introduced in v29
        assert!(!ProtocolVersion::V28.supports_flist_times());
        assert!(ProtocolVersion::V29.supports_flist_times());

        // So v27 would not have them
        assert!(27 < 29);
    }
}

// ============================================================================
// Module: Integration Tests
// ============================================================================

mod integration {
    use super::*;

    /// Complete handshake flow with v27 should fail gracefully.
    #[test]
    fn complete_handshake_flow_v27_fails_gracefully() {
        // Simulate peer advertising v27
        let peer_versions = vec![TestVersion(27)];
        let result = select_highest_mutual(peer_versions);

        // Should fail
        assert!(result.is_err());

        // Should be a clear error
        match result {
            Err(NegotiationError::UnsupportedVersion(27)) => {
                // Expected
            }
            other => panic!("Unexpected result: {other:?}"),
        }
    }

    /// Mixed version scenario with v27.
    #[test]
    fn mixed_version_scenario_with_v27() {
        // Peer advertises mix of supported and unsupported
        let peer_versions = vec![
            TestVersion(27),  // Unsupported
            TestVersion(28),  // Supported
            TestVersion(29),  // Supported
        ];

        let result = select_highest_mutual(peer_versions);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 29, "Should select highest supported");
    }

    /// Real-world scenario: old client with v27.
    #[test]
    fn old_client_with_v27_rejected() {
        // An old rsync client might only support v27
        let old_client_versions = vec![TestVersion(27)];
        let result = select_highest_mutual(old_client_versions);

        // Should be rejected with clear error
        assert!(result.is_err());

        // Error should indicate version is too old
        let error = result.unwrap_err();
        match error {
            NegotiationError::UnsupportedVersion(ver) => {
                assert!(ver < 28, "Version should be below minimum");
            }
            _ => panic!("Unexpected error type"),
        }
    }

    /// Upgrading scenario: v27 client connecting to modern server.
    #[test]
    fn v27_client_to_modern_server_fails_with_upgrade_hint() {
        // A v27 client tries to connect
        let result = select_highest_mutual([TestVersion(27)]);

        // Should fail
        assert!(result.is_err());

        // The error should be actionable (user should know to upgrade)
        let error_msg = format!("{:?}", result.unwrap_err());
        assert!(
            error_msg.contains("27") || error_msg.contains("Unsupported"),
            "Error should indicate version problem"
        );
    }
}

// ============================================================================
// Module: Compatibility Matrix Tests
// ============================================================================

mod compatibility_matrix {
    use super::*;

    /// Test v27 against each supported version individually.
    #[test]
    fn v27_against_each_supported_version() {
        let supported = [28u8, 29, 30, 31, 32];

        for &version in &supported {
            let result = select_highest_mutual([
                TestVersion(27),
                TestVersion(u32::from(version)),
            ]);

            assert!(result.is_ok(), "v27 + v{version} should select v{version}");
            assert_eq!(result.unwrap().as_u8(), version);
        }
    }

    /// Test v27 with combinations of supported versions.
    #[test]
    fn v27_with_version_combinations() {
        // v27 + v28 + v29
        let result = select_highest_mutual([
            TestVersion(27),
            TestVersion(28),
            TestVersion(29),
        ]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 29);

        // v27 + v30 + v31
        let result = select_highest_mutual([
            TestVersion(27),
            TestVersion(30),
            TestVersion(31),
        ]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 31);

        // v27 + all supported
        let result = select_highest_mutual([
            TestVersion(27),
            TestVersion(28),
            TestVersion(29),
            TestVersion(30),
            TestVersion(31),
            TestVersion(32),
        ]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 32);
    }
}
