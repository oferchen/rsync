//! Comprehensive Protocol Version 31 Compatibility Tests
//!
//! This test suite provides extensive coverage for protocol version 31 features,
//! wire format differences, compatibility flags, and handshake scenarios.
//!
//! # Protocol Version 31 Overview
//!
//! Protocol 31 was introduced in rsync 3.1.x and includes:
//! - **Safe file list always enabled**: Unlike v30 where it can be negotiated
//! - **Nanosecond mtime support**: XMIT_MOD_NSEC flag for sub-second precision
//! - **Checksum seed fix**: CF_CHKSUM_SEED_FIX compatibility flag
//! - **IO error end list**: XMIT_IO_ERROR_ENDLIST for file list termination
//! - **ID0 names support**: CF_ID0_NAMES for user/group name handling
//! - **Varint flist flags**: CF_VARINT_FLIST_FLAGS for efficient encoding
//! - **Avoid xattr optimization**: CF_AVOID_XATTR_OPTIM flag
//!
//! # Test Categories
//!
//! 1. Wire Format Tests
//! 2. Compatibility Flags Tests
//! 3. Handshake Scenario Tests
//! 4. Incremental Recursion Tests
//! 5. Feature Boundary Tests
//! 6. Interoperability Tests

use protocol::codec::{NdxCodec, ProtocolCodec, create_ndx_codec, create_protocol_codec};
use protocol::{
    CompatibilityFlags, KnownCompatibilityFlag, NegotiationError, ProtocolVersion,
    select_highest_mutual,
};
use std::io::Cursor;

// ============================================================================
// 1. Wire Format Tests - Protocol 31 Specific Encoding
// ============================================================================

mod wire_format {
    use super::*;

    #[test]
    fn v31_uses_varint_encoding() {
        let protocol = ProtocolVersion::V31;
        assert!(protocol.uses_varint_encoding());
        assert!(!protocol.uses_fixed_encoding());
    }

    #[test]
    fn v31_uses_varint_flist_flags() {
        let protocol = ProtocolVersion::V31;
        assert!(protocol.uses_varint_flist_flags());
    }

    #[test]
    fn v31_binary_advertisement_format() {
        // Protocol 31 advertises as 4-byte big-endian u32
        let protocol = ProtocolVersion::V31;
        let bytes = u32::from(protocol.as_u8()).to_be_bytes();

        assert_eq!(bytes.len(), 4);
        assert_eq!(bytes, [0, 0, 0, 31]);
    }

    #[test]
    fn v31_advertisement_roundtrip() {
        let protocol = ProtocolVersion::V31;
        let bytes = u32::from(protocol.as_u8()).to_be_bytes();
        let parsed_value = u32::from_be_bytes(bytes);
        let parsed = ProtocolVersion::from_peer_advertisement(parsed_value).unwrap();
        assert_eq!(parsed, protocol);
    }

