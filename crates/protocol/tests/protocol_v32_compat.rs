//! Protocol version 32 compatibility tests.
//!
//! Comprehensive tests for protocol version 32, the newest version (rsync 3.4.1).
//! These tests validate:
//! - Protocol version 32 handshake and negotiation
//! - All features inherited from v31 (binary negotiation, safe file list always enabled)
//! - New v32 capabilities (ID0_NAMES, XXH128 checksums, Zstd compression)
//! - Wire format encoding (varint, varlong, delta-encoded NDX)
//! - Backward compatibility with v28-v31
//! - Algorithm negotiation using vstring format
//!
//! # Protocol Version 32 Overview
//!
//! Protocol version 32 is the current newest version, introduced in rsync 3.4.1.
//! Key features:
//! - **All v31 features**: Binary negotiation, varint encoding, safe file list always enabled
//! - **XXH128 checksums**: 128-bit XXHash for better collision resistance
//! - **Zstd compression**: Modern, efficient compression algorithm
//! - **ID0_NAMES capability**: File-list entries support id0 names
//! - **Enhanced algorithm negotiation**: Full vstring-based algorithm selection
//!
//! # Upstream Reference
//!
//! Based on rsync 3.4.1 source code:
//! - `compat.c`: Compatibility flag handling and capability negotiation
//! - `flist.c`: File list encoding
//! - `io.c`: Protocol I/O and varint/varlong encoding
//! - `checksum.c`: Checksum algorithm support

