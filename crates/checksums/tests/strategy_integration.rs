//! Integration tests for the checksum Strategy pattern.
//!
//! These tests verify that the Strategy pattern correctly integrates with
//! protocol version handling and provides consistent, interoperable results.

use checksums::{
    ChecksumAlgorithmKind, ChecksumDigest, ChecksumStrategy, ChecksumStrategySelector, Md4Strategy,
    Md5SeedConfig, Md5Strategy, SeedConfig, Sha256Strategy, Xxh3Strategy,
};

// ============================================================================
// Protocol Version Selection Tests
// ============================================================================

#[test]
fn protocol_28_uses_md4() {
    let strategy = ChecksumStrategySelector::for_protocol_version(28, 0);
    assert_eq!(strategy.algorithm_kind(), ChecksumAlgorithmKind::Md4);
}

#[test]
fn protocol_29_uses_md4() {
    let strategy = ChecksumStrategySelector::for_protocol_version(29, 0);
    assert_eq!(strategy.algorithm_kind(), ChecksumAlgorithmKind::Md4);
}

#[test]
fn protocol_30_uses_md5() {
    let strategy = ChecksumStrategySelector::for_protocol_version(30, 0);
    assert_eq!(strategy.algorithm_kind(), ChecksumAlgorithmKind::Md5);
}

#[test]
fn protocol_31_uses_md5() {
    let strategy = ChecksumStrategySelector::for_protocol_version(31, 0);
    assert_eq!(strategy.algorithm_kind(), ChecksumAlgorithmKind::Md5);
}

#[test]
fn protocol_32_uses_md5() {
    let strategy = ChecksumStrategySelector::for_protocol_version(32, 0);
    assert_eq!(strategy.algorithm_kind(), ChecksumAlgorithmKind::Md5);
}

// ============================================================================
// Seed Ordering Tests (CHECKSUM_SEED_FIX compatibility)
// ============================================================================

#[test]
fn md5_proper_seed_order_matches_manual() {
    let seed_value: i32 = 0x12345678;
    let data = b"test data for seeded hashing";

    // Using the strategy
    let strategy =
        ChecksumStrategySelector::for_protocol_version_with_seed_order(30, seed_value, true);
    let strategy_digest = strategy.compute(data);

    // Manual construction
    use checksums::strong::{Md5, Md5Seed, StrongDigest};
    let manual_digest = Md5::digest_with_seed(Md5Seed::proper(seed_value), data);

    assert_eq!(strategy_digest.as_bytes(), manual_digest.as_ref());
}

#[test]
fn md5_legacy_seed_order_matches_manual() {
    let seed_value: i32 = 0x12345678;
    let data = b"test data for seeded hashing";

    // Using the strategy
    let strategy =
        ChecksumStrategySelector::for_protocol_version_with_seed_order(30, seed_value, false);
    let strategy_digest = strategy.compute(data);

    // Manual construction
    use checksums::strong::{Md5, Md5Seed, StrongDigest};
    let manual_digest = Md5::digest_with_seed(Md5Seed::legacy(seed_value), data);

    assert_eq!(strategy_digest.as_bytes(), manual_digest.as_ref());
}

#[test]
fn proper_and_legacy_seed_orders_differ() {
    let seed = 12345;
    let data = b"same input data";

    let proper = ChecksumStrategySelector::for_protocol_version_with_seed_order(30, seed, true);
    let legacy = ChecksumStrategySelector::for_protocol_version_with_seed_order(30, seed, false);

    assert_ne!(proper.compute(data), legacy.compute(data));
}

// ============================================================================
// Algorithm Consistency Tests
// ============================================================================

#[test]
fn all_algorithms_produce_expected_digest_lengths() {
    let test_data = b"consistent length verification";

    for kind in ChecksumAlgorithmKind::all() {
        let strategy = ChecksumStrategySelector::for_algorithm(*kind, 0);
        let digest = strategy.compute(test_data);

        assert_eq!(
            digest.len(),
            kind.digest_len(),
            "Digest length mismatch for {kind}"
        );
        assert_eq!(
            digest.len(),
            strategy.digest_len(),
            "Strategy digest_len() mismatch for {kind}"
        );
    }
}

#[test]
fn same_input_produces_same_digest() {
    let data = b"deterministic hashing";

    for kind in ChecksumAlgorithmKind::all() {
        let strategy = ChecksumStrategySelector::for_algorithm(*kind, 42);
        let d1 = strategy.compute(data);
        let d2 = strategy.compute(data);

        assert_eq!(d1, d2, "Non-deterministic result for {kind}");
    }
}

#[test]
fn different_inputs_produce_different_digests() {
    let data1 = b"input one";
    let data2 = b"input two";

    for kind in ChecksumAlgorithmKind::all() {
        let strategy = ChecksumStrategySelector::for_algorithm(*kind, 0);
        let d1 = strategy.compute(data1);
        let d2 = strategy.compute(data2);

        assert_ne!(d1, d2, "Collision for different inputs with {kind}");
    }
}

