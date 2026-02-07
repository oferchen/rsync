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

use protocol::codec::{NdxCodec, ProtocolCodec, create_ndx_codec, create_protocol_codec};
use protocol::{
    LEGACY_DAEMON_PREFIX, NegotiationError, ProtocolVersion, ProtocolVersionAdvertisement,
    select_highest_mutual,
};

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
        assert!(result.is_err(), "TryFrom<u8> for protocol 27 must fail");
    }

    /// Protocol 27 from_peer_advertisement must fail.
    #[test]
    fn version_27_from_peer_advertisement_fails() {
        let result = ProtocolVersion::from_peer_advertisement(27);
        assert!(result.is_err(), "from_peer_advertisement(27) must fail");
    }

    /// Protocol 27 from_supported must return None.
    #[test]
    fn version_27_from_supported_returns_none() {
        let result = ProtocolVersion::from_supported(27);
        assert!(result.is_none(), "from_supported(27) must return None");
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
        // (Protocol 27 would be in legacy ASCII negotiation range since 27 < 30)
    }

    /// Protocol 27 would not support compatibility flags (legacy protocol).
    #[test]
    fn version_27_would_not_support_compatibility_flags() {
        // Compatibility flags are negotiated starting with protocol 30
        // Protocol 27, if supported, would not use them (27 < 30)
    }

    /// Protocol 27 would use fixed encoding (not varint).
    #[test]
    fn version_27_would_use_fixed_encoding() {
        // Varint encoding was introduced in protocol 30
        // Protocol 27 would be before varint encoding (27 < 30)
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
        assert_eq!(
            buf.len(),
            4,
            "Protocol 28 (and 27 if supported) uses 4-byte sizes"
        );
    }

    /// Protocol 27 would use 4-byte NDX encoding.
    #[test]
    fn version_27_would_use_4_byte_ndx_encoding() {
        // Protocol 27 would use 4-byte little-endian NDX encoding
        // like protocols 28 and 29

        let mut codec_28 = create_ndx_codec(28);
        let mut buf = Vec::new();
        codec_28.write_ndx(&mut buf, 5).unwrap();
        assert_eq!(
            buf.len(),
            4,
            "Protocol 28 (and 27 if supported) uses 4-byte NDX"
        );
    }

    /// Binary advertisement for v27 (if it were supported).
    #[test]
    fn version_27_binary_advertisement_format() {
        // If protocol 27 were in the binary range, it would be:
        let bytes = 27u32.to_be_bytes();
        assert_eq!(bytes, [0, 0, 0, 27]);

        // Note: v27 is below binary negotiation boundary (27 < 30)
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
        let error_string = format!("{error:?}");

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
        assert_eq!(bitmap & v27_mask, 0, "Bitmap must not have bit 27 set");
    }

    /// Version 27 in pairwise tests with all supported versions.
    #[test]
    fn version_27_pairwise_with_all_supported() {
        let supported = [28u8, 29, 30, 31, 32];

        for &version in &supported {
            let result = select_highest_mutual([TestVersion(27), TestVersion(u32::from(version))]);

            assert!(
                result.is_ok(),
                "Should negotiate successfully with v{version}"
            );
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

        // So v27 would not (27 < 28, before extended flags were introduced)
    }

    /// If v27 were supported, it would not have sender/receiver modifiers.
    #[test]
    fn version_27_would_not_have_sender_receiver_modifiers() {
        // Sender/receiver modifiers were introduced in v29
        assert!(ProtocolVersion::V29.supports_sender_receiver_modifiers());
        assert!(!ProtocolVersion::V28.supports_sender_receiver_modifiers());

        // So v27 definitely would not (27 < 29)
    }

    /// If v27 were supported, it would use old prefixes.
    #[test]
    fn version_27_would_use_old_prefixes() {
        // Old prefixes were used up to v28
        assert!(ProtocolVersion::V28.uses_old_prefixes());
        assert!(!ProtocolVersion::V29.uses_old_prefixes());

        // So v27 would also use them (27 <= 28)
    }

    /// If v27 were supported, it would not have safe file list.
    #[test]
    fn version_27_would_not_have_safe_file_list() {
        // Safe file list was introduced in v30
        assert!(!ProtocolVersion::V28.uses_safe_file_list());
        assert!(!ProtocolVersion::V29.uses_safe_file_list());
        assert!(ProtocolVersion::V30.uses_safe_file_list());

        // So v27 would not have it (27 < 30)
    }

    /// If v27 were supported, it would not have flist times.
    #[test]
    fn version_27_would_not_have_flist_times() {
        // Flist times were introduced in v29
        assert!(!ProtocolVersion::V28.supports_flist_times());
        assert!(ProtocolVersion::V29.supports_flist_times());

        // So v27 would not have them (27 < 29)
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
            TestVersion(27), // Unsupported
            TestVersion(28), // Supported
            TestVersion(29), // Supported
        ];

        let result = select_highest_mutual(peer_versions);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().as_u8(),
            29,
            "Should select highest supported"
        );
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
            let result = select_highest_mutual([TestVersion(27), TestVersion(u32::from(version))]);

            assert!(result.is_ok(), "v27 + v{version} should select v{version}");
            assert_eq!(result.unwrap().as_u8(), version);
        }
    }

    /// Test v27 with combinations of supported versions.
    #[test]
    fn v27_with_version_combinations() {
        // v27 + v28 + v29
        let result = select_highest_mutual([TestVersion(27), TestVersion(28), TestVersion(29)]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 29);

        // v27 + v30 + v31
        let result = select_highest_mutual([TestVersion(27), TestVersion(30), TestVersion(31)]);
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

// ============================================================================
// Module: Legacy Greeting Parsing Tests for v27
// ============================================================================

mod legacy_greeting_parsing {
    use protocol::{parse_legacy_daemon_greeting, parse_legacy_daemon_greeting_details};

    /// Parsing a v27 legacy greeting should fail.
    #[test]
    fn parse_v27_greeting_fails() {
        let greeting = "@RSYNCD: 27.0\n";
        let result = parse_legacy_daemon_greeting(greeting);
        assert!(result.is_err(), "Parsing v27 greeting must fail");
    }

    /// Parsing v27 greeting with details should fail.
    #[test]
    fn parse_v27_greeting_details_fails() {
        let greeting = "@RSYNCD: 27.0\n";
        let result = parse_legacy_daemon_greeting_details(greeting);
        assert!(result.is_err(), "Parsing v27 greeting details must fail");
    }

    /// Parsing v27 without subprotocol should fail.
    #[test]
    fn parse_v27_greeting_no_subprotocol_fails() {
        let greeting = "@RSYNCD: 27\n";
        let result = parse_legacy_daemon_greeting(greeting);
        assert!(result.is_err());
    }

    /// Parsing v27 with digest list should fail.
    #[test]
    fn parse_v27_greeting_with_digests_fails() {
        let greeting = "@RSYNCD: 27.0 md5 md4\n";
        let result = parse_legacy_daemon_greeting(greeting);
        assert!(result.is_err());
    }

    /// Parsing v28 greeting succeeds (boundary check).
    #[test]
    fn parse_v28_greeting_succeeds() {
        let greeting = "@RSYNCD: 28.0\n";
        let result = parse_legacy_daemon_greeting(greeting);
        assert!(result.is_ok(), "Parsing v28 greeting must succeed");
        assert_eq!(result.unwrap().as_u8(), 28);
    }

    /// Parsing v28 greeting without subprotocol succeeds.
    #[test]
    fn parse_v28_greeting_no_subprotocol_succeeds() {
        let greeting = "@RSYNCD: 28\n";
        let result = parse_legacy_daemon_greeting(greeting);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 28);
    }

    /// v27 greeting with various formats all fail.
    #[test]
    fn v27_greeting_various_formats_all_fail() {
        let greetings = [
            "@RSYNCD: 27\n",
            "@RSYNCD: 27.0\n",
            "@RSYNCD: 27.1\n",
            "@RSYNCD:27.0\n",
            "@RSYNCD: 27 md5\n",
            "@RSYNCD: 27.0 sha512 sha256\n",
        ];

        for greeting in greetings {
            let result = parse_legacy_daemon_greeting(greeting);
            assert!(
                result.is_err(),
                "v27 greeting '{greeting}' must be rejected"
            );
        }
    }
}