use protocol::codec::{create_ndx_codec, create_protocol_codec, NdxCodec, ProtocolCodec};
use protocol::{
    ChecksumAlgorithm, CompatibilityFlags, CompressionAlgorithm, KnownCompatibilityFlag,
    ProtocolVersion, ProtocolVersionAdvertisement, select_highest_mutual,
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
// Module: Protocol Version 32 Handshake and Negotiation
// ============================================================================

mod protocol_32_handshake {
    use super::*;

    /// Protocol 32 is supported and should negotiate successfully.
    #[test]
    fn version_32_negotiation_succeeds() {
        let result = select_highest_mutual([TestVersion(32)]);
        assert!(
            result.is_ok(),
            "Protocol 32 negotiation must succeed: {result:?}"
        );
        assert_eq!(result.unwrap().as_u8(), 32);
    }

    /// Protocol 32 is in the supported protocol list.
    #[test]
    fn version_32_is_in_supported_list() {
        assert!(
            ProtocolVersion::is_supported_protocol_number(32),
            "Protocol 32 must be in supported list"
        );
    }

    /// Protocol 32 constant equals version from supported list.
    #[test]
    fn version_32_constant_equals_from_supported() {
        let from_supported = ProtocolVersion::from_supported(32).unwrap();
        assert_eq!(from_supported, ProtocolVersion::V32);
    }

    /// Protocol 32 try_from succeeds for u8.
    #[test]
    fn version_32_try_from_u8_succeeds() {
        let result = ProtocolVersion::try_from(32u8);
        assert!(result.is_ok(), "TryFrom<u8> for 32 should succeed");
        assert_eq!(result.unwrap(), ProtocolVersion::V32);
    }

    /// Protocol 32 from_peer_advertisement succeeds.
    #[test]
    fn version_32_from_peer_advertisement_succeeds() {
        let result = ProtocolVersion::from_peer_advertisement(32);
        assert!(result.is_ok(), "from_peer_advertisement(32) should succeed");
        assert_eq!(result.unwrap(), ProtocolVersion::V32);
    }

    /// When peer advertises multiple versions including 32, 32 should be selected.
    #[test]
    fn version_32_selected_from_multiple() {
        let result = select_highest_mutual([
            TestVersion(28),
            TestVersion(29),
            TestVersion(30),
            TestVersion(31),
            TestVersion(32),
        ]);
        assert!(result.is_ok(), "Should negotiate to 32");
        assert_eq!(result.unwrap().as_u8(), 32);
    }

    /// Protocol 32 as_u8 returns correct value.
    #[test]
    fn version_32_as_u8_returns_32() {
        assert_eq!(ProtocolVersion::V32.as_u8(), 32);
    }

    /// Protocol 32 equals NEWEST.
    #[test]
    fn version_32_equals_newest() {
        assert_eq!(ProtocolVersion::V32, ProtocolVersion::NEWEST);
    }

    /// Protocol 32 Display formatting works.
    #[test]
    fn version_32_display_formatting() {
        let version = ProtocolVersion::V32;
        let display = format!("{version}");
        assert!(
            display.contains("32"),
            "Display should include version number"
        );
    }

    /// Protocol 32 Debug formatting works.
    #[test]
    fn version_32_debug_formatting() {
        let version = ProtocolVersion::V32;
        let debug = format!("{version:?}");
        assert!(debug.contains("32"), "Debug should include version number");
    }
}

// ============================================================================
// Module: Protocol Version 32 Binary Negotiation
// ============================================================================

mod protocol_32_binary_negotiation {
    use super::*;

    /// Protocol 32 uses binary negotiation, not legacy ASCII.
    #[test]
    fn version_32_uses_binary_negotiation() {
        let v32 = ProtocolVersion::V32;
        assert!(
            v32.uses_binary_negotiation(),
            "Protocol 32 must use binary negotiation"
        );
        assert!(
            !v32.uses_legacy_ascii_negotiation(),
            "Protocol 32 must not use legacy ASCII negotiation"
        );
    }

    /// Protocol 32 handshake differs from v28/v29.
    #[test]
    fn version_32_handshake_differs_from_legacy() {
        let v28 = ProtocolVersion::V28;
        let v32 = ProtocolVersion::V32;

        // v28 uses legacy ASCII negotiation
        assert!(v28.uses_legacy_ascii_negotiation());
        // v32 uses binary negotiation
        assert!(v32.uses_binary_negotiation());

        // They should differ
        assert_ne!(v28.uses_binary_negotiation(), v32.uses_binary_negotiation());
    }

    /// Protocol 32 codec is modern, not legacy.
    #[test]
    fn version_32_codec_is_modern() {
        let codec = create_protocol_codec(ProtocolVersion::V32.as_u8());
        // Modern codec should support varint encoding
        assert!(
            ProtocolVersion::V32.uses_varint_encoding(),
            "v32 codec should use varint encoding"
        );
        assert!(!codec.is_legacy(), "v32 codec should not be legacy");
    }
}

// ============================================================================
// Module: Protocol Version 32 Feature Tests
// ============================================================================

mod protocol_32_features {
    use super::*;

    /// Protocol 32 uses varint encoding.
    #[test]
    fn version_32_uses_varint_encoding() {
        assert!(ProtocolVersion::V32.uses_varint_encoding());
        assert!(!ProtocolVersion::V32.uses_fixed_encoding());
    }

    /// Protocol 32 uses varint flist flags.
    #[test]
    fn version_32_uses_varint_flist_flags() {
        assert!(ProtocolVersion::V32.uses_varint_flist_flags());
    }

    /// Protocol 32 supports safe file list (always enabled, inherited from v31).
    #[test]
    fn version_32_safe_file_list_always_enabled() {
        assert!(ProtocolVersion::V32.uses_safe_file_list());
        assert!(ProtocolVersion::V32.safe_file_list_always_enabled());
    }

    /// Protocol 32 supports sender/receiver modifiers.
    #[test]
    fn version_32_supports_sender_receiver_modifiers() {
        assert!(ProtocolVersion::V32.supports_sender_receiver_modifiers());
    }

    /// Protocol 32 supports perishable modifier.
    #[test]
    fn version_32_supports_perishable_modifier() {
        assert!(ProtocolVersion::V32.supports_perishable_modifier());
    }

    /// Protocol 32 supports flist times.
    #[test]
    fn version_32_supports_flist_times() {
        assert!(ProtocolVersion::V32.supports_flist_times());
    }

    /// Protocol 32 does not use old prefixes.
    #[test]
    fn version_32_does_not_use_old_prefixes() {
        assert!(!ProtocolVersion::V32.uses_old_prefixes());
    }

    /// Protocol 32 supports extended flags.
    #[test]
    fn version_32_supports_extended_flags() {
        assert!(ProtocolVersion::V32.supports_extended_flags());
    }

    /// Protocol 32 has complete feature profile.
    #[test]
    fn version_32_complete_feature_profile() {
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
}

// ============================================================================
// Module: Protocol Version 32 Capability Flags
// ============================================================================

mod protocol_32_capability_flags {
    use super::*;

    /// Protocol 32 supports all known capability flags.
    #[test]
    fn version_32_supports_all_capability_flags() {
        let all_flags = CompatibilityFlags::ALL_KNOWN;

        // Verify all known flags are defined
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

    /// ID0_NAMES capability bit value is correct.
    #[test]
    fn version_32_id0_names_capability_bit() {
        let flags = CompatibilityFlags::ID0_NAMES;
        assert_eq!(flags.bits(), 1 << 8);
    }

    /// VARINT_FLIST_FLAGS capability bit value is correct.
    #[test]
    fn version_32_varint_flist_flags_capability_bit() {
        let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;
        assert_eq!(flags.bits(), 1 << 7);
    }

    /// All known capability flags have correct bit positions.
    #[test]
    fn version_32_all_flag_bit_positions() {
        assert_eq!(CompatibilityFlags::INC_RECURSE.bits(), 1 << 0);
        assert_eq!(CompatibilityFlags::SYMLINK_TIMES.bits(), 1 << 1);
        assert_eq!(CompatibilityFlags::SYMLINK_ICONV.bits(), 1 << 2);
        assert_eq!(CompatibilityFlags::SAFE_FILE_LIST.bits(), 1 << 3);
        assert_eq!(CompatibilityFlags::AVOID_XATTR_OPTIMIZATION.bits(), 1 << 4);
        assert_eq!(CompatibilityFlags::CHECKSUM_SEED_FIX.bits(), 1 << 5);
        assert_eq!(CompatibilityFlags::INPLACE_PARTIAL_DIR.bits(), 1 << 6);
        assert_eq!(CompatibilityFlags::VARINT_FLIST_FLAGS.bits(), 1 << 7);
        assert_eq!(CompatibilityFlags::ID0_NAMES.bits(), 1 << 8);
    }

    /// Capability flags can be combined.
    #[test]
    fn version_32_capability_flags_combine() {
        let combined = CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::SAFE_FILE_LIST
            | CompatibilityFlags::VARINT_FLIST_FLAGS
            | CompatibilityFlags::ID0_NAMES;

        assert!(combined.contains(CompatibilityFlags::INC_RECURSE));
        assert!(combined.contains(CompatibilityFlags::SAFE_FILE_LIST));
        assert!(combined.contains(CompatibilityFlags::VARINT_FLIST_FLAGS));
        assert!(combined.contains(CompatibilityFlags::ID0_NAMES));
        assert!(!combined.contains(CompatibilityFlags::SYMLINK_TIMES));
    }

    /// KnownCompatibilityFlag enum has all v32 flags.
    #[test]
    fn version_32_known_flag_enum() {
        assert_eq!(KnownCompatibilityFlag::ALL.len(), 9);
        assert_eq!(
            KnownCompatibilityFlag::IncRecurse.name(),
            "CF_INC_RECURSE"
        );
        assert_eq!(
            KnownCompatibilityFlag::SymlinkTimes.name(),
            "CF_SYMLINK_TIMES"
        );
        assert_eq!(
            KnownCompatibilityFlag::SymlinkIconv.name(),
            "CF_SYMLINK_ICONV"
        );
        assert_eq!(
            KnownCompatibilityFlag::SafeFileList.name(),
            "CF_SAFE_FLIST"
        );
        assert_eq!(
            KnownCompatibilityFlag::AvoidXattrOptimization.name(),
            "CF_AVOID_XATTR_OPTIM"
        );
        assert_eq!(
            KnownCompatibilityFlag::ChecksumSeedFix.name(),
            "CF_CHKSUM_SEED_FIX"
        );
        assert_eq!(
            KnownCompatibilityFlag::InplacePartialDir.name(),
            "CF_INPLACE_PARTIAL_DIR"
        );
        assert_eq!(
            KnownCompatibilityFlag::VarintFlistFlags.name(),
            "CF_VARINT_FLIST_FLAGS"
        );
        assert_eq!(KnownCompatibilityFlag::Id0Names.name(), "CF_ID0_NAMES");
    }

    /// Capability flags roundtrip through varint encoding.
    #[test]
    fn version_32_capability_flags_varint_roundtrip() {
        let original = CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::SAFE_FILE_LIST
            | CompatibilityFlags::VARINT_FLIST_FLAGS
            | CompatibilityFlags::ID0_NAMES;

        let mut buf = Vec::new();
        original.encode_to_vec(&mut buf).unwrap();

        let (decoded, remainder) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, original);
        assert!(remainder.is_empty());
    }
}

// ============================================================================
// Module: Protocol Version 32 Algorithm Negotiation
// ============================================================================

mod protocol_32_algorithms {
    use super::*;

    /// Protocol 32 supports XXH128 checksum algorithm.
    #[test]
    fn version_32_supports_xxh128() {
        let xxh128 = ChecksumAlgorithm::XXH128;
        assert_eq!(xxh128.as_str(), "xxh128");
    }

    /// Protocol 32 supports all checksum algorithms.
    #[test]
    fn version_32_supports_all_checksums() {
        assert_eq!(ChecksumAlgorithm::None.as_str(), "none");
        assert_eq!(ChecksumAlgorithm::MD4.as_str(), "md4");
        assert_eq!(ChecksumAlgorithm::MD5.as_str(), "md5");
        assert_eq!(ChecksumAlgorithm::SHA1.as_str(), "sha1");
        assert_eq!(ChecksumAlgorithm::XXH64.as_str(), "xxh64");
        assert_eq!(ChecksumAlgorithm::XXH3.as_str(), "xxh3");
        assert_eq!(ChecksumAlgorithm::XXH128.as_str(), "xxh128");
    }

    /// Protocol 32 supports Zstd compression algorithm.
    #[test]
    fn version_32_supports_zstd() {
        let zstd = CompressionAlgorithm::Zstd;
        assert_eq!(zstd.as_str(), "zstd");
    }

    /// Protocol 32 supports all compression algorithms.
    #[test]
    fn version_32_supports_all_compressions() {
        assert_eq!(CompressionAlgorithm::None.as_str(), "none");
        assert_eq!(CompressionAlgorithm::Zlib.as_str(), "zlib");
        assert_eq!(CompressionAlgorithm::ZlibX.as_str(), "zlibx");
        assert_eq!(CompressionAlgorithm::LZ4.as_str(), "lz4");
        assert_eq!(CompressionAlgorithm::Zstd.as_str(), "zstd");
    }

    /// Checksum algorithm parsing works correctly.
    #[test]
    fn version_32_checksum_parsing() {
        assert!(ChecksumAlgorithm::parse("none").is_ok());
        assert!(ChecksumAlgorithm::parse("md4").is_ok());
        assert!(ChecksumAlgorithm::parse("md5").is_ok());
        assert!(ChecksumAlgorithm::parse("sha1").is_ok());
        assert!(ChecksumAlgorithm::parse("xxh64").is_ok());
        assert!(ChecksumAlgorithm::parse("xxh").is_ok()); // alias for xxh64
        assert!(ChecksumAlgorithm::parse("xxh3").is_ok());
        assert!(ChecksumAlgorithm::parse("xxh128").is_ok());
        assert!(ChecksumAlgorithm::parse("unknown").is_err());
    }

    /// Compression algorithm parsing works correctly.
    #[test]
    fn version_32_compression_parsing() {
        assert!(CompressionAlgorithm::parse("none").is_ok());
        assert!(CompressionAlgorithm::parse("zlib").is_ok());
        assert!(CompressionAlgorithm::parse("zlibx").is_ok());
        assert!(CompressionAlgorithm::parse("lz4").is_ok());
        assert!(CompressionAlgorithm::parse("zstd").is_ok());
        assert!(CompressionAlgorithm::parse("unknown").is_err());
    }
}

// ============================================================================
// Module: Protocol Version 32 Wire Format
// ============================================================================

mod protocol_32_wire_format {
    use super::*;

    /// Protocol 32 uses modern NDX encoding (delta-based).
    #[test]
    fn version_32_ndx_encoding() {
        let mut codec = create_ndx_codec(32);

        // First NDX value: delta from prev=-1 to 0, diff=1 => byte value 0x01
        let mut buf = Vec::new();
        codec.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf, vec![0x01]);

        // Second NDX value: delta from prev=0 to 1, diff=1 => byte value 0x01
        buf.clear();
        codec.write_ndx(&mut buf, 1).unwrap();
        assert_eq!(buf, vec![0x01]);

        // Third NDX value: delta from prev=1 to 5, diff=4 => byte value 0x04
        buf.clear();
        codec.write_ndx(&mut buf, 5).unwrap();
        assert_eq!(buf, vec![0x04]);
    }

    /// Protocol 32 NDX roundtrip works correctly.
    #[test]
    fn version_32_ndx_roundtrip() {
        let mut write_codec = create_ndx_codec(32);
        let mut buf = Vec::new();

        // Write sequence of NDX values
        write_codec.write_ndx(&mut buf, 0).unwrap();
        write_codec.write_ndx(&mut buf, 1).unwrap();
        write_codec.write_ndx(&mut buf, 5).unwrap();
        write_codec.write_ndx(&mut buf, 10).unwrap();

        // Read them back
        let mut read_codec = create_ndx_codec(32);
        let mut cursor = Cursor::new(&buf);
        assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), 0);
        assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), 1);
        assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), 5);
        assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), 10);
    }

    /// Protocol 32 uses modern protocol codec.
    #[test]
    fn version_32_protocol_codec() {
        let codec = create_protocol_codec(32);
        assert_eq!(codec.protocol_version(), 32);
        assert!(!codec.is_legacy());
    }

    /// Protocol 32 file size encoding (varlong).
    #[test]
    fn version_32_file_size_encoding() {
        let codec = create_protocol_codec(32);
        let mut buf = Vec::new();

        // Small file size
        codec.write_file_size(&mut buf, 1000).unwrap();
        // Should be compact (varlong encoding)
        assert!(buf.len() < 8, "varlong should be compact for small values");

        // Read it back
        let mut cursor = Cursor::new(&buf);
        let value = codec.read_file_size(&mut cursor).unwrap();
        assert_eq!(value, 1000);
    }

    /// Protocol 32 mtime encoding (varlong).
    #[test]
    fn version_32_mtime_encoding() {
        let codec = create_protocol_codec(32);
        let mut buf = Vec::new();

        // Typical Unix timestamp
        let mtime = 1700000000i64;
        codec.write_mtime(&mut buf, mtime).unwrap();

        // Read it back
        let mut cursor = Cursor::new(&buf);
        let value = codec.read_mtime(&mut cursor).unwrap();
        assert_eq!(value, mtime);
    }

    /// Protocol 32 long name length encoding (varint).
    #[test]
    fn version_32_long_name_len_encoding() {
        let codec = create_protocol_codec(32);
        let mut buf = Vec::new();

        // Write a long name length
        codec.write_long_name_len(&mut buf, 300).unwrap();

        // Read it back
        let mut cursor = Cursor::new(&buf);
        let value = codec.read_long_name_len(&mut cursor).unwrap();
        assert_eq!(value, 300);
    }

    /// Protocol 32 encoding is more compact than legacy.
    #[test]
    fn version_32_encoding_more_compact() {
        let codec_32 = create_protocol_codec(32);
        let codec_29 = create_protocol_codec(29);

        let mut buf_32 = Vec::new();
        let mut buf_29 = Vec::new();

        // For small file sizes, modern encoding is typically smaller
        codec_32.write_file_size(&mut buf_32, 1000).unwrap();
        codec_29.write_file_size(&mut buf_29, 1000).unwrap();

        // Modern (varlong min_bytes=3) vs Legacy (4-byte fixed)
        // For a value of 1000, varlong should fit in 3 bytes
        assert!(buf_32.len() <= buf_29.len());
    }
}

