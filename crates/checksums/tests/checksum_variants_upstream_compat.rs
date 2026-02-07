//! Upstream rsync --checksum variants compatibility tests (task #60).
//!
//! This test suite validates that all checksum algorithm variants match upstream
//! rsync behavior, including:
//!
//! 1. RFC/NIST test vectors for all cryptographic algorithms
//! 2. Protocol version -> algorithm mapping (MD4 < v30, MD5 >= v30)
//! 3. Algorithm name parsing and roundtrip (matching upstream wire names)
//! 4. Seed handling (proper vs legacy ordering, CHECKSUM_SEED_FIX)
//! 5. Cross-algorithm consistency (strategy vs direct API)
//! 6. XXHash reference implementation compatibility

use checksums::strong::{
    Md4, Md5, Md5Seed, Sha1, Sha256, Sha512, StrongDigest, Xxh3, Xxh3_128, Xxh64,
};
use checksums::{
    ChecksumAlgorithmKind, ChecksumStrategy, ChecksumStrategySelector, Md4Strategy, Md5Strategy,
    Sha1Strategy, Sha256Strategy, Sha512Strategy, Xxh3Strategy, Xxh3_128Strategy, Xxh64Strategy,
};

/// Convert bytes to hex string for readable assertions.
fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut out, "{byte:02x}").unwrap();
    }
    out
}

// ============================================================================
// Section 1: RFC/NIST Test Vectors via Strategy Pattern
// ============================================================================
// These verify that the Strategy pattern produces the same results as
// direct algorithm calls, ensuring no data corruption in the abstraction layer.

#[test]
fn strategy_md4_rfc1320_vectors() {
    let strategy = Md4Strategy::new();
    let vectors = [
        (b"".as_slice(), "31d6cfe0d16ae931b73c59d7e0c089c0"),
        (b"a".as_slice(), "bde52cb31de33e46245e05fbdbd6fb24"),
        (b"abc".as_slice(), "a448017aaf21d8525fc10ae87aa6729d"),
        (
            b"message digest".as_slice(),
            "d9130a8164549fe818874806e1c7014b",
        ),
        (
            b"abcdefghijklmnopqrstuvwxyz".as_slice(),
            "d79e1c308aa5bbcdeea8ed63df412da9",
        ),
    ];
    for (input, expected_hex) in vectors {
        let digest = strategy.compute(input);
        assert_eq!(
            to_hex(digest.as_bytes()),
            expected_hex,
            "MD4 strategy mismatch for {:?}",
            std::str::from_utf8(input).unwrap_or("<binary>")
        );
    }
}

#[test]
fn strategy_md5_rfc1321_vectors() {
    let strategy = Md5Strategy::new();
    let vectors = [
        (b"".as_slice(), "d41d8cd98f00b204e9800998ecf8427e"),
        (b"a".as_slice(), "0cc175b9c0f1b6a831c399e269772661"),
        (b"abc".as_slice(), "900150983cd24fb0d6963f7d28e17f72"),
        (
            b"message digest".as_slice(),
            "f96b697d7cb7938d525a2f31aaf161d0",
        ),
        (
            b"abcdefghijklmnopqrstuvwxyz".as_slice(),
            "c3fcd3d76192e4007dfb496cca67e13b",
        ),
        (
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789".as_slice(),
            "d174ab98d277d9f5a5611c2c9f419d9f",
        ),
        (
            b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
                .as_slice(),
            "57edf4a22be3c955ac49da2e2107b67a",
        ),
    ];
    for (input, expected_hex) in vectors {
        let digest = strategy.compute(input);
        assert_eq!(
            to_hex(digest.as_bytes()),
            expected_hex,
            "MD5 strategy mismatch for {:?}",
            std::str::from_utf8(input).unwrap_or("<binary>")
        );
    }
}

#[test]
fn strategy_sha1_rfc3174_vectors() {
    let strategy = Sha1Strategy::new();
    let vectors = [
        (
            b"".as_slice(),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709",
        ),
        (
            b"a".as_slice(),
            "86f7e437faa5a7fce15d1ddcb9eaeaea377667b8",
        ),
        (
            b"abc".as_slice(),
            "a9993e364706816aba3e25717850c26c9cd0d89d",
        ),
        (
            b"message digest".as_slice(),
            "c12252ceda8be8994d5fa0290a47231c1d16aae3",
        ),
    ];
    for (input, expected_hex) in vectors {
        let digest = strategy.compute(input);
        assert_eq!(
            to_hex(digest.as_bytes()),
            expected_hex,
            "SHA1 strategy mismatch for {:?}",
            std::str::from_utf8(input).unwrap_or("<binary>")
        );
    }
}

