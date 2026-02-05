//! Protocol version 28-31 compatibility tests.
//!
//! Comprehensive tests for protocol version compatibility across versions 28-31
//! as specified in task #29. These tests validate:
//! 1. Version 28 feature set
//! 2. Version 29 additions
//! 3. Version 30 additions
//! 4. Version 31 additions
//! 5. Version negotiation between different versions
//! 6. Capability flags for each version
//!
//! # Protocol Version Overview
//!
//! | Version | Key Features |
//! |---------|-------------|
//! | 28 | Extended file flags, oldest supported, legacy ASCII negotiation |
//! | 29 | Incremental recursion foundation, sender/receiver modifiers, flist times |
//! | 30 | Binary negotiation, varint encoding, compatibility flags, perishable modifier |
//! | 31 | Safe file list always enabled, checksum seed fix, varint flist flags |
//!
//! # Upstream Reference
//!
//! Protocol details are based on rsync 3.4.1 source code.

use protocol::codec::{NdxCodec, ProtocolCodec, create_ndx_codec, create_protocol_codec};
use protocol::{
    CompatibilityFlags, KnownCompatibilityFlag, NegotiationError, ProtocolVersion,
    ProtocolVersionAdvertisement, format_legacy_daemon_greeting, select_highest_mutual,
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
// 1. Version 28 Feature Set Tests
// ============================================================================

mod version_28_feature_set {
    use super::*;

    #[test]
    fn v28_is_oldest_supported_version() {
        assert_eq!(
            ProtocolVersion::OLDEST,
            ProtocolVersion::V28,
            "Protocol 28 must be the oldest supported version"
        );
        assert_eq!(ProtocolVersion::V28.as_u8(), 28);
    }

    #[test]
    fn v28_uses_legacy_ascii_negotiation() {
        assert!(
            ProtocolVersion::V28.uses_legacy_ascii_negotiation(),
            "Protocol 28 must use ASCII negotiation"
        );
        assert!(
            !ProtocolVersion::V28.uses_binary_negotiation(),
            "Protocol 28 must NOT use binary negotiation"
        );
    }

    #[test]
    fn v28_uses_fixed_encoding() {
        assert!(
            ProtocolVersion::V28.uses_fixed_encoding(),
            "Protocol 28 must use fixed encoding"
        );
        assert!(
            !ProtocolVersion::V28.uses_varint_encoding(),
            "Protocol 28 must NOT use varint encoding"
        );
    }

    #[test]
    fn v28_supports_extended_flags() {
        assert!(
            ProtocolVersion::V28.supports_extended_flags(),
            "Protocol 28 supports extended file flags"
        );
    }

    #[test]
    fn v28_does_not_support_sender_receiver_modifiers() {
        assert!(
            !ProtocolVersion::V28.supports_sender_receiver_modifiers(),
            "Protocol 28 does NOT support sender/receiver modifiers"
        );
    }

    #[test]
    fn v28_does_not_support_perishable_modifier() {
        assert!(
            !ProtocolVersion::V28.supports_perishable_modifier(),
            "Protocol 28 does NOT support perishable modifier"
        );
    }

    #[test]
    fn v28_does_not_support_flist_times() {
        assert!(
            !ProtocolVersion::V28.supports_flist_times(),
            "Protocol 28 does NOT support flist timing stats"
        );
    }

    #[test]
    fn v28_uses_old_prefixes() {
        assert!(
            ProtocolVersion::V28.uses_old_prefixes(),
            "Protocol 28 uses old-style filter prefixes"
        );
    }

    #[test]
    fn v28_does_not_use_safe_file_list() {
        assert!(
            !ProtocolVersion::V28.uses_safe_file_list(),
            "Protocol 28 does NOT use safe file list"
        );
        assert!(
            !ProtocolVersion::V28.safe_file_list_always_enabled(),
            "Protocol 28 does NOT have safe file list always enabled"
        );
    }

    #[test]
    fn v28_does_not_use_varint_flist_flags() {
        assert!(
            !ProtocolVersion::V28.uses_varint_flist_flags(),
            "Protocol 28 does NOT use varint flist flags"
        );
    }

    #[test]
    fn v28_legacy_greeting_format() {
        let greeting = format_legacy_daemon_greeting(ProtocolVersion::V28);
        assert_eq!(greeting, "@RSYNCD: 28.0\n");
        assert!(greeting.is_ascii());
        assert_eq!(greeting.len(), 14);
    }

    #[test]
    fn v28_codec_is_legacy() {
        let codec = create_protocol_codec(28);
        assert!(codec.is_legacy(), "Protocol 28 codec must be legacy");
        assert_eq!(codec.protocol_version(), 28);
    }

    #[test]
    fn v28_file_size_encoding_fixed_4_bytes() {
        let codec = create_protocol_codec(28);
        let mut buf = Vec::new();
        codec.write_file_size(&mut buf, 1000).unwrap();
        assert_eq!(buf.len(), 4, "Protocol 28 uses 4-byte fixed size");
        assert_eq!(buf, vec![0xe8, 0x03, 0x00, 0x00]); // 1000 in LE
    }

    #[test]
    fn v28_ndx_uses_legacy_4_byte_encoding() {
        let mut codec = create_ndx_codec(28);
        let mut buf = Vec::new();
        codec.write_ndx(&mut buf, 5).unwrap();
        assert_eq!(buf.len(), 4, "Protocol 28 NDX uses 4-byte LE");
        assert_eq!(buf, vec![5, 0, 0, 0]);
    }
}

// ============================================================================
// 2. Version 29 Additions Tests
// ============================================================================

mod version_29_additions {
    use super::*;

    #[test]
    fn v29_still_uses_legacy_ascii_negotiation() {
        assert!(
            ProtocolVersion::V29.uses_legacy_ascii_negotiation(),
            "Protocol 29 must use ASCII negotiation"
        );
        assert!(
            !ProtocolVersion::V29.uses_binary_negotiation(),
            "Protocol 29 must NOT use binary negotiation"
        );
    }

    #[test]
    fn v29_still_uses_fixed_encoding() {
        assert!(
            ProtocolVersion::V29.uses_fixed_encoding(),
            "Protocol 29 must use fixed encoding"
        );
        assert!(
            !ProtocolVersion::V29.uses_varint_encoding(),
            "Protocol 29 must NOT use varint encoding"
        );
    }

    #[test]
    fn v29_adds_sender_receiver_modifiers() {
        assert!(
            ProtocolVersion::V29.supports_sender_receiver_modifiers(),
            "Protocol 29 ADDS sender/receiver modifiers (s, r)"
        );
        // Compare with v28 which doesn't have this
        assert!(
            !ProtocolVersion::V28.supports_sender_receiver_modifiers(),
            "Protocol 28 should NOT have sender/receiver modifiers"
        );
    }

    #[test]
    fn v29_adds_flist_times() {
        assert!(
            ProtocolVersion::V29.supports_flist_times(),
            "Protocol 29 ADDS flist timing stats"
        );
        // Compare with v28 which doesn't have this
        assert!(
            !ProtocolVersion::V28.supports_flist_times(),
            "Protocol 28 should NOT have flist times"
        );
    }

    #[test]
    fn v29_removes_old_prefixes() {
        assert!(
            !ProtocolVersion::V29.uses_old_prefixes(),
            "Protocol 29 does NOT use old-style filter prefixes"
        );
        // Compare with v28 which uses old prefixes
        assert!(
            ProtocolVersion::V28.uses_old_prefixes(),
            "Protocol 28 should use old prefixes"
        );
    }

    #[test]
    fn v29_still_does_not_support_perishable_modifier() {
        assert!(
            !ProtocolVersion::V29.supports_perishable_modifier(),
            "Protocol 29 does NOT support perishable modifier (added in 30)"
        );
    }

    #[test]
    fn v29_still_does_not_use_safe_file_list() {
        assert!(
            !ProtocolVersion::V29.uses_safe_file_list(),
            "Protocol 29 does NOT use safe file list"
        );
    }

    #[test]
    fn v29_is_newest_legacy_protocol() {
        // Protocol 29 is the last version using ASCII negotiation
        assert!(ProtocolVersion::V29.uses_legacy_ascii_negotiation());
        let next = ProtocolVersion::V29.next_newer().unwrap();
        assert!(
            next.uses_binary_negotiation(),
            "Protocol 30 (next) uses binary negotiation"
        );
    }

    #[test]
    fn v29_legacy_greeting_format() {
        let greeting = format_legacy_daemon_greeting(ProtocolVersion::V29);
        assert_eq!(greeting, "@RSYNCD: 29.0\n");
        assert!(greeting.is_ascii());
    }

    #[test]
    fn v29_codec_is_still_legacy() {
        let codec = create_protocol_codec(29);
        assert!(codec.is_legacy(), "Protocol 29 codec must be legacy");
        assert_eq!(codec.protocol_version(), 29);
    }

    #[test]
    fn v29_large_file_uses_longint_marker() {
        let codec = create_protocol_codec(29);
        let mut buf = Vec::new();
        let large_value = 0x1_0000_0000i64; // > 32-bit
        codec.write_file_size(&mut buf, large_value).unwrap();
        // Legacy uses 4-byte marker + 8-byte value for large values
        assert_eq!(buf.len(), 12, "Large file sizes use 12-byte longint");
        assert_eq!(&buf[0..4], &[0xff, 0xff, 0xff, 0xff]); // Marker
    }
}

// ============================================================================
// 3. Version 30 Additions Tests
// ============================================================================

mod version_30_additions {
    use super::*;

    #[test]
    fn v30_introduces_binary_negotiation() {
        assert!(
            ProtocolVersion::V30.uses_binary_negotiation(),
            "Protocol 30 INTRODUCES binary negotiation"
        );
        assert!(
            !ProtocolVersion::V30.uses_legacy_ascii_negotiation(),
            "Protocol 30 must NOT use ASCII negotiation"
        );
        assert_eq!(
            ProtocolVersion::BINARY_NEGOTIATION_INTRODUCED,
            ProtocolVersion::V30,
            "Binary negotiation must be introduced at protocol 30"
        );
    }

    #[test]
    fn v30_introduces_varint_encoding() {
        assert!(
            ProtocolVersion::V30.uses_varint_encoding(),
            "Protocol 30 INTRODUCES varint encoding"
        );
        assert!(
            !ProtocolVersion::V30.uses_fixed_encoding(),
            "Protocol 30 must NOT use fixed encoding"
        );
    }

    #[test]
    fn v30_adds_perishable_modifier() {
        assert!(
            ProtocolVersion::V30.supports_perishable_modifier(),
            "Protocol 30 ADDS perishable modifier (p)"
        );
        // Compare with v29 which doesn't have this
        assert!(
            !ProtocolVersion::V29.supports_perishable_modifier(),
            "Protocol 29 should NOT have perishable modifier"
        );
    }

    #[test]
    fn v30_adds_safe_file_list_negotiable() {
        assert!(
            ProtocolVersion::V30.uses_safe_file_list(),
            "Protocol 30 ADDS safe file list (negotiable)"
        );
        assert!(
            !ProtocolVersion::V30.safe_file_list_always_enabled(),
            "Protocol 30 safe file list is negotiable, NOT always enabled"
        );
    }

    #[test]
    fn v30_adds_varint_flist_flags() {
        assert!(
            ProtocolVersion::V30.uses_varint_flist_flags(),
            "Protocol 30 ADDS varint flist flags"
        );
        // Compare with v29
        assert!(
            !ProtocolVersion::V29.uses_varint_flist_flags(),
            "Protocol 29 should NOT have varint flist flags"
        );
    }

    #[test]
    fn v30_supports_compatibility_flags_encoding() {
        // Protocol 30 can encode and decode compatibility flags
        let flags = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SAFE_FILE_LIST;
        let mut buf = Vec::new();
        flags.encode_to_vec(&mut buf).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, flags);
    }

    #[test]
    fn v30_codec_is_modern() {
        let codec = create_protocol_codec(30);
        assert!(!codec.is_legacy(), "Protocol 30 codec must be modern");
        assert_eq!(codec.protocol_version(), 30);
    }

    #[test]
    fn v30_file_size_uses_varlong() {
        let codec = create_protocol_codec(30);
        let mut buf = Vec::new();
        codec.write_file_size(&mut buf, 100).unwrap();
        // Modern varlong is more compact for small values
        assert!(buf.len() <= 4, "varlong should be compact for small values");

        // Roundtrip test
        let mut cursor = Cursor::new(&buf);
        let read = codec.read_file_size(&mut cursor).unwrap();
        assert_eq!(read, 100);
    }

    #[test]
    fn v30_ndx_uses_delta_encoding() {
        let mut codec = create_ndx_codec(30);
        let mut buf = Vec::new();

        // First index 0 with prev=-1 gives delta=1
        codec.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf.len(), 1, "Modern NDX uses delta encoding");
        assert_eq!(buf, vec![0x01]);
    }

    #[test]
    fn v30_binary_advertisement_format() {
        let protocol = ProtocolVersion::V30;
        let bytes = u32::from(protocol.as_u8()).to_be_bytes();
        assert_eq!(bytes.len(), 4);
        assert_eq!(
            bytes,
            [0, 0, 0, 30],
            "Protocol 30 advertises as 4-byte big-endian"
        );
    }

    #[test]
    fn v30_compatibility_flag_inc_recurse() {
        let flag = CompatibilityFlags::INC_RECURSE;
        assert_eq!(flag.bits(), 1 << 0, "CF_INC_RECURSE is bit 0");

        let mut buf = Vec::new();
        flag.encode_to_vec(&mut buf).unwrap();
        assert_eq!(buf, vec![1]);
    }

    #[test]
    fn v30_compatibility_flag_symlink_times() {
        let flag = CompatibilityFlags::SYMLINK_TIMES;
        assert_eq!(flag.bits(), 1 << 1, "CF_SYMLINK_TIMES is bit 1");
    }

    #[test]
    fn v30_compatibility_flag_symlink_iconv() {
        let flag = CompatibilityFlags::SYMLINK_ICONV;
        assert_eq!(flag.bits(), 1 << 2, "CF_SYMLINK_ICONV is bit 2");
    }

    #[test]
    fn v30_compatibility_flag_safe_flist() {
        let flag = CompatibilityFlags::SAFE_FILE_LIST;
        assert_eq!(flag.bits(), 1 << 3, "CF_SAFE_FLIST is bit 3");
    }

    #[test]
    fn v30_compatibility_flag_inplace_partial_dir() {
        let flag = CompatibilityFlags::INPLACE_PARTIAL_DIR;
        assert_eq!(flag.bits(), 1 << 6, "CF_INPLACE_PARTIAL_DIR is bit 6");
    }
}

