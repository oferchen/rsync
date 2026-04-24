use super::*;

#[test]
fn digest_from_bytes() {
    let bytes = [1, 2, 3, 4, 5];
    let digest = ChecksumDigest::new(&bytes);
    assert_eq!(digest.len(), 5);
    assert_eq!(digest.as_bytes(), &bytes);
}

#[test]
fn digest_empty() {
    let digest = ChecksumDigest::new(&[]);
    assert!(digest.is_empty());
    assert_eq!(digest.len(), 0);
}

#[test]
fn digest_copy_to() {
    let bytes = [1, 2, 3, 4];
    let digest = ChecksumDigest::new(&bytes);
    let mut out = [0u8; 8];
    digest.copy_to(&mut out);
    assert_eq!(&out[..4], &bytes);
}

#[test]
fn digest_truncated() {
    let bytes = [1, 2, 3, 4, 5, 6, 7, 8];
    let digest = ChecksumDigest::new(&bytes);
    let truncated = digest.truncated(4);
    assert_eq!(truncated.len(), 4);
    assert_eq!(truncated.as_bytes(), &[1, 2, 3, 4]);
}

#[test]
fn digest_truncated_longer_than_original() {
    let bytes = [1, 2, 3, 4];
    let digest = ChecksumDigest::new(&bytes);
    let truncated = digest.truncated(10);
    assert_eq!(truncated.len(), 4);
    assert_eq!(truncated.as_bytes(), &bytes);
}

#[test]
fn digest_equality() {
    let d1 = ChecksumDigest::new(&[1, 2, 3]);
    let d2 = ChecksumDigest::new(&[1, 2, 3]);
    let d3 = ChecksumDigest::new(&[1, 2, 4]);
    assert_eq!(d1, d2);
    assert_ne!(d1, d3);
}

#[test]
fn digest_display() {
    let digest = ChecksumDigest::new(&[0xde, 0xad, 0xbe, 0xef]);
    assert_eq!(format!("{digest}"), "deadbeef");
}

#[test]
fn algorithm_kind_name() {
    assert_eq!(ChecksumAlgorithmKind::Md4.name(), "md4");
    assert_eq!(ChecksumAlgorithmKind::Md5.name(), "md5");
    assert_eq!(ChecksumAlgorithmKind::Sha1.name(), "sha1");
    assert_eq!(ChecksumAlgorithmKind::Sha256.name(), "sha256");
    assert_eq!(ChecksumAlgorithmKind::Sha512.name(), "sha512");
    assert_eq!(ChecksumAlgorithmKind::Xxh64.name(), "xxh64");
    assert_eq!(ChecksumAlgorithmKind::Xxh3.name(), "xxh3");
    assert_eq!(ChecksumAlgorithmKind::Xxh3_128.name(), "xxh128");
}

#[test]
fn algorithm_kind_digest_len() {
    assert_eq!(ChecksumAlgorithmKind::Md4.digest_len(), 16);
    assert_eq!(ChecksumAlgorithmKind::Md5.digest_len(), 16);
    assert_eq!(ChecksumAlgorithmKind::Sha1.digest_len(), 20);
    assert_eq!(ChecksumAlgorithmKind::Sha256.digest_len(), 32);
    assert_eq!(ChecksumAlgorithmKind::Sha512.digest_len(), 64);
    assert_eq!(ChecksumAlgorithmKind::Xxh64.digest_len(), 8);
    assert_eq!(ChecksumAlgorithmKind::Xxh3.digest_len(), 8);
    assert_eq!(ChecksumAlgorithmKind::Xxh3_128.digest_len(), 16);
}