#[test]
fn strategy_sha256_fips180_vectors() {
    let strategy = Sha256Strategy::new();
    let vectors = [
        (
            b"".as_slice(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            b"abc".as_slice(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            b"message digest".as_slice(),
            "f7846f55cf23e14eebeab5b4e1550cad5b509e3348fbc4efa3a1413d393cb650",
        ),
    ];
    for (input, expected_hex) in vectors {
        let digest = strategy.compute(input);
        assert_eq!(
            to_hex(digest.as_bytes()),
            expected_hex,
            "SHA256 strategy mismatch for {:?}",
            std::str::from_utf8(input).unwrap_or("<binary>")
        );
    }
}

#[test]
fn strategy_sha512_fips180_vectors() {
    let strategy = Sha512Strategy::new();
    let vectors = [
        (
            b"".as_slice(),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e",
        ),
        (
            b"abc".as_slice(),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
        ),
        (
            b"message digest".as_slice(),
            "107dbf389d9e9f71a3a95f6c055b9251bc5268c2be16d6c13492ea45b0199f3309e16455ab1e96118e8a905d5597b72038ddb372a89826046de66687bb420e7c",
        ),
    ];
    for (input, expected_hex) in vectors {
        let digest = strategy.compute(input);
        assert_eq!(
            to_hex(digest.as_bytes()),
            expected_hex,
            "SHA512 strategy mismatch for {:?}",
            std::str::from_utf8(input).unwrap_or("<binary>")
        );
    }
}

#[test]
fn strategy_xxh64_reference_vectors() {
    // XXH64("", 0) = 0xef46db3751d8e999
    let strategy = Xxh64Strategy::new(0);
    let digest = strategy.compute(b"");
    let hash = u64::from_le_bytes(digest.as_bytes().try_into().unwrap());
    assert_eq!(hash, 0xef46db3751d8e999, "XXH64 empty string seed 0");

    // Verify against xxhash-rust reference for various inputs
    let test_inputs: &[&[u8]] = &[b"", b"a", b"hello world", b"test"];
    for &input in test_inputs {
        for seed in [0u64, 1, 42, u64::MAX] {
            let strategy = Xxh64Strategy::new(seed);
            let our_digest = strategy.compute(input);
            let reference = xxhash_rust::xxh64::xxh64(input, seed).to_le_bytes();
            assert_eq!(
                our_digest.as_bytes(),
                &reference,
                "XXH64 mismatch for {:?} seed {}",
                std::str::from_utf8(input).unwrap_or("<binary>"),
                seed
            );
        }
    }
}

#[test]
fn strategy_xxh3_reference_vectors() {
    let test_inputs: &[&[u8]] = &[b"", b"a", b"hello world", b"test"];
    for &input in test_inputs {
        for seed in [0u64, 1, 42, u64::MAX] {
            let strategy = Xxh3Strategy::new(seed);
            let our_digest = strategy.compute(input);
            let reference = xxhash_rust::xxh3::xxh3_64_with_seed(input, seed).to_le_bytes();
            assert_eq!(
                our_digest.as_bytes(),
                &reference,
                "XXH3 mismatch for {:?} seed {}",
                std::str::from_utf8(input).unwrap_or("<binary>"),
                seed
            );
        }
    }
}

#[test]
fn strategy_xxh3_128_reference_vectors() {
    let test_inputs: &[&[u8]] = &[b"", b"a", b"hello world", b"test"];
    for &input in test_inputs {
        for seed in [0u64, 1, 42, u64::MAX] {
            let strategy = Xxh3_128Strategy::new(seed);
            let our_digest = strategy.compute(input);
            let reference = xxhash_rust::xxh3::xxh3_128_with_seed(input, seed).to_le_bytes();
            assert_eq!(
                our_digest.as_bytes(),
                &reference,
                "XXH3-128 mismatch for {:?} seed {}",
                std::str::from_utf8(input).unwrap_or("<binary>"),
                seed
            );
        }
    }
}

// ============================================================================
// Section 2: Protocol Version -> Algorithm Mapping
// ============================================================================
// Upstream rsync uses MD4 for protocol < 30, MD5 for >= 30. Protocol 31+
// can negotiate XXH3/XXH128 via capability negotiation, but the default
// (without negotiation) is still MD5.

#[test]
fn protocol_version_mapping_exhaustive() {
    // All protocol versions from 20..30 use MD4
    for version in 20u8..30 {
        let strategy = ChecksumStrategySelector::for_protocol_version(version, 0);
        assert_eq!(
            strategy.algorithm_kind(),
            ChecksumAlgorithmKind::Md4,
            "Protocol {version} should default to MD4"
        );
    }

    // All protocol versions from 30..=40 use MD5 (as default without negotiation)
    for version in 30u8..=40 {
        let strategy = ChecksumStrategySelector::for_protocol_version(version, 0);
        assert_eq!(
            strategy.algorithm_kind(),
            ChecksumAlgorithmKind::Md5,
            "Protocol {version} should default to MD5"
        );
    }
}

#[test]
fn protocol_version_boundary_produces_different_digests() {
    let data = b"boundary test data";
    let seed = 0x12345678;

    let v29 = ChecksumStrategySelector::for_protocol_version(29, seed);
    let v30 = ChecksumStrategySelector::for_protocol_version(30, seed);

    let d29 = v29.compute(data);
    let d30 = v30.compute(data);

    // MD4 and MD5 produce different digests for the same input
    assert_ne!(d29, d30, "v29 (MD4) and v30 (MD5) should produce different digests");

    // Both produce 16-byte digests
    assert_eq!(d29.len(), 16);
    assert_eq!(d30.len(), 16);
}

#[test]
fn protocol_version_0_uses_md4() {
    // Edge case: protocol version 0 should use MD4 (< 30)
    let strategy = ChecksumStrategySelector::for_protocol_version(0, 0);
    assert_eq!(strategy.algorithm_kind(), ChecksumAlgorithmKind::Md4);
}

#[test]
fn protocol_version_255_uses_md5() {
    // Edge case: protocol version 255 should use MD5 (>= 30)
    let strategy = ChecksumStrategySelector::for_protocol_version(255, 0);
    assert_eq!(strategy.algorithm_kind(), ChecksumAlgorithmKind::Md5);
}

// ============================================================================
// Section 3: Algorithm Name Parsing (matching upstream wire protocol names)
// ============================================================================
// Upstream rsync uses these names in capability negotiation:
// xxh128, xxh3, xxh64, md5, md4, sha1, none (from SUPPORTED_CHECKSUMS)

#[test]
fn algorithm_kind_from_name_canonical_names() {
    // Test all canonical names that upstream rsync uses
    let expected = [
        ("md4", ChecksumAlgorithmKind::Md4),
        ("md5", ChecksumAlgorithmKind::Md5),
        ("sha1", ChecksumAlgorithmKind::Sha1),
        ("sha256", ChecksumAlgorithmKind::Sha256),
        ("sha512", ChecksumAlgorithmKind::Sha512),
        ("xxh64", ChecksumAlgorithmKind::Xxh64),
        ("xxh3", ChecksumAlgorithmKind::Xxh3),
        ("xxh128", ChecksumAlgorithmKind::Xxh3_128),
    ];
    for (name, kind) in expected {
        assert_eq!(
            ChecksumAlgorithmKind::from_name(name),
            Some(kind),
            "Canonical name '{name}' should parse to {kind:?}"
        );
    }
}

#[test]
fn algorithm_kind_from_name_aliases() {
    // Test common aliases
    let aliases = [
        ("sha-1", ChecksumAlgorithmKind::Sha1),
        ("sha-256", ChecksumAlgorithmKind::Sha256),
        ("sha-512", ChecksumAlgorithmKind::Sha512),
        ("xxhash64", ChecksumAlgorithmKind::Xxh64),
        ("xxhash3", ChecksumAlgorithmKind::Xxh3),
        ("xxhash128", ChecksumAlgorithmKind::Xxh3_128),
        ("xxh3-128", ChecksumAlgorithmKind::Xxh3_128),
    ];
    for (alias, kind) in aliases {
        assert_eq!(
            ChecksumAlgorithmKind::from_name(alias),
            Some(kind),
            "Alias '{alias}' should parse to {kind:?}"
        );
    }
}

#[test]
fn algorithm_kind_from_name_case_insensitive() {
    let cases = [
        ("MD4", ChecksumAlgorithmKind::Md4),
        ("Md4", ChecksumAlgorithmKind::Md4),
        ("MD5", ChecksumAlgorithmKind::Md5),
        ("SHA1", ChecksumAlgorithmKind::Sha1),
        ("Sha1", ChecksumAlgorithmKind::Sha1),
        ("SHA256", ChecksumAlgorithmKind::Sha256),
        ("XXH64", ChecksumAlgorithmKind::Xxh64),
        ("Xxh3", ChecksumAlgorithmKind::Xxh3),
        ("XXH128", ChecksumAlgorithmKind::Xxh3_128),
    ];
    for (name, kind) in cases {
        assert_eq!(
            ChecksumAlgorithmKind::from_name(name),
            Some(kind),
            "Case-insensitive '{name}' should parse to {kind:?}"
        );
    }
}

#[test]
fn algorithm_kind_from_name_rejects_invalid() {
    let invalid = [
        "none",     // valid in protocol but not a checksum algorithm kind
        "invalid",
        "",
        "md6",
        "sha384",
        "sha3-256",
        "blake2b",
        "crc32",
    ];
    for name in invalid {
        assert_eq!(
            ChecksumAlgorithmKind::from_name(name),
            None,
            "Invalid name '{name}' should return None"
        );
    }
}

#[test]
fn algorithm_kind_name_roundtrip_all() {
    // Every algorithm's name() should roundtrip through from_name()
    for kind in ChecksumAlgorithmKind::all() {
        let name = kind.name();
        let parsed = ChecksumAlgorithmKind::from_name(name);
        assert_eq!(
            parsed,
            Some(*kind),
            "Name roundtrip failed for {kind:?} (name='{name}')"
        );
    }
}

#[test]
fn algorithm_kind_display_matches_name() {
    for kind in ChecksumAlgorithmKind::all() {
        let display = format!("{kind}");
        assert_eq!(
            display,
            kind.name(),
            "Display should match name() for {kind:?}"
        );
    }
}

// ============================================================================
// Section 4: Checksum Seed Handling (CHECKSUM_SEED_FIX)
// ============================================================================
// Upstream rsync's CHECKSUM_SEED_FIX (bit 5 of compat flags) controls
// whether the MD5 seed is hashed before or after the data:
// - proper_order=true: hash(seed || data) -- protocol 30+ with fix
// - proper_order=false: hash(data || seed) -- legacy behavior

#[test]
fn md5_seed_proper_order_is_prefix() {
    let seed_value: i32 = 0x42;
    let data = b"test data";

    // Proper order: seed bytes hashed before data
    let mut proper = Md5::with_seed(Md5Seed::proper(seed_value));
    proper.update(data);
    let proper_digest = proper.finalize();

    // Manual construction: new MD5, hash seed bytes, then data
    let mut manual = Md5::new();
    manual.update(&seed_value.to_le_bytes());
    manual.update(data);
    let manual_digest = manual.finalize();

    assert_eq!(
        proper_digest, manual_digest,
        "Proper seed order should prefix seed bytes"
    );
}

#[test]
fn md5_seed_legacy_order_is_suffix() {
    let seed_value: i32 = 0x42;
    let data = b"test data";

    // Legacy order: seed bytes hashed after data
    let mut legacy = Md5::with_seed(Md5Seed::legacy(seed_value));
    legacy.update(data);
    let legacy_digest = legacy.finalize();

    // Manual construction: new MD5, hash data, then seed bytes
    let mut manual = Md5::new();
    manual.update(data);
    manual.update(&seed_value.to_le_bytes());
    let manual_digest = manual.finalize();

    assert_eq!(
        legacy_digest, manual_digest,
        "Legacy seed order should suffix seed bytes"
    );
}

#[test]
fn md5_seed_proper_vs_legacy_differ() {
    let data = b"checksum seed fix test";

    for seed_value in [0, 1, -1, 0x7FFF_FFFF, -0x7FFF_FFFF, 0x1234_5678] {
        let proper = Md5::digest_with_seed(Md5Seed::proper(seed_value), data);
        let legacy = Md5::digest_with_seed(Md5Seed::legacy(seed_value), data);

        assert_ne!(
            proper, legacy,
            "Proper and legacy seed ordering should differ for seed {seed_value}"
        );
    }
}

#[test]
fn md5_seed_none_equals_unseeded() {
    let data = b"no seed test";

    let unseeded = Md5::digest(data);
    let none_seed = Md5::digest_with_seed(Md5Seed::none(), data);
    let default_seed = Md5::digest_with_seed(Default::default(), data);

    assert_eq!(unseeded, none_seed, "Md5Seed::none() should equal unseeded");
    assert_eq!(
        unseeded, default_seed,
        "Default seed should equal unseeded"
    );
}

#[test]
fn md5_seed_zero_differs_from_none() {
    let data = b"seed zero vs none";

    let none = Md5::digest_with_seed(Md5Seed::none(), data);
    let zero_proper = Md5::digest_with_seed(Md5Seed::proper(0), data);

    // Seed value 0 should still hash the 4 zero bytes, differing from no seed
    assert_ne!(
        none, zero_proper,
        "Seed value 0 should differ from no seed (4 zero bytes are hashed)"
    );
}

#[test]
fn checksum_seed_propagated_through_strategy_selector() {
    let data = b"strategy seed propagation";
    let seed = 0x1234_5678_i32;

    // Strategy with proper seed (what protocol 30+ with CHECKSUM_SEED_FIX uses)
    let strategy = ChecksumStrategySelector::for_protocol_version(30, seed);
    let strategy_digest = strategy.compute(data);

    // Direct with proper seed
    let direct = Md5::digest_with_seed(Md5Seed::proper(seed), data);

    assert_eq!(
        strategy_digest.as_bytes(),
        direct.as_ref(),
        "Strategy selector should use proper seed ordering for protocol 30+"
    );
}

#[test]
fn checksum_seed_not_applied_to_md4() {
    let data = b"md4 ignores seed";

    // MD4 has () as its seed type, so seed value should be ignored
    let strategy_seed_0 = ChecksumStrategySelector::for_protocol_version(29, 0);
    let strategy_seed_max = ChecksumStrategySelector::for_protocol_version(29, i32::MAX);

    let d0 = strategy_seed_0.compute(data);
    let dmax = strategy_seed_max.compute(data);

    // MD4 doesn't support seeding, so different seed values should produce same digest
    assert_eq!(
        d0, dmax,
        "MD4 should produce same digest regardless of seed parameter"
    );
}

#[test]
fn xxhash_seed_applied_through_strategy() {
    let data = b"xxhash seed test";

    // XXH3 with different seeds through strategy selector
    let s1 = ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Xxh3, 0);
    let s2 = ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Xxh3, 42);

    let d1 = s1.compute(data);
    let d2 = s2.compute(data);

    assert_ne!(d1, d2, "Different seeds should produce different XXH3 digests");

    // Verify the seed is correctly converted from i32 to u64
    let direct = Xxh3::digest(42u64, data);
    assert_eq!(
        d2.as_bytes(),
        direct.as_ref(),
        "Strategy seed should match direct API seed"
    );
}

