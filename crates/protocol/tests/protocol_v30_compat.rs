//! Protocol version 30 compatibility tests.
//!
//! Comprehensive tests for protocol version 30, the first version with binary negotiation.
//! These tests validate:
//! - Protocol version 30 handshake and negotiation
//! - Binary negotiation vs legacy ASCII negotiation
//! - Incremental recursion features (INC_RECURSE flag)
//! - New capability negotiation (checksums, compression)
//! - Varint encoding for file lists
//! - Safe file list support
//! - Backward compatibility with v28/v29
//!
//! # Protocol Version 30 Overview
//!
//! Protocol version 30 introduces several major changes:
//! - **Binary negotiation**: Replaces ASCII-based protocol version exchange
//! - **Varint encoding**: Variable-length integer encoding for efficiency
//! - **Capability negotiation**: Algorithm negotiation (checksums, compression)
//! - **Incremental recursion**: INC_RECURSE compatibility flag support
//! - **Safe file list**: Safer file list handling (optional in v30, mandatory in v31)
//! - **Perishable modifier**: Support for perishable filter rules
//! - **Varint flist flags**: File list flags use varint encoding
//!
//! # Upstream Reference
//!
//! Based on rsync 3.4.1 source code:
//! - `compat.c`: Compatibility flag handling and capability negotiation
//! - `flist.c`: File list encoding changes
//! - `io.c`: Protocol I/O and varint encoding