#[test]
fn algorithm_kind_is_cryptographic() {
    assert!(ChecksumAlgorithmKind::Md4.is_cryptographic());
    assert!(ChecksumAlgorithmKind::Md5.is_cryptographic());
    assert!(ChecksumAlgorithmKind::Sha1.is_cryptographic());
    assert!(ChecksumAlgorithmKind::Sha256.is_cryptographic());
    assert!(ChecksumAlgorithmKind::Sha512.is_cryptographic());
    assert!(!ChecksumAlgorithmKind::Xxh64.is_cryptographic());
    assert!(!ChecksumAlgorithmKind::Xxh3.is_cryptographic());
    assert!(!ChecksumAlgorithmKind::Xxh3_128.is_cryptographic());
}

#[test]
fn algorithm_kind_from_name() {
    assert_eq!(
        ChecksumAlgorithmKind::from_name("md4"),
        Some(ChecksumAlgorithmKind::Md4)
    );
    assert_eq!(
        ChecksumAlgorithmKind::from_name("MD5"),
        Some(ChecksumAlgorithmKind::Md5)
    );
    assert_eq!(
        ChecksumAlgorithmKind::from_name("sha-256"),
        Some(ChecksumAlgorithmKind::Sha256)
    );
    assert_eq!(
        ChecksumAlgorithmKind::from_name("xxhash3"),
        Some(ChecksumAlgorithmKind::Xxh3)
    );
    // upstream: checksum.c valid_checksums_items maps "xxhash" to CSUM_XXH64
    assert_eq!(
        ChecksumAlgorithmKind::from_name("xxhash"),
        Some(ChecksumAlgorithmKind::Xxh64)
    );
    assert_eq!(ChecksumAlgorithmKind::from_name("invalid"), None);
}

#[test]
fn algorithm_kind_all() {
    let all = ChecksumAlgorithmKind::all();
    assert_eq!(all.len(), 8);
    assert!(all.contains(&ChecksumAlgorithmKind::Md4));
    assert!(all.contains(&ChecksumAlgorithmKind::Xxh3_128));
}

#[test]
fn md4_strategy_compute() {
    let strategy = Md4Strategy::new();
    let digest = strategy.compute(b"test");
    assert_eq!(digest.len(), 16);
    assert_eq!(strategy.digest_len(), 16);
    assert_eq!(strategy.algorithm_name(), "md4");
}

#[test]
fn md5_strategy_unseeded() {
    let strategy = Md5Strategy::new();
    let digest = strategy.compute(b"test");
    assert_eq!(digest.len(), 16);
    assert_eq!(strategy.algorithm_name(), "md5");
}

#[test]
fn md5_strategy_proper_seed() {
    let strategy = Md5Strategy::with_proper_seed(12345);
    let digest = strategy.compute(b"test");
    assert_eq!(digest.len(), 16);

    let unseeded = Md5Strategy::new();
    assert_ne!(digest, unseeded.compute(b"test"));
}

#[test]
fn md5_strategy_legacy_seed() {
    let strategy = Md5Strategy::with_legacy_seed(12345);
    let digest = strategy.compute(b"test");
    assert_eq!(digest.len(), 16);

    let proper = Md5Strategy::with_proper_seed(12345);
    assert_ne!(digest, proper.compute(b"test"));
}

#[test]
fn sha1_strategy_compute() {
    let strategy = Sha1Strategy::new();
    let digest = strategy.compute(b"test");
    assert_eq!(digest.len(), 20);
    assert_eq!(strategy.algorithm_name(), "sha1");
}

#[test]
fn sha256_strategy_compute() {
    let strategy = Sha256Strategy::new();
    let digest = strategy.compute(b"test");
    assert_eq!(digest.len(), 32);
    assert_eq!(strategy.algorithm_name(), "sha256");
}

#[test]
fn sha512_strategy_compute() {
    let strategy = Sha512Strategy::new();
    let digest = strategy.compute(b"test");
    assert_eq!(digest.len(), 64);
    assert_eq!(strategy.algorithm_name(), "sha512");
}

#[test]
fn xxh64_strategy_compute() {
    let strategy = Xxh64Strategy::new(42);
    let digest = strategy.compute(b"test");
    assert_eq!(digest.len(), 8);
    assert_eq!(strategy.algorithm_name(), "xxh64");
}