// ============================================================================
// Section 5: Direct API vs Strategy Consistency
// ============================================================================
// Verify that using the algorithm directly produces the same result as
// using it through the Strategy pattern.

#[test]
fn direct_api_matches_strategy_all_algorithms() {
    let data = b"consistency test across all algorithms";

    // MD4
    let direct_md4 = Md4::digest(data);
    let strategy_md4 = Md4Strategy::new().compute(data);
    assert_eq!(direct_md4.as_ref(), strategy_md4.as_bytes(), "MD4 mismatch");

    // MD5
    let direct_md5 = Md5::digest(data);
    let strategy_md5 = Md5Strategy::new().compute(data);
    assert_eq!(direct_md5.as_ref(), strategy_md5.as_bytes(), "MD5 mismatch");

    // SHA1
    let direct_sha1 = Sha1::digest(data);
    let strategy_sha1 = Sha1Strategy::new().compute(data);
    assert_eq!(
        direct_sha1.as_ref(),
        strategy_sha1.as_bytes(),
        "SHA1 mismatch"
    );

    // SHA256
    let direct_sha256 = Sha256::digest(data);
    let strategy_sha256 = Sha256Strategy::new().compute(data);
    assert_eq!(
        direct_sha256.as_ref(),
        strategy_sha256.as_bytes(),
        "SHA256 mismatch"
    );

    // SHA512
    let direct_sha512 = Sha512::digest(data);
    let strategy_sha512 = Sha512Strategy::new().compute(data);
    assert_eq!(
        direct_sha512.as_ref(),
        strategy_sha512.as_bytes(),
        "SHA512 mismatch"
    );

    // XXH64
    let seed = 42u64;
    let direct_xxh64 = Xxh64::digest(seed, data);
    let strategy_xxh64 = Xxh64Strategy::new(seed).compute(data);
    assert_eq!(
        direct_xxh64.as_ref(),
        strategy_xxh64.as_bytes(),
        "XXH64 mismatch"
    );

    // XXH3
    let direct_xxh3 = Xxh3::digest(seed, data);
    let strategy_xxh3 = Xxh3Strategy::new(seed).compute(data);
    assert_eq!(
        direct_xxh3.as_ref(),
        strategy_xxh3.as_bytes(),
        "XXH3 mismatch"
    );

    // XXH3-128
    let direct_xxh3_128 = Xxh3_128::digest(seed, data);
    let strategy_xxh3_128 = Xxh3_128Strategy::new(seed).compute(data);
    assert_eq!(
        direct_xxh3_128.as_ref(),
        strategy_xxh3_128.as_bytes(),
        "XXH3-128 mismatch"
    );
}