// ============================================================================
// 4. Version 31 Additions Tests
// ============================================================================

mod version_31_additions {
    use super::*;

    #[test]
    fn v31_continues_binary_negotiation() {
        assert!(
            ProtocolVersion::V31.uses_binary_negotiation(),
            "Protocol 31 uses binary negotiation"
        );
    }

    #[test]
    fn v31_continues_varint_encoding() {
        assert!(
            ProtocolVersion::V31.uses_varint_encoding(),
            "Protocol 31 uses varint encoding"
        );
    }

    #[test]
    fn v31_safe_file_list_always_enabled() {
        assert!(
            ProtocolVersion::V31.uses_safe_file_list(),
            "Protocol 31 uses safe file list"
        );
        assert!(
            ProtocolVersion::V31.safe_file_list_always_enabled(),
            "Protocol 31 ALWAYS enables safe file list"
        );
        // Compare with v30 where it's negotiable
        assert!(
            !ProtocolVersion::V30.safe_file_list_always_enabled(),
            "Protocol 30 should NOT have safe file list always enabled"
        );
    }

    #[test]
    fn v31_adds_checksum_seed_fix_flag() {
        let flag = CompatibilityFlags::CHECKSUM_SEED_FIX;
        assert_eq!(flag.bits(), 1 << 5, "CF_CHKSUM_SEED_FIX is bit 5");

        let mut buf = Vec::new();
        flag.encode_to_vec(&mut buf).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, flag);
    }

    #[test]
    fn v31_adds_avoid_xattr_optim_flag() {
        let flag = CompatibilityFlags::AVOID_XATTR_OPTIMIZATION;
        assert_eq!(flag.bits(), 1 << 4, "CF_AVOID_XATTR_OPTIM is bit 4");

        let mut buf = Vec::new();
        flag.encode_to_vec(&mut buf).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, flag);
    }

    #[test]
    fn v31_adds_varint_flist_flags_flag() {
        let flag = CompatibilityFlags::VARINT_FLIST_FLAGS;
        assert_eq!(flag.bits(), 1 << 7, "CF_VARINT_FLIST_FLAGS is bit 7");

        let mut buf = Vec::new();
        flag.encode_to_vec(&mut buf).unwrap();

        // Bit 7 (128) needs 2-byte varint encoding
        assert_eq!(buf, vec![128, 128]);

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, flag);
    }

    #[test]
    fn v31_adds_id0_names_flag() {
        let flag = CompatibilityFlags::ID0_NAMES;
        assert_eq!(flag.bits(), 1 << 8, "CF_ID0_NAMES is bit 8");

        let mut buf = Vec::new();
        flag.encode_to_vec(&mut buf).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, flag);
    }

    #[test]
    fn v31_all_compatibility_flags_supported() {
        let all_flags = CompatibilityFlags::ALL_KNOWN;

        let mut buf = Vec::new();
        all_flags.encode_to_vec(&mut buf).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, all_flags);

        // Verify specific flags
        assert!(decoded.contains(CompatibilityFlags::INC_RECURSE));
        assert!(decoded.contains(CompatibilityFlags::SAFE_FILE_LIST));
        assert!(decoded.contains(CompatibilityFlags::CHECKSUM_SEED_FIX));
        assert!(decoded.contains(CompatibilityFlags::VARINT_FLIST_FLAGS));
        assert!(decoded.contains(CompatibilityFlags::ID0_NAMES));
    }

    #[test]
    fn v31_codec_is_modern() {
        let codec = create_protocol_codec(31);
        assert!(!codec.is_legacy(), "Protocol 31 codec must be modern");
        assert_eq!(codec.protocol_version(), 31);
    }

    #[test]
    fn v31_binary_advertisement_format() {
        let protocol = ProtocolVersion::V31;
        let bytes = u32::from(protocol.as_u8()).to_be_bytes();
        assert_eq!(
            bytes,
            [0, 0, 0, 31],
            "Protocol 31 advertises as 4-byte big-endian"
        );
    }
}