#[test]
fn xxh64_strategy_different_seeds() {
    let s1 = Xxh64Strategy::new(0);
    let s2 = Xxh64Strategy::new(1);
    assert_ne!(s1.compute(b"test"), s2.compute(b"test"));
}

#[test]
fn xxh3_strategy_compute() {
    let strategy = Xxh3Strategy::new(42);
    let digest = strategy.compute(b"test");
    assert_eq!(digest.len(), 8);
    assert_eq!(strategy.algorithm_name(), "xxh3");
}

#[test]
fn xxh3_128_strategy_compute() {
    let strategy = Xxh3_128Strategy::new(42);
    let digest = strategy.compute(b"test");
    assert_eq!(digest.len(), 16);
    assert_eq!(strategy.algorithm_name(), "xxh128");
}

#[test]
fn compute_into_works() {
    let strategy = Md5Strategy::new();
    let mut buffer = [0u8; 32];
    strategy.compute_into(b"test", &mut buffer);

    let digest = strategy.compute(b"test");
    assert_eq!(&buffer[..16], digest.as_bytes());
}

#[test]
fn selector_for_protocol_version_28() {
    let strategy = ChecksumStrategySelector::for_protocol_version(28, 0);
    assert_eq!(strategy.algorithm_name(), "md4");
}

#[test]
fn selector_for_protocol_version_29() {
    let strategy = ChecksumStrategySelector::for_protocol_version(29, 0);
    assert_eq!(strategy.algorithm_name(), "md4");
}

#[test]
fn selector_for_protocol_version_30() {
    let strategy = ChecksumStrategySelector::for_protocol_version(30, 12345);
    // Default (no CF_CHKSUM_SEED_FIX) must use legacy ordering to stay
    // wire-compatible with rsync peers that predate the seed-fix flag.
    assert_eq!(strategy.algorithm_name(), "md5");
    let proper = ChecksumStrategySelector::for_protocol_version_with_seed_order(30, 12345, true);
    let legacy = ChecksumStrategySelector::for_protocol_version_with_seed_order(30, 12345, false);
    // The unflagged default must match legacy, not proper.
    assert_eq!(
        strategy.compute(b"test"),
        legacy.compute(b"test"),
        "for_protocol_version must default to legacy seed ordering"
    );
    assert_ne!(
        strategy.compute(b"test"),
        proper.compute(b"test"),
        "legacy and proper must differ"
    );
}

#[test]
fn selector_for_protocol_version_31() {
    let strategy = ChecksumStrategySelector::for_protocol_version(31, 0);
    assert_eq!(strategy.algorithm_name(), "md5");
}

// --- Protocol version boundary tests ---
// upstream: checksum.c - protocol < 30 uses MD4, >= 30 uses MD5.

#[test]
fn strong_checksum_protocol_0_uses_md4() {
    let strategy = ChecksumStrategySelector::for_protocol_version(0, 0);
    assert_eq!(strategy.algorithm_name(), "md4");
    assert_eq!(strategy.digest_len(), 16);
}

#[test]
fn strong_checksum_protocol_27_uses_md4() {
    let strategy = ChecksumStrategySelector::for_protocol_version(27, 99999);
    assert_eq!(strategy.algorithm_name(), "md4");
    assert_eq!(strategy.digest_len(), 16);
}

#[test]
fn strong_checksum_protocol_29_is_last_md4() {
    let strategy = ChecksumStrategySelector::for_protocol_version(29, 12345);
    assert_eq!(strategy.algorithm_name(), "md4");
    assert_eq!(strategy.algorithm_kind(), ChecksumAlgorithmKind::Md4);
}

#[test]
fn strong_checksum_protocol_30_is_first_md5() {
    let strategy = ChecksumStrategySelector::for_protocol_version(30, 12345);
    assert_eq!(strategy.algorithm_name(), "md5");
    assert_eq!(strategy.algorithm_kind(), ChecksumAlgorithmKind::Md5);
}

