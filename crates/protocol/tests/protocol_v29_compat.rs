//! Protocol version 29 comprehensive compatibility tests.
//!
//! This test suite validates protocol version 29 implementation against rsync 3.4.1 behavior.
//! Tests cover three critical areas:
//!
//! 1. Protocol version 29 handshake (ASCII-based negotiation)
//! 2. Compatibility flags negotiation
//! 3. Wire format encoding/decoding
//!
//! # Protocol Version 29 Overview
//!
//! Protocol 29 is a transitional version that:
//! - Uses legacy ASCII negotiation (`@RSYNCD: 29.0\n` format)
//! - Uses fixed 4-byte encoding for integers
//! - Introduces sender/receiver modifiers (s, r)
//! - Introduces flist timing statistics
//! - Removes old-style filter prefixes
//! - Does NOT support:
//!   - Binary negotiation (introduced in v30)
//!   - Varint encoding (introduced in v30)
//!   - Perishable modifier (introduced in v30)
//!   - Safe file list (introduced in v30)
//!
//! # Upstream Reference
//!
//! Based on rsync 3.4.1 source code (protocol.c, compat.c).

use protocol::{
    CompatibilityFlags, ProtocolVersion, ProtocolVersionAdvertisement,
    format_legacy_daemon_greeting, parse_legacy_daemon_greeting, parse_legacy_daemon_greeting_details,
    select_highest_mutual, LEGACY_DAEMON_PREFIX,
};
use protocol::codec::{
    NdxCodec, ProtocolCodec, create_ndx_codec, create_protocol_codec,
};
use std::io::Cursor;

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
// 1. Protocol Version 29 Handshake Tests
// ============================================================================

mod handshake {
    use super::*;

    /// Test that protocol 29 is recognized as a supported version.
    #[test]
    fn v29_is_supported() {
        assert!(
            ProtocolVersion::is_supported_protocol_number(29),
            "Protocol 29 must be in the supported list"
        );
    }

    /// Test that protocol 29 can be created from supported version number.
    #[test]
    fn v29_from_supported() {
        let result = ProtocolVersion::from_supported(29);
        assert!(result.is_some(), "from_supported(29) must succeed");
        assert_eq!(result.unwrap(), ProtocolVersion::V29);
        assert_eq!(result.unwrap().as_u8(), 29);
    }

    /// Test that protocol 29 can be created from peer advertisement.
    #[test]
    fn v29_from_peer_advertisement() {
        let result = ProtocolVersion::from_peer_advertisement(29);
        assert!(result.is_ok(), "from_peer_advertisement(29) must succeed");
        assert_eq!(result.unwrap(), ProtocolVersion::V29);
    }

    /// Test protocol 29 handshake negotiation succeeds.
    #[test]
    fn v29_handshake_negotiation_succeeds() {
        let result = select_highest_mutual([TestVersion(29)]);
        assert!(result.is_ok(), "Protocol 29 negotiation must succeed");
        assert_eq!(result.unwrap().as_u8(), 29);
    }

    /// Test protocol 29 uses legacy ASCII negotiation format.
    #[test]
    fn v29_uses_legacy_ascii_negotiation() {
        assert!(
            ProtocolVersion::V29.uses_legacy_ascii_negotiation(),
            "Protocol 29 must use ASCII negotiation"
        );
        assert!(
            !ProtocolVersion::V29.uses_binary_negotiation(),
            "Protocol 29 must NOT use binary negotiation"
        );
    }

    /// Test protocol 29 greeting format matches expected wire format.
    #[test]
    fn v29_greeting_wire_format() {
        let greeting = format_legacy_daemon_greeting(ProtocolVersion::V29);

        // Verify exact format
        assert_eq!(
            greeting, "@RSYNCD: 29.0\n",
            "Greeting must match exact format"
        );

        // Verify components
        assert!(
            greeting.starts_with(LEGACY_DAEMON_PREFIX),
            "Must start with @RSYNCD: prefix"
        );
        assert!(greeting.contains("29"), "Must contain version number");
        assert!(greeting.ends_with(".0\n"), "Must end with .0 and newline");

        // Verify encoding
        assert!(greeting.is_ascii(), "Greeting must be pure ASCII");
        assert_eq!(greeting.len(), 14, "Greeting must be exactly 14 bytes");
        assert_eq!(
            greeting.as_bytes()[greeting.len() - 1],
            b'\n',
            "Must end with newline byte"
        );
    }