#[test]
fn for_algorithm_factory_matches_concrete_factories() {
    let data = b"factory consistency";
    let seed = 99_i32;

    for kind in ChecksumAlgorithmKind::all() {
        let boxed = ChecksumStrategySelector::for_algorithm(*kind, seed);
        let concrete: Box<dyn ChecksumStrategy> = match kind {
            ChecksumAlgorithmKind::Md4 => Box::new(ChecksumStrategySelector::md4()),
            ChecksumAlgorithmKind::Md5 => Box::new(ChecksumStrategySelector::md5_proper(seed)),
            ChecksumAlgorithmKind::Sha1 => Box::new(ChecksumStrategySelector::sha1()),
            ChecksumAlgorithmKind::Sha256 => Box::new(ChecksumStrategySelector::sha256()),
            ChecksumAlgorithmKind::Sha512 => Box::new(ChecksumStrategySelector::sha512()),
            ChecksumAlgorithmKind::Xxh64 => {
                Box::new(ChecksumStrategySelector::xxh64(seed as u64))
            }
            ChecksumAlgorithmKind::Xxh3 => Box::new(ChecksumStrategySelector::xxh3(seed as u64)),
            ChecksumAlgorithmKind::Xxh3_128 => {
                Box::new(ChecksumStrategySelector::xxh3_128(seed as u64))
            }
        };

        let boxed_digest = boxed.compute(data);
        let concrete_digest = concrete.compute(data);

        assert_eq!(
            boxed_digest, concrete_digest,
            "for_algorithm and concrete factory should produce same result for {kind}"
        );
    }
}