// ============================================================================
// Module: Protocol Version 32 Backward Compatibility
// ============================================================================

mod protocol_32_backward_compatibility {
    use super::*;

    /// v32 can downgrade to v31.
    #[test]
    fn version_32_downgrades_to_v31() {
        let result = select_highest_mutual([TestVersion(31)]).unwrap();
        assert_eq!(result, ProtocolVersion::V31);
    }

    /// v32 can downgrade to v30.
    #[test]
    fn version_32_downgrades_to_v30() {
        let result = select_highest_mutual([TestVersion(30)]).unwrap();
        assert_eq!(result, ProtocolVersion::V30);
    }

    /// v32 can downgrade to v29.
    #[test]
    fn version_32_downgrades_to_v29() {
        let result = select_highest_mutual([TestVersion(29)]).unwrap();
        assert_eq!(result, ProtocolVersion::V29);
    }

    /// v32 can downgrade to v28.
    #[test]
    fn version_32_downgrades_to_v28() {
        let result = select_highest_mutual([TestVersion(28)]).unwrap();
        assert_eq!(result, ProtocolVersion::V28);
    }

    /// v32 selects highest common version.
    #[test]
    fn version_32_selects_highest_common() {
        let result =
            select_highest_mutual([TestVersion(28), TestVersion(29), TestVersion(30)]).unwrap();
        assert_eq!(result, ProtocolVersion::V30);
    }

