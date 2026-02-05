//! Comprehensive tests for XXH64 checksum algorithm.
//!
//! This module provides thorough testing of the XXH64 implementation including:
//! - Known test vectors from the official xxHash documentation
//! - Empty input handling
//! - Various input sizes (1 byte to 1MB)
//! - Seed variations (0, 1, max, various patterns)
//! - Streaming/incremental hashing
//!
//! Reference: https://github.com/Cyan4973/xxHash

use checksums::strong::{StrongDigest, Xxh64};

// ============================================================================
// Helper Functions
// ============================================================================

/// Convert a u64 hash to little-endian bytes for comparison with Xxh64::digest output.
fn expected_le_bytes(hash: u64) -> [u8; 8] {
    hash.to_le_bytes()
}

/// Convert little-endian bytes to u64 hash value.
fn digest_to_u64(digest: [u8; 8]) -> u64 {
    u64::from_le_bytes(digest)
}

/// Convert a digest to hex string for debugging.
#[allow(dead_code)]
fn to_hex(digest: &[u8; 8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(16);
    for byte in digest {
        write!(&mut out, "{byte:02x}").expect("write! to String cannot fail");
    }
    out
}

// ============================================================================
// Section 1: Known Test Vectors from xxHash Documentation
// ============================================================================
// These test vectors are derived from the official xxHash specification
// and verified against the reference C implementation.
// Reference: https://github.com/Cyan4973/xxHash
// Reference: https://pypi.org/project/xxhash/

mod known_test_vectors {
    use super::*;

    /// XXH64("", 0) = 0xef46db3751d8e999
    /// This is a well-known test vector from the xxHash documentation.
    #[test]
    fn empty_string_seed_0() {
        let digest = Xxh64::digest(0, b"");
        let hash = digest_to_u64(digest);
        assert_eq!(
            hash,
            0xef46db3751d8e999,
            "XXH64 of empty string with seed 0 should be 0xef46db3751d8e999, got 0x{:016x}",
            hash
        );
    }

    /// Test that the empty string hash is consistent with reference.
    #[test]
    fn empty_string_seed_0_bytes() {
        let digest = Xxh64::digest(0, b"");
        // The expected value 0xef46db3751d8e999 in little-endian bytes
        let expected = expected_le_bytes(0xef46db3751d8e999);
        assert_eq!(
            digest, expected,
            "XXH64 empty string bytes: expected {:?}, got {:?}",
            expected, digest
        );
    }

    /// Test common pangram string.
    #[test]
    fn quick_brown_fox() {
        let input = b"The quick brown fox jumps over the lazy dog";
        let digest = Xxh64::digest(0, input);

        // Verify deterministic - same input always produces same output
        let digest2 = Xxh64::digest(0, input);
        assert_eq!(digest, digest2, "XXH64 should be deterministic");

        // Verify streaming matches one-shot
        let mut hasher = Xxh64::new(0);
        hasher.update(input);
        let streaming = hasher.finalize();
        assert_eq!(digest, streaming, "Streaming should match one-shot");
    }

    /// Test single character 'a'.
    #[test]
    fn single_char_a() {
        let digest = Xxh64::digest(0, b"a");
        let hash = digest_to_u64(digest);

        // Hash should be non-zero for non-empty input
        assert_ne!(hash, 0, "Hash of 'a' should be non-zero");

        // Verify consistency with xxhash-rust reference
        let expected = xxhash_rust::xxh64::xxh64(b"a", 0).to_le_bytes();
        assert_eq!(digest, expected, "Should match xxhash-rust reference");
    }

    /// Test string "abc".
    #[test]
    fn string_abc() {
        let digest = Xxh64::digest(0, b"abc");
        let expected = xxhash_rust::xxh64::xxh64(b"abc", 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    /// Test numeric string "123456789".
    #[test]
    fn numeric_string() {
        let digest = Xxh64::digest(0, b"123456789");
        let expected = xxhash_rust::xxh64::xxh64(b"123456789", 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    /// Test "Hello, World!" common test string.
    #[test]
    fn hello_world() {
        let digest = Xxh64::digest(0, b"Hello, World!");
        let expected = xxhash_rust::xxh64::xxh64(b"Hello, World!", 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    /// Test lowercase alphabet.
    #[test]
    fn lowercase_alphabet() {
        let input = b"abcdefghijklmnopqrstuvwxyz";
        let digest = Xxh64::digest(0, input);
        let expected = xxhash_rust::xxh64::xxh64(input, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    /// Test uppercase alphabet.
    #[test]
    fn uppercase_alphabet() {
        let input = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let digest = Xxh64::digest(0, input);
        let expected = xxhash_rust::xxh64::xxh64(input, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    /// Test mixed alphanumeric string.
    #[test]
    fn alphanumeric_mixed() {
        let input = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        let digest = Xxh64::digest(0, input);
        let expected = xxhash_rust::xxh64::xxh64(input, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    /// Test all zeros buffer.
    #[test]
    fn all_zeros_64_bytes() {
        let zeros = [0u8; 64];
        let digest = Xxh64::digest(0, &zeros);
        let hash = digest_to_u64(digest);
        // Even all zeros should produce a non-zero hash
        assert_ne!(hash, 0, "Hash of zeros should be non-zero");

        let expected = xxhash_rust::xxh64::xxh64(&zeros, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    /// Test all 0xFF bytes buffer.
    #[test]
    fn all_ones_64_bytes() {
        let ones = [0xFFu8; 64];
        let digest = Xxh64::digest(0, &ones);

        // Should differ from all zeros
        let zeros = [0u8; 64];
        let zeros_digest = Xxh64::digest(0, &zeros);
        assert_ne!(digest, zeros_digest, "Different inputs should produce different hashes");

        let expected = xxhash_rust::xxh64::xxh64(&ones, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    /// Test binary data with all byte values.
    #[test]
    fn all_byte_values() {
        let data: Vec<u8> = (0..=255).collect();
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    /// Test with seed from xxhash documentation.
    /// xxhash.xxh64_hexdigest('xxhash', seed=20141025) returns 'b559b98d844e0635'
    #[test]
    fn xxhash_string_with_documented_seed() {
        let digest = Xxh64::digest(20141025, b"xxhash");
        let hash = digest_to_u64(digest);
        // The documented result is 0xb559b98d844e0635 (from pypi xxhash docs)
        // which equals 13067679811253438005 as integer
        assert_eq!(
            hash,
            0xb559b98d844e0635,
            "XXH64('xxhash', seed=20141025) should be 0xb559b98d844e0635, got 0x{:016x}",
            hash
        );
    }
}

// ============================================================================
// Section 2: Empty Input Tests
// ============================================================================

mod empty_input {
    use super::*;

    #[test]
    fn empty_slice_produces_known_digest() {
        let digest = Xxh64::digest(0, b"");
        assert_eq!(digest_to_u64(digest), 0xef46db3751d8e999);
        assert_eq!(digest.len(), 8);
    }

    #[test]
    fn empty_slice_via_static_array() {
        let empty: &[u8] = &[];
        let digest = Xxh64::digest(0, empty);
        assert_eq!(digest_to_u64(digest), 0xef46db3751d8e999);
    }

    #[test]
    fn empty_vec() {
        let empty = Vec::<u8>::new();
        let digest = Xxh64::digest(0, &empty);
        assert_eq!(digest_to_u64(digest), 0xef46db3751d8e999);
    }

    #[test]
    fn empty_streaming_produces_same_digest() {
        let hasher = Xxh64::new(0);
        // No update calls
        let digest = hasher.finalize();
        assert_eq!(digest_to_u64(digest), 0xef46db3751d8e999);
    }

    #[test]
    fn empty_streaming_with_single_empty_update() {
        let mut hasher = Xxh64::new(0);
        hasher.update(&[]);
        let digest = hasher.finalize();
        assert_eq!(digest_to_u64(digest), 0xef46db3751d8e999);
    }

    #[test]
    fn empty_streaming_with_multiple_empty_updates() {
        let mut hasher = Xxh64::new(0);
        hasher.update(&[]);
        hasher.update(&[]);
        hasher.update(&[]);
        hasher.update(&[]);
        hasher.update(&[]);
        let digest = hasher.finalize();

        let oneshot = Xxh64::digest(0, b"");
        assert_eq!(digest, oneshot, "Multiple empty updates should equal empty one-shot");
    }

    #[test]
    fn empty_with_different_seeds() {
        let digest_0 = Xxh64::digest(0, b"");
        let digest_1 = Xxh64::digest(1, b"");
        let digest_42 = Xxh64::digest(42, b"");
        let digest_max = Xxh64::digest(u64::MAX, b"");

        // Even for empty input, different seeds should produce different results
        assert_ne!(digest_0, digest_1, "Seed 0 vs 1");
        assert_ne!(digest_0, digest_42, "Seed 0 vs 42");
        assert_ne!(digest_0, digest_max, "Seed 0 vs max");
        assert_ne!(digest_1, digest_42, "Seed 1 vs 42");
        assert_ne!(digest_1, digest_max, "Seed 1 vs max");
        assert_ne!(digest_42, digest_max, "Seed 42 vs max");
    }

    #[test]
    fn empty_streaming_with_seed() {
        let hasher = Xxh64::new(123456);
        let streaming = hasher.finalize();

        let oneshot = Xxh64::digest(123456, b"");
        assert_eq!(streaming, oneshot);
    }
}

// ============================================================================
// Section 3: Various Input Sizes Tests
// ============================================================================

mod various_input_sizes {
    use super::*;

    /// Helper to generate deterministic test data.
    fn generate_data(size: usize) -> Vec<u8> {
        (0..size).map(|i| (i % 256) as u8).collect()
    }

    // --- Single byte tests ---

    #[test]
    fn size_1_byte_zero() {
        let digest = Xxh64::digest(0, &[0x00]);
        assert_eq!(digest.len(), 8);
        let expected = xxhash_rust::xxh64::xxh64(&[0x00], 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_1_byte_one() {
        let digest = Xxh64::digest(0, &[0x01]);
        assert_eq!(digest.len(), 8);
        let expected = xxhash_rust::xxh64::xxh64(&[0x01], 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_1_byte_max() {
        let digest = Xxh64::digest(0, &[0xFF]);
        assert_eq!(digest.len(), 8);
        let expected = xxhash_rust::xxh64::xxh64(&[0xFF], 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_1_byte_all_256_values() {
        for byte in 0u8..=255 {
            let digest = Xxh64::digest(0, &[byte]);
            assert_eq!(digest.len(), 8, "Byte {:02x} should produce 8-byte digest", byte);

            let expected = xxhash_rust::xxh64::xxh64(&[byte], 0).to_le_bytes();
            assert_eq!(digest, expected, "Byte {:02x} mismatch", byte);
        }
    }

    // --- Small sizes around block boundaries ---

    #[test]
    fn size_2_bytes() {
        let data = generate_data(2);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_3_bytes() {
        let data = generate_data(3);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_4_bytes() {
        let data = generate_data(4);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_7_bytes() {
        let data = generate_data(7);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_8_bytes() {
        let data = generate_data(8);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_15_bytes() {
        let data = generate_data(15);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_16_bytes() {
        let data = generate_data(16);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    // --- XXH64 internal block boundary (32 bytes) ---

    #[test]
    fn size_31_bytes() {
        let data = generate_data(31);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_32_bytes_exactly_one_block() {
        let data = generate_data(32);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_33_bytes() {
        let data = generate_data(33);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_63_bytes() {
        let data = generate_data(63);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_64_bytes_two_blocks() {
        let data = generate_data(64);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_65_bytes() {
        let data = generate_data(65);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    // --- Medium sizes ---

    #[test]
    fn size_127_bytes() {
        let data = generate_data(127);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_128_bytes() {
        let data = generate_data(128);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_255_bytes() {
        let data = generate_data(255);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_256_bytes() {
        let data = generate_data(256);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_512_bytes() {
        let data = generate_data(512);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_1kb() {
        let data = generate_data(1024);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_4kb() {
        let data = generate_data(4 * 1024);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_8kb() {
        let data = generate_data(8 * 1024);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_16kb() {
        let data = generate_data(16 * 1024);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_32kb() {
        let data = generate_data(32 * 1024);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_64kb() {
        let data = generate_data(64 * 1024);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    // --- Large sizes ---

    #[test]
    fn size_128kb() {
        let data = generate_data(128 * 1024);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_256kb() {
        let data = generate_data(256 * 1024);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_512kb() {
        let data = generate_data(512 * 1024);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_1mb() {
        let data = generate_data(1024 * 1024);
        let digest = Xxh64::digest(0, &data);
        let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    // --- Prime number sizes (catch alignment edge cases) ---

    #[test]
    fn prime_sizes() {
        let primes = [2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53,
                      59, 61, 67, 71, 73, 79, 83, 89, 97, 101, 127, 131, 251, 509,
                      1021, 2039, 4093, 8191];

        for &size in &primes {
            let data = generate_data(size);
            let digest = Xxh64::digest(0, &data);
            let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
            assert_eq!(digest, expected, "Prime size {} mismatch", size);
        }
    }

    // --- Powers of two minus one (boundary edge cases) ---

    #[test]
    fn power_of_two_minus_one_sizes() {
        let sizes = [1, 3, 7, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767];

        for &size in &sizes {
            let data = generate_data(size);
            let digest = Xxh64::digest(0, &data);
            let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
            assert_eq!(digest, expected, "Size {} (2^n - 1) mismatch", size);
        }
    }

    // --- Streaming matches one-shot for all sizes ---

    #[test]
    fn streaming_matches_oneshot_various_sizes() {
        let sizes = [0, 1, 2, 7, 8, 15, 16, 31, 32, 63, 64, 127, 128, 255, 256, 1024, 4096];

        for &size in &sizes {
            let data = generate_data(size);

            let oneshot = Xxh64::digest(0, &data);

            let mut hasher = Xxh64::new(0);
            hasher.update(&data);
            let streaming = hasher.finalize();

            assert_eq!(oneshot, streaming, "Size {} streaming mismatch", size);
        }
    }
}

// ============================================================================
// Section 4: Seed Variations Tests
// ============================================================================

mod seed_variations {
    use super::*;

    #[test]
    fn seed_zero() {
        let digest = Xxh64::digest(0, b"test");
        assert_eq!(digest.len(), 8);
        let expected = xxhash_rust::xxh64::xxh64(b"test", 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn seed_one() {
        let digest = Xxh64::digest(1, b"test");
        assert_ne!(digest, Xxh64::digest(0, b"test"), "Seed 1 should differ from seed 0");
        let expected = xxhash_rust::xxh64::xxh64(b"test", 1).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn seed_max_u64() {
        let digest = Xxh64::digest(u64::MAX, b"test");
        assert_eq!(digest.len(), 8);
        assert_ne!(digest, Xxh64::digest(0, b"test"));
        let expected = xxhash_rust::xxh64::xxh64(b"test", u64::MAX).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn seed_42() {
        let digest = Xxh64::digest(42, b"test");
        let expected = xxhash_rust::xxh64::xxh64(b"test", 42).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn seed_256() {
        let digest = Xxh64::digest(256, b"test");
        let expected = xxhash_rust::xxh64::xxh64(b"test", 256).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn seed_65535() {
        let digest = Xxh64::digest(65535, b"test");
        let expected = xxhash_rust::xxh64::xxh64(b"test", 65535).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn seed_power_of_two() {
        for power in [1u64, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096] {
            let digest = Xxh64::digest(power, b"test");
            let expected = xxhash_rust::xxh64::xxh64(b"test", power).to_le_bytes();
            assert_eq!(digest, expected, "Seed {} mismatch", power);
        }
    }

    #[test]
    fn seed_large_values() {
        let seeds = [
            0x12345678u64,
            0x87654321u64,
            0xDEADBEEFu64,
            0xCAFEBABEu64,
            0xFFFFFFFF00000000u64,
            0x00000000FFFFFFFFu64,
            0x123456789ABCDEFu64,
            u32::MAX as u64,
            u32::MAX as u64 + 1,
        ];

        for &seed in &seeds {
            let digest = Xxh64::digest(seed, b"test");
            let expected = xxhash_rust::xxh64::xxh64(b"test", seed).to_le_bytes();
            assert_eq!(digest, expected, "Seed 0x{:016x} mismatch", seed);
        }
    }

    #[test]
    fn different_seeds_produce_different_hashes() {
        let data = b"seed test data";
        let seeds = [0u64, 1, 42, 256, 65535, 0x12345678, u32::MAX as u64, u64::MAX];

        let digests: Vec<_> = seeds.iter().map(|&seed| Xxh64::digest(seed, data)).collect();

        // All digests should be unique
        for i in 0..digests.len() {
            for j in (i + 1)..digests.len() {
                assert_ne!(
                    digests[i], digests[j],
                    "Seeds {} and {} should produce different hashes",
                    seeds[i], seeds[j]
                );
            }
        }
    }

    #[test]
    fn seed_streaming_consistency() {
        let seed = 0xDEADBEEFu64;
        let data = b"streaming with seed test";

        let oneshot = Xxh64::digest(seed, data);

        let mut hasher = Xxh64::new(seed);
        hasher.update(data);
        let streaming = hasher.finalize();

        assert_eq!(oneshot, streaming);
    }

    #[test]
    fn seed_via_trait_interface() {
        let seed = 12345u64;
        let data = b"trait interface test";

        // Using StrongDigest trait
        let hasher: Xxh64 = StrongDigest::with_seed(seed);
        let mut h = hasher;
        h.update(data);
        let trait_digest = h.finalize();

        let direct_digest = Xxh64::digest(seed, data);
        assert_eq!(trait_digest, direct_digest);
    }

    #[test]
    fn seed_same_input_same_seed_same_output() {
        let seed = 999999u64;
        let data = b"determinism test";

        let d1 = Xxh64::digest(seed, data);
        let d2 = Xxh64::digest(seed, data);
        let d3 = Xxh64::digest(seed, data);

        assert_eq!(d1, d2);
        assert_eq!(d2, d3);
    }
}

// ============================================================================
// Section 5: Streaming/Incremental Hashing Tests
// ============================================================================

mod streaming_incremental {
    use super::*;

    #[test]
    fn streaming_byte_by_byte() {
        let data = b"streaming test data for xxh64 algorithm verification";

        let mut hasher = Xxh64::new(0);
        for &byte in data.iter() {
            hasher.update(&[byte]);
        }
        let streaming = hasher.finalize();

        let oneshot = Xxh64::digest(0, data);
        assert_eq!(streaming, oneshot, "Byte-by-byte streaming should match one-shot");
    }

    #[test]
    fn streaming_two_halves() {
        let data = b"first half|second half of data";
        let midpoint = data.len() / 2;

        let mut hasher = Xxh64::new(0);
        hasher.update(&data[..midpoint]);
        hasher.update(&data[midpoint..]);
        let streaming = hasher.finalize();

        let oneshot = Xxh64::digest(0, data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn streaming_three_parts() {
        let data = b"part one|part two|part three";
        let third = data.len() / 3;

        let mut hasher = Xxh64::new(0);
        hasher.update(&data[..third]);
        hasher.update(&data[third..2*third]);
        hasher.update(&data[2*third..]);
        let streaming = hasher.finalize();

        let oneshot = Xxh64::digest(0, data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn streaming_various_chunk_sizes() {
        let data: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
        let oneshot = Xxh64::digest(0, &data);

        for chunk_size in [1, 2, 3, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, 100, 1000] {
            let mut hasher = Xxh64::new(0);
            for chunk in data.chunks(chunk_size) {
                hasher.update(chunk);
            }
            let streaming = hasher.finalize();
            assert_eq!(
                oneshot, streaming,
                "Chunk size {} should produce same result as one-shot",
                chunk_size
            );
        }
    }

    #[test]
    fn streaming_with_empty_updates_interspersed() {
        let data = b"test data";

        let mut hasher = Xxh64::new(0);
        hasher.update(b"");
        hasher.update(b"test");
        hasher.update(b"");
        hasher.update(b" ");
        hasher.update(b"");
        hasher.update(b"");
        hasher.update(b"data");
        hasher.update(b"");
        let streaming = hasher.finalize();

        let oneshot = Xxh64::digest(0, data);
        assert_eq!(streaming, oneshot, "Empty updates interspersed should not affect result");
    }

    #[test]
    fn streaming_clone_and_diverge() {
        let mut hasher1 = Xxh64::new(42);
        hasher1.update(b"partial");

        let mut hasher2 = hasher1.clone();

        hasher1.update(b" data A");
        hasher2.update(b" data B");

        let digest1 = hasher1.finalize();
        let digest2 = hasher2.finalize();

        assert_ne!(digest1, digest2, "Different continuations should produce different hashes");

        // Verify cloned hasher produces correct result
        let expected_b = Xxh64::digest(42, b"partial data B");
        assert_eq!(digest2, expected_b);

        let expected_a = Xxh64::digest(42, b"partial data A");
        assert_eq!(digest1, expected_a);
    }

    #[test]
    fn streaming_clone_at_various_points() {
        let data = b"abcdefghijklmnopqrstuvwxyz";
        let full_digest = Xxh64::digest(0, data);

        // Clone after each byte and verify final result
        for i in 0..data.len() {
            let mut hasher = Xxh64::new(0);
            hasher.update(&data[..i]);

            let mut cloned = hasher.clone();
            cloned.update(&data[i..]);
            let cloned_digest = cloned.finalize();

            assert_eq!(cloned_digest, full_digest, "Clone at position {} should produce correct result", i);
        }
    }

    #[test]
    fn streaming_incremental_large_data() {
        let data: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();
        let oneshot = Xxh64::digest(0, &data);

        // Stream in irregular chunk sizes
        let mut hasher = Xxh64::new(0);
        let chunk_sizes = [17, 31, 64, 100, 256, 500, 1000, 2048, 5000];
        let mut offset = 0;

        for &size in chunk_sizes.iter().cycle() {
            if offset >= data.len() {
                break;
            }
            let end = (offset + size).min(data.len());
            hasher.update(&data[offset..end]);
            offset = end;
        }
        let streaming = hasher.finalize();
        assert_eq!(oneshot, streaming, "Irregular chunk streaming should match one-shot");
    }

    #[test]
    fn streaming_1mb_in_various_chunk_sizes() {
        let size = 1024 * 1024; // 1MB
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let oneshot = Xxh64::digest(0, &data);

        for chunk_size in [1024, 4096, 32768, 65536] {
            let mut hasher = Xxh64::new(0);
            for chunk in data.chunks(chunk_size) {
                hasher.update(chunk);
            }
            let streaming = hasher.finalize();
            assert_eq!(
                oneshot, streaming,
                "1MB with chunk size {} should match one-shot",
                chunk_size
            );
        }
    }

    #[test]
    fn streaming_with_seed() {
        let seed = 0xCAFEBABEu64;
        let data = b"seeded streaming test data";

        let mut hasher = Xxh64::new(seed);
        for chunk in data.chunks(5) {
            hasher.update(chunk);
        }
        let streaming = hasher.finalize();

        let oneshot = Xxh64::digest(seed, data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn streaming_trait_new_vs_inherent_new() {
        let seed = 99u64;
        let data = b"compare constructors test data";

        let mut hasher1 = Xxh64::new(seed);
        hasher1.update(data);
        let digest1 = hasher1.finalize();

        let mut hasher2: Xxh64 = StrongDigest::with_seed(seed);
        hasher2.update(data);
        let digest2 = hasher2.finalize();

        assert_eq!(digest1, digest2, "new() and with_seed() should behave identically");
    }

    #[test]
    fn streaming_single_large_update() {
        let data: Vec<u8> = (0..50_000).map(|i| (i % 256) as u8).collect();

        let mut hasher = Xxh64::new(0);
        hasher.update(&data);
        let streaming = hasher.finalize();

        let oneshot = Xxh64::digest(0, &data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn streaming_multiple_small_updates() {
        let data = b"a".repeat(1000);

        let mut hasher = Xxh64::new(0);
        for _ in 0..1000 {
            hasher.update(b"a");
        }
        let streaming = hasher.finalize();

        let oneshot = Xxh64::digest(0, &data);
        assert_eq!(streaming, oneshot);
    }
}

// ============================================================================
// Section 6: Additional Verification Tests
// ============================================================================

mod verification {
    use super::*;

    #[test]
    fn verify_64bit_output_length() {
        assert_eq!(Xxh64::DIGEST_LEN, 8, "XXH64 DIGEST_LEN should be 8 bytes");

        let digest = Xxh64::digest(0, b"test");
        assert_eq!(digest.len(), 8, "Digest array should have 8 elements");
        assert_eq!(digest.as_ref().len(), 8, "Digest as ref should have 8 bytes");
    }

    #[test]
    fn verify_deterministic_output() {
        let data = b"determinism test";
        let d1 = Xxh64::digest(0, data);
        let d2 = Xxh64::digest(0, data);
        let d3 = Xxh64::digest(0, data);
        assert_eq!(d1, d2);
        assert_eq!(d2, d3);
    }

    #[test]
    fn verify_different_inputs_different_outputs() {
        let d1 = Xxh64::digest(0, b"input1");
        let d2 = Xxh64::digest(0, b"input2");
        assert_ne!(d1, d2);
    }

    #[test]
    fn verify_similar_inputs_different_outputs() {
        let d1 = Xxh64::digest(0, b"test");
        let d2 = Xxh64::digest(0, b"Test"); // Different case
        let d3 = Xxh64::digest(0, b"test "); // Trailing space
        let d4 = Xxh64::digest(0, b" test"); // Leading space

        assert_ne!(d1, d2);
        assert_ne!(d1, d3);
        assert_ne!(d1, d4);
        assert_ne!(d2, d3);
        assert_ne!(d2, d4);
        assert_ne!(d3, d4);
    }

    #[test]
    fn verify_little_endian_encoding() {
        // The implementation returns little-endian bytes
        let digest = Xxh64::digest(0, b"test");
        let from_le = u64::from_le_bytes(digest);
        let from_be = u64::from_be_bytes(digest);

        // These should be different (unless palindromic, which is unlikely)
        assert_ne!(
            from_le, from_be,
            "Little-endian and big-endian interpretations should differ"
        );

        // Verify against xxhash-rust reference
        let expected_hash = xxhash_rust::xxh64::xxh64(b"test", 0);
        assert_eq!(from_le, expected_hash, "Should match xxhash-rust when read as little-endian");
    }

    #[test]
    fn verify_avalanche_effect() {
        // Changing a single bit should produce a very different hash
        let data1 = vec![0u8; 100];
        let mut data2 = data1.clone();
        data2[50] = 1; // Change single bit

        let digest1 = Xxh64::digest(0, &data1);
        let digest2 = Xxh64::digest(0, &data2);

        let hash1 = u64::from_le_bytes(digest1);
        let hash2 = u64::from_le_bytes(digest2);

        // Count differing bits (should be roughly half due to avalanche effect)
        let diff = (hash1 ^ hash2).count_ones();
        assert!(
            diff >= 20,
            "Avalanche effect: {} bits differ, expected >= 20",
            diff
        );
    }

    #[test]
    fn verify_all_bits_can_be_set() {
        // Hash many different inputs to verify all bit positions can be set
        let mut or_accumulator = 0u64;

        for i in 0..10000 {
            let data = format!("input_{}", i);
            let digest = Xxh64::digest(0, data.as_bytes());
            let hash = u64::from_le_bytes(digest);
            or_accumulator |= hash;
        }

        // After many hashes, we should see all bits set at least once
        assert_eq!(
            or_accumulator.count_ones(),
            64,
            "All 64 bits should be exercised by various inputs"
        );
    }

    #[test]
    fn verify_bit_distribution() {
        // Check that bits are reasonably distributed
        let mut bit_counts = [0u32; 64];

        for i in 0..10000 {
            let data = format!("distribution_test_{}", i);
            let digest = Xxh64::digest(0, data.as_bytes());
            let hash = u64::from_le_bytes(digest);

            for bit in 0..64 {
                if (hash >> bit) & 1 == 1 {
                    bit_counts[bit] += 1;
                }
            }
        }

        // Each bit should be set roughly 50% of the time (5000 +/- some tolerance)
        for (bit, &count) in bit_counts.iter().enumerate() {
            assert!(
                count > 4000 && count < 6000,
                "Bit {} has count {}, expected ~5000 (40%-60% range)",
                bit,
                count
            );
        }
    }

    #[test]
    fn verify_hash_range() {
        // Verify the hash can produce values across the full 64-bit range
        let mut min_hash = u64::MAX;
        let mut max_hash = 0u64;

        for i in 0..10000 {
            let data = format!("range_test_{}", i);
            let digest = Xxh64::digest(0, data.as_bytes());
            let hash = u64::from_le_bytes(digest);
            min_hash = min_hash.min(hash);
            max_hash = max_hash.max(hash);
        }

        // The range should span a significant portion of the 64-bit space
        let range = max_hash - min_hash;
        assert!(
            range > u64::MAX / 2,
            "Hash range {} is too small for a 64-bit hash",
            range
        );
    }
}

// ============================================================================
// Section 7: Compatibility and Regression Tests
// ============================================================================

mod compatibility {
    use super::*;

    #[test]
    fn compatibility_with_xxhash_rust_empty() {
        let our_digest = Xxh64::digest(0, b"");
        let reference = xxhash_rust::xxh64::xxh64(b"", 0).to_le_bytes();
        assert_eq!(our_digest, reference);
    }

    #[test]
    fn compatibility_with_xxhash_rust_various_inputs() {
        let test_inputs: &[&[u8]] = &[
            b"",
            b"a",
            b"ab",
            b"abc",
            b"hello world",
            b"The quick brown fox jumps over the lazy dog",
            b"0123456789",
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZ",
        ];

        for input in test_inputs {
            for seed in [0u64, 1, 42, 12345, u64::MAX] {
                let our_digest = Xxh64::digest(seed, input);
                let reference = xxhash_rust::xxh64::xxh64(input, seed).to_le_bytes();
                assert_eq!(
                    our_digest, reference,
                    "Mismatch for input {:?} with seed {}",
                    String::from_utf8_lossy(input),
                    seed
                );
            }
        }
    }

    #[test]
    fn compatibility_with_xxhash_rust_large_inputs() {
        for size in [1000, 10000, 100000] {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

            for seed in [0u64, 42, u64::MAX] {
                let our_digest = Xxh64::digest(seed, &data);
                let reference = xxhash_rust::xxh64::xxh64(&data, seed).to_le_bytes();
                assert_eq!(
                    our_digest, reference,
                    "Mismatch for size {} with seed {}",
                    size, seed
                );
            }
        }
    }

    #[test]
    fn compatibility_streaming_matches_xxhash_rust() {
        let data = b"streaming compatibility test with xxhash-rust reference";

        for seed in [0u64, 123, 999999] {
            let mut our_hasher = Xxh64::new(seed);
            our_hasher.update(data);
            let our_digest = our_hasher.finalize();

            let reference = xxhash_rust::xxh64::xxh64(data, seed).to_le_bytes();
            assert_eq!(our_digest, reference, "Streaming mismatch for seed {}", seed);
        }
    }

    #[test]
    fn regression_known_hash_values() {
        // Store some known hash values to catch regressions
        // These are computed against xxhash-rust reference
        let test_cases = [
            (0u64, b"".as_slice(), 0xef46db3751d8e999u64),
            (20141025u64, b"xxhash".as_slice(), 0xb559b98d844e0635u64),
        ];

        for (seed, data, expected_hash) in test_cases {
            let digest = Xxh64::digest(seed, data);
            let hash = u64::from_le_bytes(digest);
            assert_eq!(
                hash, expected_hash,
                "Regression: seed={}, data={:?}, expected=0x{:016x}, got=0x{:016x}",
                seed, String::from_utf8_lossy(data), expected_hash, hash
            );
        }
    }

    #[test]
    fn regression_consistency_across_runs() {
        // Verify results remain consistent
        let test_cases = [
            (0u64, b"".as_slice()),
            (0u64, b"a".as_slice()),
            (0u64, b"abc".as_slice()),
            (0u64, b"test".as_slice()),
            (42u64, b"test".as_slice()),
            (u64::MAX, b"test".as_slice()),
        ];

        // Compute hashes
        let hashes: Vec<_> = test_cases
            .iter()
            .map(|(seed, data)| Xxh64::digest(*seed, data))
            .collect();

        // Verify they match on subsequent runs
        for (i, (seed, data)) in test_cases.iter().enumerate() {
            let digest = Xxh64::digest(*seed, data);
            assert_eq!(
                hashes[i], digest,
                "Hash should be consistent for seed={}, data={:?}",
                seed, String::from_utf8_lossy(data)
            );
        }
    }
}

// ============================================================================
// Section 8: Edge Cases and Boundary Conditions
// ============================================================================

mod edge_cases {
    use super::*;

    #[test]
    fn boundary_at_internal_block_size_32() {
        // XXH64 processes data in 32-byte chunks internally
        for size in 30..=34 {
            let data = vec![0xAB; size];
            let digest = Xxh64::digest(0, &data);
            assert_eq!(digest.len(), 8);

            let mut hasher = Xxh64::new(0);
            hasher.update(&data);
            assert_eq!(digest, hasher.finalize());

            let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
            assert_eq!(digest, expected, "Size {} boundary test failed", size);
        }
    }

    #[test]
    fn boundary_at_64_bytes() {
        for size in 62..=66 {
            let data = vec![0xCD; size];
            let digest = Xxh64::digest(0, &data);
            let expected = xxhash_rust::xxh64::xxh64(&data, 0).to_le_bytes();
            assert_eq!(digest, expected, "Size {} boundary test failed", size);
        }
    }

    #[test]
    fn repeated_pattern_input() {
        let pattern = b"ABCD";
        let repeated: Vec<u8> = pattern.iter().cycle().take(10000).cloned().collect();

        let digest = Xxh64::digest(0, &repeated);
        assert_eq!(digest.len(), 8);

        // Different repeat counts should produce different hashes
        let repeated2: Vec<u8> = pattern.iter().cycle().take(10001).cloned().collect();
        let digest2 = Xxh64::digest(0, &repeated2);
        assert_ne!(digest, digest2);
    }

    #[test]
    fn all_same_byte_patterns() {
        for byte in [0x00, 0x55, 0xAA, 0xFF] {
            let data = vec![byte; 1000];
            let digest = Xxh64::digest(0, &data);
            assert_eq!(digest.len(), 8);

            // Different byte values should produce different hashes
            let other_byte = byte.wrapping_add(1);
            let other_data = vec![other_byte; 1000];
            let other_digest = Xxh64::digest(0, &other_data);
            assert_ne!(digest, other_digest);
        }
    }

    #[test]
    fn alternating_patterns() {
        let alternating: Vec<u8> = (0..1000).map(|i| if i % 2 == 0 { 0xAA } else { 0x55 }).collect();
        let digest = Xxh64::digest(0, &alternating);
        let expected = xxhash_rust::xxh64::xxh64(&alternating, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn sequential_byte_values() {
        let sequential: Vec<u8> = (0..=255).collect();
        let digest = Xxh64::digest(0, &sequential);
        let expected = xxhash_rust::xxh64::xxh64(&sequential, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn reverse_sequential_byte_values() {
        let reverse: Vec<u8> = (0..=255).rev().collect();
        let digest = Xxh64::digest(0, &reverse);
        let expected = xxhash_rust::xxh64::xxh64(&reverse, 0).to_le_bytes();
        assert_eq!(digest, expected);

        // Should differ from forward sequence
        let forward: Vec<u8> = (0..=255).collect();
        let forward_digest = Xxh64::digest(0, &forward);
        assert_ne!(digest, forward_digest);
    }

    #[test]
    fn utf8_strings() {
        let utf8_strings = [
            "Hello, World!",
            "Rust programming",
            "Unicode: \u{1F600} \u{1F389}", // Emojis
            "\u{4E2D}\u{6587}", // Chinese characters
            "\u{0420}\u{0443}\u{0441}\u{0441}\u{043A}\u{0438}\u{0439}", // Russian
        ];

        for s in utf8_strings {
            let bytes = s.as_bytes();
            let digest = Xxh64::digest(0, bytes);
            let expected = xxhash_rust::xxh64::xxh64(bytes, 0).to_le_bytes();
            assert_eq!(digest, expected, "UTF-8 string {:?} mismatch", s);
        }
    }

    #[test]
    fn binary_data_with_nulls() {
        let data_with_nulls = b"hello\x00world\x00test\x00";
        let digest = Xxh64::digest(0, data_with_nulls);
        let expected = xxhash_rust::xxh64::xxh64(data_with_nulls, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn very_long_repeated_null() {
        let nulls = vec![0u8; 100000];
        let digest = Xxh64::digest(0, &nulls);
        let expected = xxhash_rust::xxh64::xxh64(&nulls, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }
}