// ============================================================================
// Section 6: Digest Length Consistency
// ============================================================================

#[test]
fn digest_len_matches_actual_output_all_algorithms() {
    let data = b"digest length verification";

    for kind in ChecksumAlgorithmKind::all() {
        let strategy = ChecksumStrategySelector::for_algorithm(*kind, 0);
        let digest = strategy.compute(data);

        assert_eq!(
            digest.len(),
            kind.digest_len(),
            "Kind::digest_len() mismatch for {kind}"
        );
        assert_eq!(
            digest.len(),
            strategy.digest_len(),
            "Strategy::digest_len() mismatch for {kind}"
        );
        assert_eq!(
            digest.as_bytes().len(),
            kind.digest_len(),
            "Actual bytes length mismatch for {kind}"
        );
    }
}

#[test]
fn digest_lengths_match_upstream_rsync() {
    // Upstream rsync digest sizes (from checksum.c):
    // MD4: 16 bytes (128 bits)
    // MD5: 16 bytes (128 bits)
    // SHA1: 20 bytes (160 bits)
    // SHA256: 32 bytes (256 bits) -- only for daemon auth, not block checksums
    // SHA512: 64 bytes (512 bits)
    // XXH64: 8 bytes (64 bits)
    // XXH3: 8 bytes (64 bits)
    // XXH128: 16 bytes (128 bits)
    assert_eq!(ChecksumAlgorithmKind::Md4.digest_len(), 16);
    assert_eq!(ChecksumAlgorithmKind::Md5.digest_len(), 16);
    assert_eq!(ChecksumAlgorithmKind::Sha1.digest_len(), 20);
    assert_eq!(ChecksumAlgorithmKind::Sha256.digest_len(), 32);
    assert_eq!(ChecksumAlgorithmKind::Sha512.digest_len(), 64);
    assert_eq!(ChecksumAlgorithmKind::Xxh64.digest_len(), 8);
    assert_eq!(ChecksumAlgorithmKind::Xxh3.digest_len(), 8);
    assert_eq!(ChecksumAlgorithmKind::Xxh3_128.digest_len(), 16);
}