// ============================================================================
// 5. Version Negotiation Between Different Versions Tests
// ============================================================================

mod version_negotiation {
    use super::*;

    #[test]
    fn negotiate_v28_only() {
        let result = select_highest_mutual([TestVersion(28)]);
        assert!(result.is_ok(), "Negotiation with v28 only must succeed");
        assert_eq!(result.unwrap().as_u8(), 28);
    }

    #[test]
    fn negotiate_v29_only() {
        let result = select_highest_mutual([TestVersion(29)]);
        assert!(result.is_ok(), "Negotiation with v29 only must succeed");
        assert_eq!(result.unwrap().as_u8(), 29);
    }

    #[test]
    fn negotiate_v30_only() {
        let result = select_highest_mutual([TestVersion(30)]);
        assert!(result.is_ok(), "Negotiation with v30 only must succeed");
        assert_eq!(result.unwrap().as_u8(), 30);
    }

    #[test]
    fn negotiate_v31_only() {
        let result = select_highest_mutual([TestVersion(31)]);
        assert!(result.is_ok(), "Negotiation with v31 only must succeed");
        assert_eq!(result.unwrap().as_u8(), 31);
    }

    #[test]
    fn negotiate_v28_and_v29_selects_v29() {
        let result = select_highest_mutual([TestVersion(28), TestVersion(29)]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 29, "Should select highest (29)");
    }