    #[test]
    fn v31_ndx_codec_uses_delta_encoding() {
        let mut codec = create_ndx_codec(31);
        let mut buf = Vec::new();

        // First positive: prev=-1, ndx=0, diff=1
        codec.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf, vec![0x01], "v31 should use delta-encoded NDX");
    }

    #[test]
    fn v31_ndx_done_is_single_byte() {
        let mut codec = create_ndx_codec(31);
        let mut buf = Vec::new();

        codec.write_ndx_done(&mut buf).unwrap();
        assert_eq!(buf, vec![0x00], "v31 NDX_DONE should be single byte 0x00");
    }

    #[test]
    fn v31_ndx_roundtrip_sequence() {
        let mut write_codec = create_ndx_codec(31);
        let mut buf = Vec::new();

        // Write a sequence of indices
        for ndx in [0, 1, 5, 100, 500, 10000] {
            write_codec.write_ndx(&mut buf, ndx).unwrap();
        }

        let mut read_codec = create_ndx_codec(31);
        let mut cursor = Cursor::new(&buf);

        for expected in [0, 1, 5, 100, 500, 10000] {
            assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), expected);
        }
    }

    #[test]
    fn v31_file_size_uses_varlong() {
        let codec = create_protocol_codec(31);
        let mut buf = Vec::new();

        // Small value should be compact
        codec.write_file_size(&mut buf, 100).unwrap();
        assert!(buf.len() <= 4, "varlong should be compact for small values");

        // Roundtrip test
        let mut cursor = Cursor::new(&buf);
        let read = codec.read_file_size(&mut cursor).unwrap();
        assert_eq!(read, 100);
    }

    #[test]
    fn v31_large_file_size_encoding() {
        let codec = create_protocol_codec(31);
        let mut buf = Vec::new();

        // Large value (> 4GB)
        let large_value = 0x1_0000_0000i64;
        codec.write_file_size(&mut buf, large_value).unwrap();

        // Roundtrip test
        let mut cursor = Cursor::new(&buf);
        let read = codec.read_file_size(&mut cursor).unwrap();
        assert_eq!(read, large_value);
    }

    #[test]
    fn v31_codec_is_modern() {
        let codec = create_protocol_codec(31);
        assert!(!codec.is_legacy());
        assert_eq!(codec.protocol_version(), 31);
    }

    #[test]
    fn v31_vs_v30_wire_format_consistency() {
        // Both v30 and v31 use the same modern wire format
        let codec30 = create_protocol_codec(30);
        let codec31 = create_protocol_codec(31);

        let mut buf30 = Vec::new();
        let mut buf31 = Vec::new();

        codec30.write_file_size(&mut buf30, 12345).unwrap();
        codec31.write_file_size(&mut buf31, 12345).unwrap();

        assert_eq!(
            buf30, buf31,
            "v30 and v31 should use same file size encoding"
        );
    }
}

// ============================================================================
// 2. Compatibility Flags Tests - v31 Specific Flags
// ============================================================================

mod compatibility_flags {
    use super::*;