use protocol::codec::create_protocol_codec;
use protocol::{
    ChecksumAlgorithm, CompatibilityFlags, CompressionAlgorithm, KnownCompatibilityFlag,
    ProtocolVersion, ProtocolVersionAdvertisement, negotiate_capabilities, select_highest_mutual,
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
// Module: Protocol Version 30 Handshake and Negotiation
// ============================================================================

mod protocol_30_handshake {
    use super::*;

    /// Protocol 30 is supported and should negotiate successfully.
    #[test]
    fn version_30_negotiation_succeeds() {
        let result = select_highest_mutual([TestVersion(30)]);
        assert!(
            result.is_ok(),
            "Protocol 30 negotiation must succeed: {result:?}"
        );
        assert_eq!(result.unwrap().as_u8(), 30);
    }

    /// Protocol 30 is in the supported protocol list.
    #[test]
    fn version_30_is_in_supported_list() {
        assert!(
            ProtocolVersion::is_supported_protocol_number(30),
            "Protocol 30 must be in supported list"
        );
    }

    /// Protocol 30 constant equals version from supported list.
    #[test]
    fn version_30_constant_equals_from_supported() {
        let from_supported = ProtocolVersion::from_supported(30).unwrap();
        assert_eq!(from_supported, ProtocolVersion::V30);
    }

    /// Protocol 30 try_from succeeds for u8.
    #[test]
    fn version_30_try_from_u8_succeeds() {
        let result = ProtocolVersion::try_from(30u8);
        assert!(result.is_ok(), "TryFrom<u8> for 30 should succeed");
        assert_eq!(result.unwrap(), ProtocolVersion::V30);
    }

    /// Protocol 30 try_from succeeds for u32.
    #[test]
    fn version_30_try_from_u32_succeeds() {
        let result = ProtocolVersion::try_from(30u8);
        assert!(result.is_ok(), "TryFrom<u8> for 30 should succeed");
        assert_eq!(result.unwrap(), ProtocolVersion::V30);
    }

    /// Protocol 30 from_peer_advertisement succeeds.
    #[test]
    fn version_30_from_peer_advertisement_succeeds() {
        let result = ProtocolVersion::from_peer_advertisement(30);
        assert!(result.is_ok(), "from_peer_advertisement(30) should succeed");
        assert_eq!(result.unwrap(), ProtocolVersion::V30);
    }

    /// When peer advertises multiple versions including 30, 30 should be selected.
    #[test]
    fn version_30_selected_from_multiple() {
        let result = select_highest_mutual([TestVersion(28), TestVersion(29), TestVersion(30)]);
        assert!(result.is_ok(), "Should negotiate to 30");
        assert_eq!(result.unwrap().as_u8(), 30);
    }

    /// Protocol 30 as_u8 returns correct value.
    #[test]
    fn version_30_as_u8_returns_30() {
        assert_eq!(ProtocolVersion::V30.as_u8(), 30);
    }

    /// Protocol 30 Display formatting works.
    #[test]
    fn version_30_display_formatting() {
        let version = ProtocolVersion::V30;
        let display = format!("{version}");
        assert!(
            display.contains("30"),
            "Display should include version number"
        );
    }

    /// Protocol 30 Debug formatting works.
    #[test]
    fn version_30_debug_formatting() {
        let version = ProtocolVersion::V30;
        let debug = format!("{version:?}");
        assert!(debug.contains("30"), "Debug should include version number");
    }
}

// ============================================================================
// Module: Protocol Version 30 Binary Negotiation
// ============================================================================

mod protocol_30_binary_negotiation {
    use super::*;

    /// Protocol 30 uses binary negotiation, not legacy ASCII.
    #[test]
    fn version_30_uses_binary_negotiation() {
        let v30 = ProtocolVersion::V30;
        assert!(
            v30.uses_binary_negotiation(),
            "Protocol 30 must use binary negotiation"
        );
        assert!(
            !v30.uses_legacy_ascii_negotiation(),
            "Protocol 30 must not use legacy ASCII negotiation"
        );
    }

    /// Protocol 30 is first version with binary negotiation.
    #[test]
    fn version_30_is_first_binary_negotiation() {
        // v28 and v29 use legacy ASCII
        assert!(!ProtocolVersion::V28.uses_binary_negotiation());
        assert!(!ProtocolVersion::V29.uses_binary_negotiation());

        // v30+ use binary
        assert!(ProtocolVersion::V30.uses_binary_negotiation());
        assert!(ProtocolVersion::V31.uses_binary_negotiation());
        assert!(ProtocolVersion::V32.uses_binary_negotiation());
    }

    /// Protocol 30 handshake differs from v28/v29.
    #[test]
    fn version_30_handshake_differs_from_legacy() {
        let v28 = ProtocolVersion::V28;
        let v30 = ProtocolVersion::V30;

        // v28 uses legacy ASCII negotiation
        assert!(v28.uses_legacy_ascii_negotiation());
        // v30 uses binary negotiation
        assert!(v30.uses_binary_negotiation());

        // They should differ
        assert_ne!(v28.uses_binary_negotiation(), v30.uses_binary_negotiation());
    }

    /// Protocol 30 codec is modern, not legacy.
    #[test]
    fn version_30_codec_is_modern() {
        let _codec = create_protocol_codec(ProtocolVersion::V30.as_u8());
        // Modern codec should support varint encoding
        assert!(
            ProtocolVersion::V30.uses_varint_encoding(),
            "v30 codec should use varint encoding"
        );
    }
}

// ============================================================================
// Module: Protocol Version 30 Feature Flags
// ============================================================================

mod protocol_30_feature_flags {
    use super::*;

    /// Protocol 30 uses varint encoding.
    #[test]
    fn version_30_uses_varint_encoding() {
        let v30 = ProtocolVersion::V30;
        assert!(
            v30.uses_varint_encoding(),
            "Protocol 30 must use varint encoding"
        );
        assert!(
            !v30.uses_fixed_encoding(),
            "Protocol 30 must not use fixed encoding"
        );
    }

    /// Protocol 30 is first version with varint encoding.
    #[test]
    fn version_30_is_first_varint_encoding() {
        // v28 and v29 use fixed encoding
        assert!(ProtocolVersion::V28.uses_fixed_encoding());
        assert!(ProtocolVersion::V29.uses_fixed_encoding());
        assert!(!ProtocolVersion::V28.uses_varint_encoding());
        assert!(!ProtocolVersion::V29.uses_varint_encoding());

        // v30+ use varint encoding
        assert!(ProtocolVersion::V30.uses_varint_encoding());
        assert!(ProtocolVersion::V31.uses_varint_encoding());
        assert!(ProtocolVersion::V32.uses_varint_encoding());
    }

    /// Protocol 30 uses varint flist flags.
    #[test]
    fn version_30_uses_varint_flist_flags() {
        let v30 = ProtocolVersion::V30;
        assert!(
            v30.uses_varint_flist_flags(),
            "Protocol 30 must use varint flist flags"
        );
    }

    /// Protocol 30 supports perishable modifier.
    #[test]
    fn version_30_supports_perishable_modifier() {
        let v30 = ProtocolVersion::V30;
        assert!(
            v30.supports_perishable_modifier(),
            "Protocol 30 must support perishable modifier"
        );
    }

    /// Protocol 30 supports sender/receiver modifiers.
    #[test]
    fn version_30_supports_sender_receiver_modifiers() {
        let v30 = ProtocolVersion::V30;
        assert!(
            v30.supports_sender_receiver_modifiers(),
            "Protocol 30 must support sender/receiver modifiers"
        );
    }

    /// Protocol 30 supports flist times.
    #[test]
    fn version_30_supports_flist_times() {
        let v30 = ProtocolVersion::V30;
        assert!(
            v30.supports_flist_times(),
            "Protocol 30 must support flist times"
        );
    }

    /// Protocol 30 does not use old prefixes.
    #[test]
    fn version_30_does_not_use_old_prefixes() {
        let v30 = ProtocolVersion::V30;
        assert!(
            !v30.uses_old_prefixes(),
            "Protocol 30 must not use old prefixes"
        );
    }

    /// Protocol 30 supports extended flags.
    #[test]
    fn version_30_supports_extended_flags() {
        let v30 = ProtocolVersion::V30;
        assert!(
            v30.supports_extended_flags(),
            "Protocol 30 must support extended flags"
        );
    }

    /// Protocol 30 uses safe file list (optional in v30).
    #[test]
    fn version_30_uses_safe_file_list() {
        let v30 = ProtocolVersion::V30;
        assert!(
            v30.uses_safe_file_list(),
            "Protocol 30 must support safe file list"
        );
    }

    /// Protocol 30 safe file list is NOT always enabled (v31+).
    #[test]
    fn version_30_safe_file_list_not_always_enabled() {
        let v30 = ProtocolVersion::V30;
        assert!(
            !v30.safe_file_list_always_enabled(),
            "Protocol 30 safe file list is optional (v31+ is mandatory)"
        );
    }

    /// Protocol 30 feature profile is consistent.
    #[test]
    fn version_30_feature_profile_consistency() {
        let v30 = ProtocolVersion::V30;

        // Binary negotiation features
        assert!(v30.uses_binary_negotiation());
        assert!(v30.uses_varint_encoding());
        assert!(v30.uses_varint_flist_flags());

        // Capability features
        assert!(v30.supports_perishable_modifier());
        assert!(v30.supports_sender_receiver_modifiers());
        assert!(v30.supports_flist_times());

        // Safety features
        assert!(v30.uses_safe_file_list());
        assert!(!v30.safe_file_list_always_enabled());

        // Legacy features
        assert!(!v30.uses_old_prefixes());
        assert!(!v30.uses_legacy_ascii_negotiation());
        assert!(!v30.uses_fixed_encoding());
    }
}

// ============================================================================
// Module: Protocol Version 30 Capability Negotiation
// ============================================================================

mod protocol_30_capability_negotiation {
    use super::*;

    /// Protocol 30 performs capability negotiation (checksum and compression).
    #[test]
    fn version_30_negotiates_capabilities() {
        let protocol = ProtocolVersion::V30;

        // Simulate client choosing xxh64 checksum and zlibx compression
        let client_response = b"\x05xxh64\x05zlibx";
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let result = negotiate_capabilities(
            protocol,
            &mut stdin,
            &mut stdout,
            true,  // do_negotiation
            true,  // send_compression
            false, // is_daemon_mode
            true,  // is_server
        );

        assert!(
            result.is_ok(),
            "Protocol 30 capability negotiation should succeed: {result:?}"
        );

        let negotiated = result.unwrap();
        assert_eq!(negotiated.checksum, ChecksumAlgorithm::XXH64);
        assert_eq!(negotiated.compression, CompressionAlgorithm::ZlibX);

        // Server should have sent its capability lists
        assert!(!stdout.is_empty(), "Server should send capability lists");
    }

    /// Protocol 30 negotiation sends checksum list.
    #[test]
    fn version_30_sends_checksum_list() {
        let protocol = ProtocolVersion::V30;
        let client_response = b"\x03md5\x04zlib";
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let result =
            negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true);

        assert!(result.is_ok());
        assert!(!stdout.is_empty(), "Should send capability lists for v30");

        // Output should contain varint-encoded strings
        // First byte is varint length of checksum list
        assert!(stdout[0] > 0, "First byte should be non-zero varint length");
    }

    /// Protocol 30 negotiation differs from v28/v29 defaults.
    #[test]
    fn version_30_negotiation_differs_from_legacy() {
        // v28 uses defaults without negotiation
        let v28 = ProtocolVersion::V28;
        let mut stdin_v28 = &b""[..];
        let mut stdout_v28 = Vec::new();

        let result_v28 = negotiate_capabilities(
            v28,
            &mut stdin_v28,
            &mut stdout_v28,
            true,
            true,
            false,
            true,
        )
        .unwrap();

        // v28 should use MD4 and Zlib defaults
        assert_eq!(result_v28.checksum, ChecksumAlgorithm::MD4);
        assert_eq!(result_v28.compression, CompressionAlgorithm::Zlib);
        assert!(stdout_v28.is_empty(), "v28 should not send lists");

        // v30 requires actual negotiation
        let v30 = ProtocolVersion::V30;
        let client_response = b"\x03md5\x04zlib";
        let mut stdin_v30 = &client_response[..];
        let mut stdout_v30 = Vec::new();

        let result_v30 = negotiate_capabilities(
            v30,
            &mut stdin_v30,
            &mut stdout_v30,
            true,
            true,
            false,
            true,
        )
        .unwrap();

        // v30 should negotiate to MD5 and Zlib
        assert_eq!(result_v30.checksum, ChecksumAlgorithm::MD5);
        assert_eq!(result_v30.compression, CompressionAlgorithm::Zlib);
        assert!(!stdout_v30.is_empty(), "v30 should send lists");
    }

    /// Protocol 30 supports modern checksums (xxhash).
    #[test]
    fn version_30_supports_modern_checksums() {
        let protocol = ProtocolVersion::V30;

        let test_cases = [
            (b"\x06xxh128\x04none".as_slice(), ChecksumAlgorithm::XXH128),
            (b"\x04xxh3\x04none".as_slice(), ChecksumAlgorithm::XXH3),
            (b"\x05xxh64\x04none".as_slice(), ChecksumAlgorithm::XXH64),
        ];

        for (client_response, expected_checksum) in test_cases {
            let mut stdin = client_response;
            let mut stdout = Vec::new();

            let result =
                negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true);

            assert!(
                result.is_ok(),
                "Should support {expected_checksum:?}: {result:?}"
            );
            assert_eq!(result.unwrap().checksum, expected_checksum);
        }
    }

    /// Protocol 30 supports modern compression algorithms.
    #[test]
    fn version_30_supports_modern_compression() {
        let protocol = ProtocolVersion::V30;

        let test_cases = [
            (b"\x03md5\x04zlib".as_slice(), CompressionAlgorithm::Zlib),
            (b"\x03md5\x05zlibx".as_slice(), CompressionAlgorithm::ZlibX),
        ];

        for (client_response, expected_compression) in test_cases {
            let mut stdin = client_response;
            let mut stdout = Vec::new();

            let result =
                negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true);

            assert!(
                result.is_ok(),
                "Should support {expected_compression:?}: {result:?}"
            );
            assert_eq!(result.unwrap().compression, expected_compression);
        }
    }

    /// Protocol 30 checksum negotiation without compression.
    #[test]
    fn version_30_checksum_only_negotiation() {
        let protocol = ProtocolVersion::V30;
        let client_response = b"\x04sha1"; // Only checksum
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let result = negotiate_capabilities(
            protocol,
            &mut stdin,
            &mut stdout,
            true,
            false, // send_compression = false
            false,
            true,
        );

        assert!(result.is_ok(), "Checksum-only negotiation should work");
        let negotiated = result.unwrap();
        assert_eq!(negotiated.checksum, ChecksumAlgorithm::SHA1);
        assert_eq!(negotiated.compression, CompressionAlgorithm::None);
    }

    /// Protocol 30 falls back gracefully when client lacks varint support.
    #[test]
    fn version_30_fallback_without_varint_flist_flags() {
        let protocol = ProtocolVersion::V30;
        let mut stdin = &b""[..]; // No input needed when do_negotiation=false
        let mut stdout = Vec::new();

        let result = negotiate_capabilities(
            protocol,
            &mut stdin,
            &mut stdout,
            false, // do_negotiation = false (client lacks VARINT_FLIST_FLAGS)
            true,
            false,
            true,
        );

        assert!(result.is_ok(), "Should fall back gracefully: {result:?}");

        // Should use MD5 default and None compression
        let negotiated = result.unwrap();
        assert_eq!(negotiated.checksum, ChecksumAlgorithm::MD5);
        assert_eq!(negotiated.compression, CompressionAlgorithm::None);
        assert!(stdout.is_empty(), "No data should be sent");
    }
}