    #[test]
    fn negotiate_v29_and_v30_selects_v30() {
        let result = select_highest_mutual([TestVersion(29), TestVersion(30)]);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().as_u8(),
            30,
            "Should select highest (30) - crosses negotiation boundary"
        );
    }

    #[test]
    fn negotiate_v30_and_v31_selects_v31() {
        let result = select_highest_mutual([TestVersion(30), TestVersion(31)]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 31, "Should select highest (31)");
    }

    #[test]
    fn negotiate_v28_and_v31_selects_v31() {
        let result = select_highest_mutual([TestVersion(28), TestVersion(31)]);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().as_u8(),
            31,
            "Should select highest (31) even with v28 present"
        );
    }

    #[test]
    fn negotiate_all_v28_to_v31_selects_v31() {
        let result = select_highest_mutual([
            TestVersion(28),
            TestVersion(29),
            TestVersion(30),
            TestVersion(31),
        ]);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().as_u8(),
            31,
            "Should select highest from all versions"
        );
    }

    #[test]
    fn negotiate_reverse_order_still_selects_highest() {
        let result = select_highest_mutual([
            TestVersion(31),
            TestVersion(30),
            TestVersion(29),
            TestVersion(28),
        ]);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().as_u8(),
            31,
            "Order of advertisement should not matter"
        );
    }

    #[test]
    fn negotiate_v27_rejected() {
        let result = select_highest_mutual([TestVersion(27)]);
        assert!(result.is_err(), "Protocol 27 must be rejected");

        if let Err(NegotiationError::UnsupportedVersion(ver)) = result {
            assert_eq!(ver, 27);
        } else {
            panic!("Expected UnsupportedVersion(27)");
        }
    }

    #[test]
    fn negotiate_v27_and_v28_uses_v28() {
        // When 27 and 28 are both advertised, should use 28
        let result = select_highest_mutual([TestVersion(28), TestVersion(27)]);
        assert!(result.is_ok(), "Should negotiate to 28");
        assert_eq!(result.unwrap().as_u8(), 28);
    }

    #[test]
    fn negotiate_empty_list_fails() {
        let result = select_highest_mutual::<Vec<TestVersion>, _>(vec![]);
        assert!(result.is_err(), "Empty version list must fail");
    }

    #[test]
    fn negotiate_with_duplicates_works() {
        let result = select_highest_mutual([
            TestVersion(30),
            TestVersion(30),
            TestVersion(30),
            TestVersion(29),
        ]);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().as_u8(),
            30,
            "Duplicates should not affect selection"
        );
    }

    #[test]
    fn negotiate_pairwise_matrix_v28_to_v31() {
        let versions = [28u8, 29, 30, 31];

        for &v1 in &versions {
            for &v2 in &versions {
                let result =
                    select_highest_mutual([TestVersion(u32::from(v1)), TestVersion(u32::from(v2))]);
                assert!(
                    result.is_ok(),
                    "Negotiation between {v1} and {v2} must succeed"
                );

                let expected = std::cmp::max(v1, v2);
                assert_eq!(
                    result.unwrap().as_u8(),
                    expected,
                    "Should select highest between {v1} and {v2}"
                );
            }
        }
    }

    #[test]
    fn negotiate_selects_correct_negotiation_type() {
        // When negotiating to v28 or v29, should use ASCII negotiation
        for version in [28u32, 29] {
            let result = select_highest_mutual([TestVersion(version)]);
            let protocol = result.unwrap();
            assert!(
                protocol.uses_legacy_ascii_negotiation(),
                "Protocol {version} should use ASCII negotiation"
            );
        }

        // When negotiating to v30 or v31, should use binary negotiation
        for version in [30u32, 31] {
            let result = select_highest_mutual([TestVersion(version)]);
            let protocol = result.unwrap();
            assert!(
                protocol.uses_binary_negotiation(),
                "Protocol {version} should use binary negotiation"
            );
        }
    }
}

