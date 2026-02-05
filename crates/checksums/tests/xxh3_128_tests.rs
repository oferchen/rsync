//! Comprehensive tests for XXH3-128 checksum algorithm.
//!
//! This module provides thorough testing of the XXH3-128 implementation including:
//! - Known test vectors from the xxHash reference implementation
//! - Empty input handling
//! - Various input sizes (1 byte to 1MB)
//! - Seed variations (0, 1, max, various patterns)
//! - Streaming/incremental hashing vs one-shot comparison
//!
//! Reference: https://github.com/Cyan4973/xxHash

use checksums::strong::{StrongDigest, Xxh3_128};

// ============================================================================
// Helper Functions
// ============================================================================

/// Convert a u128 hash to little-endian bytes for comparison with Xxh3_128::digest output.
#[allow(dead_code)]
fn expected_le_bytes(hash: u128) -> [u8; 16] {
    hash.to_le_bytes()
}

/// Convert little-endian bytes to u128 hash value.
fn digest_to_u128(digest: [u8; 16]) -> u128 {
    u128::from_le_bytes(digest)
}

/// Convert a digest to hex string for debugging.
#[allow(dead_code)]
fn to_hex(digest: &[u8; 16]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(32);
    for byte in digest {
        write!(&mut out, "{byte:02x}").expect("write! to String cannot fail");
    }
    out
}

// ============================================================================
// Section 1: Known Test Vectors from xxHash Reference Implementation
// ============================================================================

mod known_test_vectors {
    use super::*;

    #[test]
    fn empty_string_seed_0() {
        let digest = Xxh3_128::digest(0, b"");
        let hash = digest_to_u128(digest);

        // Verify deterministic - same input always produces same output
        let digest2 = Xxh3_128::digest(0, b"");
        assert_eq!(digest, digest2, "XXH3-128 should be deterministic");

        // Verify against reference implementation
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(b"", 0).to_le_bytes();
        assert_eq!(digest, expected, "Should match reference implementation");

        // Hash should be non-zero (xxhash produces non-zero hash for empty with seed 0)
        assert_ne!(hash, 0, "Hash of empty string with seed 0 should be non-zero");
    }