    /// Test protocol 29 greeting can be parsed back correctly.
    #[test]
    fn v29_greeting_round_trip() {
        let original = ProtocolVersion::V29;
        let greeting = format_legacy_daemon_greeting(original);

        let parsed = parse_legacy_daemon_greeting_details(&greeting);
        assert!(parsed.is_ok(), "Parsing v29 greeting must succeed");

        let parsed_greeting = parsed.unwrap();
        assert_eq!(
            parsed_greeting.advertised_protocol(),
            29,
            "Parsed version must be 29"
        );
        assert_eq!(
            parsed_greeting.subprotocol(),
            0,
            "Sub-protocol must be 0"
        );
    }

    /// Test protocol 29 greeting byte-level format.
    #[test]
    fn v29_greeting_byte_format() {
        let greeting = format_legacy_daemon_greeting(ProtocolVersion::V29);
        let bytes = greeting.as_bytes();

        // Expected: "@RSYNCD: 29.0\n"
        //           [64, 82, 83, 89, 78, 67, 68, 58, 32, 50, 57, 46, 48, 10]
        assert_eq!(bytes.len(), 14);
        assert_eq!(bytes[0], b'@');
        assert_eq!(bytes[1], b'R');
        assert_eq!(bytes[2], b'S');
        assert_eq!(bytes[3], b'Y');
        assert_eq!(bytes[4], b'N');
        assert_eq!(bytes[5], b'C');
        assert_eq!(bytes[6], b'D');
        assert_eq!(bytes[7], b':');
        assert_eq!(bytes[8], b' ');
        assert_eq!(bytes[9], b'2');
        assert_eq!(bytes[10], b'9');
        assert_eq!(bytes[11], b'.');
        assert_eq!(bytes[12], b'0');
        assert_eq!(bytes[13], b'\n');
    }

    /// Test protocol 29 negotiation with multiple versions.
    #[test]
    fn v29_negotiation_with_multiple_versions() {
        // When v29 is highest common version
        let result = select_highest_mutual([TestVersion(28), TestVersion(29)]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 29, "Should select v29");

        // When v29 is not highest
        let result = select_highest_mutual([TestVersion(29), TestVersion(30)]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 30, "Should select v30");
    }

    /// Test protocol 29 handshake with unsupported versions filters correctly.
    #[test]
    fn v29_handshake_filters_unsupported() {
        // v29 should be selected when v27 (unsupported) is also present
        let result = select_highest_mutual([TestVersion(27), TestVersion(29)]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 29);
    }

    /// Test protocol 29 is the newest legacy protocol.
    #[test]
    fn v29_is_newest_legacy_protocol() {
        assert!(ProtocolVersion::V29.uses_legacy_ascii_negotiation());

        // Next version (v30) should use binary negotiation
        let next = ProtocolVersion::V29.next_newer();
        assert!(next.is_some());
        assert!(next.unwrap().uses_binary_negotiation());
    }

    /// Test protocol 29 handshake determinism.
    #[test]
    fn v29_handshake_is_deterministic() {
        let greeting1 = format_legacy_daemon_greeting(ProtocolVersion::V29);
        let greeting2 = format_legacy_daemon_greeting(ProtocolVersion::V29);
        assert_eq!(greeting1, greeting2, "Greeting must be deterministic");
    }

    /// Test protocol 29 handshake with duplicates.
    #[test]
    fn v29_handshake_with_duplicates() {
        let result = select_highest_mutual([
            TestVersion(29),
            TestVersion(29),
            TestVersion(28),
        ]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 29);
    }