    /// v32 vs v31 feature comparison.
    #[test]
    fn version_32_vs_v31_features() {
        let v31 = ProtocolVersion::V31;
        let v32 = ProtocolVersion::V32;

        // Same features
        assert_eq!(v31.uses_binary_negotiation(), v32.uses_binary_negotiation());
        assert_eq!(v31.uses_varint_encoding(), v32.uses_varint_encoding());
        assert_eq!(
            v31.safe_file_list_always_enabled(),
            v32.safe_file_list_always_enabled()
        );
        assert_eq!(
            v31.uses_varint_flist_flags(),
            v32.uses_varint_flist_flags()
        );
    }

    /// v32 vs v30 feature comparison.
    #[test]
    fn version_32_vs_v30_features() {
        let v30 = ProtocolVersion::V30;
        let v32 = ProtocolVersion::V32;

        // Same binary negotiation
        assert_eq!(v30.uses_binary_negotiation(), v32.uses_binary_negotiation());

        // Key difference: safe file list always enabled
        assert!(!v30.safe_file_list_always_enabled());
        assert!(v32.safe_file_list_always_enabled());
    }

    /// v32 vs v29 feature comparison.
    #[test]
    fn version_32_vs_v29_features() {
        let v29 = ProtocolVersion::V29;
        let v32 = ProtocolVersion::V32;

        // Key differences
        assert!(v29.uses_legacy_ascii_negotiation());
        assert!(v32.uses_binary_negotiation());

        assert!(v29.uses_fixed_encoding());
        assert!(v32.uses_varint_encoding());
    }