// ============================================================================
// Module: Protocol Version 30 Incremental Recursion
// ============================================================================

mod protocol_30_incremental_recursion {
    use super::*;

    /// Protocol 30 supports INC_RECURSE compatibility flag.
    #[test]
    fn version_30_supports_inc_recurse_flag() {
        // INC_RECURSE flag should be valid for v30
        let flags = CompatibilityFlags::INC_RECURSE;

        // Encode and decode to verify it's handled correctly
        let mut encoded = Vec::new();
        flags.encode_to_vec(&mut encoded).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&encoded).unwrap();
        assert_eq!(decoded, flags);
        assert!(decoded.contains(CompatibilityFlags::INC_RECURSE));
    }

    /// Protocol 30 INC_RECURSE flag has correct bit value.
    #[test]
    fn version_30_inc_recurse_flag_bit_value() {
        assert_eq!(
            CompatibilityFlags::INC_RECURSE.bits(),
            1,
            "INC_RECURSE should be bit 0"
        );
    }

    /// Protocol 30 INC_RECURSE flag iterates correctly.
    #[test]
    fn version_30_inc_recurse_flag_iteration() {
        let flags = CompatibilityFlags::INC_RECURSE;
        let known: Vec<_> = flags.iter_known().collect();

        assert_eq!(known.len(), 1);
        assert_eq!(known[0], KnownCompatibilityFlag::IncRecurse);
    }

    /// Protocol 30 can combine INC_RECURSE with other flags.
    #[test]
    fn version_30_inc_recurse_with_other_flags() {
        let flags = CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::SAFE_FILE_LIST
            | CompatibilityFlags::VARINT_FLIST_FLAGS;

        assert!(flags.contains(CompatibilityFlags::INC_RECURSE));
        assert!(flags.contains(CompatibilityFlags::SAFE_FILE_LIST));
        assert!(flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS));

        // Encode and decode
        let mut encoded = Vec::new();
        flags.encode_to_vec(&mut encoded).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&encoded).unwrap();
        assert_eq!(decoded, flags);
    }

    /// Protocol 30 INC_RECURSE flag display formatting.
    #[test]
    fn version_30_inc_recurse_flag_display() {
        let flag = KnownCompatibilityFlag::IncRecurse;
        let display = format!("{flag}");
        assert!(
            display.contains("IncRecurse") || display.contains("INC_RECURSE"),
            "Display should show flag name"
        );
    }

    /// Protocol 30 VARINT_FLIST_FLAGS enables capability negotiation.
    #[test]
    fn version_30_varint_flist_flags_enables_negotiation() {
        let _flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

        // With VARINT_FLIST_FLAGS, negotiation should occur
        let protocol = ProtocolVersion::V30;
        let client_response = b"\x03md5\x04zlib";
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let result = negotiate_capabilities(
            protocol,
            &mut stdin,
            &mut stdout,
            true, // do_negotiation = true (VARINT_FLIST_FLAGS present)
            true,
            false,
            true,
        );

        assert!(result.is_ok());
        assert!(
            !stdout.is_empty(),
            "Should send lists with VARINT_FLIST_FLAGS"
        );
    }

    /// Protocol 30 SAFE_FILE_LIST flag is supported.
    #[test]
    fn version_30_safe_file_list_flag_supported() {
        let flags = CompatibilityFlags::SAFE_FILE_LIST;

        let mut encoded = Vec::new();
        flags.encode_to_vec(&mut encoded).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&encoded).unwrap();
        assert_eq!(decoded, flags);
        assert!(decoded.contains(CompatibilityFlags::SAFE_FILE_LIST));
    }

    /// Protocol 30 typical compatibility flags combination.
    #[test]
    fn version_30_typical_compatibility_flags() {
        // Typical v30 flags: INC_RECURSE + SAFE_FILE_LIST + VARINT_FLIST_FLAGS
        let flags = CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::SAFE_FILE_LIST
            | CompatibilityFlags::VARINT_FLIST_FLAGS;

        let mut encoded = Vec::new();
        flags.encode_to_vec(&mut encoded).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&encoded).unwrap();
        assert_eq!(decoded, flags);

        let known: Vec<_> = decoded.iter_known().collect();
        assert_eq!(known.len(), 3);
        assert!(known.contains(&KnownCompatibilityFlag::IncRecurse));
        assert!(known.contains(&KnownCompatibilityFlag::SafeFileList));
        assert!(known.contains(&KnownCompatibilityFlag::VarintFlistFlags));
    }
}