    #[test]
    fn v31_checksum_seed_fix_flag() {
        // CF_CHKSUM_SEED_FIX (bit 5) - Added in protocol 31
        let flag = CompatibilityFlags::CHECKSUM_SEED_FIX;
        assert_eq!(flag.bits(), 1 << 5);

        let mut buf = Vec::new();
        flag.encode_to_vec(&mut buf).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, flag);
    }

    #[test]
    fn v31_avoid_xattr_optim_flag() {
        // CF_AVOID_XATTR_OPTIM (bit 4) - Added in protocol 31
        let flag = CompatibilityFlags::AVOID_XATTR_OPTIMIZATION;
        assert_eq!(flag.bits(), 1 << 4);

        let mut buf = Vec::new();
        flag.encode_to_vec(&mut buf).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, flag);
    }

    #[test]
    fn v31_varint_flist_flags_flag() {
        // CF_VARINT_FLIST_FLAGS (bit 7) - Added in protocol 31
        let flag = CompatibilityFlags::VARINT_FLIST_FLAGS;
        assert_eq!(flag.bits(), 1 << 7);

        let mut buf = Vec::new();
        flag.encode_to_vec(&mut buf).unwrap();

        // Bit 7 (128) needs 2-byte varint encoding
        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, flag);
    }

    #[test]
    fn v31_id0_names_flag() {
        // CF_ID0_NAMES (bit 8) - Added in protocol 31
        let flag = CompatibilityFlags::ID0_NAMES;
        assert_eq!(flag.bits(), 1 << 8);

        let mut buf = Vec::new();
        flag.encode_to_vec(&mut buf).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, flag);
    }

    #[test]
    fn v31_all_new_flags_combined() {
        // All flags introduced or relevant to protocol 31
        let v31_flags = CompatibilityFlags::CHECKSUM_SEED_FIX
            | CompatibilityFlags::AVOID_XATTR_OPTIMIZATION
            | CompatibilityFlags::VARINT_FLIST_FLAGS
            | CompatibilityFlags::ID0_NAMES;

        let mut buf = Vec::new();
        v31_flags.encode_to_vec(&mut buf).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, v31_flags);

        // Verify individual flags are present
        assert!(decoded.contains(CompatibilityFlags::CHECKSUM_SEED_FIX));
        assert!(decoded.contains(CompatibilityFlags::AVOID_XATTR_OPTIMIZATION));
        assert!(decoded.contains(CompatibilityFlags::VARINT_FLIST_FLAGS));
        assert!(decoded.contains(CompatibilityFlags::ID0_NAMES));
    }

    #[test]
    fn v31_typical_flags_combination() {
        // Typical v31 handshake flags
        let typical = CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::SAFE_FILE_LIST
            | CompatibilityFlags::CHECKSUM_SEED_FIX
            | CompatibilityFlags::VARINT_FLIST_FLAGS;

        let mut buf = Vec::new();
        typical.encode_to_vec(&mut buf).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, typical);
    }

    #[test]
    fn v31_flags_from_known_enum() {
        // All v31-era flags via KnownCompatibilityFlag enum
        let flags: CompatibilityFlags = [
            KnownCompatibilityFlag::ChecksumSeedFix,
            KnownCompatibilityFlag::AvoidXattrOptimization,
            KnownCompatibilityFlag::VarintFlistFlags,
            KnownCompatibilityFlag::Id0Names,
        ]
        .into_iter()
        .collect();

        assert!(flags.contains(CompatibilityFlags::CHECKSUM_SEED_FIX));
        assert!(flags.contains(CompatibilityFlags::AVOID_XATTR_OPTIMIZATION));
        assert!(flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS));
        assert!(flags.contains(CompatibilityFlags::ID0_NAMES));
    }

    #[test]
    fn v31_flags_bit_positions_unique() {
        // Verify v31-specific flags don't overlap
        let v31_flags = [
            CompatibilityFlags::CHECKSUM_SEED_FIX,
            CompatibilityFlags::AVOID_XATTR_OPTIMIZATION,
            CompatibilityFlags::VARINT_FLIST_FLAGS,
            CompatibilityFlags::ID0_NAMES,
        ];

        for (i, &flag1) in v31_flags.iter().enumerate() {
            for (j, &flag2) in v31_flags.iter().enumerate() {
                if i != j {
                    assert_ne!(
                        flag1.bits(),
                        flag2.bits(),
                        "v31 flags at indices {i} and {j} must have unique bit positions"
                    );
                }
            }
        }
    }

    #[test]
    fn v31_all_known_flags_encode_decode() {
        let all_flags = CompatibilityFlags::ALL_KNOWN;

        let mut buf = Vec::new();
        all_flags.encode_to_vec(&mut buf).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, all_flags);
    }
}

// ============================================================================
// 3. Handshake Scenario Tests - v31 Peer Interactions
// ============================================================================

mod handshake_scenarios {
    use super::*;

    #[test]
    fn handshake_v31_client_to_v31_server() {
        // Both peers support v31
        let server_offers = [31_u8];
        let result = select_highest_mutual(server_offers).unwrap();
        assert_eq!(result, ProtocolVersion::V31);
    }

    #[test]
    fn handshake_v31_client_to_v32_server() {
        // Server offers v32, client supports v31
        let server_offers = [31_u8, 32];
        let result = select_highest_mutual(server_offers).unwrap();
        assert_eq!(result, ProtocolVersion::V32, "should upgrade to v32");
    }

    #[test]
    fn handshake_v31_client_to_v30_server() {
        // Server only supports v30
        let server_offers = [30_u8];
        let result = select_highest_mutual(server_offers).unwrap();
        assert_eq!(result, ProtocolVersion::V30, "should downgrade to v30");
    }

    #[test]
    fn handshake_v31_client_to_legacy_v29_server() {
        // Server only supports legacy v29
        let server_offers = [29_u8];
        let result = select_highest_mutual(server_offers).unwrap();
        assert_eq!(result, ProtocolVersion::V29, "should downgrade to v29");
        assert!(result.uses_legacy_ascii_negotiation());
    }