// ============================================================================
// Section 7: Cryptographic vs Non-Cryptographic Classification
// ============================================================================
// This matters for upstream rsync's --checksum-choice validation.

#[test]
fn is_cryptographic_classification_matches_upstream() {
    // Cryptographic hashes (broken but still classified as crypto)
    assert!(ChecksumAlgorithmKind::Md4.is_cryptographic());
    assert!(ChecksumAlgorithmKind::Md5.is_cryptographic());
    assert!(ChecksumAlgorithmKind::Sha1.is_cryptographic());
    assert!(ChecksumAlgorithmKind::Sha256.is_cryptographic());
    assert!(ChecksumAlgorithmKind::Sha512.is_cryptographic());

    // Non-cryptographic hashes
    assert!(!ChecksumAlgorithmKind::Xxh64.is_cryptographic());
    assert!(!ChecksumAlgorithmKind::Xxh3.is_cryptographic());
    assert!(!ChecksumAlgorithmKind::Xxh3_128.is_cryptographic());
}

// ============================================================================
// Section 8: Streaming vs One-Shot Consistency
// ============================================================================
// rsync uses both streaming (for large files read in blocks) and one-shot
// (for small files/blocks). They must produce identical results.

#[test]
fn streaming_matches_oneshot_all_algorithms() {
    let data = b"The quick brown fox jumps over the lazy dog";

    // MD4
    {
        let oneshot = Md4::digest(data);
        let mut streaming = Md4::new();
        streaming.update(&data[..10]);
        streaming.update(&data[10..]);
        assert_eq!(oneshot, streaming.finalize(), "MD4 streaming mismatch");
    }

    // MD5
    {
        let oneshot = Md5::digest(data);
        let mut streaming = Md5::new();
        streaming.update(&data[..10]);
        streaming.update(&data[10..]);
        assert_eq!(oneshot, streaming.finalize(), "MD5 streaming mismatch");
    }

    // SHA1
    {
        let oneshot = Sha1::digest(data);
        let mut streaming = Sha1::new();
        streaming.update(&data[..10]);
        streaming.update(&data[10..]);
        assert_eq!(oneshot, streaming.finalize(), "SHA1 streaming mismatch");
    }

    // SHA256
    {
        let oneshot = Sha256::digest(data);
        let mut streaming = Sha256::new();
        streaming.update(&data[..10]);
        streaming.update(&data[10..]);
        assert_eq!(oneshot, streaming.finalize(), "SHA256 streaming mismatch");
    }

    // SHA512
    {
        let oneshot = Sha512::digest(data);
        let mut streaming = Sha512::new();
        streaming.update(&data[..10]);
        streaming.update(&data[10..]);
        assert_eq!(oneshot, streaming.finalize(), "SHA512 streaming mismatch");
    }

    // XXH64
    {
        let seed = 42u64;
        let oneshot = Xxh64::digest(seed, data);
        let mut streaming = Xxh64::new(seed);
        streaming.update(&data[..10]);
        streaming.update(&data[10..]);
        assert_eq!(oneshot, streaming.finalize(), "XXH64 streaming mismatch");
    }

    // XXH3
    {
        let seed = 42u64;
        let oneshot = Xxh3::digest(seed, data);
        let mut streaming = Xxh3::new(seed);
        streaming.update(&data[..10]);
        streaming.update(&data[10..]);
        assert_eq!(oneshot, streaming.finalize(), "XXH3 streaming mismatch");
    }

    // XXH3-128
    {
        let seed = 42u64;
        let oneshot = Xxh3_128::digest(seed, data);
        let mut streaming = Xxh3_128::new(seed);
        streaming.update(&data[..10]);
        streaming.update(&data[10..]);
        assert_eq!(
            oneshot,
            streaming.finalize(),
            "XXH3-128 streaming mismatch"
        );
    }
}