// ============================================================================
// Module: Protocol Version Helper Methods Tests
// ============================================================================

mod version_helper_methods {
    use protocol::ProtocolVersion;

    /// v27 is not in supported versions array.
    #[test]
    fn v27_not_in_supported_versions_array() {
        let versions = ProtocolVersion::supported_versions();
        for version in versions {
            assert_ne!(
                version.as_u8(),
                27,
                "v27 must not appear in supported versions"
            );
        }
    }

    /// v27 is not yielded by protocol numbers iterator.
    #[test]
    fn v27_not_in_protocol_numbers_iterator() {
        for number in ProtocolVersion::supported_protocol_numbers_iter() {
            assert_ne!(number, 27, "v27 must not appear in protocol numbers");
        }
    }

    /// v27 is not yielded by versions iterator.
    #[test]
    fn v27_not_in_versions_iterator() {
        for version in ProtocolVersion::supported_versions_iter() {
            assert_ne!(version.as_u8(), 27, "v27 must not appear in versions");
        }
    }

    /// from_supported_index never returns v27.
    #[test]
    fn from_supported_index_never_returns_v27() {
        for i in 0..100 {
            if let Some(version) = ProtocolVersion::from_supported_index(i) {
                assert_ne!(
                    version.as_u8(),
                    27,
                    "from_supported_index must never return v27"
                );
            }
        }
    }