    #[test]
    fn handshake_v31_client_to_oldest_v28_server() {
        // Server only supports oldest supported v28
        let server_offers = [28_u8];
        let result = select_highest_mutual(server_offers).unwrap();
        assert_eq!(result, ProtocolVersion::V28, "should downgrade to v28");
    }

    #[test]
    fn handshake_v31_with_future_versions() {
        // Server offers v31 and future versions (33-40)
        let server_offers = [31_u8, 35, 38];
        let result = select_highest_mutual(server_offers).unwrap();
        assert_eq!(result, ProtocolVersion::V32, "future versions clamp to v32");
    }

    #[test]
    fn handshake_v31_rejects_too_old() {
        // Server only offers v27 (too old)
        let server_offers = [27_u8];
        let result = select_highest_mutual(server_offers);
        assert!(result.is_err());
        match result.unwrap_err() {
            NegotiationError::UnsupportedVersion(v) => assert_eq!(v, 27),
            _ => panic!("expected UnsupportedVersion"),
        }
    }

    #[test]
    fn handshake_v31_from_mixed_versions() {
        // Mix of supported versions including v31
        let server_offers = [28_u8, 29, 30, 31];
        let result = select_highest_mutual(server_offers).unwrap();
        assert_eq!(result, ProtocolVersion::V31);
    }

    #[test]
    fn handshake_v31_from_advertisement() {
        let result = ProtocolVersion::from_peer_advertisement(31).unwrap();
        assert_eq!(result, ProtocolVersion::V31);
    }

    #[test]
    fn handshake_v31_with_duplicates() {
        let server_offers = [31_u8, 31, 31, 30, 30];
        let result = select_highest_mutual(server_offers).unwrap();
        assert_eq!(result, ProtocolVersion::V31);
    }

    #[test]
    fn handshake_v31_order_independence() {
        // Order of offered versions shouldn't matter
        let ascending = [28_u8, 29, 30, 31];
        let descending = [31_u8, 30, 29, 28];
        let scrambled = [30_u8, 28, 31, 29];

        let result1 = select_highest_mutual(ascending).unwrap();
        let result2 = select_highest_mutual(descending).unwrap();
        let result3 = select_highest_mutual(scrambled).unwrap();

        assert_eq!(result1, ProtocolVersion::V31);
        assert_eq!(result2, ProtocolVersion::V31);
        assert_eq!(result3, ProtocolVersion::V31);
    }

    #[test]
    fn handshake_rsync_31x_typical_advertisement() {
        // rsync 3.1.x typically advertises protocol 31
        let result = select_highest_mutual([31_u32]).unwrap();
        assert_eq!(result, ProtocolVersion::V31);
        assert!(result.uses_binary_negotiation());
        assert!(result.safe_file_list_always_enabled());
    }
}

// ============================================================================
// 4. Incremental Recursion Tests - CF_INC_RECURSE with v31
// ============================================================================

mod incremental_recursion {
    use super::*;