#[test]
fn strong_checksum_protocol_29_30_boundary_differ() {
    let pre = ChecksumStrategySelector::for_protocol_version(29, 42);
    let post = ChecksumStrategySelector::for_protocol_version(30, 42);
    assert_eq!(pre.algorithm_name(), "md4");
    assert_eq!(post.algorithm_name(), "md5");
    assert_ne!(
        pre.compute(b"boundary test data"),
        post.compute(b"boundary test data"),
        "MD4 and MD5 must produce different digests"
    );
}

#[test]
fn strong_checksum_protocol_32_uses_md5() {
    let strategy = ChecksumStrategySelector::for_protocol_version(32, 0);
    assert_eq!(strategy.algorithm_name(), "md5");
    assert_eq!(strategy.algorithm_kind(), ChecksumAlgorithmKind::Md5);
}

#[test]
fn strong_checksum_protocol_u8_max_uses_md5() {
    let strategy = ChecksumStrategySelector::for_protocol_version(u8::MAX, 0);
    assert_eq!(strategy.algorithm_name(), "md5");
    assert_eq!(strategy.digest_len(), 16);
}

#[test]
fn strong_checksum_protocol_32_uses_xxh3_when_negotiated() {
    // XXH3 requires explicit negotiation via for_algorithm, not for_protocol_version.
    // Protocol version alone defaults to MD5 for >= 30.
    let default = ChecksumStrategySelector::for_protocol_version(32, 42);
    assert_eq!(default.algorithm_name(), "md5");

    let negotiated = ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Xxh3, 42);
    assert_eq!(negotiated.algorithm_name(), "xxh3");
    assert_eq!(negotiated.algorithm_kind(), ChecksumAlgorithmKind::Xxh3);
    assert_eq!(negotiated.digest_len(), 8);
}

#[test]
fn seed_order_ignored_for_pre30_protocols() {
    // MD4 has no seed, so proper vs legacy seed order must not affect output.
    let proper = ChecksumStrategySelector::for_protocol_version_with_seed_order(29, 12345, true);
    let legacy = ChecksumStrategySelector::for_protocol_version_with_seed_order(29, 12345, false);
    assert_eq!(proper.algorithm_name(), "md4");
    assert_eq!(legacy.algorithm_name(), "md4");
    assert_eq!(
        proper.compute(b"seed order test"),
        legacy.compute(b"seed order test"),
        "MD4 ignores seed ordering"
    );
}

#[test]
fn seed_order_matters_for_protocol_30_and_above() {
    for version in [30, 31, 32] {
        let proper =
            ChecksumStrategySelector::for_protocol_version_with_seed_order(version, 9999, true);
        let legacy =
            ChecksumStrategySelector::for_protocol_version_with_seed_order(version, 9999, false);
        assert_eq!(proper.algorithm_name(), "md5");
        assert_eq!(legacy.algorithm_name(), "md5");
        assert_ne!(
            proper.compute(b"seed order test"),
            legacy.compute(b"seed order test"),
            "proper and legacy seed ordering must differ for protocol {version}"
        );
    }
}

#[test]
fn md4_pre30_seed_does_not_affect_output() {
    // MD4 ignores the seed parameter entirely.
    let s1 = ChecksumStrategySelector::for_protocol_version(29, 0);
    let s2 = ChecksumStrategySelector::for_protocol_version(29, i32::MAX);
    let s3 = ChecksumStrategySelector::for_protocol_version(29, i32::MIN);
    let data = b"seed independence test";
    assert_eq!(s1.compute(data), s2.compute(data));
    assert_eq!(s2.compute(data), s3.compute(data));
}

#[test]
fn md5_post30_seed_affects_output() {
    let s1 = ChecksumStrategySelector::for_protocol_version(30, 0);
    let s2 = ChecksumStrategySelector::for_protocol_version(30, 1);
    assert_ne!(
        s1.compute(b"seed sensitivity test"),
        s2.compute(b"seed sensitivity test"),
        "different seeds must produce different MD5 digests"
    );
}

