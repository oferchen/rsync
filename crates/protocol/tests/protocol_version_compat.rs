//! Protocol version 27-31 compatibility tests.
//!
//! Comprehensive tests for protocol version compatibility across versions 27-31.
//! These tests validate:
//! - Capability negotiation for each protocol version
//! - Wire format differences between versions
//! - Feature availability and version gates
//! - Backward compatibility with older versions
//!
//! # Protocol Version Overview
//!
//! | Version | Key Features |
//! |---------|-------------|
//! | 27 | Base version (unsupported - too old) |
//! | 28 | Extended file flags, oldest supported |
//! | 29 | Incremental recursion foundation, sender/receiver modifiers |
//! | 30 | Binary negotiation, varint encoding, compatibility flags |
//! | 31 | Safe file list always enabled, checksum seed fix |
//! | 32 | Latest features (current implementation) |
//!
//! # Upstream Reference
//!
//! Protocol details are based on rsync 3.4.1 source code.

use protocol::{
    CompatibilityFlags, NegotiationError, ProtocolVersion,
    ProtocolVersionAdvertisement, SUPPORTED_PROTOCOLS, format_legacy_daemon_greeting,
    select_highest_mutual,
};
use protocol::codec::{
    NdxCodec, ProtocolCodec,
    create_ndx_codec, create_protocol_codec,
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
// Module: Protocol Version 27 (Unsupported Base Version)
// ============================================================================

mod protocol_27_unsupported {
    use super::*;

    /// Protocol 27 is below the minimum supported version and should be rejected.
    #[test]
    fn version_27_rejected_in_negotiation() {
        let result = select_highest_mutual([TestVersion(27)]);
        assert!(
            result.is_err(),
            "Protocol 27 must be rejected as unsupported"
        );

        if let Err(NegotiationError::UnsupportedVersion(ver)) = result {
            assert_eq!(ver, 27);
        } else {
            panic!("Expected UnsupportedVersion(27), got: {result:?}");
        }
    }

    /// Protocol 27 is not in the supported protocol list.
    #[test]
    fn version_27_not_in_supported_list() {
        assert!(
            !ProtocolVersion::is_supported_protocol_number(27),
            "Protocol 27 should not be in supported list"
        );
    }

    /// Attempting to create a ProtocolVersion from 27 should fail.
    #[test]
    fn version_27_try_from_fails() {
        let result = ProtocolVersion::try_from(27u8);
        assert!(result.is_err(), "TryFrom<u8> for 27 should fail");
    }

    /// Protocol 27 from_peer_advertisement should fail.
    #[test]
    fn version_27_from_peer_advertisement_fails() {
        let result = ProtocolVersion::from_peer_advertisement(27);
        assert!(result.is_err(), "from_peer_advertisement(27) should fail");
    }

    /// Backward compatibility: when peer advertises 27 and 28, should use 28.
    #[test]
    fn version_27_with_28_fallback() {
        // When 27 and 28 are both advertised, 28 is rejected first, causing
        // the negotiation to try 28 which succeeds
        let result = select_highest_mutual([TestVersion(28), TestVersion(27)]);
        assert!(result.is_ok(), "Should negotiate to 28");
        assert_eq!(result.unwrap().as_u8(), 28);
    }
}

// ============================================================================
// Module: Protocol Version 28 (Base Supported Version)
// ============================================================================

mod protocol_28_base_version {
    use super::*;

    // ------------------------------------------------------------------------
    // Capability Negotiation Tests
    // ------------------------------------------------------------------------

    #[test]
    fn version_28_is_oldest_supported() {
        assert_eq!(
            ProtocolVersion::OLDEST,
            ProtocolVersion::V28,
            "Protocol 28 must be the oldest supported version"
        );
    }

    #[test]
    fn version_28_negotiation_succeeds() {
        let result = select_highest_mutual([TestVersion(28)]);
        assert!(result.is_ok(), "Protocol 28 negotiation must succeed");
        assert_eq!(result.unwrap().as_u8(), 28);
    }

    #[test]
    fn version_28_is_in_supported_list() {
        assert!(
            ProtocolVersion::is_supported_protocol_number(28),
            "Protocol 28 must be in supported list"
        );
    }

    #[test]
    fn version_28_constant_equals_from_supported() {
        let from_supported = ProtocolVersion::from_supported(28).unwrap();
        assert_eq!(from_supported, ProtocolVersion::V28);
    }

    // ------------------------------------------------------------------------
    // Wire Format Tests
    // ------------------------------------------------------------------------

    #[test]
    fn version_28_uses_legacy_ascii_negotiation() {
        assert!(
            ProtocolVersion::V28.uses_legacy_ascii_negotiation(),
            "Protocol 28 must use ASCII negotiation"
        );
        assert!(
            !ProtocolVersion::V28.uses_binary_negotiation(),
            "Protocol 28 must not use binary negotiation"
        );
    }

    #[test]
    fn version_28_legacy_greeting_format() {
        let greeting = format_legacy_daemon_greeting(ProtocolVersion::V28);
        assert_eq!(greeting, "@RSYNCD: 28.0\n");
        assert!(greeting.is_ascii());
        assert_eq!(greeting.len(), 14);
    }

    #[test]
    fn version_28_uses_fixed_encoding() {
        assert!(
            ProtocolVersion::V28.uses_fixed_encoding(),
            "Protocol 28 must use fixed encoding"
        );
        assert!(
            !ProtocolVersion::V28.uses_varint_encoding(),
            "Protocol 28 must not use varint encoding"
        );
    }

    #[test]
    fn version_28_codec_is_legacy() {
        let codec = create_protocol_codec(28);
        assert!(codec.is_legacy(), "Protocol 28 codec must be legacy");
        assert_eq!(codec.protocol_version(), 28);
    }

    #[test]
    fn version_28_file_size_encoding_fixed_4_bytes() {
        let codec = create_protocol_codec(28);
        let mut buf = Vec::new();
        codec.write_file_size(&mut buf, 1000).unwrap();
        assert_eq!(buf.len(), 4, "Protocol 28 uses 4-byte fixed size");
        assert_eq!(buf, vec![0xe8, 0x03, 0x00, 0x00]); // 1000 in LE
    }

    #[test]
    fn version_28_ndx_uses_legacy_encoding() {
        let mut codec = create_ndx_codec(28);
        let mut buf = Vec::new();
        codec.write_ndx(&mut buf, 5).unwrap();
        assert_eq!(buf.len(), 4, "Protocol 28 NDX uses 4-byte LE");
        assert_eq!(buf, vec![5, 0, 0, 0]);
    }

    // ------------------------------------------------------------------------
    // Feature Availability Tests
    // ------------------------------------------------------------------------

    #[test]
    fn version_28_supports_extended_flags() {
        assert!(
            ProtocolVersion::V28.supports_extended_flags(),
            "Protocol 28 supports extended file flags"
        );
    }

    #[test]
    fn version_28_no_sender_receiver_modifiers() {
        let codec = create_protocol_codec(28);
        assert!(
            !codec.supports_sender_receiver_modifiers(),
            "Protocol 28 does not support sender/receiver modifiers"
        );
    }

    #[test]
    fn version_28_uses_old_prefixes() {
        let codec = create_protocol_codec(28);
        assert!(
            codec.uses_old_prefixes(),
            "Protocol 28 uses old-style filter prefixes"
        );
    }

    #[test]
    fn version_28_no_perishable_modifier() {
        let codec = create_protocol_codec(28);
        assert!(
            !codec.supports_perishable_modifier(),
            "Protocol 28 does not support perishable modifier"
        );
    }

    #[test]
    fn version_28_no_flist_times() {
        let codec = create_protocol_codec(28);
        assert!(
            !codec.supports_flist_times(),
            "Protocol 28 does not support flist timing stats"
        );
    }

    #[test]
    fn version_28_no_safe_file_list() {
        assert!(
            !ProtocolVersion::V28.uses_safe_file_list(),
            "Protocol 28 does not use safe file list"
        );
    }

    // ------------------------------------------------------------------------
    // Backward Compatibility Tests
    // ------------------------------------------------------------------------

    #[test]
    fn version_28_compatible_with_newer_versions() {
        // When negotiating with a peer that supports 32, should fall back to 28
        // if we only advertise 28
        let result = select_highest_mutual([TestVersion(28)]);
        assert!(result.is_ok());
        let negotiated = result.unwrap();

        // Should be able to use the negotiated version
        let codec = create_protocol_codec(negotiated.as_u8());
        assert!(codec.is_legacy());
    }
}

// ============================================================================
// Module: Protocol Version 29 (Incremental Recursion Foundation)
// ============================================================================

mod protocol_29_incremental_recursion {
    use super::*;

    // ------------------------------------------------------------------------
    // Capability Negotiation Tests
    // ------------------------------------------------------------------------

    #[test]
    fn version_29_negotiation_succeeds() {
        let result = select_highest_mutual([TestVersion(29)]);
        assert!(result.is_ok(), "Protocol 29 negotiation must succeed");
        assert_eq!(result.unwrap().as_u8(), 29);
    }

    #[test]
    fn version_29_is_in_supported_list() {
        assert!(
            ProtocolVersion::is_supported_protocol_number(29),
            "Protocol 29 must be in supported list"
        );
    }

    #[test]
    fn version_29_constant_equals_from_supported() {
        let from_supported = ProtocolVersion::from_supported(29).unwrap();
        assert_eq!(from_supported, ProtocolVersion::V29);
    }

    #[test]
    fn version_29_is_newest_legacy_protocol() {
        // Protocol 29 is the last version using ASCII negotiation
        assert!(
            ProtocolVersion::V29.uses_legacy_ascii_negotiation(),
            "Protocol 29 is the newest legacy protocol"
        );
        let next = ProtocolVersion::V29.next_newer().unwrap();
        assert!(
            next.uses_binary_negotiation(),
            "Protocol 30 (next) uses binary negotiation"
        );
    }

    // ------------------------------------------------------------------------
    // Wire Format Tests
    // ------------------------------------------------------------------------

    #[test]
    fn version_29_uses_legacy_ascii_negotiation() {
        assert!(
            ProtocolVersion::V29.uses_legacy_ascii_negotiation(),
            "Protocol 29 must use ASCII negotiation"
        );
        assert!(
            !ProtocolVersion::V29.uses_binary_negotiation(),
            "Protocol 29 must not use binary negotiation"
        );
    }

    #[test]
    fn version_29_legacy_greeting_format() {
        let greeting = format_legacy_daemon_greeting(ProtocolVersion::V29);
        assert_eq!(greeting, "@RSYNCD: 29.0\n");
        assert!(greeting.is_ascii());
        assert_eq!(greeting.len(), 14);
    }

    #[test]
    fn version_29_uses_fixed_encoding() {
        assert!(
            ProtocolVersion::V29.uses_fixed_encoding(),
            "Protocol 29 must use fixed encoding"
        );
        assert!(
            !ProtocolVersion::V29.uses_varint_encoding(),
            "Protocol 29 must not use varint encoding"
        );
    }

    #[test]
    fn version_29_codec_is_legacy() {
        let codec = create_protocol_codec(29);
        assert!(codec.is_legacy(), "Protocol 29 codec must be legacy");
        assert_eq!(codec.protocol_version(), 29);
    }

    #[test]
    fn version_29_file_size_encoding_fixed_4_bytes() {
        let codec = create_protocol_codec(29);
        let mut buf = Vec::new();
        codec.write_file_size(&mut buf, 1000).unwrap();
        assert_eq!(buf.len(), 4, "Protocol 29 uses 4-byte fixed size");
    }

    #[test]
    fn version_29_large_file_uses_longint() {
        let codec = create_protocol_codec(29);
        let mut buf = Vec::new();
        let large_value = 0x1_0000_0000i64; // > 32-bit
        codec.write_file_size(&mut buf, large_value).unwrap();
        // Legacy uses 4-byte marker + 8-byte value for large values
        assert_eq!(buf.len(), 12, "Large file sizes use 12-byte longint");
        assert_eq!(&buf[0..4], &[0xff, 0xff, 0xff, 0xff]); // Marker
    }

    #[test]
    fn version_29_ndx_uses_legacy_encoding() {
        let mut codec = create_ndx_codec(29);
        let mut buf = Vec::new();
        codec.write_ndx(&mut buf, 5).unwrap();
        assert_eq!(buf.len(), 4, "Protocol 29 NDX uses 4-byte LE");
    }

    // ------------------------------------------------------------------------
    // Feature Availability Tests (Incremental Recursion Foundation)
    // ------------------------------------------------------------------------

    #[test]
    fn version_29_supports_sender_receiver_modifiers() {
        let codec = create_protocol_codec(29);
        assert!(
            codec.supports_sender_receiver_modifiers(),
            "Protocol 29 supports sender/receiver modifiers (s, r)"
        );
    }

    #[test]
    fn version_29_no_old_prefixes() {
        let codec = create_protocol_codec(29);
        assert!(
            !codec.uses_old_prefixes(),
            "Protocol 29 does not use old-style filter prefixes"
        );
    }

    #[test]
    fn version_29_supports_flist_times() {
        let codec = create_protocol_codec(29);
        assert!(
            codec.supports_flist_times(),
            "Protocol 29 supports flist timing stats"
        );
    }

    #[test]
    fn version_29_no_perishable_modifier() {
        let codec = create_protocol_codec(29);
        assert!(
            !codec.supports_perishable_modifier(),
            "Protocol 29 does not support perishable modifier (introduced in 30)"
        );
    }

    #[test]
    fn version_29_no_safe_file_list() {
        assert!(
            !ProtocolVersion::V29.uses_safe_file_list(),
            "Protocol 29 does not use safe file list (introduced in 30)"
        );
    }

    #[test]
    fn version_29_no_varint_flist_flags() {
        assert!(
            !ProtocolVersion::V29.uses_varint_flist_flags(),
            "Protocol 29 does not use varint flist flags"
        );
    }

    // ------------------------------------------------------------------------
    // Backward Compatibility Tests
    // ------------------------------------------------------------------------

    #[test]
    fn version_29_with_28_selects_29() {
        let result = select_highest_mutual([TestVersion(29), TestVersion(28)]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 29, "Should select highest (29)");
    }

    #[test]
    fn version_29_feature_boundary_from_28() {
        // Protocol 29 adds sender/receiver modifiers that 28 doesn't have
        let codec_28 = create_protocol_codec(28);
        let codec_29 = create_protocol_codec(29);

        assert!(!codec_28.supports_sender_receiver_modifiers());
        assert!(codec_29.supports_sender_receiver_modifiers());

        assert!(codec_28.uses_old_prefixes());
        assert!(!codec_29.uses_old_prefixes());
    }
}

// ============================================================================
// Module: Protocol Version 30 (Binary Negotiation & Extended Features)
// ============================================================================

mod protocol_30_binary_negotiation {
    use super::*;

    // ------------------------------------------------------------------------
    // Capability Negotiation Tests
    // ------------------------------------------------------------------------

    #[test]
    fn version_30_negotiation_succeeds() {
        let result = select_highest_mutual([TestVersion(30)]);
        assert!(result.is_ok(), "Protocol 30 negotiation must succeed");
        assert_eq!(result.unwrap().as_u8(), 30);
    }

    #[test]
    fn version_30_is_first_binary_protocol() {
        assert_eq!(
            ProtocolVersion::BINARY_NEGOTIATION_INTRODUCED,
            ProtocolVersion::V30,
            "Protocol 30 introduces binary negotiation"
        );
    }

    #[test]
    fn version_30_is_in_supported_list() {
        assert!(
            ProtocolVersion::is_supported_protocol_number(30),
            "Protocol 30 must be in supported list"
        );
    }

    // ------------------------------------------------------------------------
    // Wire Format Tests (Binary Negotiation)
    // ------------------------------------------------------------------------

    #[test]
    fn version_30_uses_binary_negotiation() {
        assert!(
            ProtocolVersion::V30.uses_binary_negotiation(),
            "Protocol 30 must use binary negotiation"
        );
        assert!(
            !ProtocolVersion::V30.uses_legacy_ascii_negotiation(),
            "Protocol 30 must not use ASCII negotiation"
        );
    }

    #[test]
    fn version_30_binary_advertisement_format() {
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
    fn version_30_uses_varint_encoding() {
        assert!(
            ProtocolVersion::V30.uses_varint_encoding(),
            "Protocol 30 must use varint encoding"
        );
        assert!(
            !ProtocolVersion::V30.uses_fixed_encoding(),
            "Protocol 30 must not use fixed encoding"
        );
    }

    #[test]
    fn version_30_codec_is_modern() {
        let codec = create_protocol_codec(30);
        assert!(!codec.is_legacy(), "Protocol 30 codec must be modern");
        assert_eq!(codec.protocol_version(), 30);
    }

    #[test]
    fn version_30_file_size_uses_varlong() {
        let codec = create_protocol_codec(30);
        let mut buf = Vec::new();
        codec.write_file_size(&mut buf, 100).unwrap();
        // Modern varlong with min_bytes=3 is more compact
        assert!(buf.len() <= 4, "varlong should be compact for small values");

        // Roundtrip test
        let mut cursor = Cursor::new(&buf);
        let read = codec.read_file_size(&mut cursor).unwrap();
        assert_eq!(read, 100);
    }

    #[test]
    fn version_30_ndx_uses_delta_encoding() {
        let mut codec = create_ndx_codec(30);
        let mut buf = Vec::new();

        // First index 0 with prev=-1 gives delta=1
        codec.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf.len(), 1, "Modern NDX uses delta encoding");
        assert_eq!(buf, vec![0x01]);
    }

    #[test]
    fn version_30_ndx_done_single_byte() {
        let mut codec = create_ndx_codec(30);
        let mut buf = Vec::new();
        codec.write_ndx_done(&mut buf).unwrap();
        assert_eq!(buf, vec![0x00], "NDX_DONE is single byte 0x00");
    }

    // ------------------------------------------------------------------------
    // Feature Availability Tests (Extended Attributes)
    // ------------------------------------------------------------------------

    #[test]
    fn version_30_supports_compatibility_flags() {
        // Protocol 30 can encode and decode compatibility flags
        let flags = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SAFE_FILE_LIST;
        let mut buf = Vec::new();
        flags.encode_to_vec(&mut buf).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, flags);
    }

    #[test]
    fn version_30_supports_perishable_modifier() {
        let codec = create_protocol_codec(30);
        assert!(
            codec.supports_perishable_modifier(),
            "Protocol 30 supports perishable modifier (p)"
        );
    }

    #[test]
    fn version_30_uses_safe_file_list() {
        assert!(
            ProtocolVersion::V30.uses_safe_file_list(),
            "Protocol 30 uses safe file list (negotiable)"
        );
        assert!(
            !ProtocolVersion::V30.safe_file_list_always_enabled(),
            "Protocol 30 safe file list is negotiable, not always enabled"
        );
    }

    #[test]
    fn version_30_uses_varint_flist_flags() {
        assert!(
            ProtocolVersion::V30.uses_varint_flist_flags(),
            "Protocol 30 uses varint-encoded flist flags"
        );
    }

    #[test]
    fn version_30_inc_recurse_flag() {
        let flag = CompatibilityFlags::INC_RECURSE;
        assert_eq!(flag.bits(), 1 << 0, "CF_INC_RECURSE is bit 0");

        let mut buf = Vec::new();
        flag.encode_to_vec(&mut buf).unwrap();
        assert_eq!(buf, vec![1]);
    }

    // ------------------------------------------------------------------------
    // Backward Compatibility Tests
    // ------------------------------------------------------------------------

    #[test]
    fn version_30_boundary_with_29() {
        // This is the critical boundary: ASCII vs binary negotiation
        let protocol_29 = ProtocolVersion::V29;
        let protocol_30 = ProtocolVersion::V30;

        // Negotiation style changes
        assert!(protocol_29.uses_legacy_ascii_negotiation());
        assert!(protocol_30.uses_binary_negotiation());

        // Encoding style changes
        assert!(protocol_29.uses_fixed_encoding());
        assert!(protocol_30.uses_varint_encoding());

        // Feature additions
        let codec_29 = create_protocol_codec(29);
        let codec_30 = create_protocol_codec(30);
        assert!(!codec_29.supports_perishable_modifier());
        assert!(codec_30.supports_perishable_modifier());
    }

    #[test]
    fn version_30_encoding_more_efficient() {
        let legacy = create_protocol_codec(29);
        let modern = create_protocol_codec(30);

        // Small file size
        let mut legacy_buf = Vec::new();
        let mut modern_buf = Vec::new();

        legacy.write_file_size(&mut legacy_buf, 100).unwrap();
        modern.write_file_size(&mut modern_buf, 100).unwrap();

        // Legacy always uses 4 bytes
        assert_eq!(legacy_buf.len(), 4);
        // Modern may use fewer bytes
        assert!(modern_buf.len() <= legacy_buf.len());
    }
}