    /// v32 vs v28 feature comparison.
    #[test]
    fn version_32_vs_v28_features() {
        let v28 = ProtocolVersion::V28;
        let v32 = ProtocolVersion::V32;

        // Key differences
        assert!(v28.uses_old_prefixes());
        assert!(!v32.uses_old_prefixes());

        assert!(!v28.supports_sender_receiver_modifiers());
        assert!(v32.supports_sender_receiver_modifiers());
    }
}

// ============================================================================
// Module: Protocol Version 32 Future Version Handling
// ============================================================================

mod protocol_32_future_versions {
    use super::*;

    /// Future versions within MAXIMUM_PROTOCOL_ADVERTISEMENT clamp to v32.
    #[test]
    fn version_32_clamps_future_versions() {
        // Version 33-40 should clamp to 32
        for v in 33..=40 {
            let result = ProtocolVersion::from_peer_advertisement(v).unwrap();
            assert_eq!(result, ProtocolVersion::V32);
        }
    }

    /// Version 41+ is rejected.
    #[test]
    fn version_32_rejects_beyond_maximum() {
        let result = ProtocolVersion::from_peer_advertisement(41);
        assert!(result.is_err());
    }

    /// Zero is rejected.
    #[test]
    fn version_32_rejects_zero() {
        let result = ProtocolVersion::from_peer_advertisement(0);
        assert!(result.is_err());
    }