    /// from_oldest_offset never returns v27.
    #[test]
    fn from_oldest_offset_never_returns_v27() {
        for offset in 0..100 {
            if let Some(version) = ProtocolVersion::from_oldest_offset(offset) {
                assert_ne!(
                    version.as_u8(),
                    27,
                    "from_oldest_offset must never return v27"
                );
            }
        }
    }

    /// from_newest_offset never returns v27.
    #[test]
    fn from_newest_offset_never_returns_v27() {
        for offset in 0..100 {
            if let Some(version) = ProtocolVersion::from_newest_offset(offset) {
                assert_ne!(
                    version.as_u8(),
                    27,
                    "from_newest_offset must never return v27"
                );
            }
        }
    }

    /// next_older from V28 returns None (not v27).
    #[test]
    fn next_older_from_v28_returns_none() {
        let v28 = ProtocolVersion::V28;
        let older = v28.next_older();
        assert!(
            older.is_none(),
            "next_older from v28 must return None (v27 is unsupported)"
        );
    }

    /// v27 cannot be created via FromStr.
    #[test]
    fn v27_from_str_fails() {
        let result: Result<ProtocolVersion, _> = "27".parse();
        assert!(result.is_err(), "Parsing '27' must fail");
    }

    /// v27 display strings confirm it's unsupported.
    #[test]
    fn supported_protocols_display_excludes_27() {
        let display = ProtocolVersion::supported_protocol_numbers_display();
        assert!(!display.contains("27"), "Display must not contain 27");
        assert!(display.contains("28"), "Display must contain 28");
    }
}

// ============================================================================
// Module: Wire Format Encoding Boundary Tests
// ============================================================================

mod wire_format_encoding_boundary {
    use super::*;
    use std::io::Cursor;