#[test]
fn different_seeds_produce_different_digests_for_seeded_algorithms() {
    let data = b"seeded algorithm test";
    let seed1 = 0;
    let seed2 = 12345;

    // XXH64
    let xxh64_s1 = ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Xxh64, seed1);
    let xxh64_s2 = ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Xxh64, seed2);
    assert_ne!(xxh64_s1.compute(data), xxh64_s2.compute(data));

    // XXH3
    let xxh3_s1 = ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Xxh3, seed1);
    let xxh3_s2 = ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Xxh3, seed2);
    assert_ne!(xxh3_s1.compute(data), xxh3_s2.compute(data));

    // XXH3-128
    let xxh128_s1 = ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Xxh3_128, seed1);
    let xxh128_s2 = ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Xxh3_128, seed2);
    assert_ne!(xxh128_s1.compute(data), xxh128_s2.compute(data));

    // MD5 with proper seed
    let md5_s1 = ChecksumStrategySelector::with_seed_config(
        ChecksumAlgorithmKind::Md5,
        SeedConfig::Md5(Md5SeedConfig::Proper(seed1)),
    );
    let md5_s2 = ChecksumStrategySelector::with_seed_config(
        ChecksumAlgorithmKind::Md5,
        SeedConfig::Md5(Md5SeedConfig::Proper(seed2)),
    );
    assert_ne!(md5_s1.compute(data), md5_s2.compute(data));
}

// ============================================================================
// Digest Manipulation Tests
// ============================================================================

#[test]
fn digest_truncation_works() {
    let strategy = Sha256Strategy::new();
    let digest = strategy.compute(b"truncation test");

    let truncated = digest.truncated(8);
    assert_eq!(truncated.len(), 8);
    assert_eq!(truncated.as_bytes(), &digest.as_bytes()[..8]);
}

#[test]
fn digest_copy_to_buffer() {
    let strategy = Md4Strategy::new();
    let digest = strategy.compute(b"copy test");

    let mut buffer = [0u8; 32];
    digest.copy_to(&mut buffer);

    assert_eq!(&buffer[..16], digest.as_bytes());
}

#[test]
fn digest_display_formatting() {
    let digest = ChecksumDigest::new(&[0xde, 0xad, 0xbe, 0xef]);
    let formatted = format!("{digest}");
    assert_eq!(formatted, "deadbeef");
}

// ============================================================================
// Trait Object / Dynamic Dispatch Tests
// ============================================================================

#[test]
fn boxed_strategies_work_polymorphically() {
    let strategies: Vec<Box<dyn ChecksumStrategy>> = vec![
        Box::new(Md4Strategy::new()),
        Box::new(Md5Strategy::new()),
        Box::new(Sha256Strategy::new()),
        Box::new(Xxh3Strategy::new(42)),
    ];

    let data = b"polymorphic test";

    for strategy in &strategies {
        let digest = strategy.compute(data);
        assert_eq!(digest.len(), strategy.digest_len());
    }
}

#[test]
fn strategy_can_be_stored_in_struct() {
    struct ChecksumContext {
        strategy: Box<dyn ChecksumStrategy>,
    }

    impl ChecksumContext {
        fn compute(&self, data: &[u8]) -> ChecksumDigest {
            self.strategy.compute(data)
        }
    }

    let context = ChecksumContext {
        strategy: ChecksumStrategySelector::for_protocol_version(31, 12345),
    };

    let digest = context.compute(b"struct-stored strategy");
    assert_eq!(digest.len(), 16); // MD5 produces 16-byte digest
}

#[test]
fn strategies_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<Box<dyn ChecksumStrategy>>();
    assert_send_sync::<Md4Strategy>();
    assert_send_sync::<Md5Strategy>();
    assert_send_sync::<Sha256Strategy>();
    assert_send_sync::<Xxh3Strategy>();
}

// ============================================================================
// Edge Case Tests
// ============================================================================

#[test]
fn empty_input_produces_valid_digest() {
    let empty = b"";

    for kind in ChecksumAlgorithmKind::all() {
        let strategy = ChecksumStrategySelector::for_algorithm(*kind, 0);
        let digest = strategy.compute(empty);

        assert_eq!(
            digest.len(),
            kind.digest_len(),
            "Empty input failed for {kind}"
        );
    }
}

#[test]
fn large_input_produces_valid_digest() {
    let large_data: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();

    for kind in ChecksumAlgorithmKind::all() {
        let strategy = ChecksumStrategySelector::for_algorithm(*kind, 0);
        let digest = strategy.compute(&large_data);

        assert_eq!(
            digest.len(),
            kind.digest_len(),
            "Large input failed for {kind}"
        );
    }
}

#[test]
fn negative_seed_handled_correctly() {
    let data = b"negative seed test";
    let negative_seed: i32 = -1;

    // Should not panic and should produce valid output
    let strategy =
        ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Xxh64, negative_seed);
    let digest = strategy.compute(data);
    assert_eq!(digest.len(), 8);

    // Verify it differs from seed 0
    let zero_seed_strategy =
        ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Xxh64, 0);
    assert_ne!(digest, zero_seed_strategy.compute(data));
}