// ============================================================================
// Module: Protocol Version 31 (Safe File List & Checksum Seed Fix)
// ============================================================================

mod protocol_31_safe_flist {
    use super::*;

    // ------------------------------------------------------------------------
    // Capability Negotiation Tests
    // ------------------------------------------------------------------------

    #[test]
    fn version_31_negotiation_succeeds() {
        let result = select_highest_mutual([TestVersion(31)]);
        assert!(result.is_ok(), "Protocol 31 negotiation must succeed");
        assert_eq!(result.unwrap().as_u8(), 31);
    }

    #[test]
    fn version_31_is_in_supported_list() {
        assert!(
            ProtocolVersion::is_supported_protocol_number(31),
            "Protocol 31 must be in supported list"
        );
    }

    #[test]
    fn version_31_constant_equals_from_supported() {
        let from_supported = ProtocolVersion::from_supported(31).unwrap();
        assert_eq!(from_supported, ProtocolVersion::V31);
    }

    // ------------------------------------------------------------------------
    // Wire Format Tests
    // ------------------------------------------------------------------------

    #[test]
    fn version_31_uses_binary_negotiation() {
        assert!(
            ProtocolVersion::V31.uses_binary_negotiation(),
            "Protocol 31 must use binary negotiation"
        );
    }

    #[test]
    fn version_31_binary_advertisement_format() {
        let protocol = ProtocolVersion::V31;
        let bytes = u32::from(protocol.as_u8()).to_be_bytes();
        assert_eq!(
            bytes,
            [0, 0, 0, 31],
            "Protocol 31 advertises as 4-byte big-endian"
        );
    }