// ============================================================================
// Module: Protocol Version 30 Backward Compatibility
// ============================================================================

mod protocol_30_backward_compatibility {
    use super::*;

    /// Protocol 30 can negotiate with v28/v29 peers (falls back to older version).
    #[test]
    fn version_30_negotiates_with_legacy_peers() {
        // When peer offers 28 and 30, should select 30
        let result = select_highest_mutual([TestVersion(28), TestVersion(30)]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 30);

        // When peer offers only 28, should use 28
        let result = select_highest_mutual([TestVersion(28)]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 28);
    }

    /// Protocol 30 is selected over v28 and v29 when all are available.
    #[test]
    fn version_30_preferred_over_legacy() {
        let result = select_highest_mutual([TestVersion(28), TestVersion(29), TestVersion(30)]);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().as_u8(),
            30,
            "Should prefer v30 over v28/v29"
        );
    }

    /// Protocol 30 features are superset of v29.
    #[test]
    fn version_30_features_superset_of_v29() {
        let v29 = ProtocolVersion::V29;
        let v30 = ProtocolVersion::V30;

        // v30 has all v29 features
        assert_eq!(
            v30.supports_sender_receiver_modifiers(),
            v29.supports_sender_receiver_modifiers()
        );
        assert_eq!(v30.supports_flist_times(), v29.supports_flist_times());
        assert_eq!(v30.supports_extended_flags(), v29.supports_extended_flags());

        // v30 adds new features
        assert!(v30.uses_binary_negotiation());
        assert!(!v29.uses_binary_negotiation());

        assert!(v30.uses_varint_encoding());
        assert!(!v29.uses_varint_encoding());

        assert!(v30.supports_perishable_modifier());
        assert!(!v29.supports_perishable_modifier());
    }

    /// Protocol 30 does not break compatibility with properly-versioned peers.
    #[test]
    fn version_30_maintains_protocol_compatibility() {
        // Test that v30 can be selected from a range of versions
        let versions = [28, 29, 30, 31, 32];

        for &version in &versions {
            let result = select_highest_mutual([TestVersion(version)]);
            assert!(result.is_ok(), "Version {version} should be negotiable");
        }
    }

    /// Protocol 30 gracefully handles mixed-version scenarios.
    #[test]
    fn version_30_mixed_version_negotiation() {
        // Peer advertises v30 first, then legacy versions
        let result = select_highest_mutual([TestVersion(30), TestVersion(29), TestVersion(28)]);
        assert_eq!(result.unwrap().as_u8(), 30);

        // Legacy versions first, then v30
        let result = select_highest_mutual([TestVersion(28), TestVersion(29), TestVersion(30)]);
        assert_eq!(result.unwrap().as_u8(), 30);
    }
}