#[test]
fn all_supported_protocol_versions_produce_valid_digests() {
    // Verify every protocol version from 27 (minimum supported) through 32
    // (current) produces a non-empty, correctly-sized digest.
    let data = b"protocol version sweep";
    for version in 27..=32 {
        let strategy = ChecksumStrategySelector::for_protocol_version(version, 42);
        let digest = strategy.compute(data);
        assert!(
            !digest.is_empty(),
            "protocol {version} produced empty digest"
        );
        assert_eq!(
            digest.len(),
            strategy.digest_len(),
            "protocol {version} digest length mismatch"
        );
    }
}

#[test]
fn selector_for_protocol_version_with_seed_order() {
    let proper = ChecksumStrategySelector::for_protocol_version_with_seed_order(30, 123, true);
    let legacy = ChecksumStrategySelector::for_protocol_version_with_seed_order(30, 123, false);

    assert_eq!(proper.algorithm_name(), "md5");
    assert_eq!(legacy.algorithm_name(), "md5");
    assert_ne!(proper.compute(b"test"), legacy.compute(b"test"));
}

#[test]
fn selector_for_algorithm() {
    let test_data = b"test data for algorithm selection";

    for kind in ChecksumAlgorithmKind::all() {
        let strategy = ChecksumStrategySelector::for_algorithm(*kind, 42);
        assert_eq!(strategy.algorithm_kind(), *kind);
        assert_eq!(strategy.digest_len(), kind.digest_len());

        let digest = strategy.compute(test_data);
        assert_eq!(digest.len(), kind.digest_len());
    }
}

#[test]
fn selector_with_seed_config_md5_legacy() {
    let strategy = ChecksumStrategySelector::with_seed_config(
        ChecksumAlgorithmKind::Md5,
        SeedConfig::Md5(Md5SeedConfig::Legacy(12345)),
    );
    assert_eq!(strategy.algorithm_name(), "md5");

    let proper = ChecksumStrategySelector::with_seed_config(
        ChecksumAlgorithmKind::Md5,
        SeedConfig::Md5(Md5SeedConfig::Proper(12345)),
    );
    assert_ne!(strategy.compute(b"test"), proper.compute(b"test"));
}

#[test]
fn selector_with_seed_config_xxh3() {
    let strategy = ChecksumStrategySelector::with_seed_config(
        ChecksumAlgorithmKind::Xxh3,
        SeedConfig::Seed64(0x12345678),
    );
    assert_eq!(strategy.algorithm_name(), "xxh3");
}

#[test]
fn selector_concrete_factories() {
    let md4 = ChecksumStrategySelector::md4();
    assert_eq!(md4.algorithm_name(), "md4");

    let md5 = ChecksumStrategySelector::md5();
    assert_eq!(md5.algorithm_name(), "md5");

    let md5_proper = ChecksumStrategySelector::md5_proper(123);
    let md5_legacy = ChecksumStrategySelector::md5_legacy(123);
    assert_ne!(md5_proper.compute(b"test"), md5_legacy.compute(b"test"));

    let sha1 = ChecksumStrategySelector::sha1();
    assert_eq!(sha1.digest_len(), 20);

    let sha256 = ChecksumStrategySelector::sha256();
    assert_eq!(sha256.digest_len(), 32);

    let sha512 = ChecksumStrategySelector::sha512();
    assert_eq!(sha512.digest_len(), 64);

    let xxh64 = ChecksumStrategySelector::xxh64(42);
    assert_eq!(xxh64.digest_len(), 8);

    let xxh3 = ChecksumStrategySelector::xxh3(42);
    assert_eq!(xxh3.digest_len(), 8);

    let xxh3_128 = ChecksumStrategySelector::xxh3_128(42);
    assert_eq!(xxh3_128.digest_len(), 16);
}