    #[test]
    fn version_31_uses_varint_encoding() {
        assert!(
            ProtocolVersion::V31.uses_varint_encoding(),
            "Protocol 31 must use varint encoding"
        );
    }

    #[test]
    fn version_31_codec_is_modern() {
        let codec = create_protocol_codec(31);
        assert!(!codec.is_legacy(), "Protocol 31 codec must be modern");
        assert_eq!(codec.protocol_version(), 31);
    }

    // ------------------------------------------------------------------------
    // Feature Availability Tests (Checksum Seeds, Safe File List)
    // ------------------------------------------------------------------------

    #[test]
    fn version_31_safe_file_list_always_enabled() {
        assert!(
            ProtocolVersion::V31.safe_file_list_always_enabled(),
            "Protocol 31 always enables safe file list"
        );
    }

    #[test]
    fn version_31_checksum_seed_fix_flag() {
        let flag = CompatibilityFlags::CHECKSUM_SEED_FIX;
        assert_eq!(flag.bits(), 1 << 5, "CF_CHKSUM_SEED_FIX is bit 5");

        let mut buf = Vec::new();
        flag.encode_to_vec(&mut buf).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, flag);
    }

    #[test]
    fn version_31_avoid_xattr_optim_flag() {
        let flag = CompatibilityFlags::AVOID_XATTR_OPTIMIZATION;
        assert_eq!(flag.bits(), 1 << 4, "CF_AVOID_XATTR_OPTIM is bit 4");

        let mut buf = Vec::new();
        flag.encode_to_vec(&mut buf).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, flag);
    }

    #[test]
    fn version_31_varint_flist_flags_flag() {
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
    fn version_31_id0_names_flag() {
        let flag = CompatibilityFlags::ID0_NAMES;
        assert_eq!(flag.bits(), 1 << 8, "CF_ID0_NAMES is bit 8");

        let mut buf = Vec::new();
        flag.encode_to_vec(&mut buf).unwrap();

        let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
        assert_eq!(decoded, flag);
    }

    #[test]
    fn version_31_all_compatibility_flags() {
        // Protocol 31 supports all defined compatibility flags
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

    // ------------------------------------------------------------------------
    // Backward Compatibility Tests
    // ------------------------------------------------------------------------

    #[test]
    fn version_31_boundary_with_30() {
        let protocol_30 = ProtocolVersion::V30;
        let protocol_31 = ProtocolVersion::V31;

        // Safe file list changes
        assert!(protocol_30.uses_safe_file_list());
        assert!(!protocol_30.safe_file_list_always_enabled());
        assert!(protocol_31.uses_safe_file_list());
        assert!(protocol_31.safe_file_list_always_enabled());

        // Both use binary negotiation
        assert!(protocol_30.uses_binary_negotiation());
        assert!(protocol_31.uses_binary_negotiation());

        // Both use varint encoding
        assert!(protocol_30.uses_varint_encoding());
        assert!(protocol_31.uses_varint_encoding());
    }

    #[test]
    fn version_31_with_30_selects_31() {
        let result = select_highest_mutual([TestVersion(31), TestVersion(30)]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_u8(), 31, "Should select highest (31)");
    }
}

// ============================================================================
// Cross-Version Compatibility Matrix Tests
// ============================================================================

mod cross_version_compatibility {
    use super::*;

    /// Test that all supported versions can negotiate with each other.
    #[test]
    fn all_versions_mutual_negotiation() {
        let supported: Vec<u8> = SUPPORTED_PROTOCOLS.to_vec();

        for &version in &supported {
            let result = select_highest_mutual([TestVersion(u32::from(version))]);
            assert!(
                result.is_ok(),
                "Protocol {version} must negotiate successfully"
            );
            assert_eq!(result.unwrap().as_u8(), version);
        }
    }

    /// Test negotiation across all supported version pairs.
    #[test]
    fn pairwise_negotiation_matrix() {
        let supported: Vec<u8> = SUPPORTED_PROTOCOLS.to_vec();

        for &version_a in &supported {
            for &version_b in &supported {
                let result = select_highest_mutual([
                    TestVersion(u32::from(version_a)),
                    TestVersion(u32::from(version_b)),
                ]);

                assert!(
                    result.is_ok(),
                    "Negotiation between {version_a} and {version_b} must succeed"
                );

                let expected = std::cmp::max(version_a, version_b);
                assert_eq!(
                    result.unwrap().as_u8(),
                    expected,
                    "Should select highest version"
                );
            }
        }
    }

    /// Test wire format changes across version boundaries.
    #[test]
    fn wire_format_boundary_at_30() {
        // Protocol 29 and below use legacy format
        for version in [28, 29] {
            let codec = create_protocol_codec(version);
            assert!(codec.is_legacy(), "Protocol {version} should be legacy");

            let protocol = ProtocolVersion::from_supported(version).unwrap();
            assert!(protocol.uses_legacy_ascii_negotiation());
            assert!(protocol.uses_fixed_encoding());
        }

        // Protocol 30 and above use modern format
        for version in [30, 31, 32] {
            let codec = create_protocol_codec(version);
            assert!(!codec.is_legacy(), "Protocol {version} should be modern");

            let protocol = ProtocolVersion::from_supported(version).unwrap();
            assert!(protocol.uses_binary_negotiation());
            assert!(protocol.uses_varint_encoding());
        }
    }

    /// Test feature progressive enablement across versions.
    #[test]
    fn feature_progressive_enablement() {
        // Feature matrix for each version
        struct VersionFeatures {
            version: u8,
            has_sender_receiver_modifiers: bool,
            has_perishable_modifier: bool,
            has_flist_times: bool,
            uses_old_prefixes: bool,
            uses_safe_flist: bool,
            safe_flist_always: bool,
        }

        let feature_matrix = [
            VersionFeatures {
                version: 28,
                has_sender_receiver_modifiers: false,
                has_perishable_modifier: false,
                has_flist_times: false,
                uses_old_prefixes: true,
                uses_safe_flist: false,
                safe_flist_always: false,
            },
            VersionFeatures {
                version: 29,
                has_sender_receiver_modifiers: true,
                has_perishable_modifier: false,
                has_flist_times: true,
                uses_old_prefixes: false,
                uses_safe_flist: false,
                safe_flist_always: false,
            },
            VersionFeatures {
                version: 30,
                has_sender_receiver_modifiers: true,
                has_perishable_modifier: true,
                has_flist_times: true,
                uses_old_prefixes: false,
                uses_safe_flist: true,
                safe_flist_always: false,
            },
            VersionFeatures {
                version: 31,
                has_sender_receiver_modifiers: true,
                has_perishable_modifier: true,
                has_flist_times: true,
                uses_old_prefixes: false,
                uses_safe_flist: true,
                safe_flist_always: true,
            },
        ];

        for features in &feature_matrix {
            let codec = create_protocol_codec(features.version);
            let protocol = ProtocolVersion::from_supported(features.version).unwrap();

            assert_eq!(
                codec.supports_sender_receiver_modifiers(),
                features.has_sender_receiver_modifiers,
                "v{} sender_receiver mismatch",
                features.version
            );
            assert_eq!(
                codec.supports_perishable_modifier(),
                features.has_perishable_modifier,
                "v{} perishable mismatch",
                features.version
            );
            assert_eq!(
                codec.supports_flist_times(),
                features.has_flist_times,
                "v{} flist_times mismatch",
                features.version
            );
            assert_eq!(
                codec.uses_old_prefixes(),
                features.uses_old_prefixes,
                "v{} old_prefixes mismatch",
                features.version
            );
            assert_eq!(
                protocol.uses_safe_file_list(),
                features.uses_safe_flist,
                "v{} safe_flist mismatch",
                features.version
            );
            assert_eq!(
                protocol.safe_file_list_always_enabled(),
                features.safe_flist_always,
                "v{} safe_flist_always mismatch",
                features.version
            );
        }
    }

    /// Test encoding roundtrip across all versions.
    #[test]
    fn encoding_roundtrip_all_versions() {
        let test_file_sizes = [0i64, 1, 100, 1000, 65535, 0x7FFF_FFFF, 0x1_0000_0000];

        for version in [28, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);

            for &size in &test_file_sizes {
                let mut buf = Vec::new();
                codec.write_file_size(&mut buf, size).unwrap();

                let mut cursor = Cursor::new(&buf);
                let read = codec.read_file_size(&mut cursor).unwrap();

                assert_eq!(read, size, "v{version} file_size roundtrip failed for {size}");
            }
        }
    }

    /// Test NDX encoding roundtrip across all versions.
    #[test]
    fn ndx_roundtrip_all_versions() {
        use protocol::codec::NDX_DONE;

        let test_indices = [0, 1, 5, 100, 1000, 10000, NDX_DONE];

        for version in [28, 29, 30, 31, 32] {
            let mut write_codec = create_ndx_codec(version);
            let mut buf = Vec::new();

            for &ndx in &test_indices {
                write_codec.write_ndx(&mut buf, ndx).unwrap();
            }

            let mut read_codec = create_ndx_codec(version);
            let mut cursor = Cursor::new(&buf);

            for &expected in &test_indices {
                let read = read_codec.read_ndx(&mut cursor).unwrap();
                assert_eq!(read, expected, "v{version} ndx roundtrip failed for {expected}");
            }
        }
    }

    /// Features never disable in newer versions.
    #[test]
    fn features_monotonic_enablement() {
        let mut prev_sender_receiver = false;
        let mut prev_perishable = false;
        let mut prev_flist_times = false;
        let mut prev_safe_flist = false;

        for version in [28, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);
            let protocol = ProtocolVersion::from_supported(version).unwrap();

            // Once enabled, features stay enabled
            if prev_sender_receiver {
                assert!(
                    codec.supports_sender_receiver_modifiers(),
                    "sender_receiver must stay enabled at v{version}"
                );
            }
            if prev_perishable {
                assert!(
                    codec.supports_perishable_modifier(),
                    "perishable must stay enabled at v{version}"
                );
            }
            if prev_flist_times {
                assert!(
                    codec.supports_flist_times(),
                    "flist_times must stay enabled at v{version}"
                );
            }
            if prev_safe_flist {
                assert!(
                    protocol.uses_safe_file_list(),
                    "safe_flist must stay enabled at v{version}"
                );
            }

            // Update state
            prev_sender_receiver = codec.supports_sender_receiver_modifiers();
            prev_perishable = codec.supports_perishable_modifier();
            prev_flist_times = codec.supports_flist_times();
            prev_safe_flist = protocol.uses_safe_file_list();
        }
    }
}

