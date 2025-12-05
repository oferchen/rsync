//! Protocol feature version gate tests.
//!
//! Validates that protocol features are correctly enabled/disabled based on
//! the negotiated protocol version, matching upstream rsync behavior.

use protocol::{
    select_highest_mutual, CompatibilityFlags, ProtocolVersionAdvertisement,
    SUPPORTED_PROTOCOLS,
};

/// Helper wrapper to implement ProtocolVersionAdvertisement for testing.
#[derive(Clone, Copy, Debug)]
struct TestVersion(u32);

impl ProtocolVersionAdvertisement for TestVersion {
    #[inline]
    fn into_advertised_version(self) -> u32 {
        self.0
    }
}

#[test]
fn test_binary_negotiation_gate_protocol_30() {
    // Binary negotiation was introduced in protocol 30
    // Protocols 28-29 use ASCII negotiation
    // Protocols 30-32 use binary negotiation

    for &version in &SUPPORTED_PROTOCOLS {
        let client_advertises = [TestVersion(u32::from(version))];
        let result = select_highest_mutual(client_advertises);
        assert!(result.is_ok());

        let protocol = result.unwrap();

        if version < 30 {
            assert!(
                protocol.uses_legacy_ascii_negotiation(),
                "Protocol {version} should use ASCII negotiation"
            );
            assert!(
                !protocol.uses_binary_negotiation(),
                "Protocol {version} should not use binary negotiation"
            );
        } else {
            assert!(
                !protocol.uses_legacy_ascii_negotiation(),
                "Protocol {version} should not use ASCII negotiation"
            );
            assert!(
                protocol.uses_binary_negotiation(),
                "Protocol {version} should use binary negotiation"
            );
        }
    }
}

#[test]
fn test_compatibility_flags_gate_protocol_30() {
    // Compatibility flags were introduced in protocol 30
    // They allow fine-grained feature negotiation

    // Protocol 29 and below don't use compat flags in the binary protocol
    let client_29 = [TestVersion(29)];
    let result_29 = select_highest_mutual(client_29);
    assert!(result_29.is_ok());
    let protocol_29 = result_29.unwrap();
    assert_eq!(protocol_29.as_u8(), 29);

    // Protocol 30 and above support compat flags
    let client_30 = [TestVersion(30)];
    let result_30 = select_highest_mutual(client_30);
    assert!(result_30.is_ok());
    let protocol_30 = result_30.unwrap();
    assert_eq!(protocol_30.as_u8(), 30);

    // Both should succeed, but protocol 30+ can use compatibility flags
    // The actual usage is determined by the compatibility flags encoding/decoding
}

#[test]
fn test_incremental_recursion_flag() {
    // CF_INC_RECURSE (bit 0) - Incremental recursion
    // Available in protocol 30+
    let flag = CompatibilityFlags::INC_RECURSE;

    // Flag value should be consistent
    assert_eq!(flag.bits(), 1 << 0, "CF_INC_RECURSE should be bit 0");

    // Can encode and decode
    let mut buf = Vec::new();
    flag.encode_to_vec(&mut buf).expect("encode should succeed");

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf)
        .expect("decode should succeed");
    assert_eq!(decoded, flag);
}

#[test]
fn test_symlink_times_flag() {
    // CF_SYMLINK_TIMES (bit 1) - Preserve symlink modification times
    // Available in protocol 30+
    let flag = CompatibilityFlags::SYMLINK_TIMES;

    assert_eq!(flag.bits(), 1 << 1, "CF_SYMLINK_TIMES should be bit 1");

    let mut buf = Vec::new();
    flag.encode_to_vec(&mut buf).expect("encode should succeed");

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf)
        .expect("decode should succeed");
    assert_eq!(decoded, flag);
}

#[test]
fn test_symlink_iconv_flag() {
    // CF_SYMLINK_ICONV (bit 2) - Character set conversion for symlink targets
    // Available in protocol 30+
    let flag = CompatibilityFlags::SYMLINK_ICONV;

    assert_eq!(flag.bits(), 1 << 2, "CF_SYMLINK_ICONV should be bit 2");

    let mut buf = Vec::new();
    flag.encode_to_vec(&mut buf).expect("encode should succeed");

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf)
        .expect("decode should succeed");
    assert_eq!(decoded, flag);
}

#[test]
fn test_safe_flist_flag() {
    // CF_SAFE_FLIST (bit 3) - Safe file list handling
    // Available in protocol 30+
    let flag = CompatibilityFlags::SAFE_FILE_LIST;

    assert_eq!(flag.bits(), 1 << 3, "CF_SAFE_FLIST should be bit 3");

    let mut buf = Vec::new();
    flag.encode_to_vec(&mut buf).expect("encode should succeed");

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf)
        .expect("decode should succeed");
    assert_eq!(decoded, flag);
}