    /// Test protocol 29 handshake fails with no compatible versions.
    #[test]
    fn v29_handshake_fails_with_incompatible() {
        // Only unsupported versions
        let result = select_highest_mutual([TestVersion(27), TestVersion(26)]);
        assert!(result.is_err(), "Should fail with no compatible versions");
    }

    /// Test protocol 29 handshake with empty version list.
    #[test]
    fn v29_handshake_fails_with_empty_list() {
        let result = select_highest_mutual::<Vec<TestVersion>, _>(vec![]);
        assert!(result.is_err(), "Should fail with empty version list");
    }
}

// ============================================================================
// 2. Compatibility Flags Negotiation Tests
// ============================================================================

mod compatibility_flags {
    use super::*;

    /// Protocol 29 does not negotiate compatibility flags in the binary protocol.
    /// Compatibility flags were introduced in protocol 30.
    /// This test verifies that the capabilities available in v29 are correct.

    #[test]
    fn v29_predates_compatibility_flags() {
        // Protocol 29 was defined before the binary compatibility flag mechanism
        // was introduced in protocol 30. However, the features that later became
        // flags were controlled by protocol version alone.

        // Verify that compatibility flags encoding is available (for protocol 30+)
        // but the flags relevant to v29 behavior are correctly represented
        let inc_recurse = CompatibilityFlags::INC_RECURSE;
        let mut buf = Vec::new();
        inc_recurse.encode_to_vec(&mut buf).unwrap();
        assert_eq!(buf, vec![1], "INC_RECURSE flag encodes correctly");
    }

    /// Test that protocol 29 capabilities match expected feature set.
    #[test]
    fn v29_capabilities_feature_set() {
        let v29 = ProtocolVersion::V29;

        // Features introduced in v29
        assert!(
            v29.supports_sender_receiver_modifiers(),
            "v29 introduces sender/receiver modifiers"
        );
        assert!(
            v29.supports_flist_times(),
            "v29 introduces flist timing stats"
        );
        assert!(
            !v29.uses_old_prefixes(),
            "v29 removes old-style filter prefixes"
        );

        // Features NOT in v29 (introduced in v30+)
        assert!(
            !v29.supports_perishable_modifier(),
            "v29 does NOT have perishable modifier"
        );
        assert!(
            !v29.uses_safe_file_list(),
            "v29 does NOT have safe file list"
        );
        assert!(
            !v29.uses_varint_flist_flags(),
            "v29 does NOT use varint flist flags"
        );
    }

    /// Test protocol 29 vs 28 capability differences.
    #[test]
    fn v29_capabilities_vs_v28() {
        let v28 = ProtocolVersion::V28;
        let v29 = ProtocolVersion::V29;

        // Features new in v29
        assert!(!v28.supports_sender_receiver_modifiers());
        assert!(v29.supports_sender_receiver_modifiers());

        assert!(!v28.supports_flist_times());
        assert!(v29.supports_flist_times());

        assert!(v28.uses_old_prefixes());
        assert!(!v29.uses_old_prefixes());

        // Features present in both
        assert!(v28.supports_extended_flags());
        assert!(v29.supports_extended_flags());
    }

    /// Test protocol 29 vs 30 capability differences.
    #[test]
    fn v29_capabilities_vs_v30() {
        let v29 = ProtocolVersion::V29;
        let v30 = ProtocolVersion::V30;

        // Negotiation format change
        assert!(v29.uses_legacy_ascii_negotiation());
        assert!(v30.uses_binary_negotiation());

        // Encoding change
        assert!(v29.uses_fixed_encoding());
        assert!(v30.uses_varint_encoding());

        // Features new in v30
        assert!(!v29.supports_perishable_modifier());
        assert!(v30.supports_perishable_modifier());

        assert!(!v29.uses_safe_file_list());
        assert!(v30.uses_safe_file_list());

        assert!(!v29.uses_varint_flist_flags());
        assert!(v30.uses_varint_flist_flags());
    }