#[test]
fn streaming_byte_by_byte_matches_oneshot_all_algorithms() {
    let data = b"byte-by-byte streaming verification";

    // MD5
    {
        let oneshot = Md5::digest(data);
        let mut streaming = Md5::new();
        for &byte in data.iter() {
            streaming.update(&[byte]);
        }
        assert_eq!(
            oneshot,
            streaming.finalize(),
            "MD5 byte-by-byte mismatch"
        );
    }

    // SHA256
    {
        let oneshot = Sha256::digest(data);
        let mut streaming = Sha256::new();
        for &byte in data.iter() {
            streaming.update(&[byte]);
        }
        assert_eq!(
            oneshot,
            streaming.finalize(),
            "SHA256 byte-by-byte mismatch"
        );
    }

    // XXH3
    {
        let seed = 0u64;
        let oneshot = Xxh3::digest(seed, data);
        let mut streaming = Xxh3::new(seed);
        for &byte in data.iter() {
            streaming.update(&[byte]);
        }
        let streaming_result = streaming.finalize();
        // Verify streaming matches both one-shot and reference
        assert_eq!(
            streaming_result, oneshot,
            "XXH3 byte-by-byte streaming should match one-shot"
        );
        let reference = xxhash_rust::xxh3::xxh3_64_with_seed(data, seed).to_le_bytes();
        assert_eq!(streaming_result, reference, "XXH3 byte-by-byte mismatch");
    }
}

// ============================================================================
// Section 9: Seeded MD5 with Streaming (rsync block checksum pattern)
// ============================================================================
// rsync computes seeded MD5 checksums on file blocks. The seed is applied
// once at construction, and then data is fed incrementally.

#[test]
fn seeded_md5_streaming_pattern() {
    let seed = 0xDEAD_BEEF_u32 as i32;

    // Simulate checksumming a file in blocks with a seed
    let file_data: Vec<u8> = (0..4096).map(|i| (i % 256) as u8).collect();
    let block_size = 1024;

    for block in file_data.chunks(block_size) {
        let proper_seed = Md5Seed::proper(seed);

        // One-shot
        let oneshot = Md5::digest_with_seed(proper_seed, block);

        // Streaming
        let mut streaming = Md5::with_seed(proper_seed);
        for chunk in block.chunks(100) {
            streaming.update(chunk);
        }
        let streaming_result = streaming.finalize();

        assert_eq!(
            oneshot, streaming_result,
            "Seeded MD5 streaming should match one-shot for block"
        );
    }
}