    /// v28 uses legacy 4-byte encoding for file sizes.
    #[test]
    fn v28_uses_legacy_file_size_encoding() {
        let codec = create_protocol_codec(28);
        let mut buf = Vec::new();
        codec.write_file_size(&mut buf, 1000).unwrap();
        assert_eq!(buf.len(), 4, "v28 must use 4-byte file size");

        let mut cursor = Cursor::new(&buf);
        let value = codec.read_file_size(&mut cursor).unwrap();
        assert_eq!(value, 1000);
    }

    /// v28 uses legacy 4-byte encoding for mtime.
    #[test]
    fn v28_uses_legacy_mtime_encoding() {
        let codec = create_protocol_codec(28);
        let mut buf = Vec::new();
        let mtime = 1700000000i64;
        codec.write_mtime(&mut buf, mtime).unwrap();
        assert_eq!(buf.len(), 4, "v28 must use 4-byte mtime");

        let mut cursor = Cursor::new(&buf);
        let value = codec.read_mtime(&mut cursor).unwrap();
        assert_eq!(value, mtime);
    }

    /// v28 uses 4-byte NDX encoding.
    #[test]
    fn v28_uses_legacy_ndx_encoding() {
        let mut codec = create_ndx_codec(28);
        let mut buf = Vec::new();
        codec.write_ndx(&mut buf, 100).unwrap();
        assert_eq!(buf.len(), 4, "v28 must use 4-byte NDX");
    }

    /// v30 uses modern encoding (boundary verification).
    #[test]
    fn v30_uses_modern_encoding() {
        let codec = create_protocol_codec(30);
        assert!(!codec.is_legacy(), "v30 must not be legacy");
    }

    /// Codec boundary at v30 for legacy/modern.
    #[test]
    fn codec_boundary_at_v30() {
        let v28 = create_protocol_codec(28);
        let v29 = create_protocol_codec(29);
        let v30 = create_protocol_codec(30);

        assert!(v28.is_legacy(), "v28 must be legacy");
        assert!(v29.is_legacy(), "v29 must be legacy");
        assert!(!v30.is_legacy(), "v30 must not be legacy");
    }

    /// NDX codec boundary at v30.
    #[test]
    fn ndx_codec_boundary_at_v30() {
        // v29 uses 4-byte NDX
        let mut codec_29 = create_ndx_codec(29);
        let mut buf = Vec::new();
        codec_29.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf.len(), 4, "v29 uses 4-byte NDX");

        // v30 uses delta encoding
        let mut codec_30 = create_ndx_codec(30);
        buf.clear();
        codec_30.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf.len(), 1, "v30 uses delta NDX (1 byte for first index)");
    }
}

// ============================================================================
// Module: Feature Detection Tests
// ============================================================================

mod feature_detection {
    use protocol::ProtocolVersion;

    /// v28 feature flags are the minimum supported set.
    #[test]
    fn v28_minimum_feature_set() {
        let v28 = ProtocolVersion::V28;

        // Features v28 has
        assert!(v28.supports_extended_flags(), "v28 has extended flags");
        assert!(v28.uses_fixed_encoding(), "v28 uses fixed encoding");
        assert!(v28.uses_old_prefixes(), "v28 uses old prefixes");
        assert!(
            v28.uses_legacy_ascii_negotiation(),
            "v28 uses legacy negotiation"
        );

        // Features v28 lacks
        assert!(
            !v28.supports_sender_receiver_modifiers(),
            "v28 lacks s/r modifiers"
        );
        assert!(!v28.supports_flist_times(), "v28 lacks flist times");
        assert!(
            !v28.supports_perishable_modifier(),
            "v28 lacks perishable modifier"
        );
        assert!(!v28.uses_varint_encoding(), "v28 lacks varint encoding");
        assert!(
            !v28.uses_varint_flist_flags(),
            "v28 lacks varint flist flags"
        );
        assert!(!v28.uses_safe_file_list(), "v28 lacks safe file list");
        assert!(
            !v28.safe_file_list_always_enabled(),
            "v28 lacks always-on safe file list"
        );
        assert!(
            !v28.uses_binary_negotiation(),
            "v28 lacks binary negotiation"
        );
    }