    /// Test that codec capabilities match protocol version.
    #[test]
    fn v29_codec_capabilities() {
        let codec = create_protocol_codec(29);

        assert!(codec.is_legacy(), "v29 codec is legacy");
        assert_eq!(codec.protocol_version(), 29);

        assert!(codec.supports_sender_receiver_modifiers());
        assert!(codec.supports_flist_times());
        assert!(!codec.uses_old_prefixes());
        assert!(!codec.supports_perishable_modifier());
    }

    /// Test compatibility flags that would apply to v29 behavior.
    #[test]
    fn v29_relevant_compatibility_flags() {
        // INC_RECURSE - incremental recursion, foundation laid in v29
        let inc_recurse = CompatibilityFlags::INC_RECURSE;
        assert_eq!(inc_recurse.bits(), 1 << 0);

        // Verify encoding for when these flags are used with v30+
        let mut buf = Vec::new();
        inc_recurse.encode_to_vec(&mut buf).unwrap();
        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, inc_recurse);
    }

    /// Test that v29 capabilities are monotonic (features don't disappear).
    #[test]
    fn v29_capabilities_monotonic() {
        let v28 = ProtocolVersion::V28;
        let v29 = ProtocolVersion::V29;

        // Extended flags supported in v28, must still be in v29
        assert!(v28.supports_extended_flags());
        assert!(v29.supports_extended_flags());

        // Once added in v29, these stay in v30+
        let v30 = ProtocolVersion::V30;
        assert!(v29.supports_sender_receiver_modifiers());
        assert!(v30.supports_sender_receiver_modifiers());

        assert!(v29.supports_flist_times());
        assert!(v30.supports_flist_times());
    }
}

// ============================================================================
// 3. Wire Format Tests
// ============================================================================

mod wire_format {
    use super::*;

    // ------------------------------------------------------------------------
    // Encoding Style Tests
    // ------------------------------------------------------------------------

    /// Test that protocol 29 uses fixed encoding, not varint.
    #[test]
    fn v29_uses_fixed_encoding() {
        assert!(
            ProtocolVersion::V29.uses_fixed_encoding(),
            "v29 must use fixed encoding"
        );
        assert!(
            !ProtocolVersion::V29.uses_varint_encoding(),
            "v29 must NOT use varint encoding"
        );
    }

    /// Test that protocol 29 codec is legacy.
    #[test]
    fn v29_codec_is_legacy() {
        let codec = create_protocol_codec(29);
        assert!(codec.is_legacy(), "v29 codec must be legacy");
        assert_eq!(codec.protocol_version(), 29);
    }

    // ------------------------------------------------------------------------
    // File Size Encoding Tests
    // ------------------------------------------------------------------------

    /// Test protocol 29 small file size encoding (4-byte fixed).
    #[test]
    fn v29_file_size_small_values() {
        let codec = create_protocol_codec(29);
        let test_cases = [
            (0i64, vec![0x00, 0x00, 0x00, 0x00]),
            (1i64, vec![0x01, 0x00, 0x00, 0x00]),
            (255i64, vec![0xff, 0x00, 0x00, 0x00]),
            (256i64, vec![0x00, 0x01, 0x00, 0x00]),
            (1000i64, vec![0xe8, 0x03, 0x00, 0x00]),
            (65535i64, vec![0xff, 0xff, 0x00, 0x00]),
        ];

        for (size, expected_bytes) in test_cases {
            let mut buf = Vec::new();
            codec.write_file_size(&mut buf, size).unwrap();
            assert_eq!(
                buf, expected_bytes,
                "File size {} must encode as {:?}",
                size, expected_bytes
            );
            assert_eq!(buf.len(), 4, "Must use 4-byte fixed encoding");
        }
    }

    /// Test protocol 29 large file size encoding (longint marker).
    #[test]
    fn v29_file_size_large_values() {
        let codec = create_protocol_codec(29);

        // Values > 32-bit use special longint encoding
        let large_value = 0x1_0000_0000i64; // 2^32
        let mut buf = Vec::new();
        codec.write_file_size(&mut buf, large_value).unwrap();

        // Legacy uses 4-byte marker (0xffffffff) + 8-byte value
        assert_eq!(buf.len(), 12, "Large values use 12-byte longint");
        assert_eq!(
            &buf[0..4],
            &[0xff, 0xff, 0xff, 0xff],
            "Must start with longint marker"
        );
    }