// ============================================================================
// Version-Specific Wire Format Verification Tests
// ============================================================================

mod wire_format_verification {
    use super::*;

    /// Verify legacy wire format byte patterns.
    #[test]
    fn legacy_wire_format_patterns() {
        let codec = create_protocol_codec(29);

        // Zero
        let mut buf = Vec::new();
        codec.write_file_size(&mut buf, 0).unwrap();
        assert_eq!(buf, [0x00, 0x00, 0x00, 0x00]);

        // 255
        buf.clear();
        codec.write_file_size(&mut buf, 255).unwrap();
        assert_eq!(buf, [0xff, 0x00, 0x00, 0x00]);

        // 1000 (0x3E8)
        buf.clear();
        codec.write_file_size(&mut buf, 1000).unwrap();
        assert_eq!(buf, [0xe8, 0x03, 0x00, 0x00]);
    }

    /// Verify modern varint encoding.
    #[test]
    fn modern_varint_encoding() {
        let codec = create_protocol_codec(30);

        // Small values should be compact
        let mut buf = Vec::new();
        codec.write_file_size(&mut buf, 100).unwrap();
        assert!(buf.len() <= 4);

        // Roundtrip
        let mut cursor = Cursor::new(&buf);
        let read = codec.read_file_size(&mut cursor).unwrap();
        assert_eq!(read, 100);
    }