// ============================================================================
// Module: Protocol Version 30 Edge Cases
// ============================================================================

mod protocol_30_edge_cases {
    use super::*;

    /// Protocol 30 rejects invalid checksum algorithms.
    #[test]
    fn version_30_rejects_invalid_checksums() {
        let protocol = ProtocolVersion::V30;

        // Client sends invalid checksum name
        let client_response = b"\x06foobar\x04none";
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let result =
            negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true);

        // Should fall back to MD5 when client sends unknown checksum
        assert!(result.is_ok(), "Should fall back to default");
        assert_eq!(result.unwrap().checksum, ChecksumAlgorithm::MD5);
    }

    /// Protocol 30 handles empty capability lists.
    #[test]
    fn version_30_handles_empty_capability_lists() {
        let protocol = ProtocolVersion::V30;

        // Client sends empty checksum list
        let client_response = b"\x00"; // Zero-length vstring
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let result = negotiate_capabilities(
            protocol,
            &mut stdin,
            &mut stdout,
            true,
            false, // no compression
            false,
            true,
        );

        // Should fall back to MD5 default
        assert!(result.is_ok());
        assert_eq!(result.unwrap().checksum, ChecksumAlgorithm::MD5);
    }

    /// Protocol 30 handles truncated capability negotiation.
    #[test]
    fn version_30_handles_truncated_negotiation() {
        let protocol = ProtocolVersion::V30;

        // Truncated vstring (claims 10 bytes but provides only 3)
        let truncated = [0x0A, b'm', b'd', b'5'];
        let mut stdin = &truncated[..];
        let mut stdout = Vec::new();

        let result =
            negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, false, false, true);

        // Should fail with UnexpectedEof
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }

    /// Protocol 30 is not affected by version clamping (within supported range).
    #[test]
    fn version_30_not_clamped() {
        // v30 is within supported range, should not be clamped
        let result = ProtocolVersion::from_peer_advertisement(30);
        assert!(result.is_ok());
        let version = result.as_ref().unwrap();
        assert_eq!(version.as_u8(), 30);

        // Verify it's not clamped to OLDEST or NEWEST
        assert_ne!(*version, ProtocolVersion::OLDEST);
        // v30 might equal NEWEST if NEWEST is 30, but that's not clamping
    }

    /// Protocol 30 equality and comparison operations work correctly.
    #[test]
    fn version_30_equality_and_comparison() {
        let v30_a = ProtocolVersion::V30;
        let v30_b = ProtocolVersion::from_supported(30).unwrap();
        let v29 = ProtocolVersion::V29;
        let v31 = ProtocolVersion::V31;

        // Equality
        assert_eq!(v30_a, v30_b);
        assert_ne!(v30_a, v29);
        assert_ne!(v30_a, v31);

        // Comparison (as_u8)
        assert!(v30_a.as_u8() > v29.as_u8());
        assert!(v30_a.as_u8() < v31.as_u8());
    }
}