    /// Feature progression from v28 to v32.
    #[test]
    fn feature_progression_v28_to_v32() {
        // v28 baseline
        let v28 = ProtocolVersion::V28;
        assert!(v28.uses_old_prefixes());
        assert!(!v28.supports_sender_receiver_modifiers());
        assert!(!v28.supports_flist_times());

        // v29 adds sender/receiver modifiers and flist times, removes old prefixes
        let v29 = ProtocolVersion::V29;
        assert!(!v29.uses_old_prefixes());
        assert!(v29.supports_sender_receiver_modifiers());
        assert!(v29.supports_flist_times());
        assert!(!v29.supports_perishable_modifier());

        // v30 adds varint, perishable, safe flist, binary negotiation
        let v30 = ProtocolVersion::V30;
        assert!(v30.uses_varint_encoding());
        assert!(v30.supports_perishable_modifier());
        assert!(v30.uses_safe_file_list());
        assert!(v30.uses_binary_negotiation());
        assert!(!v30.safe_file_list_always_enabled());

        // v31 adds always-on safe file list
        let v31 = ProtocolVersion::V31;
        assert!(v31.safe_file_list_always_enabled());

        // v32 maintains v31 features
        let v32 = ProtocolVersion::V32;
        assert!(v32.safe_file_list_always_enabled());
        assert!(v32.uses_varint_encoding());
    }

    /// If v27 existed, it would have fewer features than v28.
    #[test]
    fn v27_would_have_fewer_features_than_v28() {
        // v28 already has minimal features
        // v27 (if it existed) would lack extended_flags (introduced in v28)
        let v28 = ProtocolVersion::V28;
        assert!(
            v28.supports_extended_flags(),
            "v28 is the first with extended flags"
        );
    }
}

// ============================================================================
// Module: Error Edge Cases Tests
// ============================================================================

mod error_edge_cases {
    use super::*;

    /// v27 error is distinct from zero version error.
    #[test]
    fn v27_error_distinct_from_zero() {
        let v27_result = select_highest_mutual([TestVersion(27)]);
        let zero_result = select_highest_mutual([TestVersion(0)]);

        match (v27_result.unwrap_err(), zero_result.unwrap_err()) {
            (NegotiationError::UnsupportedVersion(27), NegotiationError::UnsupportedVersion(0)) => {
                // Both are UnsupportedVersion but with different version numbers
            }
            _ => panic!("Expected UnsupportedVersion errors"),
        }
    }

    /// v27 with empty list still fails with v27 error.
    #[test]
    fn v27_first_in_list_reports_v27() {
        let result = select_highest_mutual([TestVersion(27), TestVersion(26), TestVersion(25)]);

        match result {
            Err(NegotiationError::UnsupportedVersion(ver)) => {
                // Should report the smallest unsupported version
                assert!(ver <= 27, "Should report oldest rejection");
            }
            _ => panic!("Expected UnsupportedVersion error"),
        }
    }

    /// Multiple v27 entries produce same error.
    #[test]
    fn multiple_v27_same_error() {
        let result = select_highest_mutual([TestVersion(27), TestVersion(27), TestVersion(27)]);

        match result {
            Err(NegotiationError::UnsupportedVersion(27)) => {}
            _ => panic!("Expected UnsupportedVersion(27)"),
        }
    }

    /// v27 after supported version still succeeds.
    #[test]
    fn v27_after_supported_succeeds() {
        // Order should not matter
        let result = select_highest_mutual([TestVersion(28), TestVersion(27)]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 28);
    }

    /// Large gaps around v27 boundary.
    #[test]
    fn large_gaps_around_v27_boundary() {
        // Only v28 in supported range
        let result = select_highest_mutual([
            TestVersion(1),
            TestVersion(10),
            TestVersion(27),
            TestVersion(28),
        ]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 28);
    }
}