    /// Verify legacy NDX wire format.
    #[test]
    fn legacy_ndx_wire_format() {
        let mut codec = create_ndx_codec(29);

        // Positive index
        let mut buf = Vec::new();
        codec.write_ndx(&mut buf, 0x12345678).unwrap();
        assert_eq!(buf, [0x78, 0x56, 0x34, 0x12]); // Little-endian

        // NDX_DONE (-1)
        buf.clear();
        codec.write_ndx(&mut buf, -1).unwrap();
        assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0xFF]);
    }

    /// Verify modern NDX delta encoding.
    #[test]
    fn modern_ndx_delta_encoding() {
        let mut codec = create_ndx_codec(30);
        let mut buf = Vec::new();

        // First positive (diff=1 from prev=-1)
        codec.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf, [0x01]);

        // Sequential increments
        buf.clear();
        codec.write_ndx(&mut buf, 1).unwrap();
        assert_eq!(buf, [0x01]);

        buf.clear();
        codec.write_ndx(&mut buf, 2).unwrap();
        assert_eq!(buf, [0x01]);
    }

    /// Verify compatibility flag wire format.
    #[test]
    fn compatibility_flags_wire_format() {
        // Single flags
        let test_cases = [
            (CompatibilityFlags::INC_RECURSE, vec![1u8]),
            (CompatibilityFlags::SYMLINK_TIMES, vec![2]),
            (CompatibilityFlags::SYMLINK_ICONV, vec![4]),
            (CompatibilityFlags::SAFE_FILE_LIST, vec![8]),
            (CompatibilityFlags::AVOID_XATTR_OPTIMIZATION, vec![16]),
            (CompatibilityFlags::CHECKSUM_SEED_FIX, vec![32]),
            (CompatibilityFlags::INPLACE_PARTIAL_DIR, vec![64]),
            (CompatibilityFlags::VARINT_FLIST_FLAGS, vec![128, 128]),
            (CompatibilityFlags::ID0_NAMES, vec![129, 0]),
        ];

        for (flag, expected) in test_cases {
            let mut buf = Vec::new();
            flag.encode_to_vec(&mut buf).unwrap();
            assert_eq!(buf, expected, "Flag {:?} wire format mismatch", flag);
        }
    }
}