    #[test]
    fn v31_supports_incremental_recursion_flag() {
        let flag = CompatibilityFlags::INC_RECURSE;
        assert_eq!(flag.bits(), 1 << 0);

        let mut buf = Vec::new();
        flag.encode_to_vec(&mut buf).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, flag);
    }

    #[test]
    fn v31_incremental_recursion_with_safe_flist() {
        // Protocol 31 uses incremental recursion with safe file list always enabled
        let flags = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SAFE_FILE_LIST;

        let mut buf = Vec::new();
        flags.encode_to_vec(&mut buf).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert!(decoded.contains(CompatibilityFlags::INC_RECURSE));
        assert!(decoded.contains(CompatibilityFlags::SAFE_FILE_LIST));
    }

    #[test]
    fn v31_typical_incremental_recursion_setup() {
        // Typical flags for incremental recursion in protocol 31
        let flags = CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::SAFE_FILE_LIST
            | CompatibilityFlags::VARINT_FLIST_FLAGS
            | CompatibilityFlags::CHECKSUM_SEED_FIX;

        // Verify all expected flags
        assert!(flags.contains(CompatibilityFlags::INC_RECURSE));
        assert!(flags.contains(CompatibilityFlags::SAFE_FILE_LIST));
        assert!(flags.contains(CompatibilityFlags::VARINT_FLIST_FLAGS));
        assert!(flags.contains(CompatibilityFlags::CHECKSUM_SEED_FIX));
    }

    #[test]
    fn v31_safe_file_list_always_enabled() {
        // Key v31 feature: safe file list is mandatory
        let protocol = ProtocolVersion::V31;
        assert!(protocol.safe_file_list_always_enabled());
        assert!(protocol.uses_safe_file_list());
    }

    #[test]
    fn v31_vs_v30_safe_file_list_difference() {
        // v30: safe file list is negotiable
        // v31: safe file list is always enabled
        assert!(!ProtocolVersion::V30.safe_file_list_always_enabled());
        assert!(ProtocolVersion::V31.safe_file_list_always_enabled());

        // Both support safe file list
        assert!(ProtocolVersion::V30.uses_safe_file_list());
        assert!(ProtocolVersion::V31.uses_safe_file_list());
    }

    #[test]
    fn v31_inc_recurse_known_flag_mapping() {
        let known = KnownCompatibilityFlag::IncRecurse;
        let flag = known.as_flag();
        assert_eq!(flag, CompatibilityFlags::INC_RECURSE);
    }
}

// ============================================================================
// 5. Feature Boundary Tests - v31 at Protocol Version Boundaries
// ============================================================================

mod feature_boundaries {
    use super::*;

    #[test]
    fn v31_feature_profile() {
        let v = ProtocolVersion::V31;

        // Binary negotiation (v30+)
        assert!(v.uses_binary_negotiation());
        assert!(!v.uses_legacy_ascii_negotiation());

        // Varint encoding (v30+)
        assert!(v.uses_varint_encoding());
        assert!(!v.uses_fixed_encoding());

        // Varint flist flags (v30+)
        assert!(v.uses_varint_flist_flags());

        // Safe file list (v30+, always enabled in v31+)
        assert!(v.uses_safe_file_list());
        assert!(v.safe_file_list_always_enabled());

        // Perishable modifier (v30+)
        assert!(v.supports_perishable_modifier());

        // Sender/receiver modifiers (v29+)
        assert!(v.supports_sender_receiver_modifiers());

        // Flist times (v29+)
        assert!(v.supports_flist_times());

        // New prefixes (v29+)
        assert!(!v.uses_old_prefixes());

        // Extended flags (v28+)
        assert!(v.supports_extended_flags());
    }

    #[test]
    fn v31_boundary_with_v30() {
        let v30 = ProtocolVersion::V30;
        let v31 = ProtocolVersion::V31;

        // Same features
        assert_eq!(v30.uses_binary_negotiation(), v31.uses_binary_negotiation());
        assert_eq!(v30.uses_varint_encoding(), v31.uses_varint_encoding());
        assert_eq!(v30.uses_varint_flist_flags(), v31.uses_varint_flist_flags());
        assert_eq!(v30.uses_safe_file_list(), v31.uses_safe_file_list());
        assert_eq!(
            v30.supports_perishable_modifier(),
            v31.supports_perishable_modifier()
        );

        // Key difference: safe file list always enabled
        assert!(!v30.safe_file_list_always_enabled());
        assert!(v31.safe_file_list_always_enabled());
    }

    #[test]
    fn v31_boundary_with_v32() {
        let v31 = ProtocolVersion::V31;
        let v32 = ProtocolVersion::V32;

        // v31 and v32 have the same feature set in this implementation
        assert_eq!(v31.uses_binary_negotiation(), v32.uses_binary_negotiation());
        assert_eq!(v31.uses_varint_encoding(), v32.uses_varint_encoding());
        assert_eq!(v31.uses_varint_flist_flags(), v32.uses_varint_flist_flags());
        assert_eq!(v31.uses_safe_file_list(), v32.uses_safe_file_list());
        assert_eq!(
            v31.safe_file_list_always_enabled(),
            v32.safe_file_list_always_enabled()
        );
    }