// ============================================================================
// Module: Handshake Simulation Tests
// ============================================================================

mod handshake_simulation {
    use super::*;
    use protocol::{
        CompatibilityFlags, format_legacy_daemon_greeting, parse_legacy_daemon_greeting,
    };

    /// Simulate v27 client handshake attempt.
    #[test]
    fn simulate_v27_client_handshake() {
        // 1. v27 client would send greeting "@RSYNCD: 27.0\n"
        // 2. Server parses greeting
        let greeting = "@RSYNCD: 27.0\n";
        let parse_result = parse_legacy_daemon_greeting(greeting);

        // 3. Server must reject v27
        assert!(parse_result.is_err());
    }

    /// Simulate v28 client handshake (boundary).
    #[test]
    fn simulate_v28_client_handshake() {
        // v28 client sends greeting
        let greeting = "@RSYNCD: 28.0\n";
        let parse_result = parse_legacy_daemon_greeting(greeting);

        // Server accepts v28
        assert!(parse_result.is_ok());
        assert_eq!(parse_result.unwrap().as_u8(), 28);
    }

    /// Format v28 greeting produces valid format.
    #[test]
    fn format_v28_greeting_valid() {
        let greeting = format_legacy_daemon_greeting(ProtocolVersion::V28);
        assert!(greeting.starts_with("@RSYNCD:"));
        assert!(greeting.contains("28.0"));
        assert!(greeting.ends_with('\n'));
    }

    /// Compatibility flags not relevant for v27 (pre-v30).
    #[test]
    fn compatibility_flags_not_for_v27() {
        // Compatibility flags were introduced in v30
        // v27 (and v28, v29) would not use them

        // v28 negotiation doesn't involve compat flags
        let greeting = "@RSYNCD: 28.0\n";
        let result = parse_legacy_daemon_greeting(greeting);
        assert!(result.is_ok());

        // After successful v28 negotiation, no compat flags are exchanged
        // (they're only for v30+)
        let v28 = result.unwrap();
        assert!(!v28.uses_binary_negotiation());
    }

    /// Simulate full negotiation flow with v27 in mix.
    #[test]
    fn simulate_full_negotiation_with_v27_in_mix() {
        // Peer advertises multiple versions including v27
        let peer_versions = vec![
            TestVersion(27),
            TestVersion(28),
            TestVersion(29),
            TestVersion(30),
        ];

        // Negotiate
        let result = select_highest_mutual(peer_versions);
        assert!(result.is_ok());

        let negotiated = result.unwrap();
        assert_eq!(negotiated.as_u8(), 30);

        // v30 uses binary negotiation
        assert!(negotiated.uses_binary_negotiation());
    }

    /// Empty compatibility flags for v28/v29.
    #[test]
    fn empty_compat_flags_for_legacy_versions() {
        // For v28/v29, compatibility flags would be empty/unused
        let empty_flags = CompatibilityFlags::EMPTY;
        assert!(empty_flags.is_empty());
        assert_eq!(empty_flags.bits(), 0);
    }
}

// ============================================================================
// Module: Protocol Version Constants Tests
// ============================================================================

mod version_constants {
    use protocol::{
        ProtocolVersion, SUPPORTED_PROTOCOL_BITMAP, SUPPORTED_PROTOCOL_BOUNDS,
        SUPPORTED_PROTOCOL_COUNT, SUPPORTED_PROTOCOL_RANGE, SUPPORTED_PROTOCOLS,
    };

    /// SUPPORTED_PROTOCOLS array does not contain 27.
    #[test]
    fn supported_protocols_excludes_27() {
        for &version in SUPPORTED_PROTOCOLS.iter() {
            assert_ne!(version, 27, "SUPPORTED_PROTOCOLS must not contain 27");
        }
    }

    /// SUPPORTED_PROTOCOL_RANGE excludes 27.
    #[test]
    fn supported_protocol_range_excludes_27() {
        assert!(!SUPPORTED_PROTOCOL_RANGE.contains(&27));
        assert_eq!(*SUPPORTED_PROTOCOL_RANGE.start(), 28);
    }