// ============================================================================
// 6. Capability Flags for Each Version Tests
// ============================================================================

mod capability_flags_per_version {
    use super::*;

    /// Feature matrix for versions 28-31.
    struct VersionCapabilities {
        version: u8,
        uses_ascii_negotiation: bool,
        uses_binary_negotiation: bool,
        uses_fixed_encoding: bool,
        uses_varint_encoding: bool,
        supports_sender_receiver_modifiers: bool,
        supports_perishable_modifier: bool,
        supports_flist_times: bool,
        uses_old_prefixes: bool,
        uses_safe_file_list: bool,
        safe_file_list_always_enabled: bool,
        uses_varint_flist_flags: bool,
        supports_extended_flags: bool,
    }

    const VERSION_CAPABILITIES: [VersionCapabilities; 4] = [
        VersionCapabilities {
            version: 28,
            uses_ascii_negotiation: true,
            uses_binary_negotiation: false,
            uses_fixed_encoding: true,
            uses_varint_encoding: false,
            supports_sender_receiver_modifiers: false,
            supports_perishable_modifier: false,
            supports_flist_times: false,
            uses_old_prefixes: true,
            uses_safe_file_list: false,
            safe_file_list_always_enabled: false,
            uses_varint_flist_flags: false,
            supports_extended_flags: true,
        },
        VersionCapabilities {
            version: 29,
            uses_ascii_negotiation: true,
            uses_binary_negotiation: false,
            uses_fixed_encoding: true,
            uses_varint_encoding: false,
            supports_sender_receiver_modifiers: true, // Added in v29
            supports_perishable_modifier: false,
            supports_flist_times: true, // Added in v29
            uses_old_prefixes: false,   // Removed in v29
            uses_safe_file_list: false,
            safe_file_list_always_enabled: false,
            uses_varint_flist_flags: false,
            supports_extended_flags: true,
        },
        VersionCapabilities {
            version: 30,
            uses_ascii_negotiation: false,
            uses_binary_negotiation: true, // Added in v30
            uses_fixed_encoding: false,
            uses_varint_encoding: true, // Added in v30
            supports_sender_receiver_modifiers: true,
            supports_perishable_modifier: true, // Added in v30
            supports_flist_times: true,
            uses_old_prefixes: false,
            uses_safe_file_list: true, // Added in v30
            safe_file_list_always_enabled: false,
            uses_varint_flist_flags: true, // Added in v30
            supports_extended_flags: true,
        },
        VersionCapabilities {
            version: 31,
            uses_ascii_negotiation: false,
            uses_binary_negotiation: true,
            uses_fixed_encoding: false,
            uses_varint_encoding: true,
            supports_sender_receiver_modifiers: true,
            supports_perishable_modifier: true,
            supports_flist_times: true,
            uses_old_prefixes: false,
            uses_safe_file_list: true,
            safe_file_list_always_enabled: true, // Changed in v31
            uses_varint_flist_flags: true,
            supports_extended_flags: true,
        },
    ];