#[test]
fn strategies_are_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<Md4Strategy>();
    assert_send_sync::<Md5Strategy>();
    assert_send_sync::<Sha1Strategy>();
    assert_send_sync::<Sha256Strategy>();
    assert_send_sync::<Sha512Strategy>();
    assert_send_sync::<Xxh64Strategy>();
    assert_send_sync::<Xxh3Strategy>();
    assert_send_sync::<Xxh3_128Strategy>();
}

#[test]
fn boxed_strategy_works() {
    let strategies: Vec<Box<dyn ChecksumStrategy>> = vec![
        Box::new(Md4Strategy::new()),
        Box::new(Md5Strategy::new()),
        Box::new(Sha1Strategy::new()),
        Box::new(Xxh3Strategy::new(0)),
    ];

    for strategy in &strategies {
        let digest = strategy.compute(b"test");
        assert_eq!(digest.len(), strategy.digest_len());
    }
}

#[test]
fn consistent_results_across_calls() {
    let strategy = Sha256Strategy::new();
    let d1 = strategy.compute(b"deterministic");
    let d2 = strategy.compute(b"deterministic");
    assert_eq!(d1, d2);
}

#[test]
fn different_inputs_different_results() {
    let strategy = Sha256Strategy::new();
    let d1 = strategy.compute(b"input1");
    let d2 = strategy.compute(b"input2");
    assert_ne!(d1, d2);
}

#[test]
fn empty_input_produces_valid_digest() {
    for kind in ChecksumAlgorithmKind::all() {
        let strategy = ChecksumStrategySelector::for_algorithm(*kind, 0);
        let digest = strategy.compute(b"");
        assert_eq!(digest.len(), kind.digest_len());
    }
}

// --- Exhaustive protocol version boundary tests ---
// upstream: checksum.c - protocol < 30 uses MD4, >= 30 uses MD5.
// These tests verify the boundary holds across the entire u8 range.

#[test]
fn protocol_version_boundary_exhaustive_algorithm_kind() {
    // Every version below 30 must select MD4; every version 30+ must select MD5.
    for version in 0..30u8 {
        let strategy = ChecksumStrategySelector::for_protocol_version(version, 0);
        assert_eq!(
            strategy.algorithm_kind(),
            ChecksumAlgorithmKind::Md4,
            "protocol {version} should use MD4"
        );
    }
    for version in 30..=u8::MAX {
        let strategy = ChecksumStrategySelector::for_protocol_version(version, 0);
        assert_eq!(
            strategy.algorithm_kind(),
            ChecksumAlgorithmKind::Md5,
            "protocol {version} should use MD5"
        );
    }
}

#[test]
fn protocol_version_boundary_digest_always_16_bytes() {
    // Both MD4 and MD5 produce 16-byte digests, so the digest length must
    // be 16 regardless of which side of the boundary we land on.
    let data = b"digest length invariant";
    for version in [0, 1, 15, 28, 29, 30, 31, 32, 100, u8::MAX] {
        let strategy = ChecksumStrategySelector::for_protocol_version(version, 42);
        let digest = strategy.compute(data);
        assert_eq!(
            digest.len(),
            16,
            "protocol {version} digest length must be 16"
        );
    }
}

#[test]
fn protocol_version_boundary_compute_into_consistent() {
    // Verify compute_into matches compute across the boundary.
    let data = b"compute_into boundary test";
    for version in [0, 29, 30, u8::MAX] {
        let strategy = ChecksumStrategySelector::for_protocol_version(version, 99);
        let digest = strategy.compute(data);
        let mut buf = [0u8; 16];
        strategy.compute_into(data, &mut buf);
        assert_eq!(
            &buf[..digest.len()],
            digest.as_bytes(),
            "protocol {version}: compute_into must match compute"
        );
    }
}