    #[test]
    fn v31_is_second_newest() {
        assert_eq!(ProtocolVersion::V31.offset_from_newest(), 1);
        assert_eq!(
            ProtocolVersion::V31.next_newer(),
            Some(ProtocolVersion::V32)
        );
    }

    #[test]
    fn v31_navigation() {
        let v31 = ProtocolVersion::V31;
        assert_eq!(v31.next_newer(), Some(ProtocolVersion::V32));
        assert_eq!(v31.next_older(), Some(ProtocolVersion::V30));
    }

    #[test]
    fn v31_offset_calculations() {
        let v31 = ProtocolVersion::V31;
        assert_eq!(v31.offset_from_oldest(), 3); // v28=0, v29=1, v30=2, v31=3
        assert_eq!(v31.offset_from_newest(), 1); // v32=0, v31=1
    }

    #[test]
    fn v31_from_supported_index() {
        // Supported versions in newest-to-oldest order: 32, 31, 30, 29, 28
        let version = ProtocolVersion::from_supported_index(1);
        assert_eq!(version, Some(ProtocolVersion::V31));
    }
}

// ============================================================================
// 6. Interoperability Tests - v31 with Various Peers
// ============================================================================

mod interoperability {
    use super::*;

    #[test]
    fn interop_rsync_31x_release() {
        // rsync 3.1.x uses protocol 31
        let result = select_highest_mutual([31_u8]).unwrap();
        assert_eq!(result, ProtocolVersion::V31);

        // Verify expected features
        assert!(result.uses_binary_negotiation());
        assert!(result.uses_varint_encoding());
        assert!(result.safe_file_list_always_enabled());
    }

    #[test]
    fn interop_rsync_30x_release() {
        // rsync 3.0.x uses protocol 30
        let result = select_highest_mutual([30_u8]).unwrap();
        assert_eq!(result, ProtocolVersion::V30);
        assert!(!result.safe_file_list_always_enabled());
    }

    #[test]
    fn interop_rsync_34x_release() {
        // rsync 3.4.x uses protocol 32, should work with v31 peers
        let result = select_highest_mutual([31_u8, 32]).unwrap();
        assert_eq!(result, ProtocolVersion::V32);
    }