#[test]
fn test_avoid_xattr_optim_flag() {
    // CF_AVOID_XATTR_OPTIM (bit 4) - Avoid xattr optimization
    // Available in protocol 31+
    let flag = CompatibilityFlags::AVOID_XATTR_OPTIMIZATION;

    assert_eq!(flag.bits(), 1 << 4, "CF_AVOID_XATTR_OPTIM should be bit 4");

    let mut buf = Vec::new();
    flag.encode_to_vec(&mut buf).expect("encode should succeed");

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf)
        .expect("decode should succeed");
    assert_eq!(decoded, flag);
}

#[test]
fn test_chksum_seed_fix_flag() {
    // CF_CHKSUM_SEED_FIX (bit 5) - Checksum seed fix
    // Available in protocol 31+
    let flag = CompatibilityFlags::CHECKSUM_SEED_FIX;

    assert_eq!(flag.bits(), 1 << 5, "CF_CHKSUM_SEED_FIX should be bit 5");

    let mut buf = Vec::new();
    flag.encode_to_vec(&mut buf).expect("encode should succeed");

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf)
        .expect("decode should succeed");
    assert_eq!(decoded, flag);
}

#[test]
fn test_inplace_partial_dir_flag() {
    // CF_INPLACE_PARTIAL_DIR (bit 6) - In-place with partial directory
    // Available in protocol 30+
    let flag = CompatibilityFlags::INPLACE_PARTIAL_DIR;

    assert_eq!(flag.bits(), 1 << 6, "CF_INPLACE_PARTIAL_DIR should be bit 6");

    let mut buf = Vec::new();
    flag.encode_to_vec(&mut buf).expect("encode should succeed");

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf)
        .expect("decode should succeed");
    assert_eq!(decoded, flag);
}

#[test]
fn test_varint_flist_flags_flag() {
    // CF_VARINT_FLIST_FLAGS (bit 7) - File-list flags encoded as varints
    // Available in protocol 31+
    let flag = CompatibilityFlags::VARINT_FLIST_FLAGS;

    assert_eq!(flag.bits(), 1 << 7, "CF_VARINT_FLIST_FLAGS should be bit 7");

    let mut buf = Vec::new();
    flag.encode_to_vec(&mut buf).expect("encode should succeed");

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf)
        .expect("decode should succeed");
    assert_eq!(decoded, flag);
}

#[test]
fn test_id0_names_flag() {
    // CF_ID0_NAMES (bit 8) - ID 0 name handling
    // Available in protocol 31+
    let flag = CompatibilityFlags::ID0_NAMES;

    assert_eq!(flag.bits(), 1 << 8, "CF_ID0_NAMES should be bit 8");

    let mut buf = Vec::new();
    flag.encode_to_vec(&mut buf).expect("encode should succeed");

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf)
        .expect("decode should succeed");
    assert_eq!(decoded, flag);
}

#[test]
fn test_all_compatibility_flags_unique() {
    // Verify all known flags have unique bit positions
    let all_flags = [
        CompatibilityFlags::INC_RECURSE,
        CompatibilityFlags::SYMLINK_TIMES,
        CompatibilityFlags::SYMLINK_ICONV,
        CompatibilityFlags::SAFE_FILE_LIST,
        CompatibilityFlags::AVOID_XATTR_OPTIMIZATION,
        CompatibilityFlags::CHECKSUM_SEED_FIX,
        CompatibilityFlags::INPLACE_PARTIAL_DIR,
        CompatibilityFlags::VARINT_FLIST_FLAGS,
        CompatibilityFlags::ID0_NAMES,
    ];

    // Check that all flags have different bit patterns
    for (i, &flag1) in all_flags.iter().enumerate() {
        for (j, &flag2) in all_flags.iter().enumerate() {
            if i != j {
                assert_ne!(
                    flag1.bits(),
                    flag2.bits(),
                    "Flags at indices {i} and {j} must have unique bit patterns"
                );
            }
        }
    }

    // Verify each flag sets exactly one bit
    for &flag in &all_flags {
        assert_eq!(
            flag.bits().count_ones(),
            1,
            "Each flag should set exactly one bit"
        );
    }
}

#[test]
fn test_compatibility_flags_combination() {
    // Multiple flags can be combined using bitwise OR
    let combined = CompatibilityFlags::INC_RECURSE
        | CompatibilityFlags::SYMLINK_TIMES
        | CompatibilityFlags::SAFE_FILE_LIST;

    // Combined flags should have 3 bits set
    assert_eq!(combined.bits().count_ones(), 3);

    // Individual flags should be testable
    assert!(combined.contains(CompatibilityFlags::INC_RECURSE));
    assert!(combined.contains(CompatibilityFlags::SYMLINK_TIMES));
    assert!(combined.contains(CompatibilityFlags::SAFE_FILE_LIST));
    assert!(!combined.contains(CompatibilityFlags::SYMLINK_ICONV));
}