    #[test]
    fn test_version_capability_matrix() {
        for caps in VERSION_CAPABILITIES.iter() {
            let protocol = ProtocolVersion::from_supported(caps.version)
                .unwrap_or_else(|| panic!("Protocol {} should be supported", caps.version));

            assert_eq!(
                protocol.uses_legacy_ascii_negotiation(),
                caps.uses_ascii_negotiation,
                "v{} uses_ascii_negotiation mismatch",
                caps.version
            );
            assert_eq!(
                protocol.uses_binary_negotiation(),
                caps.uses_binary_negotiation,
                "v{} uses_binary_negotiation mismatch",
                caps.version
            );
            assert_eq!(
                protocol.uses_fixed_encoding(),
                caps.uses_fixed_encoding,
                "v{} uses_fixed_encoding mismatch",
                caps.version
            );
            assert_eq!(
                protocol.uses_varint_encoding(),
                caps.uses_varint_encoding,
                "v{} uses_varint_encoding mismatch",
                caps.version
            );
            assert_eq!(
                protocol.supports_sender_receiver_modifiers(),
                caps.supports_sender_receiver_modifiers,
                "v{} supports_sender_receiver_modifiers mismatch",
                caps.version
            );
            assert_eq!(
                protocol.supports_perishable_modifier(),
                caps.supports_perishable_modifier,
                "v{} supports_perishable_modifier mismatch",
                caps.version
            );
            assert_eq!(
                protocol.supports_flist_times(),
                caps.supports_flist_times,
                "v{} supports_flist_times mismatch",
                caps.version
            );
            assert_eq!(
                protocol.uses_old_prefixes(),
                caps.uses_old_prefixes,
                "v{} uses_old_prefixes mismatch",
                caps.version
            );
            assert_eq!(
                protocol.uses_safe_file_list(),
                caps.uses_safe_file_list,
                "v{} uses_safe_file_list mismatch",
                caps.version
            );
            assert_eq!(
                protocol.safe_file_list_always_enabled(),
                caps.safe_file_list_always_enabled,
                "v{} safe_file_list_always_enabled mismatch",
                caps.version
            );
            assert_eq!(
                protocol.uses_varint_flist_flags(),
                caps.uses_varint_flist_flags,
                "v{} uses_varint_flist_flags mismatch",
                caps.version
            );
            assert_eq!(
                protocol.supports_extended_flags(),
                caps.supports_extended_flags,
                "v{} supports_extended_flags mismatch",
                caps.version
            );
        }
    }