    #[test]
    fn single_char_a() {
        let digest = Xxh3_128::digest(0, b"a");
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(b"a", 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn string_abc() {
        let digest = Xxh3_128::digest(0, b"abc");
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(b"abc", 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn quick_brown_fox() {
        let input = b"The quick brown fox jumps over the lazy dog";
        let digest = Xxh3_128::digest(0, input);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(input, 0).to_le_bytes();
        assert_eq!(digest, expected);

        // Verify streaming matches one-shot
        let mut hasher = Xxh3_128::new(0);
        hasher.update(input);
        let streaming = hasher.finalize();
        assert_eq!(digest, streaming, "Streaming should match one-shot");
    }

    #[test]
    fn numeric_string() {
        let digest = Xxh3_128::digest(0, b"123456789");
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(b"123456789", 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn hello_world() {
        let digest = Xxh3_128::digest(0, b"Hello, World!");
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(b"Hello, World!", 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn lowercase_alphabet() {
        let input = b"abcdefghijklmnopqrstuvwxyz";
        let digest = Xxh3_128::digest(0, input);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(input, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn uppercase_alphabet() {
        let input = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let digest = Xxh3_128::digest(0, input);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(input, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn alphanumeric_mixed() {
        let input = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        let digest = Xxh3_128::digest(0, input);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(input, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn all_zeros_64_bytes() {
        let zeros = [0u8; 64];
        let digest = Xxh3_128::digest(0, &zeros);
        let hash = digest_to_u128(digest);

        // Even all zeros should produce a non-zero hash
        assert_ne!(hash, 0, "Hash of zeros should be non-zero");

        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&zeros, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn all_ones_64_bytes() {
        let ones = [0xFFu8; 64];
        let digest = Xxh3_128::digest(0, &ones);

        // Should differ from all zeros
        let zeros = [0u8; 64];
        let zeros_digest = Xxh3_128::digest(0, &zeros);
        assert_ne!(digest, zeros_digest, "Different inputs should produce different hashes");

        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&ones, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn all_byte_values() {
        let data: Vec<u8> = (0..=255).collect();
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }
}

// ============================================================================
// Section 2: Empty Input Tests
// ============================================================================

mod empty_input {
    use super::*;

    #[test]
    fn empty_slice_produces_known_digest() {
        let digest = Xxh3_128::digest(0, b"");
        assert_eq!(digest.len(), 16);

        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(b"", 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn empty_slice_via_static_array() {
        let empty: &[u8] = &[];
        let digest = Xxh3_128::digest(0, empty);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(b"", 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn empty_vec() {
        let empty = Vec::<u8>::new();
        let digest = Xxh3_128::digest(0, &empty);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(b"", 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn empty_streaming_produces_same_digest() {
        let hasher = Xxh3_128::new(0);
        // No update calls
        let digest = hasher.finalize();
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(b"", 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn empty_streaming_with_single_empty_update() {
        let mut hasher = Xxh3_128::new(0);
        hasher.update(&[]);
        let digest = hasher.finalize();
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(b"", 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn empty_streaming_with_multiple_empty_updates() {
        let mut hasher = Xxh3_128::new(0);
        hasher.update(&[]);
        hasher.update(&[]);
        hasher.update(&[]);
        hasher.update(&[]);
        hasher.update(&[]);
        let digest = hasher.finalize();

        let oneshot = Xxh3_128::digest(0, b"");
        assert_eq!(digest, oneshot, "Multiple empty updates should equal empty one-shot");
    }

    #[test]
    fn empty_with_different_seeds() {
        let digest_0 = Xxh3_128::digest(0, b"");
        let digest_1 = Xxh3_128::digest(1, b"");
        let digest_42 = Xxh3_128::digest(42, b"");
        let digest_max = Xxh3_128::digest(u64::MAX, b"");

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
        let hasher = Xxh3_128::new(123456);
        let streaming = hasher.finalize();

        let oneshot = Xxh3_128::digest(123456, b"");
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn empty_one_shot_equals_streaming() {
        let one_shot = Xxh3_128::digest(0, b"");
        let streaming = Xxh3_128::new(0).finalize();
        assert_eq!(one_shot, streaming);
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
        let digest = Xxh3_128::digest(0, &[0x00]);
        assert_eq!(digest.len(), 16);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&[0x00], 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_1_byte_one() {
        let digest = Xxh3_128::digest(0, &[0x01]);
        assert_eq!(digest.len(), 16);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&[0x01], 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_1_byte_max() {
        let digest = Xxh3_128::digest(0, &[0xFF]);
        assert_eq!(digest.len(), 16);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&[0xFF], 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_1_byte_all_256_values() {
        for byte in 0u8..=255 {
            let digest = Xxh3_128::digest(0, &[byte]);
            assert_eq!(digest.len(), 16, "Byte {:02x} should produce 16-byte digest", byte);

            let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&[byte], 0).to_le_bytes();
            assert_eq!(digest, expected, "Byte {:02x} mismatch", byte);
        }
    }

    // --- Small sizes ---

    #[test]
    fn size_2_bytes() {
        let data = generate_data(2);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_3_bytes() {
        let data = generate_data(3);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_7_bytes() {
        let data = generate_data(7);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_16_bytes() {
        let data = generate_data(16);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    // --- XXH3 internal block boundary (usually around 128 or 256 bytes) ---

    #[test]
    fn size_31_bytes() {
        let data = generate_data(31);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_32_bytes() {
        let data = generate_data(32);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_63_bytes() {
        let data = generate_data(63);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_64_bytes() {
        let data = generate_data(64);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_127_bytes() {
        let data = generate_data(127);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_128_bytes() {
        let data = generate_data(128);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_255_bytes() {
        let data = generate_data(255);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_256_bytes() {
        let data = generate_data(256);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    // --- Medium sizes ---

    #[test]
    fn size_512_bytes() {
        let data = generate_data(512);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_1kb() {
        let data = generate_data(1024);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_4kb() {
        let data = generate_data(4 * 1024);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_8kb() {
        let data = generate_data(8 * 1024);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_16kb() {
        let data = generate_data(16 * 1024);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_64kb() {
        let data = generate_data(64 * 1024);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    // --- Large sizes ---

    #[test]
    fn size_256kb() {
        let data = generate_data(256 * 1024);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_512kb() {
        let data = generate_data(512 * 1024);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn size_1mb() {
        let data = generate_data(1024 * 1024);
        let digest = Xxh3_128::digest(0, &data);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
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
            let digest = Xxh3_128::digest(0, &data);
            let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&data, 0).to_le_bytes();
            assert_eq!(digest, expected, "Prime size {} mismatch", size);
        }
    }

    // --- Streaming matches one-shot for all sizes ---

    #[test]
    fn streaming_matches_oneshot_various_sizes() {
        let sizes = [0, 1, 2, 7, 8, 15, 16, 31, 32, 63, 64, 127, 128, 255, 256, 1024, 4096];

        for &size in &sizes {
            let data = generate_data(size);

            let oneshot = Xxh3_128::digest(0, &data);

            let mut hasher = Xxh3_128::new(0);
            hasher.update(&data);
            let streaming = hasher.finalize();

            assert_eq!(oneshot, streaming, "Size {} streaming mismatch", size);
        }
    }

    #[test]
    fn large_data_deterministic() {
        let data = generate_data(100_000);
        let d1 = Xxh3_128::digest(0, &data);
        let d2 = Xxh3_128::digest(0, &data);
        let d3 = Xxh3_128::digest(0, &data);
        assert_eq!(d1, d2);
        assert_eq!(d2, d3);
    }
}

// ============================================================================
// Section 4: Streaming vs One-Shot Tests
// ============================================================================

mod streaming_vs_oneshot {
    use super::*;

    #[test]
    fn streaming_byte_by_byte() {
        let data = b"streaming test data for xxh3-128 algorithm verification";

        let mut hasher = Xxh3_128::new(0);
        for &byte in data.iter() {
            hasher.update(&[byte]);
        }
        let streaming = hasher.finalize();

        let oneshot = Xxh3_128::digest(0, data);
        assert_eq!(streaming, oneshot, "Byte-by-byte streaming should match one-shot");
    }

    #[test]
    fn streaming_two_halves() {
        let data = b"first half|second half of data";
        let midpoint = data.len() / 2;

        let mut hasher = Xxh3_128::new(0);
        hasher.update(&data[..midpoint]);
        hasher.update(&data[midpoint..]);
        let streaming = hasher.finalize();

        let oneshot = Xxh3_128::digest(0, data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn streaming_three_parts() {
        let data = b"part one|part two|part three";
        let third = data.len() / 3;

        let mut hasher = Xxh3_128::new(0);
        hasher.update(&data[..third]);
        hasher.update(&data[third..2*third]);
        hasher.update(&data[2*third..]);
        let streaming = hasher.finalize();

        let oneshot = Xxh3_128::digest(0, data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn streaming_various_chunk_sizes() {
        let data: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
        let oneshot = Xxh3_128::digest(0, &data);

        for chunk_size in [1, 2, 3, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, 100, 1000] {
            let mut hasher = Xxh3_128::new(0);
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

        let mut hasher = Xxh3_128::new(0);
        hasher.update(b"");
        hasher.update(b"test");
        hasher.update(b"");
        hasher.update(b" ");
        hasher.update(b"");
        hasher.update(b"");
        hasher.update(b"data");
        hasher.update(b"");
        let streaming = hasher.finalize();

        let oneshot = Xxh3_128::digest(0, data);
        assert_eq!(streaming, oneshot, "Empty updates interspersed should not affect result");
    }

    #[test]
    fn streaming_clone_and_diverge() {
        let mut hasher1 = Xxh3_128::new(42);
        hasher1.update(b"partial");

        let mut hasher2 = hasher1.clone();

        hasher1.update(b" data A");
        hasher2.update(b" data B");

        let digest1 = hasher1.finalize();
        let digest2 = hasher2.finalize();

        assert_ne!(digest1, digest2, "Different continuations should produce different hashes");

        // Verify cloned hasher produces correct result
        let expected_b = Xxh3_128::digest(42, b"partial data B");
        assert_eq!(digest2, expected_b);

        let expected_a = Xxh3_128::digest(42, b"partial data A");
        assert_eq!(digest1, expected_a);
    }

    #[test]
    fn streaming_clone_at_various_points() {
        let data = b"abcdefghijklmnopqrstuvwxyz";
        let full_digest = Xxh3_128::digest(0, data);

        // Clone after each byte and verify final result
        for i in 0..data.len() {
            let mut hasher = Xxh3_128::new(0);
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
        let oneshot = Xxh3_128::digest(0, &data);

        // Stream in irregular chunk sizes
        let mut hasher = Xxh3_128::new(0);
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
        let oneshot = Xxh3_128::digest(0, &data);

        for chunk_size in [1024, 4096, 32768, 65536] {
            let mut hasher = Xxh3_128::new(0);
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

        let mut hasher = Xxh3_128::new(seed);
        for chunk in data.chunks(5) {
            hasher.update(chunk);
        }
        let streaming = hasher.finalize();

        let oneshot = Xxh3_128::digest(seed, data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn streaming_split_at_all_positions() {
        let data = b"0123456789abcdef"; // 16 bytes
        let expected = Xxh3_128::digest(0, data);

        for split_pos in 0..=data.len() {
            let mut hasher = Xxh3_128::new(0);
            hasher.update(&data[..split_pos]);
            hasher.update(&data[split_pos..]);
            let result = hasher.finalize();
            assert_eq!(
                result, expected,
                "Split at position {} should produce same result",
                split_pos
            );
        }
    }

    #[test]
    fn streaming_multiple_splits() {
        let data = b"The quick brown fox";
        let expected = Xxh3_128::digest(0, data);

        // Split into 5 parts
        let mut hasher = Xxh3_128::new(0);
        hasher.update(b"The ");
        hasher.update(b"qui");
        hasher.update(b"ck ");
        hasher.update(b"brown");
        hasher.update(b" fox");
        assert_eq!(hasher.finalize(), expected);
    }

    #[test]
    fn size_1mb_chunked() {
        // Hash 1MB in 4KB chunks
        let data: Vec<u8> = (0..1024 * 1024).map(|i| (i % 256) as u8).collect();
        let mut hasher = Xxh3_128::new(0);
        for chunk in data.chunks(4096) {
            hasher.update(chunk);
        }
        let chunked = hasher.finalize();

        let oneshot = Xxh3_128::digest(0, &data);
        assert_eq!(chunked, oneshot);
    }
}

// ============================================================================
// Section 5: Seed Variations Tests
// ============================================================================

mod seed_variations {
    use super::*;

    #[test]
    fn seed_zero() {
        let digest = Xxh3_128::digest(0, b"test");
        assert_eq!(digest.len(), 16);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(b"test", 0).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn seed_one() {
        let digest = Xxh3_128::digest(1, b"test");
        assert_ne!(digest, Xxh3_128::digest(0, b"test"), "Seed 1 should differ from seed 0");
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(b"test", 1).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn seed_max_u64() {
        let digest = Xxh3_128::digest(u64::MAX, b"test");
        assert_eq!(digest.len(), 16);
        assert_ne!(digest, Xxh3_128::digest(0, b"test"));
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(b"test", u64::MAX).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn seed_42() {
        let digest = Xxh3_128::digest(42, b"test");
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(b"test", 42).to_le_bytes();
        assert_eq!(digest, expected);
    }

    #[test]
    fn seed_power_of_two() {
        for power in [1u64, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096] {
            let digest = Xxh3_128::digest(power, b"test");
            let expected = xxhash_rust::xxh3::xxh3_128_with_seed(b"test", power).to_le_bytes();
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
            let digest = Xxh3_128::digest(seed, b"test");
            let expected = xxhash_rust::xxh3::xxh3_128_with_seed(b"test", seed).to_le_bytes();
            assert_eq!(digest, expected, "Seed 0x{:016x} mismatch", seed);
        }
    }

    #[test]
    fn different_seeds_produce_different_hashes() {
        let data = b"seed test data";
        let seeds = [0u64, 1, 42, 256, 65535, 0x12345678, u32::MAX as u64, u64::MAX];

        let digests: Vec<_> = seeds.iter().map(|&seed| Xxh3_128::digest(seed, data)).collect();

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

        let oneshot = Xxh3_128::digest(seed, data);

        let mut hasher = Xxh3_128::new(seed);
        hasher.update(data);
        let streaming = hasher.finalize();

        assert_eq!(oneshot, streaming);
    }

    #[test]
    fn seed_via_trait_interface() {
        let seed = 12345u64;
        let data = b"trait interface test";

        // Using StrongDigest trait
        let hasher: Xxh3_128 = StrongDigest::with_seed(seed);
        let mut h = hasher;
        h.update(data);
        let trait_digest = h.finalize();

        let direct_digest = Xxh3_128::digest(seed, data);
        assert_eq!(trait_digest, direct_digest);
    }

    #[test]
    fn seed_same_input_same_seed_same_output() {
        let seed = 999999u64;
        let data = b"determinism test";

        let d1 = Xxh3_128::digest(seed, data);
        let d2 = Xxh3_128::digest(seed, data);
        let d3 = Xxh3_128::digest(seed, data);

        assert_eq!(d1, d2);
        assert_eq!(d2, d3);
    }
}

// ============================================================================
// Section 6: Edge Cases and Boundary Conditions
// ============================================================================

mod edge_cases {
    use super::*;

    #[test]
    fn digest_output_is_16_bytes() {
        let digest = Xxh3_128::digest(0, b"test");
        assert_eq!(digest.len(), 16);
        assert_eq!(digest.as_ref().len(), 16);
    }

    #[test]
    fn deterministic_output() {
        let data = b"determinism test";
        let d1 = Xxh3_128::digest(0, data);
        let d2 = Xxh3_128::digest(0, data);
        let d3 = Xxh3_128::digest(0, data);
        assert_eq!(d1, d2);
        assert_eq!(d2, d3);
    }

    #[test]
    fn different_inputs_different_outputs() {
        let d1 = Xxh3_128::digest(0, b"input1");
        let d2 = Xxh3_128::digest(0, b"input2");
        assert_ne!(d1, d2);
    }

    #[test]
    fn similar_inputs_different_outputs() {
        let d1 = Xxh3_128::digest(0, b"test");
        let d2 = Xxh3_128::digest(0, b"Test"); // Different case
        let d3 = Xxh3_128::digest(0, b"test "); // Trailing space
        let d4 = Xxh3_128::digest(0, b" test"); // Leading space

        assert_ne!(d1, d2);
        assert_ne!(d1, d3);
        assert_ne!(d1, d4);
        assert_ne!(d2, d3);
        assert_ne!(d2, d4);
        assert_ne!(d3, d4);
    }

    #[test]
    fn all_zero_input_various_sizes() {
        for size in [0, 1, 16, 64, 128, 1024] {
            let data = vec![0u8; size];
            let digest = Xxh3_128::digest(0, &data);
            assert_eq!(digest.len(), 16, "Size {}: digest should be 16 bytes", size);
        }
    }

    #[test]
    fn all_ones_input_various_sizes() {
        for size in [0, 1, 16, 64, 128, 1024] {
            let data = vec![0xFFu8; size];
            let digest = Xxh3_128::digest(0, &data);
            assert_eq!(digest.len(), 16, "Size {}: digest should be 16 bytes", size);
        }
    }

    #[test]
    fn binary_data_with_null_bytes() {
        let data_with_null = b"before\x00after";
        let data_without_null = b"beforeafter";

        let d1 = Xxh3_128::digest(0, data_with_null);
        let d2 = Xxh3_128::digest(0, data_without_null);

        // Null byte should affect the hash
        assert_ne!(d1, d2);
    }

    #[test]
    fn all_byte_values() {
        // Test with data containing all possible byte values
        let data: Vec<u8> = (0..=255).collect();
        let digest = Xxh3_128::digest(0, &data);
        assert_eq!(digest.len(), 16);

        // Verify streaming matches
        let mut hasher = Xxh3_128::new(0);
        hasher.update(&data);
        assert_eq!(hasher.finalize(), digest);
    }

    #[test]
    fn repeated_patterns() {
        // Test with repeated patterns of various sizes
        let patterns: &[&[u8]] = &[
            &[0xAA; 1000],
            &[0x00; 1000],
            &[0xFF; 1000],
            &[0x55; 1000],
        ];

        let mut digests = Vec::new();
        for pattern in patterns {
            let digest = Xxh3_128::digest(0, *pattern);
            assert_eq!(digest.len(), 16);
            digests.push(digest);
        }

        // All patterns should produce unique digests
        for i in 0..digests.len() {
            for j in (i + 1)..digests.len() {
                assert_ne!(digests[i], digests[j], "Patterns {} and {} should differ", i, j);
            }
        }
    }

    #[test]
    fn alternating_patterns() {
        let pattern1: Vec<u8> = (0..1000).map(|i| if i % 2 == 0 { 0xAA } else { 0x55 }).collect();
        let pattern2: Vec<u8> = (0..1000).map(|i| if i % 2 == 0 { 0x55 } else { 0xAA }).collect();

        let d1 = Xxh3_128::digest(0, &pattern1);
        let d2 = Xxh3_128::digest(0, &pattern2);

        assert_ne!(d1, d2);
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
            let digest = Xxh3_128::digest(0, bytes);
            let expected = xxhash_rust::xxh3::xxh3_128_with_seed(bytes, 0).to_le_bytes();
            assert_eq!(digest, expected, "UTF-8 string {:?} mismatch", s);
        }
    }

    #[test]
    fn very_long_repeated_null() {
        let nulls = vec![0u8; 100000];
        let digest = Xxh3_128::digest(0, &nulls);
        let expected = xxhash_rust::xxh3::xxh3_128_with_seed(&nulls, 0).to_le_bytes();
        assert_eq!(digest, expected);
    }
}

// ============================================================================
// Section 7: StrongDigest Trait Tests
// ============================================================================

mod strong_digest_trait {
    use super::*;

    #[test]
    fn trait_new_matches_inherent_new() {
        let mut trait_hasher: Xxh3_128 = StrongDigest::with_seed(0);
        trait_hasher.update(b"trait test");
        let trait_result = trait_hasher.finalize();

        let mut inherent_hasher = Xxh3_128::new(0);
        inherent_hasher.update(b"trait test");
        let inherent_result = inherent_hasher.finalize();

        assert_eq!(trait_result, inherent_result);
    }

    #[test]
    fn trait_digest_matches_inherent_digest() {
        let trait_result = <Xxh3_128 as StrongDigest>::digest_with_seed(0, b"quick test");
        let inherent_result = Xxh3_128::digest(0, b"quick test");
        assert_eq!(trait_result, inherent_result);
    }

    #[test]
    fn digest_len_constant() {
        assert_eq!(Xxh3_128::DIGEST_LEN, 16);
    }

    #[test]
    fn with_seed_matches_new() {
        let seed = 12345u64;
        let mut seeded: Xxh3_128 = StrongDigest::with_seed(seed);
        seeded.update(b"test");
        let seeded_result = seeded.finalize();

        let mut new_hasher = Xxh3_128::new(seed);
        new_hasher.update(b"test");
        let new_result = new_hasher.finalize();

        assert_eq!(seeded_result, new_result);
    }

    #[test]
    fn digest_with_seed_matches_digest() {
        let seed = 99999u64;
        let seeded = <Xxh3_128 as StrongDigest>::digest_with_seed(seed, b"test");
        let direct = Xxh3_128::digest(seed, b"test");
        assert_eq!(seeded, direct);
    }
}

// ============================================================================
// Section 8: Verification Tests
// ============================================================================

mod verification {
    use super::*;

    #[test]
    fn verify_128bit_output_length() {
        assert_eq!(Xxh3_128::DIGEST_LEN, 16, "XXH3-128 DIGEST_LEN should be 16 bytes");

        let digest = Xxh3_128::digest(0, b"test");
        assert_eq!(digest.len(), 16, "Digest array should have 16 elements");
        assert_eq!(digest.as_ref().len(), 16, "Digest as ref should have 16 bytes");
    }

    #[test]
    fn verify_deterministic_output() {
        let data = b"determinism test";
        let d1 = Xxh3_128::digest(0, data);
        let d2 = Xxh3_128::digest(0, data);
        let d3 = Xxh3_128::digest(0, data);
        assert_eq!(d1, d2);
        assert_eq!(d2, d3);
    }

    #[test]
    fn verify_different_inputs_different_outputs() {
        let d1 = Xxh3_128::digest(0, b"input1");
        let d2 = Xxh3_128::digest(0, b"input2");
        assert_ne!(d1, d2);
    }

    #[test]
    fn verify_little_endian_encoding() {
        // The implementation returns little-endian bytes
        let digest = Xxh3_128::digest(0, b"test");
        let from_le = u128::from_le_bytes(digest);
        let from_be = u128::from_be_bytes(digest);

        // These should be different (unless palindromic, which is unlikely)
        assert_ne!(
            from_le, from_be,
            "Little-endian and big-endian interpretations should differ"
        );

        // Verify against xxhash-rust reference
        let expected_hash = xxhash_rust::xxh3::xxh3_128_with_seed(b"test", 0);
        assert_eq!(from_le, expected_hash, "Should match xxhash-rust when read as little-endian");
    }

    #[test]
    fn verify_avalanche_effect() {
        // Changing a single bit should produce a very different hash
        let data1 = vec![0u8; 100];
        let mut data2 = data1.clone();
        data2[50] = 1; // Change single bit

        let digest1 = Xxh3_128::digest(0, &data1);
        let digest2 = Xxh3_128::digest(0, &data2);

        let hash1 = u128::from_le_bytes(digest1);
        let hash2 = u128::from_le_bytes(digest2);

        // Count differing bits (should be roughly half due to avalanche effect)
        let diff = (hash1 ^ hash2).count_ones();
        assert!(
            diff >= 40,
            "Avalanche effect: {} bits differ, expected >= 40",
            diff
        );
    }

    #[test]
    fn verify_all_bits_can_be_set() {
        // Hash many different inputs to verify all bit positions can be set
        let mut or_accumulator = 0u128;

        for i in 0..10000 {
            let data = format!("input_{}", i);
            let digest = Xxh3_128::digest(0, data.as_bytes());
            let hash = u128::from_le_bytes(digest);
            or_accumulator |= hash;
        }

        // After many hashes, we should see all bits set at least once
        assert_eq!(
            or_accumulator.count_ones(),
            128,
            "All 128 bits should be exercised by various inputs"
        );
    }

    #[test]
    fn verify_bit_distribution() {
        // Check that bits are reasonably distributed
        let mut bit_counts = [0u32; 128];

        for i in 0..10000 {
            let data = format!("distribution_test_{}", i);
            let digest = Xxh3_128::digest(0, data.as_bytes());
            let hash = u128::from_le_bytes(digest);

            for bit in 0..128 {
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
    fn compatibility_with_xxhash_rust_empty() {
        let our_digest = Xxh3_128::digest(0, b"");
        let reference = xxhash_rust::xxh3::xxh3_128_with_seed(b"", 0).to_le_bytes();
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
                let our_digest = Xxh3_128::digest(seed, input);
                let reference = xxhash_rust::xxh3::xxh3_128_with_seed(input, seed).to_le_bytes();
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
                let our_digest = Xxh3_128::digest(seed, &data);
                let reference = xxhash_rust::xxh3::xxh3_128_with_seed(&data, seed).to_le_bytes();
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
            let mut our_hasher = Xxh3_128::new(seed);
            our_hasher.update(data);
            let our_digest = our_hasher.finalize();

            let reference = xxhash_rust::xxh3::xxh3_128_with_seed(data, seed).to_le_bytes();
            assert_eq!(our_digest, reference, "Streaming mismatch for seed {}", seed);
        }
    }
}