// ============================================================================
// Error Handling and Edge Cases
// ============================================================================

mod error_handling {
    use super::*;

    #[test]
    fn negotiation_empty_list_fails() {
        let result = select_highest_mutual::<Vec<TestVersion>, _>(vec![]);
        assert!(result.is_err(), "Empty version list must fail");
    }

    #[test]
    fn negotiation_all_unsupported_fails() {
        let result = select_highest_mutual([TestVersion(20), TestVersion(25)]);
        assert!(result.is_err(), "All unsupported versions must fail");
    }

    #[test]
    fn version_zero_rejected() {
        let result = select_highest_mutual([TestVersion(0)]);
        assert!(result.is_err(), "Protocol 0 must be rejected");
    }

    #[test]
    fn version_above_max_advertisement_rejected() {
        let result = select_highest_mutual([TestVersion(100)]);
        assert!(result.is_err(), "Protocol 100 (> MAX 40) must be rejected");
    }

    #[test]
    fn version_between_supported_and_max_clamped() {
        // Versions 33-40 should be clamped to 32
        for version in 33..=40 {
            let result = select_highest_mutual([TestVersion(version)]);
            assert!(result.is_ok(), "Protocol {version} should clamp to 32");
            assert_eq!(result.unwrap().as_u8(), 32);
        }
    }