    #[test]
    fn test_compatibility_flags_available_from_v30() {
        // Compatibility flags are available starting from protocol 30
        // Versions 28-29 don't use compatibility flags in the binary protocol

        // All known flags should encode/decode correctly
        let all_flags = [
            (CompatibilityFlags::INC_RECURSE, "INC_RECURSE"),
            (CompatibilityFlags::SYMLINK_TIMES, "SYMLINK_TIMES"),
            (CompatibilityFlags::SYMLINK_ICONV, "SYMLINK_ICONV"),
            (CompatibilityFlags::SAFE_FILE_LIST, "SAFE_FILE_LIST"),
            (
                CompatibilityFlags::AVOID_XATTR_OPTIMIZATION,
                "AVOID_XATTR_OPTIM",
            ),
            (CompatibilityFlags::CHECKSUM_SEED_FIX, "CHKSUM_SEED_FIX"),
            (
                CompatibilityFlags::INPLACE_PARTIAL_DIR,
                "INPLACE_PARTIAL_DIR",
            ),
            (CompatibilityFlags::VARINT_FLIST_FLAGS, "VARINT_FLIST_FLAGS"),
            (CompatibilityFlags::ID0_NAMES, "ID0_NAMES"),
        ];

        for (flag, name) in all_flags {
            let mut buf = Vec::new();
            flag.encode_to_vec(&mut buf)
                .unwrap_or_else(|_| panic!("Encoding {name} should succeed"));

            let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf)
                .unwrap_or_else(|_| panic!("Decoding {name} should succeed"));

            assert_eq!(decoded, flag, "Round-trip for {name} should preserve flag");
        }
    }

    #[test]
    fn test_known_compatibility_flag_enum() {
        // All known flags should have a corresponding enum variant
        let all_variants = KnownCompatibilityFlag::ALL;
        assert_eq!(all_variants.len(), 9, "Should have 9 known flags");

        // Each variant should map to the correct flag bits
        let expected_mappings = [
            (
                KnownCompatibilityFlag::IncRecurse,
                CompatibilityFlags::INC_RECURSE,
            ),
            (
                KnownCompatibilityFlag::SymlinkTimes,
                CompatibilityFlags::SYMLINK_TIMES,
            ),
            (
                KnownCompatibilityFlag::SymlinkIconv,
                CompatibilityFlags::SYMLINK_ICONV,
            ),
            (
                KnownCompatibilityFlag::SafeFileList,
                CompatibilityFlags::SAFE_FILE_LIST,
            ),
            (
                KnownCompatibilityFlag::AvoidXattrOptimization,
                CompatibilityFlags::AVOID_XATTR_OPTIMIZATION,
            ),
            (
                KnownCompatibilityFlag::ChecksumSeedFix,
                CompatibilityFlags::CHECKSUM_SEED_FIX,
            ),
            (
                KnownCompatibilityFlag::InplacePartialDir,
                CompatibilityFlags::INPLACE_PARTIAL_DIR,
            ),
            (
                KnownCompatibilityFlag::VarintFlistFlags,
                CompatibilityFlags::VARINT_FLIST_FLAGS,
            ),
            (
                KnownCompatibilityFlag::Id0Names,
                CompatibilityFlags::ID0_NAMES,
            ),
        ];

        for (variant, expected_flag) in expected_mappings {
            assert_eq!(
                variant.as_flag(),
                expected_flag,
                "{variant:?} should map to correct flag"
            );
        }
    }

    #[test]
    fn test_feature_monotonic_enablement() {
        // Once a feature is enabled, it should stay enabled in newer versions
        let versions = [
            ProtocolVersion::V28,
            ProtocolVersion::V29,
            ProtocolVersion::V30,
            ProtocolVersion::V31,
        ];

        let mut prev_sender_receiver = false;
        let mut prev_perishable = false;
        let mut prev_flist_times = false;
        let mut prev_safe_flist = false;
        let mut prev_varint_flist = false;

        for &protocol in &versions {
            // Once enabled, features stay enabled
            if prev_sender_receiver {
                assert!(
                    protocol.supports_sender_receiver_modifiers(),
                    "sender_receiver must stay enabled at {protocol}"
                );
            }
            if prev_perishable {
                assert!(
                    protocol.supports_perishable_modifier(),
                    "perishable must stay enabled at {protocol}"
                );
            }
            if prev_flist_times {
                assert!(
                    protocol.supports_flist_times(),
                    "flist_times must stay enabled at {protocol}"
                );
            }
            if prev_safe_flist {
                assert!(
                    protocol.uses_safe_file_list(),
                    "safe_flist must stay enabled at {protocol}"
                );
            }
            if prev_varint_flist {
                assert!(
                    protocol.uses_varint_flist_flags(),
                    "varint_flist must stay enabled at {protocol}"
                );
            }

            // Update state for next iteration
            prev_sender_receiver = protocol.supports_sender_receiver_modifiers();
            prev_perishable = protocol.supports_perishable_modifier();
            prev_flist_times = protocol.supports_flist_times();
            prev_safe_flist = protocol.uses_safe_file_list();
            prev_varint_flist = protocol.uses_varint_flist_flags();
        }
    }

    #[test]
    fn test_negotiation_boundary_at_v29_v30() {
        // This is the critical boundary: ASCII vs binary negotiation
        let v29 = ProtocolVersion::V29;
        let v30 = ProtocolVersion::V30;

        // Negotiation style changes
        assert!(v29.uses_legacy_ascii_negotiation());
        assert!(v30.uses_binary_negotiation());

        // Encoding style changes
        assert!(v29.uses_fixed_encoding());
        assert!(v30.uses_varint_encoding());

        // Feature additions at v30
        assert!(!v29.supports_perishable_modifier());
        assert!(v30.supports_perishable_modifier());

        assert!(!v29.uses_safe_file_list());
        assert!(v30.uses_safe_file_list());

        assert!(!v29.uses_varint_flist_flags());
        assert!(v30.uses_varint_flist_flags());
    }

    #[test]
    fn test_codec_encoding_roundtrip_all_versions() {
        let test_file_sizes = [0i64, 1, 100, 1000, 65535, 0x7FFF_FFFF, 0x1_0000_0000];

        for version in [28, 29, 30, 31] {
            let codec = create_protocol_codec(version);

            for &size in &test_file_sizes {
                let mut buf = Vec::new();
                codec.write_file_size(&mut buf, size).unwrap();

                let mut cursor = Cursor::new(&buf);
                let read = codec.read_file_size(&mut cursor).unwrap();

                assert_eq!(
                    read, size,
                    "v{version} file_size roundtrip failed for {size}"
                );
            }
        }
    }
}