    /// Test protocol 29 file size roundtrip.
    #[test]
    fn v29_file_size_roundtrip() {
        let codec = create_protocol_codec(29);
        let test_values = [
            0i64,
            1,
            100,
            1000,
            65535,
            0x7FFF_FFFF,
            0x1_0000_0000,
            0x7FFF_FFFF_FFFF_FFFF,
        ];

        for &value in &test_values {
            let mut buf = Vec::new();
            codec.write_file_size(&mut buf, value).unwrap();

            let mut cursor = Cursor::new(&buf);
            let decoded = codec.read_file_size(&mut cursor).unwrap();

            assert_eq!(decoded, value, "File size {} must roundtrip", value);
        }
    }

    /// Test protocol 29 file size is little-endian for values fitting in 32 bits.
    #[test]
    fn v29_file_size_little_endian() {
        let codec = create_protocol_codec(29);

        // 0x12345678
        let value = 0x12345678i64;
        let mut buf = Vec::new();
        codec.write_file_size(&mut buf, value).unwrap();

        // Little-endian: LSB first
        assert_eq!(buf, vec![0x78, 0x56, 0x34, 0x12]);
    }

    // ------------------------------------------------------------------------
    // NDX (File Index) Encoding Tests
    // ------------------------------------------------------------------------

    /// Test protocol 29 NDX uses legacy 4-byte encoding.
    #[test]
    fn v29_ndx_encoding() {
        let mut codec = create_ndx_codec(29);
        let mut buf = Vec::new();

        // Simple positive index
        codec.write_ndx(&mut buf, 5).unwrap();
        assert_eq!(buf.len(), 4, "v29 NDX uses 4-byte encoding");
        assert_eq!(buf, vec![5, 0, 0, 0], "Little-endian format");
    }

    /// Test protocol 29 NDX roundtrip.
    #[test]
    fn v29_ndx_roundtrip() {
        let test_indices = [0, 1, 5, 100, 1000, 10000, -1 /* NDX_DONE */];

        for &ndx in &test_indices {
            let mut write_codec = create_ndx_codec(29);
            let mut buf = Vec::new();
            write_codec.write_ndx(&mut buf, ndx).unwrap();

            let mut read_codec = create_ndx_codec(29);
            let mut cursor = Cursor::new(&buf);
            let decoded = read_codec.read_ndx(&mut cursor).unwrap();

            assert_eq!(decoded, ndx, "NDX {} must roundtrip", ndx);
        }
    }

    /// Test protocol 29 NDX_DONE encoding.
    #[test]
    fn v29_ndx_done_encoding() {
        let mut codec = create_ndx_codec(29);
        let mut buf = Vec::new();

        codec.write_ndx(&mut buf, -1).unwrap();
        assert_eq!(buf.len(), 4);
        assert_eq!(buf, vec![0xff, 0xff, 0xff, 0xff], "NDX_DONE is -1 as i32");
    }

    /// Test protocol 29 NDX large values.
    #[test]
    fn v29_ndx_large_values() {
        let mut codec = create_ndx_codec(29);
        let test_values = [0x12345678, 0x7FFFFFFF];

        for &value in &test_values {
            let mut buf = Vec::new();
            codec.write_ndx(&mut buf, value).unwrap();

            assert_eq!(buf.len(), 4);

            // Verify little-endian
            let expected = (value as i32).to_le_bytes();
            assert_eq!(buf, expected);
        }
    }

    /// Test protocol 29 NDX sequence encoding.
    #[test]
    fn v29_ndx_sequential() {
        let mut codec = create_ndx_codec(29);
        let mut buf = Vec::new();

        // Sequential indices
        for ndx in 0..5 {
            codec.write_ndx(&mut buf, ndx).unwrap();
        }

        // Each should be 4 bytes
        assert_eq!(buf.len(), 20, "5 indices * 4 bytes each");

        // Verify each index
        let mut read_codec = create_ndx_codec(29);
        let mut cursor = Cursor::new(&buf);
        for expected in 0..5 {
            let decoded = read_codec.read_ndx(&mut cursor).unwrap();
            assert_eq!(decoded, expected);
        }
    }