    #[test]
    fn codec_read_handles_truncated_input() {
        let legacy = create_protocol_codec(29);
        let modern = create_protocol_codec(30);

        // Legacy needs at least 4 bytes
        let truncated = [0u8, 0, 0];
        let mut cursor = Cursor::new(&truncated[..]);
        assert!(legacy.read_file_size(&mut cursor).is_err());

        // Modern needs at least min_bytes=3
        let truncated = [0u8, 0];
        let mut cursor = Cursor::new(&truncated[..]);
        assert!(modern.read_file_size(&mut cursor).is_err());
    }
}

// ============================================================================
// Protocol Version Iteration and Bounds Tests
// ============================================================================

mod version_iteration {
    use super::*;

    #[test]
    fn supported_versions_count() {
        assert_eq!(
            SUPPORTED_PROTOCOLS.len(),
            5,
            "Should support exactly 5 protocols (28-32)"
        );
    }

    #[test]
    fn supported_versions_in_descending_order() {
        let protocols = ProtocolVersion::supported_protocol_numbers();
        for window in protocols.windows(2) {
            assert!(
                window[0] > window[1],
                "Protocols should be in descending order"
            );
        }
    }

    #[test]
    fn version_navigation() {
        assert_eq!(ProtocolVersion::V28.next_newer(), Some(ProtocolVersion::V29));
        assert_eq!(ProtocolVersion::V29.next_newer(), Some(ProtocolVersion::V30));
        assert_eq!(ProtocolVersion::V30.next_newer(), Some(ProtocolVersion::V31));
        assert_eq!(ProtocolVersion::V31.next_newer(), Some(ProtocolVersion::V32));
        assert_eq!(ProtocolVersion::V32.next_newer(), None);

        assert_eq!(ProtocolVersion::V32.next_older(), Some(ProtocolVersion::V31));
        assert_eq!(ProtocolVersion::V31.next_older(), Some(ProtocolVersion::V30));
        assert_eq!(ProtocolVersion::V30.next_older(), Some(ProtocolVersion::V29));
        assert_eq!(ProtocolVersion::V29.next_older(), Some(ProtocolVersion::V28));
        assert_eq!(ProtocolVersion::V28.next_older(), None);
    }