#[test]
fn protocol_version_boundary_seed_order_at_extremes() {
    // Seed order must be irrelevant for protocol 0 (MD4) and must matter
    // for protocol u8::MAX (MD5).
    let proper_0 = ChecksumStrategySelector::for_protocol_version_with_seed_order(0, 12345, true);
    let legacy_0 = ChecksumStrategySelector::for_protocol_version_with_seed_order(0, 12345, false);
    assert_eq!(proper_0.algorithm_kind(), ChecksumAlgorithmKind::Md4);
    assert_eq!(
        proper_0.compute(b"extremes"),
        legacy_0.compute(b"extremes"),
        "MD4 (protocol 0) must ignore seed ordering"
    );

    let proper_max =
        ChecksumStrategySelector::for_protocol_version_with_seed_order(u8::MAX, 12345, true);
    let legacy_max =
        ChecksumStrategySelector::for_protocol_version_with_seed_order(u8::MAX, 12345, false);
    assert_eq!(proper_max.algorithm_kind(), ChecksumAlgorithmKind::Md5);
    assert_ne!(
        proper_max.compute(b"extremes"),
        legacy_max.compute(b"extremes"),
        "MD5 (protocol 255) must differentiate seed ordering"
    );
}

#[test]
fn protocol_version_boundary_adjacent_versions_differ() {
    // Versions 29 and 30 are the only adjacent pair where algorithm changes.
    // All other adjacent pairs within the same range must produce identical kinds.
    for version in 1..30u8 {
        let prev = ChecksumStrategySelector::for_protocol_version(version - 1, 0);
        let curr = ChecksumStrategySelector::for_protocol_version(version, 0);
        assert_eq!(
            prev.algorithm_kind(),
            curr.algorithm_kind(),
            "protocols {} and {} must use the same algorithm (MD4)",
            version - 1,
            version
        );
    }
    for version in 31..=u8::MAX {
        let prev = ChecksumStrategySelector::for_protocol_version(version - 1, 0);
        let curr = ChecksumStrategySelector::for_protocol_version(version, 0);
        assert_eq!(
            prev.algorithm_kind(),
            curr.algorithm_kind(),
            "protocols {} and {version} must use the same algorithm (MD5)",
            version - 1
        );
    }
    // The boundary itself: 29 and 30 must differ.
    let v29 = ChecksumStrategySelector::for_protocol_version(29, 0);
    let v30 = ChecksumStrategySelector::for_protocol_version(30, 0);
    assert_ne!(v29.algorithm_kind(), v30.algorithm_kind());
}

#[test]
fn protocol_version_boundary_md4_deterministic_across_seeds() {
    // MD4 (pre-30) ignores the seed, so all seed values must produce
    // identical digests for the same input and protocol version.
    let data = b"md4 seed invariance";
    let seeds = [i32::MIN, -1, 0, 1, 42, 99999, i32::MAX];
    let reference = ChecksumStrategySelector::for_protocol_version(0, 0).compute(data);
    for &seed in &seeds {
        let digest = ChecksumStrategySelector::for_protocol_version(0, seed).compute(data);
        assert_eq!(
            digest, reference,
            "MD4 must produce identical output regardless of seed {seed}"
        );
    }
}

#[test]
fn protocol_version_boundary_md5_seed_sensitive() {
    // MD5 (post-30) must produce different output for different non-zero seeds.
    let data = b"md5 seed sensitivity";
    let d0 = ChecksumStrategySelector::for_protocol_version(u8::MAX, 0).compute(data);
    let d1 = ChecksumStrategySelector::for_protocol_version(u8::MAX, 1).compute(data);
    let d_neg = ChecksumStrategySelector::for_protocol_version(u8::MAX, -1).compute(data);
    let d_max = ChecksumStrategySelector::for_protocol_version(u8::MAX, i32::MAX).compute(data);
    let d_min = ChecksumStrategySelector::for_protocol_version(u8::MAX, i32::MIN).compute(data);

    // All five must be distinct (seed affects the MD5 prefix).
    let all = [&d0, &d1, &d_neg, &d_max, &d_min];
    for i in 0..all.len() {
        for j in (i + 1)..all.len() {
            assert_ne!(all[i], all[j], "MD5 digests for distinct seeds must differ");
        }
    }
}