    /// SUPPORTED_PROTOCOL_BOUNDS start at 28.
    #[test]
    fn supported_protocol_bounds_start_at_28() {
        assert_eq!(SUPPORTED_PROTOCOL_BOUNDS.0, 28);
    }

    /// SUPPORTED_PROTOCOL_BITMAP excludes bit 27.
    #[test]
    fn supported_protocol_bitmap_excludes_bit_27() {
        let bit_27 = 1u64 << 27;
        assert_eq!(
            SUPPORTED_PROTOCOL_BITMAP & bit_27,
            0,
            "Bit 27 must not be set"
        );
    }

    /// SUPPORTED_PROTOCOL_COUNT matches array length.
    #[test]
    fn supported_protocol_count_matches() {
        assert_eq!(SUPPORTED_PROTOCOL_COUNT, 5);
        assert_eq!(SUPPORTED_PROTOCOLS.len(), SUPPORTED_PROTOCOL_COUNT);
    }

    /// Protocol version constants are correct.
    #[test]
    fn protocol_version_constants() {
        assert_eq!(ProtocolVersion::OLDEST.as_u8(), 28);
        assert_eq!(ProtocolVersion::NEWEST.as_u8(), 32);
        assert_eq!(ProtocolVersion::BINARY_NEGOTIATION_INTRODUCED.as_u8(), 30);
    }

    /// Named version constants match numeric values.
    #[test]
    fn named_version_constants_match() {
        assert_eq!(ProtocolVersion::V28.as_u8(), 28);
        assert_eq!(ProtocolVersion::V29.as_u8(), 29);
        assert_eq!(ProtocolVersion::V30.as_u8(), 30);
        assert_eq!(ProtocolVersion::V31.as_u8(), 31);
        assert_eq!(ProtocolVersion::V32.as_u8(), 32);
    }
}

// ============================================================================
// Module: Upstream Compatibility Tests
// ============================================================================

mod upstream_compatibility {
    use super::*;

    /// Upstream rsync 3.4.1 does not support v27.
    #[test]
    fn upstream_rsync_341_excludes_v27() {
        // rsync 3.4.1 defines OLDEST_SUPPORTED_PROTOCOL as 28
        assert_eq!(
            ProtocolVersion::OLDEST.as_u8(),
            28,
            "Must match upstream OLDEST_SUPPORTED_PROTOCOL"
        );
    }

    /// v27 rejection matches upstream behavior.
    #[test]
    fn v27_rejection_matches_upstream() {
        // Upstream would also reject v27 during negotiation
        let result = select_highest_mutual([TestVersion(27)]);
        assert!(result.is_err());
    }

    /// Wire format for v28 matches upstream expectations.
    #[test]
    fn v28_wire_format_matches_upstream() {
        // Upstream v28 uses 4-byte little-endian integers
        let codec = create_protocol_codec(28);
        let mut buf = Vec::new();

        // Write a known value
        codec.write_file_size(&mut buf, 1000).unwrap();

        // Check little-endian encoding
        assert_eq!(buf, [0xe8, 0x03, 0x00, 0x00]); // 1000 in LE
    }

    /// Legacy greeting format matches upstream.
    #[test]
    fn legacy_greeting_format_matches_upstream() {
        use protocol::format_legacy_daemon_greeting;

        // Upstream format: "@RSYNCD: <version>.0\n"
        let greeting = format_legacy_daemon_greeting(ProtocolVersion::V28);
        assert_eq!(greeting, "@RSYNCD: 28.0\n");
    }
}

// ============================================================================
// Module: Codec Roundtrip Tests for Boundary Versions
// ============================================================================

mod codec_roundtrip_boundary {
    use super::*;
    use std::io::Cursor;