    #[test]
    fn version_offset_calculations() {
        assert_eq!(ProtocolVersion::V28.offset_from_oldest(), 0);
        assert_eq!(ProtocolVersion::V29.offset_from_oldest(), 1);
        assert_eq!(ProtocolVersion::V30.offset_from_oldest(), 2);
        assert_eq!(ProtocolVersion::V31.offset_from_oldest(), 3);
        assert_eq!(ProtocolVersion::V32.offset_from_oldest(), 4);

        assert_eq!(ProtocolVersion::V32.offset_from_newest(), 0);
        assert_eq!(ProtocolVersion::V31.offset_from_newest(), 1);
        assert_eq!(ProtocolVersion::V30.offset_from_newest(), 2);
        assert_eq!(ProtocolVersion::V29.offset_from_newest(), 3);
        assert_eq!(ProtocolVersion::V28.offset_from_newest(), 4);
    }

    #[test]
    fn supported_bitmap_correct() {
        let bitmap = ProtocolVersion::supported_protocol_bitmap();

        for version in [28, 29, 30, 31, 32] {
            let mask = 1u64 << version;
            assert!(
                (bitmap & mask) != 0,
                "Bit for protocol {version} must be set"
            );
        }

        for version in [0, 27, 33] {
            let mask = 1u64 << version;
            assert!(
                (bitmap & mask) == 0,
                "Bit for protocol {version} must not be set"
            );
        }
    }
}