// ============================================================================
// Algorithm Kind Tests
// ============================================================================

#[test]
fn algorithm_kind_from_name_case_insensitive() {
    assert_eq!(
        ChecksumAlgorithmKind::from_name("MD5"),
        Some(ChecksumAlgorithmKind::Md5)
    );
    assert_eq!(
        ChecksumAlgorithmKind::from_name("md5"),
        Some(ChecksumAlgorithmKind::Md5)
    );
    assert_eq!(
        ChecksumAlgorithmKind::from_name("Md5"),
        Some(ChecksumAlgorithmKind::Md5)
    );
    assert_eq!(
        ChecksumAlgorithmKind::from_name("SHA-256"),
        Some(ChecksumAlgorithmKind::Sha256)
    );
    assert_eq!(
        ChecksumAlgorithmKind::from_name("sha256"),
        Some(ChecksumAlgorithmKind::Sha256)
    );
}

#[test]
fn algorithm_kind_name_roundtrip() {
    for kind in ChecksumAlgorithmKind::all() {
        let name = kind.name();
        let parsed = ChecksumAlgorithmKind::from_name(name);
        assert_eq!(parsed, Some(*kind), "Roundtrip failed for {kind}");
    }
}

#[test]
fn cryptographic_vs_non_cryptographic() {
    // Cryptographic
    assert!(ChecksumAlgorithmKind::Md4.is_cryptographic());
    assert!(ChecksumAlgorithmKind::Md5.is_cryptographic());
    assert!(ChecksumAlgorithmKind::Sha1.is_cryptographic());
    assert!(ChecksumAlgorithmKind::Sha256.is_cryptographic());
    assert!(ChecksumAlgorithmKind::Sha512.is_cryptographic());

    // Non-cryptographic
    assert!(!ChecksumAlgorithmKind::Xxh64.is_cryptographic());
    assert!(!ChecksumAlgorithmKind::Xxh3.is_cryptographic());
    assert!(!ChecksumAlgorithmKind::Xxh3_128.is_cryptographic());
}

// ============================================================================
// Concrete Factory Tests
// ============================================================================

#[test]
fn concrete_factories_produce_correct_strategies() {
    let md4 = ChecksumStrategySelector::md4();
    assert_eq!(md4.algorithm_kind(), ChecksumAlgorithmKind::Md4);

    let md5 = ChecksumStrategySelector::md5();
    assert_eq!(md5.algorithm_kind(), ChecksumAlgorithmKind::Md5);

    let sha1 = ChecksumStrategySelector::sha1();
    assert_eq!(sha1.algorithm_kind(), ChecksumAlgorithmKind::Sha1);

    let sha256 = ChecksumStrategySelector::sha256();
    assert_eq!(sha256.algorithm_kind(), ChecksumAlgorithmKind::Sha256);

    let sha512 = ChecksumStrategySelector::sha512();
    assert_eq!(sha512.algorithm_kind(), ChecksumAlgorithmKind::Sha512);

    let xxh64 = ChecksumStrategySelector::xxh64(0);
    assert_eq!(xxh64.algorithm_kind(), ChecksumAlgorithmKind::Xxh64);

    let xxh3 = ChecksumStrategySelector::xxh3(0);
    assert_eq!(xxh3.algorithm_kind(), ChecksumAlgorithmKind::Xxh3);

    let xxh3_128 = ChecksumStrategySelector::xxh3_128(0);
    assert_eq!(xxh3_128.algorithm_kind(), ChecksumAlgorithmKind::Xxh3_128);
}

// ============================================================================
// Interoperability Tests (verify against known values)
// ============================================================================

#[test]
fn md5_matches_rfc_test_vector() {
    // RFC 1321 test vector: MD5("") = d41d8cd98f00b204e9800998ecf8427e
    let strategy = Md5Strategy::new();
    let digest = strategy.compute(b"");

    let expected = [
        0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8, 0x42,
        0x7e,
    ];

    assert_eq!(digest.as_bytes(), &expected);
}

#[test]
fn md4_matches_rfc_test_vector() {
    // RFC 1320 test vector: MD4("") = 31d6cfe0d16ae931b73c59d7e0c089c0
    let strategy = Md4Strategy::new();
    let digest = strategy.compute(b"");

    let expected = [
        0x31, 0xd6, 0xcf, 0xe0, 0xd1, 0x6a, 0xe9, 0x31, 0xb7, 0x3c, 0x59, 0xd7, 0xe0, 0xc0, 0x89,
        0xc0,
    ];

    assert_eq!(digest.as_bytes(), &expected);
}

#[test]
fn sha256_matches_nist_test_vector() {
    // NIST test vector: SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
    let strategy = Sha256Strategy::new();
    let digest = strategy.compute(b"abc");

    let expected = [
        0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22,
        0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00,
        0x15, 0xad,
    ];

    assert_eq!(digest.as_bytes(), &expected);
}