    /// Max u32 is rejected.
    #[test]
    fn version_32_rejects_max_u32() {
        let result = ProtocolVersion::from_peer_advertisement(u32::MAX);
        assert!(result.is_err());
    }
}

// ============================================================================
// Module: Protocol Version 32 Ordering and Comparison
// ============================================================================

mod protocol_32_ordering {
    use super::*;

    /// v32 ordering relative to other versions.
    #[test]
    fn version_32_ordering() {
        assert!(ProtocolVersion::V32 > ProtocolVersion::V31);
        assert!(ProtocolVersion::V32 > ProtocolVersion::V30);
        assert!(ProtocolVersion::V32 > ProtocolVersion::V29);
        assert!(ProtocolVersion::V32 > ProtocolVersion::V28);
    }

    /// v32 equality comparisons.
    #[test]
    fn version_32_equality() {
        assert_eq!(ProtocolVersion::V32, ProtocolVersion::V32);
        assert_eq!(ProtocolVersion::V32, ProtocolVersion::NEWEST);
        assert_ne!(ProtocolVersion::V32, ProtocolVersion::V31);
    }

    /// v32 comparison with integers.
    #[test]
    fn version_32_integer_comparison() {
        assert_eq!(ProtocolVersion::V32, 32u8);
        assert_eq!(32u8, ProtocolVersion::V32);
        assert_ne!(ProtocolVersion::V32, 31u8);
    }