// ============================================================================
// Section 10: compute_into consistency
// ============================================================================

#[test]
fn compute_into_matches_compute_all_algorithms() {
    let data = b"compute_into verification";

    for kind in ChecksumAlgorithmKind::all() {
        let strategy = ChecksumStrategySelector::for_algorithm(*kind, 42);
        let digest = strategy.compute(data);

        let mut buffer = vec![0u8; kind.digest_len() + 16]; // extra space
        strategy.compute_into(data, &mut buffer);

        assert_eq!(
            &buffer[..kind.digest_len()],
            digest.as_bytes(),
            "compute_into mismatch for {kind}"
        );
    }
}

// ============================================================================
// Section 11: Upstream preference order verification
// ============================================================================
// Upstream rsync 3.4.1 preference order: xxh128 xxh3 xxh64 md5 md4 sha1 none
// We verify that our algorithm enumeration covers all of these.

#[test]
fn all_upstream_algorithms_are_supported() {
    let upstream_names = ["xxh128", "xxh3", "xxh64", "md5", "md4", "sha1"];

    for name in upstream_names {
        let kind = ChecksumAlgorithmKind::from_name(name);
        assert!(
            kind.is_some(),
            "Upstream algorithm '{name}' should be supported"
        );
    }
}

#[test]
fn algorithm_kind_all_contains_eight_variants() {
    // MD4, MD5, SHA1, SHA256, SHA512, XXH64, XXH3, XXH3-128
    assert_eq!(
        ChecksumAlgorithmKind::all().len(),
        8,
        "Should have 8 algorithm variants"
    );
}

// ============================================================================
// Section 12: Negative seed handling (i32 -> u64 conversion)
// ============================================================================
// rsync's checksum_seed is an i32. When passed to XXHash (which takes u64),
// it must be correctly sign-extended or zero-extended.

#[test]
fn negative_seed_xxhash_produces_valid_output() {
    let data = b"negative seed test";
    let negative_seed: i32 = -1;

    // Should not panic
    let strategy =
        ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Xxh64, negative_seed);
    let digest = strategy.compute(data);
    assert_eq!(digest.len(), 8);

    // -1 as i32 = 0xFFFFFFFF, when cast to u64 = 0x00000000_FFFFFFFF
    // (not 0xFFFFFFFF_FFFFFFFF which would be -1i64 as u64)
    let expected_seed = negative_seed as u64;
    let reference = Xxh64::digest(expected_seed, data);
    assert_eq!(
        digest.as_bytes(),
        reference.as_ref(),
        "Negative seed should be cast to u64 correctly"
    );
}

#[test]
fn seed_i32_min_and_max_produce_valid_output() {
    let data = b"extreme seed values";

    for seed in [i32::MIN, i32::MAX, 0, 1, -1] {
        for kind in ChecksumAlgorithmKind::all() {
            let strategy = ChecksumStrategySelector::for_algorithm(*kind, seed);
            let digest = strategy.compute(data);
            assert_eq!(
                digest.len(),
                kind.digest_len(),
                "Seed {seed} should produce valid digest for {kind}"
            );
        }
    }
}

// ============================================================================
// Section 13: Empty input handling for all algorithms
// ============================================================================
// rsync may compute checksums on empty blocks (e.g., empty files).

#[test]
fn empty_input_all_algorithms_produce_valid_digests() {
    let empty = b"";

    for kind in ChecksumAlgorithmKind::all() {
        let strategy = ChecksumStrategySelector::for_algorithm(*kind, 0);
        let digest = strategy.compute(empty);

        assert_eq!(
            digest.len(),
            kind.digest_len(),
            "Empty input should produce valid digest for {kind}"
        );
        assert!(
            !digest.is_empty(),
            "Empty input should produce non-empty digest for {kind}"
        );
    }
}

#[test]
fn empty_input_known_values() {
    // Well-known empty string digests
    assert_eq!(
        to_hex(&Md4::digest(b"")),
        "31d6cfe0d16ae931b73c59d7e0c089c0"
    );
    assert_eq!(
        to_hex(&Md5::digest(b"")),
        "d41d8cd98f00b204e9800998ecf8427e"
    );
    assert_eq!(
        to_hex(&Sha1::digest(b"")),
        "da39a3ee5e6b4b0d3255bfef95601890afd80709"
    );
    assert_eq!(
        to_hex(&Sha256::digest(b"")),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}