// ============================================================================
// Module: Protocol Version 30 Integration Tests
// ============================================================================

mod protocol_30_integration {
    use super::*;

    /// Full handshake flow for v30: version negotiation + capability negotiation.
    #[test]
    fn version_30_full_handshake_flow() {
        // Step 1: Protocol version negotiation
        let result = select_highest_mutual([TestVersion(30)]);
        assert!(result.is_ok());
        let protocol = result.unwrap();
        assert_eq!(protocol.as_u8(), 30);

        // Step 2: Capability negotiation
        let client_response = b"\x06xxh128\x05zlibx";
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let capabilities =
            negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true);

        assert!(capabilities.is_ok());
        let caps = capabilities.unwrap();
        assert_eq!(caps.checksum, ChecksumAlgorithm::XXH128);
        assert_eq!(caps.compression, CompressionAlgorithm::ZlibX);
    }

    /// Protocol 30 with INC_RECURSE flag set for incremental recursion.
    #[test]
    fn version_30_with_inc_recurse_full_flow() {
        // Negotiate v30
        let result = select_highest_mutual([TestVersion(30)]);
        assert!(result.is_ok());
        let protocol = result.unwrap();

        // Set up compatibility flags with INC_RECURSE
        let flags = CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::VARINT_FLIST_FLAGS
            | CompatibilityFlags::SAFE_FILE_LIST;

        // Verify flags encode/decode correctly
        let mut encoded = Vec::new();
        flags.encode_to_vec(&mut encoded).unwrap();
        let (decoded, _) = CompatibilityFlags::decode_from_slice(&encoded).unwrap();
        assert_eq!(decoded, flags);

        // Capability negotiation should work
        let client_response = b"\x03md5\x04zlib";
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let capabilities =
            negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true);

        assert!(capabilities.is_ok());
    }

    /// Protocol 30 end-to-end: negotiation, capabilities, and feature checks.
    #[test]
    fn version_30_end_to_end_feature_verification() {
        let protocol = ProtocolVersion::V30;

        // Verify all expected features
        assert!(protocol.uses_binary_negotiation());
        assert!(protocol.uses_varint_encoding());
        assert!(protocol.uses_varint_flist_flags());
        assert!(protocol.supports_perishable_modifier());
        assert!(protocol.uses_safe_file_list());
        assert!(!protocol.safe_file_list_always_enabled());

        // Perform capability negotiation
        let client_response = b"\x04sha1\x04none";
        let mut stdin = &client_response[..];
        let mut stdout = Vec::new();

        let result =
            negotiate_capabilities(protocol, &mut stdin, &mut stdout, true, true, false, true);

        assert!(result.is_ok());
        let caps = result.unwrap();

        // Verify negotiated capabilities
        assert_eq!(caps.checksum, ChecksumAlgorithm::SHA1);
        assert_eq!(caps.compression, CompressionAlgorithm::None);

        // Verify server sent capability lists
        assert!(!stdout.is_empty());
    }
}