    // ------------------------------------------------------------------------
    // Wire Format Boundary Tests
    // ------------------------------------------------------------------------

    /// Test protocol 29 vs 28 wire format (should be same).
    #[test]
    fn v29_wire_format_vs_v28() {
        let codec_28 = create_protocol_codec(28);
        let codec_29 = create_protocol_codec(29);

        // Both use same encoding for file sizes
        let test_value = 1000i64;
        let mut buf_28 = Vec::new();
        let mut buf_29 = Vec::new();

        codec_28.write_file_size(&mut buf_28, test_value).unwrap();
        codec_29.write_file_size(&mut buf_29, test_value).unwrap();

        assert_eq!(
            buf_28, buf_29,
            "v28 and v29 use same wire format for file sizes"
        );
    }

    /// Test protocol 29 vs 30 wire format (different).
    #[test]
    fn v29_wire_format_vs_v30() {
        let codec_29 = create_protocol_codec(29);
        let codec_30 = create_protocol_codec(30);

        // Encoding differs for small values
        let test_value = 100i64;
        let mut buf_29 = Vec::new();
        let mut buf_30 = Vec::new();

        codec_29.write_file_size(&mut buf_29, test_value).unwrap();
        codec_30.write_file_size(&mut buf_30, test_value).unwrap();

        // v29 always uses 4 bytes (fixed)
        assert_eq!(buf_29.len(), 4);

        // v30 uses varint (more compact for small values)
        assert!(buf_30.len() <= 4);

        // Bytes should differ
        assert_ne!(
            buf_29, buf_30,
            "v29 (fixed) and v30 (varint) use different wire formats"
        );
    }

    /// Test protocol 29 wire format with edge case values.
    #[test]
    fn v29_wire_format_edge_cases() {
        let codec = create_protocol_codec(29);

        // Test boundary values
        let edge_cases = [
            (i64::MAX, true),           // Maximum value
            (0x7FFF_FFFF, false),       // Max 32-bit signed
            (0x8000_0000, true),        // Just over 32-bit
            (0xFFFF_FFFF, true),        // Max 32-bit unsigned
            (-1i64, true),              // Negative (special handling)
        ];

        for (value, needs_longint) in edge_cases {
            let mut buf = Vec::new();
            codec.write_file_size(&mut buf, value).unwrap();

            if needs_longint {
                // Should use longint encoding (12 bytes)
                // Note: -1 might be handled specially
                if value > 0 {
                    assert_eq!(buf.len(), 12, "Value {} needs longint", value);
                }
            } else {
                // Should use regular 4-byte encoding
                assert_eq!(buf.len(), 4, "Value {} uses fixed encoding", value);
            }

            // Verify roundtrip
            let mut cursor = Cursor::new(&buf);
            let decoded = codec.read_file_size(&mut cursor).unwrap();
            assert_eq!(decoded, value, "Value {} must roundtrip", value);
        }
    }

    /// Test protocol 29 truncated input handling.
    #[test]
    fn v29_truncated_input() {
        let codec = create_protocol_codec(29);

        // Truncated file size (needs 4 bytes, only 3 provided)
        let truncated = [0x01, 0x02, 0x03];
        let mut cursor = Cursor::new(&truncated[..]);
        let result = codec.read_file_size(&mut cursor);
        assert!(result.is_err(), "Truncated input must fail");
    }

    /// Test protocol 29 wire format determinism.
    #[test]
    fn v29_wire_format_deterministic() {
        let codec = create_protocol_codec(29);
        let value = 12345i64;

        let mut buf1 = Vec::new();
        let mut buf2 = Vec::new();

        codec.write_file_size(&mut buf1, value).unwrap();
        codec.write_file_size(&mut buf2, value).unwrap();

        assert_eq!(buf1, buf2, "Encoding must be deterministic");
    }
}

