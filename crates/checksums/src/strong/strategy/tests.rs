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