    /// v32 offset from oldest.
    #[test]
    fn version_32_offset_from_oldest() {
        assert_eq!(ProtocolVersion::V32.offset_from_oldest(), 4);
    }

    /// v32 offset from newest.
    #[test]
    fn version_32_offset_from_newest() {
        assert_eq!(ProtocolVersion::V32.offset_from_newest(), 0);
    }

    /// v32 is first in supported list.
    #[test]
    fn version_32_first_in_list() {
        let versions = ProtocolVersion::supported_versions();
        assert_eq!(versions[0], ProtocolVersion::V32);
    }

    /// v32 is at index 0 in supported versions.
    #[test]
    fn version_32_at_index_zero() {
        let version = ProtocolVersion::from_supported_index(0);
        assert_eq!(version, Some(ProtocolVersion::V32));
    }
}

// ============================================================================
// Module: Protocol Version 32 Real-World Scenarios
// ============================================================================

mod protocol_32_scenarios {
    use super::*;

    /// Typical v32 client-server interaction.
    #[test]
    fn version_32_typical_interaction() {
        // Both support v28-v32
        let client = [
            TestVersion(28),
            TestVersion(29),
            TestVersion(30),
            TestVersion(31),
            TestVersion(32),
        ];
        let server = [
            TestVersion(28),
            TestVersion(29),
            TestVersion(30),
            TestVersion(31),
            TestVersion(32),
        ];

        let negotiated = select_highest_mutual(client).unwrap();
        assert_eq!(negotiated, ProtocolVersion::V32);

        let negotiated = select_highest_mutual(server).unwrap();
        assert_eq!(negotiated, ProtocolVersion::V32);
    }