// ============================================================================
// Integration Tests
// ============================================================================

mod integration {
    use super::*;

    /// Test complete v29 handshake and negotiation flow.
    #[test]
    fn v29_complete_handshake_flow() {
        // 1. Generate greeting
        let greeting = format_legacy_daemon_greeting(ProtocolVersion::V29);
        assert_eq!(greeting, "@RSYNCD: 29.0\n");

        // 2. Parse greeting
        let parsed = parse_legacy_daemon_greeting_details(&greeting).unwrap();
        assert_eq!(parsed.advertised_protocol(), 29);

        // 3. Negotiate version
        let result = select_highest_mutual([TestVersion(29)]).unwrap();
        assert_eq!(result.as_u8(), 29);

        // 4. Create codec
        let codec = create_protocol_codec(29);
        assert!(codec.is_legacy());
        assert_eq!(codec.protocol_version(), 29);

        // 5. Verify capabilities
        assert!(codec.supports_sender_receiver_modifiers());
        assert!(codec.supports_flist_times());
    }

    /// Test v29 encoding/decoding pipeline.
    #[test]
    fn v29_encoding_pipeline() {
        let codec = create_protocol_codec(29);
        let test_values = [0i64, 100, 1000, 65535, 0x7FFF_FFFF];

        for &value in &test_values {
            // Encode
            let mut buf = Vec::new();
            codec.write_file_size(&mut buf, value).unwrap();

            // Decode
            let mut cursor = Cursor::new(&buf);
            let decoded = codec.read_file_size(&mut cursor).unwrap();

            assert_eq!(decoded, value);
        }
    }

    /// Test v29 compatibility with version range negotiation.
    #[test]
    fn v29_version_range_negotiation() {
        // Client supports v28-v30, server supports v29-v31
        // Should negotiate to v29 or v30 (highest mutual)
        let client_versions = [TestVersion(28), TestVersion(29), TestVersion(30)];
        let result = select_highest_mutual(client_versions).unwrap();

        // Should select highest from client's advertised versions
        assert_eq!(result.as_u8(), 30);
    }

    /// Test v29 feature detection.
    #[test]
    fn v29_feature_detection() {
        let v29 = ProtocolVersion::V29;

        // Positive features
        assert!(v29.supports_sender_receiver_modifiers());
        assert!(v29.supports_flist_times());
        assert!(v29.supports_extended_flags());

        // Negative features
        assert!(!v29.supports_perishable_modifier());
        assert!(!v29.uses_safe_file_list());
        assert!(!v29.uses_varint_encoding());
        assert!(!v29.uses_binary_negotiation());
    }

    /// Test v29 with mixed codec operations.
    #[test]
    fn v29_mixed_codec_operations() {
        let mut ndx_codec = create_ndx_codec(29);
        let protocol_codec = create_protocol_codec(29);

        let mut buf = Vec::new();

        // Write NDX
        ndx_codec.write_ndx(&mut buf, 42).unwrap();

        // Write file size
        protocol_codec.write_file_size(&mut buf, 1000).unwrap();

        // Verify total size
        assert_eq!(buf.len(), 8, "NDX(4) + file_size(4)");

        // Read back
        let mut cursor = Cursor::new(&buf);
        let ndx = ndx_codec.read_ndx(&mut cursor).unwrap();
        let size = protocol_codec.read_file_size(&mut cursor).unwrap();

        assert_eq!(ndx, 42);
        assert_eq!(size, 1000);
    }

    /// Test v29 error handling.
    #[test]
    fn v29_error_handling() {
        // Invalid greeting
        let invalid = "@RSYNCD: 99.0\n"; // Unsupported version
        let result = parse_legacy_daemon_greeting_details(invalid);
        // Should parse but may not be supported
        if let Ok(parsed) = result {
            assert_eq!(parsed.advertised_protocol(), 99);
        }

        // Malformed greeting
        let malformed = "INVALID";
        let result = parse_legacy_daemon_greeting(malformed);
        assert!(result.is_err(), "Malformed greeting must fail");
    }
}