    #[test]
    fn interop_mixed_infrastructure() {
        // Various servers with different protocol support
        let old_server = [28_u8, 29];
        let modern_server = [28_u8, 29, 30, 31];
        let latest_server = [28_u8, 29, 30, 31, 32];

        assert_eq!(
            select_highest_mutual(old_server).unwrap(),
            ProtocolVersion::V29
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

    #[test]
    fn interop_codec_compatibility_v31() {
        // Test that v31 codec works correctly for file sizes
        let codec = create_protocol_codec(31);
        let test_sizes: Vec<i64> = vec![0, 1, 100, 1000, 65535, 0x7FFF_FFFF, 0x1_0000_0000];

        for &size in &test_sizes {
            let mut buf = Vec::new();
            codec.write_file_size(&mut buf, size).unwrap();

            let mut cursor = Cursor::new(&buf);
            let read = codec.read_file_size(&mut cursor).unwrap();
            assert_eq!(read, size, "v31 codec failed for size {size}");
        }
    }

    #[test]
    fn interop_ndx_compatibility_v31() {
        // Test that v31 NDX codec works correctly
        let mut codec = create_ndx_codec(31);
        let test_indices: Vec<i32> = vec![0, 1, 5, 100, 253, 254, 500, 10000];

        let mut buf = Vec::new();
        for &ndx in &test_indices {
            codec.write_ndx(&mut buf, ndx).unwrap();
        }

        let mut read_codec = create_ndx_codec(31);
        let mut cursor = Cursor::new(&buf);

        for &expected in &test_indices {
            let read = read_codec.read_ndx(&mut cursor).unwrap();
            assert_eq!(read, expected, "v31 NDX codec failed for index {expected}");
        }
    }
}

// ============================================================================
// 7. Error Handling Tests
// ============================================================================

mod error_handling {
    use super::*;

    #[test]
    fn v31_negotiation_empty_list() {
        let result = select_highest_mutual(Vec::<u8>::new());
        assert!(result.is_err());
        match result.unwrap_err() {
            NegotiationError::NoMutualProtocol { peer_versions } => {
                assert!(peer_versions.is_empty());
            }
            _ => panic!("expected NoMutualProtocol"),
        }
    }

    #[test]
    fn v31_negotiation_only_unsupported() {
        let result = select_highest_mutual([27_u8, 26, 25]);
        assert!(result.is_err());
        match result.unwrap_err() {
            NegotiationError::UnsupportedVersion(v) => {
                // Should report oldest rejected version
                assert_eq!(v, 25);
            }
            _ => panic!("expected UnsupportedVersion"),
        }
    }

    #[test]
    fn v31_negotiation_beyond_maximum() {
        // Versions beyond MAXIMUM_PROTOCOL_ADVERTISEMENT (40) fail
        let result = select_highest_mutual([41_u32]);
        assert!(result.is_err());
        match result.unwrap_err() {
            NegotiationError::UnsupportedVersion(v) => {
                assert_eq!(v, 41);
            }
            _ => panic!("expected UnsupportedVersion"),
        }
    }

    #[test]
    fn v31_negotiation_zero_version() {
        let result = select_highest_mutual([0_u32]);
        assert!(result.is_err());
        match result.unwrap_err() {
            NegotiationError::UnsupportedVersion(v) => {
                assert_eq!(v, 0);
            }
            _ => panic!("expected UnsupportedVersion"),
        }
    }
}

// ============================================================================
// 8. Comparison and Ordering Tests
// ============================================================================

mod comparison_ordering {
    use super::*;

    #[test]
    fn v31_ordering() {
        assert!(ProtocolVersion::V31 > ProtocolVersion::V30);
        assert!(ProtocolVersion::V31 > ProtocolVersion::V29);
        assert!(ProtocolVersion::V31 > ProtocolVersion::V28);
        assert!(ProtocolVersion::V31 < ProtocolVersion::V32);
    }

    #[test]
    fn v31_equality() {
        assert_eq!(ProtocolVersion::V31, ProtocolVersion::V31);
        assert_ne!(ProtocolVersion::V31, ProtocolVersion::V30);
        assert_ne!(ProtocolVersion::V31, ProtocolVersion::V32);
    }

    #[test]
    fn v31_integer_comparison() {
        assert_eq!(ProtocolVersion::V31, 31u8);
        assert_eq!(31u8, ProtocolVersion::V31);
        assert_ne!(ProtocolVersion::V31, 30u8);
        assert_ne!(ProtocolVersion::V31, 32u8);
    }

    #[test]
    fn v31_as_u8() {
        assert_eq!(ProtocolVersion::V31.as_u8(), 31);
    }

    #[test]
    fn v31_from_u8_conversion() {
        let version = ProtocolVersion::try_from(31u8).unwrap();
        assert_eq!(version, ProtocolVersion::V31);
    }

    #[test]
    fn v31_to_u8_conversion() {
        let byte: u8 = ProtocolVersion::V31.into();
        assert_eq!(byte, 31);
    }

    #[test]
    fn v31_to_wider_conversions() {
        let u16_val: u16 = ProtocolVersion::V31.into();
        assert_eq!(u16_val, 31);

        let u32_val: u32 = ProtocolVersion::V31.into();
        assert_eq!(u32_val, 31);

        let u64_val: u64 = ProtocolVersion::V31.into();
        assert_eq!(u64_val, 31);
    }

    #[test]
    fn v31_parse_from_string() {
        let version: ProtocolVersion = "31".parse().unwrap();
        assert_eq!(version, ProtocolVersion::V31);
    }

    #[test]
    fn v31_display_format() {
        assert_eq!(format!("{}", ProtocolVersion::V31), "31");
    }

    #[test]
    fn v31_debug_format() {
        let debug = format!("{:?}", ProtocolVersion::V31);
        assert!(debug.contains("31"));
    }
}

// ============================================================================
// 9. Supported Versions Tests
// ============================================================================

mod supported_versions {
    use super::*;

    #[test]
    fn v31_in_supported_versions() {
        let versions = ProtocolVersion::supported_versions();
        assert!(versions.contains(&ProtocolVersion::V31));
    }

    #[test]
    fn v31_in_supported_protocol_numbers() {
        let numbers = ProtocolVersion::supported_protocol_numbers();
        assert!(numbers.contains(&31));
    }

    #[test]
    fn v31_in_protocol_bitmap() {
        let bitmap = ProtocolVersion::supported_protocol_bitmap();
        assert_ne!(bitmap & (1u64 << 31), 0);
    }

    #[test]
    fn v31_is_supported() {
        assert!(ProtocolVersion::is_supported(31));
    }

    #[test]
    fn v31_from_supported() {
        let version = ProtocolVersion::from_supported(31);
        assert_eq!(version, Some(ProtocolVersion::V31));
    }

    #[test]
    fn v31_within_range_bounds() {
        let (oldest, newest) = ProtocolVersion::supported_range_bounds();
        assert!(31 >= oldest);
        assert!(31 <= newest);
    }
}

// ============================================================================
// 10. Consistency and Invariant Tests
// ============================================================================

mod consistency {
    use super::*;

    #[test]
    fn v31_constant_value() {
        assert_eq!(ProtocolVersion::V31.as_u8(), 31);
    }

    #[test]
    fn v31_feature_consistency() {
        let v31 = ProtocolVersion::V31;

        // Encoding methods should be mutually exclusive
        assert!(v31.uses_varint_encoding() != v31.uses_fixed_encoding());

        // Negotiation methods should be mutually exclusive
        assert!(v31.uses_binary_negotiation() != v31.uses_legacy_ascii_negotiation());

        // Binary negotiation implies varint encoding
        if v31.uses_binary_negotiation() {
            assert!(v31.uses_varint_encoding());
        }
    }

    #[test]
    fn v31_safe_file_list_implies_support() {
        let v31 = ProtocolVersion::V31;

        // If safe file list is always enabled, it must be supported
        if v31.safe_file_list_always_enabled() {
            assert!(v31.uses_safe_file_list());
        }
    }

    #[test]
    fn v31_monotonic_feature_progression() {
        // Features enabled in v31 should also be enabled in v32
        let v31 = ProtocolVersion::V31;
        let v32 = ProtocolVersion::V32;

        if v31.uses_binary_negotiation() {
            assert!(v32.uses_binary_negotiation());
        }
        if v31.uses_varint_encoding() {
            assert!(v32.uses_varint_encoding());
        }
        if v31.uses_safe_file_list() {
            assert!(v32.uses_safe_file_list());
        }
        if v31.safe_file_list_always_enabled() {
            assert!(v32.safe_file_list_always_enabled());
        }
    }

    #[test]
    fn v31_flag_encoding_deterministic() {
        let flags = CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::SAFE_FILE_LIST
            | CompatibilityFlags::CHECKSUM_SEED_FIX;

        let mut buf1 = Vec::new();
        let mut buf2 = Vec::new();

        flags.encode_to_vec(&mut buf1).unwrap();
        flags.encode_to_vec(&mut buf2).unwrap();

        assert_eq!(buf1, buf2, "flag encoding should be deterministic");
    }

    #[test]
    fn v31_ndx_encoding_deterministic() {
        let mut codec1 = create_ndx_codec(31);
        let mut codec2 = create_ndx_codec(31);

        let mut buf1 = Vec::new();
        let mut buf2 = Vec::new();

        for ndx in [0, 1, 5, 100] {
            codec1.write_ndx(&mut buf1, ndx).unwrap();
            codec2.write_ndx(&mut buf2, ndx).unwrap();
        }

        assert_eq!(buf1, buf2, "NDX encoding should be deterministic");
    }
}