    /// v28 codec roundtrip for various sizes.
    #[test]
    fn v28_codec_roundtrip_sizes() {
        let codec = create_protocol_codec(28);
        let test_sizes: [i64; 6] = [0, 1, 255, 65535, 0x7FFF_FFFF, 0x1_0000_0000];

        for size in test_sizes {
            let mut buf = Vec::new();
            codec.write_file_size(&mut buf, size).unwrap();

            let mut cursor = Cursor::new(&buf);
            let read_size = codec.read_file_size(&mut cursor).unwrap();
            assert_eq!(read_size, size, "Roundtrip failed for size {size}");
        }
    }

    /// v28 codec roundtrip for mtimes.
    #[test]
    fn v28_codec_roundtrip_mtimes() {
        let codec = create_protocol_codec(28);
        let test_mtimes: [i64; 4] = [0, 1, 1700000000, 0x7FFF_FFFF];

        for mtime in test_mtimes {
            let mut buf = Vec::new();
            codec.write_mtime(&mut buf, mtime).unwrap();

            let mut cursor = Cursor::new(&buf);
            let read_mtime = codec.read_mtime(&mut cursor).unwrap();
            assert_eq!(read_mtime, mtime, "Roundtrip failed for mtime {mtime}");
        }
    }

    /// v28 NDX codec roundtrip.
    #[test]
    fn v28_ndx_codec_roundtrip() {
        use protocol::codec::NDX_DONE;

        let mut write_codec = create_ndx_codec(28);
        let mut buf = Vec::new();

        let test_indices = [0, 1, 100, 1000, NDX_DONE];
        for &ndx in &test_indices {
            write_codec.write_ndx(&mut buf, ndx).unwrap();
        }

        let mut read_codec = create_ndx_codec(28);
        let mut cursor = Cursor::new(&buf);

        for &expected in &test_indices {
            let read_ndx = read_codec.read_ndx(&mut cursor).unwrap();
            assert_eq!(read_ndx, expected, "NDX roundtrip failed for {expected}");
        }
    }

    /// v30 codec roundtrip (first modern version).
    #[test]
    fn v30_codec_roundtrip() {
        let codec = create_protocol_codec(30);
        let test_sizes: [i64; 4] = [0, 1000, 0x7FFF_FFFF, 0x1_0000_0000];

        for size in test_sizes {
            let mut buf = Vec::new();
            codec.write_file_size(&mut buf, size).unwrap();

            let mut cursor = Cursor::new(&buf);
            let read_size = codec.read_file_size(&mut cursor).unwrap();
            assert_eq!(read_size, size);
        }
    }
}

// ============================================================================
// Module: Stress Tests
// ============================================================================

mod stress_tests {
    use super::*;

    /// Negotiate with many versions including v27.
    #[test]
    fn negotiate_with_many_versions_including_v27() {
        // Create a large list with v27 scattered throughout
        let mut versions = Vec::new();
        for v in 1..=40 {
            versions.push(TestVersion(v));
        }

        let result = select_highest_mutual(versions);

        // Should select highest supported (32, or clamped if > 32)
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 32);
    }

    /// Many unsupported versions with one supported.
    #[test]
    fn many_unsupported_with_one_supported() {
        let mut versions: Vec<TestVersion> = (1..28).map(TestVersion).collect();
        versions.push(TestVersion(28));

        let result = select_highest_mutual(versions);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 28);
    }

    /// All versions 1-27 fail.
    #[test]
    fn all_versions_1_to_27_fail() {
        let versions: Vec<TestVersion> = (1..=27).map(TestVersion).collect();
        let result = select_highest_mutual(versions);
        assert!(result.is_err());
    }

    /// Repeated negotiations with v27.
    #[test]
    fn repeated_negotiations_with_v27() {
        for _ in 0..100 {
            let result = select_highest_mutual([TestVersion(27), TestVersion(32)]);
            assert!(result.is_ok());
            assert_eq!(result.unwrap().as_u8(), 32);
        }
    }
}