    /// v32 client to v31 server.
    #[test]
    fn version_32_client_to_v31_server() {
        let server = [
            TestVersion(28),
            TestVersion(29),
            TestVersion(30),
            TestVersion(31),
        ];
        let negotiated = select_highest_mutual(server).unwrap();
        assert_eq!(negotiated, ProtocolVersion::V31);
    }

    /// v32 client to legacy v29 server.
    #[test]
    fn version_32_client_to_legacy_server() {
        let server = [TestVersion(28), TestVersion(29)];
        let negotiated = select_highest_mutual(server).unwrap();
        assert_eq!(negotiated, ProtocolVersion::V29);
    }

    /// Mixed version infrastructure.
    #[test]
    fn version_32_mixed_infrastructure() {
        // Different servers with different versions
        let old_server = [TestVersion(28), TestVersion(29)];
        let medium_server = [TestVersion(28), TestVersion(29), TestVersion(30)];
        let modern_server = [
            TestVersion(28),
            TestVersion(29),
            TestVersion(30),
            TestVersion(31),
        ];
        let latest_server = [
            TestVersion(28),
            TestVersion(29),
            TestVersion(30),
            TestVersion(31),
            TestVersion(32),
        ];

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
}

// ============================================================================
// Module: Protocol Version 32 Edge Cases
// ============================================================================

mod protocol_32_edge_cases {
    use super::*;

    /// All supported versions can negotiate.
    #[test]
    fn version_32_all_supported_negotiate() {
        for &version in ProtocolVersion::supported_protocol_numbers() {
            let result = select_highest_mutual([TestVersion(version.into())]);
            assert!(
                result.is_ok(),
                "version {} should negotiate successfully",
                version
            );
        }
    }

    /// Hash and equality in collections.
    #[test]
    fn version_32_hash_collections() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(ProtocolVersion::V32);
        set.insert(ProtocolVersion::V32);
        set.insert(ProtocolVersion::NEWEST);

        // All are the same, so 1 element
        assert_eq!(set.len(), 1);
        assert!(set.contains(&ProtocolVersion::V32));
    }

    /// Duplicate versions in negotiation.
    #[test]
    fn version_32_duplicate_versions() {
        let result = select_highest_mutual([
            TestVersion(32),
            TestVersion(32),
            TestVersion(32),
            TestVersion(31),
        ])
        .unwrap();
        assert_eq!(result, ProtocolVersion::V32);
    }

    /// Protocol bitmap has v32 bit set.
    #[test]
    fn version_32_in_bitmap() {
        let bitmap = ProtocolVersion::supported_protocol_bitmap();
        assert_ne!(bitmap & (1u64 << 32), 0);
    }

    /// v32 has no next newer version.
    #[test]
    fn version_32_no_next_newer() {
        assert!(ProtocolVersion::V32.next_newer().is_none());
    }

    /// v32 next older is v31.
    #[test]
    fn version_32_next_older() {
        assert_eq!(
            ProtocolVersion::V32.next_older(),
            Some(ProtocolVersion::V31)
        );
    }
}