#[test]
fn test_compatibility_flags_empty() {
    // Empty flags (no features enabled)
    let empty = CompatibilityFlags::EMPTY;

    assert_eq!(empty.bits(), 0);
    assert!(!empty.contains(CompatibilityFlags::INC_RECURSE));
    assert!(!empty.contains(CompatibilityFlags::SYMLINK_TIMES));

    // Can encode and decode empty flags
    let mut buf = Vec::new();
    empty.encode_to_vec(&mut buf).expect("encode should succeed");

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf)
        .expect("decode should succeed");
    assert_eq!(decoded, empty);
}

#[test]
fn test_protocol_feature_matrix() {
    // Comprehensive matrix of protocol versions and feature availability
    struct FeatureGate {
        version: u8,
        has_binary_negotiation: bool,
        has_compat_flags: bool,
    }

    let gates = [
        FeatureGate {
            version: 28,
            has_binary_negotiation: false,
            has_compat_flags: false,
        },
        FeatureGate {
            version: 29,
            has_binary_negotiation: false,
            has_compat_flags: false,
        },
        FeatureGate {
            version: 30,
            has_binary_negotiation: true,
            has_compat_flags: true,
        },
        FeatureGate {
            version: 31,
            has_binary_negotiation: true,
            has_compat_flags: true,
        },
        FeatureGate {
            version: 32,
            has_binary_negotiation: true,
            has_compat_flags: true,
        },
    ];

    for gate in &gates {
        let client_advertises = [TestVersion(u32::from(gate.version))];
        let result = select_highest_mutual(client_advertises);
        assert!(result.is_ok());

        let protocol = result.unwrap();
        assert_eq!(protocol.as_u8(), gate.version);

        assert_eq!(
            protocol.uses_binary_negotiation(),
            gate.has_binary_negotiation,
            "Protocol {} binary negotiation mismatch",
            gate.version
        );

        // Compatibility flags are available in protocol 30+
        // (Testing is done via encoding/decoding, not a protocol method)
        if gate.has_compat_flags {
            // Protocol supports compat flags - can encode/decode them
            let test_flag = CompatibilityFlags::INC_RECURSE;
            let mut buf = Vec::new();
            test_flag.encode_to_vec(&mut buf).expect("encode should work");
            let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf)
                .expect("decode should work");
            assert_eq!(decoded, test_flag);
        }
    }
}

#[test]
fn test_feature_gates_are_backward_compatible() {
    // Features introduced in newer protocols should not break older ones
    // This tests that negotiating an older protocol doesn't enable new features

    // Negotiate to protocol 28
    let client_28 = [TestVersion(28)];
    let result_28 = select_highest_mutual(client_28);
    assert!(result_28.is_ok());
    let protocol_28 = result_28.unwrap();

    // Protocol 28 should not have binary negotiation
    assert!(!protocol_28.uses_binary_negotiation());
    assert!(protocol_28.uses_legacy_ascii_negotiation());

    // Negotiate to protocol 30
    let client_30 = [TestVersion(30)];
    let result_30 = select_highest_mutual(client_30);
    assert!(result_30.is_ok());
    let protocol_30 = result_30.unwrap();

    // Protocol 30 should have binary negotiation
    assert!(protocol_30.uses_binary_negotiation());
    assert!(!protocol_30.uses_legacy_ascii_negotiation());

    // Features are version-specific and don't leak across versions
    assert_ne!(
        protocol_28.uses_binary_negotiation(),
        protocol_30.uses_binary_negotiation()
    );
}

#[test]
fn test_feature_gates_are_deterministic() {
    // Feature availability should be deterministic based solely on protocol version
    for &version in &SUPPORTED_PROTOCOLS {
        let client_advertises = [TestVersion(u32::from(version))];

        // Run negotiation 10 times
        for _ in 0..10 {
            let result = select_highest_mutual(client_advertises.clone());
            assert!(result.is_ok());
            let protocol = result.unwrap();

            // Check that features are consistently gated
            let uses_binary = protocol.uses_binary_negotiation();
            let uses_ascii = protocol.uses_legacy_ascii_negotiation();

            // Re-negotiate and verify same results
            let result2 = select_highest_mutual(client_advertises.clone());
            assert!(result2.is_ok());
            let protocol2 = result2.unwrap();

            assert_eq!(protocol2.uses_binary_negotiation(), uses_binary);
            assert_eq!(protocol2.uses_legacy_ascii_negotiation(), uses_ascii);
        }
    }
}
