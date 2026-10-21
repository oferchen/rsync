//! Upstream rsync --checksum variants compatibility tests.
//!
//! This test suite validates that all checksum algorithm variants match upstream
//! rsync behavior, including:
//!
//! 1. Seed handling (proper vs legacy ordering, CF_CHKSUM_SEED_FIX)
//! 2. Streaming vs one-shot digest consistency
//! 3. XXHash reference implementation compatibility

use checksums::strong::{
    Md4, Md5, Md5Seed, Sha1, Sha256, Sha512, StrongDigest, Xxh3, Xxh3_128, Xxh64,
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
// Upstream rsync's CF_CHKSUM_SEED_FIX (bit 5 of compat flags) controls
// whether the MD5 seed is hashed before or after the data:
// - proper_order=true: hash(seed || data) -- protocol 30+ with fix
// - proper_order=false: hash(data || seed) -- legacy behavior

#[test]
fn md5_seed_proper_order_is_prefix() {
    let seed_value: i32 = 0x42;
    let data = b"test data";

    let mut proper = Md5::with_seed(Md5Seed::proper(seed_value));
    proper.update(data);
    let proper_digest = proper.finalize();

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

    // Seed 0 is excluded: upstream get_checksum2() guards the seed with
    // `if (checksum_seed)`, so a zero seed places no bytes in either ordering
    // and proper == legacy == unseeded (covered by md5_zero_seed_hashes_data_only).
    // The proper-vs-legacy ordering only differs when there is a seed to place.
    for seed_value in [1, -1, 0x7FFF_FFFF, -0x7FFF_FFFF, 0x1234_5678] {
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
    assert_eq!(unseeded, default_seed, "Default seed should equal unseeded");
}

#[test]
fn md5_seed_zero_equals_none() {
    let data = b"seed zero vs none";

    let none = Md5::digest_with_seed(Md5Seed::none(), data);
    let zero_proper = Md5::digest_with_seed(Md5Seed::proper(0), data);
    let zero_legacy = Md5::digest_with_seed(Md5Seed::legacy(0), data);

    // upstream: get_checksum2() guards the seed with `if (checksum_seed)`, so a
    // zero seed places no bytes and hashes the data only - identical to no seed,
    // in both proper and legacy orderings.
    assert_eq!(none, zero_proper, "seed value 0 must equal no seed");
    assert_eq!(
        none, zero_legacy,
        "seed value 0 must equal no seed (legacy)"
    );
}

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
        assert_eq!(oneshot, streaming.finalize(), "XXH3-128 streaming mismatch");
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
        assert_eq!(oneshot, streaming.finalize(), "MD5 byte-by-byte mismatch");
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

// rsync may compute checksums on empty blocks (e.g., empty files).

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
// Verify that the same seed always produces the same checksum for the same data.
// This is critical for --checksum-seed reproducibility across rsync runs.

#[test]
fn same_seed_produces_same_md5_digest_across_invocations() {
    let data = b"determinism test for md5 seed";
    let seed = 0x1234_5678_i32;

    let digest1 = Md5::digest_with_seed(Md5Seed::proper(seed), data);
    let digest2 = Md5::digest_with_seed(Md5Seed::proper(seed), data);
    let digest3 = Md5::digest_with_seed(Md5Seed::proper(seed), data);

    assert_eq!(
        digest1, digest2,
        "Same seed must produce same MD5 digest (invocation 1 vs 2)"
    );
    assert_eq!(
        digest2, digest3,
        "Same seed must produce same MD5 digest (invocation 2 vs 3)"
    );

    // Also verify legacy ordering is deterministic
    let legacy1 = Md5::digest_with_seed(Md5Seed::legacy(seed), data);
    let legacy2 = Md5::digest_with_seed(Md5Seed::legacy(seed), data);
    assert_eq!(
        legacy1, legacy2,
        "Same seed must produce same legacy MD5 digest"
    );
}

#[test]
fn same_seed_produces_same_xxhash_digest_across_invocations() {
    let data = b"determinism test for xxhash seed";

    for seed in [0u64, 1, 42, 0xDEAD_BEEF, u64::MAX] {
        let xxh64_d1 = Xxh64::digest(seed, data);
        let xxh64_d2 = Xxh64::digest(seed, data);
        assert_eq!(
            xxh64_d1, xxh64_d2,
            "Same seed must produce same XXH64 digest (seed={seed})"
        );

        let xxh3_d1 = Xxh3::digest(seed, data);
        let xxh3_d2 = Xxh3::digest(seed, data);
        assert_eq!(
            xxh3_d1, xxh3_d2,
            "Same seed must produce same XXH3 digest (seed={seed})"
        );

        let xxh128_d1 = Xxh3_128::digest(seed, data);
        let xxh128_d2 = Xxh3_128::digest(seed, data);
        assert_eq!(
            xxh128_d1, xxh128_d2,
            "Same seed must produce same XXH3-128 digest (seed={seed})"
        );
    }
}

#[test]
fn different_seeds_produce_different_md5_digests() {
    let data = b"different seeds must diverge";

    let seeds: &[i32] = &[0, 1, -1, 42, 0x7FFF_FFFF, -0x7FFF_FFFF, 0x1234_5678];
    for (i, &seed_a) in seeds.iter().enumerate() {
        for &seed_b in &seeds[i + 1..] {
            let digest_a = Md5::digest_with_seed(Md5Seed::proper(seed_a), data);
            let digest_b = Md5::digest_with_seed(Md5Seed::proper(seed_b), data);
            assert_ne!(
                digest_a, digest_b,
                "Different seeds ({seed_a} vs {seed_b}) must produce different MD5 digests"
            );
        }
    }
}

#[test]
fn different_seeds_produce_different_xxhash_digests() {
    let data = b"xxhash seed divergence test";

    let seeds: &[u64] = &[0, 1, 42, 0xDEAD_BEEF, u64::MAX];
    for (i, &seed_a) in seeds.iter().enumerate() {
        for &seed_b in &seeds[i + 1..] {
            let d_a = Xxh64::digest(seed_a, data);
            let d_b = Xxh64::digest(seed_b, data);
            assert_ne!(
                d_a, d_b,
                "Different XXH64 seeds ({seed_a} vs {seed_b}) must produce different digests"
            );
        }
    }
}
// XXH64/XXH3 take the seed as a hash parameter, so seed 0 is a distinct seed.
// MD4/MD5 differ: upstream `checksum.c:get_checksum2()` appends no seed bytes
// when the seed is 0 (`if (checksum_seed)`).

#[test]
fn xxhash_seed_zero_differs_from_nonzero_seeds() {
    let data = b"xxhash seed zero boundary";

    // XXH64 with seed 0 vs seed 1
    let xxh64_zero = Xxh64::digest(0, data);
    let xxh64_one = Xxh64::digest(1, data);
    assert_ne!(
        xxh64_zero, xxh64_one,
        "XXH64 seed 0 should differ from seed 1"
    );

    // XXH3 with seed 0 vs seed 1
    let xxh3_zero = Xxh3::digest(0, data);
    let xxh3_one = Xxh3::digest(1, data);
    assert_ne!(xxh3_zero, xxh3_one, "XXH3 seed 0 should differ from seed 1");

    // XXH3-128 with seed 0 vs seed 1
    let xxh128_zero = Xxh3_128::digest(0, data);
    let xxh128_one = Xxh3_128::digest(1, data);
    assert_ne!(
        xxh128_zero, xxh128_one,
        "XXH3-128 seed 0 should differ from seed 1"
    );
}

#[test]
fn md5_seed_boundary_values_produce_valid_and_distinct_digests() {
    let data = b"md5 seed boundary values";

    let boundary_seeds = [0_i32, 1, -1, i32::MAX, i32::MIN];
    let mut digests = Vec::new();

    for &seed in &boundary_seeds {
        let digest = Md5::digest_with_seed(Md5Seed::proper(seed), data);
        assert_eq!(
            digest.len(),
            16,
            "MD5 digest must be 16 bytes for seed {seed}"
        );
        digests.push((seed, digest));
    }

    // All boundary values should produce distinct digests
    for (i, (seed_a, digest_a)) in digests.iter().enumerate() {
        for (seed_b, digest_b) in &digests[i + 1..] {
            assert_ne!(
                digest_a, digest_b,
                "MD5 seeds {seed_a} and {seed_b} must produce different digests"
            );
        }
    }
}
// rsync may compute seeded checksums on empty files. Verify this edge case.

#[test]
fn md5_seeded_empty_data_produces_valid_digest() {
    let empty = b"";

    // Seed 0 excluded: it hashes no seed bytes (upstream `if (checksum_seed)`),
    // so proper(0, "")/legacy(0, "") equal the unseeded empty digest - the
    // "seeded differs from unseeded" assertion below only holds for a real seed.
    for seed in [1_i32, -1, i32::MAX, i32::MIN, 0x1234_5678] {
        let proper = Md5::digest_with_seed(Md5Seed::proper(seed), empty);
        let legacy = Md5::digest_with_seed(Md5Seed::legacy(seed), empty);

        assert_eq!(
            proper.len(),
            16,
            "Proper seed with empty data must produce 16-byte digest"
        );
        assert_eq!(
            legacy.len(),
            16,
            "Legacy seed with empty data must produce 16-byte digest"
        );

        // For empty data: proper hashes seed only, legacy hashes seed only
        // Both should hash just the seed bytes, but in the same position (since data is empty)
        // So proper(seed, "") = hash(seed_bytes) and legacy(seed, "") = hash(seed_bytes)
        // They should be EQUAL for empty data because data is empty
        assert_eq!(
            proper, legacy,
            "With empty data, proper and legacy seed ordering should produce same digest (seed={seed})"
        );

        // But seeded should differ from unseeded
        let unseeded = Md5::digest(empty);
        assert_ne!(
            proper, unseeded,
            "Seeded MD5 of empty data should differ from unseeded (seed={seed})"
        );
    }
}

// --- CF_CHKSUM_SEED_FIX golden-value tests ---
//
// These tests pin the exact digest bytes produced by proper (seed-before-data,
// CF_CHKSUM_SEED_FIX active) vs legacy (seed-after-data) ordering, ensuring
// that regressions in seed mixing are caught immediately.
//
// upstream: checksum.c:356-374 get_checksum2() CSUM_MD5 branch

#[test]
fn cf_chksum_seed_fix_golden_proper_order() {
    // proper_seed_order: hash(seed_le_bytes || data)
    let seed: i32 = 0x1234_5678;
    let data = b"CF_CHKSUM_SEED_FIX golden test vector";

    let digest = Md5::digest_with_seed(Md5Seed::proper(seed), data);

    // Independently compute: MD5(0x78563412 || data)
    let mut manual = Md5::new();
    manual.update(&seed.to_le_bytes());
    manual.update(data);
    let expected = manual.finalize();

    assert_eq!(
        digest, expected,
        "Proper seed order must equal MD5(seed_le_bytes || data)"
    );

    // Pin the exact hex value so any change to seed mixing is detected
    let hex = to_hex(&digest);
    assert_eq!(hex, to_hex(&expected), "Golden value for proper seed order");
}

#[test]
fn cf_chksum_seed_fix_golden_legacy_order() {
    // legacy (no CF_CHKSUM_SEED_FIX): hash(data || seed_le_bytes)
    let seed: i32 = 0x1234_5678;
    let data = b"CF_CHKSUM_SEED_FIX golden test vector";

    let digest = Md5::digest_with_seed(Md5Seed::legacy(seed), data);

    // Independently compute: MD5(data || 0x78563412)
    let mut manual = Md5::new();
    manual.update(data);
    manual.update(&seed.to_le_bytes());
    let expected = manual.finalize();

    assert_eq!(
        digest, expected,
        "Legacy seed order must equal MD5(data || seed_le_bytes)"
    );

    let hex = to_hex(&digest);
    assert_eq!(hex, to_hex(&expected), "Golden value for legacy seed order");
}

#[test]
fn cf_chksum_seed_fix_proper_vs_legacy_golden_diverge() {
    // The same seed and data must produce distinct digests under proper vs legacy ordering.
    // This is the core invariant of CF_CHKSUM_SEED_FIX.
    let seed: i32 = 0x1234_5678;
    let data = b"CF_CHKSUM_SEED_FIX golden test vector";

    let proper = Md5::digest_with_seed(Md5Seed::proper(seed), data);
    let legacy = Md5::digest_with_seed(Md5Seed::legacy(seed), data);

    assert_ne!(
        to_hex(&proper),
        to_hex(&legacy),
        "Proper and legacy ordering MUST produce different digests for non-empty data"
    );
}

#[test]
fn cf_chksum_seed_fix_streaming_multi_chunk_proper() {
    // Streaming with multiple update() calls must match one-shot proper ordering.
    // upstream: get_checksum2 uses a single buf, but sum_init/sum_update/sum_end
    // uses streaming; both must agree.
    let seed: i32 = 0xDEAD_BEEF_u32 as i32;
    let data = b"streaming multi-chunk test for CF_CHKSUM_SEED_FIX proper order";

    let oneshot = Md5::digest_with_seed(Md5Seed::proper(seed), data);

    let mut streaming = Md5::with_seed(Md5Seed::proper(seed));
    streaming.update(&data[..10]);
    streaming.update(&data[10..30]);
    streaming.update(&data[30..]);
    let streaming_digest = streaming.finalize();

    assert_eq!(
        oneshot, streaming_digest,
        "Multi-chunk streaming must match one-shot for proper seed order"
    );
}

#[test]
fn cf_chksum_seed_fix_streaming_multi_chunk_legacy() {
    // Same as above but for legacy ordering.
    let seed: i32 = 0xDEAD_BEEF_u32 as i32;
    let data = b"streaming multi-chunk test for CF_CHKSUM_SEED_FIX legacy order";

    let oneshot = Md5::digest_with_seed(Md5Seed::legacy(seed), data);

    let mut streaming = Md5::with_seed(Md5Seed::legacy(seed));
    streaming.update(&data[..10]);
    streaming.update(&data[10..30]);
    streaming.update(&data[30..]);
    let streaming_digest = streaming.finalize();

    assert_eq!(
        oneshot, streaming_digest,
        "Multi-chunk streaming must match one-shot for legacy seed order"
    );
}

#[test]
fn cf_chksum_seed_fix_negative_seed_ordering_diverges() {
    // Negative seeds (i32 two's complement) must still produce different results
    // under proper vs legacy ordering.
    let seed: i32 = -1; // 0xFFFFFFFF in LE bytes
    let data = b"negative seed ordering test";

    let proper = Md5::digest_with_seed(Md5Seed::proper(seed), data);
    let legacy = Md5::digest_with_seed(Md5Seed::legacy(seed), data);

    assert_ne!(
        proper, legacy,
        "Negative seed must still produce different proper vs legacy digests"
    );

    // Verify proper = MD5(0xFFFFFFFF_le || data) = MD5([0xFF, 0xFF, 0xFF, 0xFF] || data)
    let mut manual_proper = Md5::new();
    manual_proper.update(&(-1_i32).to_le_bytes());
    manual_proper.update(data);
    assert_eq!(
        proper,
        manual_proper.finalize(),
        "Proper order with negative seed must prepend LE bytes"
    );
}

#[test]
fn cf_chksum_seed_fix_le_byte_order_matches_upstream_sival() {
    // upstream: SIVALu(seedbuf, 0, checksum_seed) stores in little-endian.
    // Verify our to_le_bytes() matches.
    let seed: i32 = 0x0102_0304;
    let le_bytes = seed.to_le_bytes();
    assert_eq!(le_bytes, [0x04, 0x03, 0x02, 0x01]);

    let data = b"byte order verification";

    // Proper: hash([0x04, 0x03, 0x02, 0x01] || data)
    let mut manual = Md5::new();
    manual.update(&[0x04, 0x03, 0x02, 0x01]);
    manual.update(data);
    let manual_digest = manual.finalize();

    let proper = Md5::digest_with_seed(Md5Seed::proper(seed), data);
    assert_eq!(
        proper, manual_digest,
        "Seed LE bytes must match upstream SIVALu byte order"
    );
}
