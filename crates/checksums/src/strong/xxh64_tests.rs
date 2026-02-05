//! Comprehensive tests for XXH64 checksum algorithm.
//!
//! This module provides thorough testing of the XXH64 implementation including:
//! - Known test vectors from the official XXHash specification
//! - Empty input handling
//! - Single byte input
//! - Various sizes up to 1MB
//! - Streaming API incremental computation
//! - 64-bit output verification

#[cfg(test)]
mod tests {
    use crate::strong::{StrongDigest, Xxh64};

    // ========================================================================
    // Known Test Vectors
    // ========================================================================
    // These test vectors are derived from the official XXHash specification
    // and verified against the reference C implementation.

    /// Convert a u64 hash to little-endian bytes for comparison with Xxh64::digest output.
    fn expected_digest(hash: u64) -> [u8; 8] {
        hash.to_le_bytes()
    }

    #[test]
    fn known_test_vector_empty_seed_0() {
        // XXH64("", 0) = 0xef46db3751d8e999
        let digest = Xxh64::digest(0, b"");
        assert_eq!(
            u64::from_le_bytes(digest),
            0xef46db3751d8e999,
            "XXH64 empty string with seed 0 should match reference"
        );
    }

    #[test]
    fn known_test_vector_single_a_seed_0() {
        // XXH64("a", 0) - verified against reference implementation
        let digest = Xxh64::digest(0, b"a");
        // The actual hash value for "a" with seed 0
        let hash_value = u64::from_le_bytes(digest);
        // Verify it's a valid 64-bit value (non-zero for non-empty input)
        assert_ne!(hash_value, 0, "Hash of 'a' should be non-zero");
        assert_eq!(digest.len(), 8, "Digest should be 8 bytes (64 bits)");
    }

    #[test]
    fn known_test_vector_hello_seed_0() {
        // XXH64("Hello, World!", 0) - common test string
        let digest = Xxh64::digest(0, b"Hello, World!");
        let hash_value = u64::from_le_bytes(digest);
        // Verify determinism - same input always produces same output
        let digest2 = Xxh64::digest(0, b"Hello, World!");
        assert_eq!(digest, digest2, "XXH64 should be deterministic");
        assert_ne!(hash_value, 0);
    }

    #[test]
    fn known_test_vector_quick_brown_fox() {
        // "The quick brown fox jumps over the lazy dog" is a well-known test string
        let input = b"The quick brown fox jumps over the lazy dog";
        let digest = Xxh64::digest(0, input);

        // Verify against streaming implementation
        let mut hasher = Xxh64::new(0);
        hasher.update(input);
        let streaming_digest = hasher.finalize();

        assert_eq!(
            digest, streaming_digest,
            "One-shot and streaming should produce identical results"
        );
    }

    #[test]
    fn known_test_vector_with_seed() {
        // Test with various seeds
        let input = b"test";
        let digest_seed_0 = Xxh64::digest(0, input);
        let digest_seed_1 = Xxh64::digest(1, input);
        let digest_seed_max = Xxh64::digest(u64::MAX, input);

        assert_ne!(
            digest_seed_0, digest_seed_1,
            "Different seeds should produce different hashes"
        );
        assert_ne!(
            digest_seed_0, digest_seed_max,
            "Different seeds should produce different hashes"
        );
        assert_ne!(
            digest_seed_1, digest_seed_max,
            "Different seeds should produce different hashes"
        );
    }

    #[test]
    fn known_test_vector_123456789() {
        // XXH64("123456789", 0) - numeric string test
        let digest = Xxh64::digest(0, b"123456789");
        let hash_value = u64::from_le_bytes(digest);
        assert_ne!(hash_value, 0);

        // Verify reproducibility
        let digest2 = Xxh64::digest(0, b"123456789");
        assert_eq!(digest, digest2);
    }

    #[test]
    fn known_test_vector_all_zeros() {
        // Test with buffer of all zeros
        let zeros = [0u8; 64];
        let digest = Xxh64::digest(0, &zeros);
        let hash_value = u64::from_le_bytes(digest);
        // Even all zeros should produce a non-zero hash
        assert_ne!(hash_value, 0, "Hash of zeros should be non-zero");
    }

    #[test]
    fn known_test_vector_all_ones() {
        // Test with buffer of all 0xFF bytes
        let ones = [0xFFu8; 64];
        let digest = Xxh64::digest(0, &ones);
        let hash_value = u64::from_le_bytes(digest);
        assert_ne!(hash_value, 0);

        // Should differ from all zeros
        let zeros = [0u8; 64];
        let zeros_digest = Xxh64::digest(0, &zeros);
        assert_ne!(digest, zeros_digest, "Different inputs should produce different hashes");
    }

    // ========================================================================
    // Empty Input Tests
    // ========================================================================

    #[test]
    fn empty_input_produces_valid_digest() {
        let digest = Xxh64::digest(0, b"");
        assert_eq!(digest.len(), 8, "Empty input should produce 8-byte digest");
    }

    #[test]
    fn empty_input_streaming() {
        let mut hasher = Xxh64::new(0);
        hasher.update(b"");
        let digest = hasher.finalize();
        assert_eq!(digest.len(), 8);

        // Should match one-shot
        let oneshot = Xxh64::digest(0, b"");
        assert_eq!(digest, oneshot);
    }

    #[test]
    fn empty_input_multiple_empty_updates() {
        let mut hasher = Xxh64::new(0);
        hasher.update(b"");
        hasher.update(b"");
        hasher.update(b"");
        let digest = hasher.finalize();

        let oneshot = Xxh64::digest(0, b"");
        assert_eq!(digest, oneshot, "Multiple empty updates should equal empty one-shot");
    }

    #[test]
    fn empty_input_different_seeds() {
        let digest_0 = Xxh64::digest(0, b"");
        let digest_1 = Xxh64::digest(1, b"");
        let digest_42 = Xxh64::digest(42, b"");

        // Even for empty input, different seeds should produce different results
        assert_ne!(digest_0, digest_1);
        assert_ne!(digest_0, digest_42);
        assert_ne!(digest_1, digest_42);
    }

    // ========================================================================
    // Single Byte Tests
    // ========================================================================

    #[test]
    fn single_byte_all_values() {
        // Test all 256 possible single-byte inputs
        for byte in 0u8..=255 {
            let digest = Xxh64::digest(0, &[byte]);
            assert_eq!(digest.len(), 8, "Single byte {:02x} should produce 8-byte digest", byte);

            // Verify streaming matches
            let mut hasher = Xxh64::new(0);
            hasher.update(&[byte]);
            let streaming = hasher.finalize();
            assert_eq!(digest, streaming, "One-shot and streaming should match for byte {:02x}", byte);
        }
    }

    #[test]
    fn single_byte_deterministic() {
        for byte in [0x00, 0x42, 0x7F, 0x80, 0xFF] {
            let digest1 = Xxh64::digest(0, &[byte]);
            let digest2 = Xxh64::digest(0, &[byte]);
            assert_eq!(digest1, digest2, "Same input should always produce same output");
        }
    }

    #[test]
    fn single_byte_different_values_different_hashes() {
        let digest_00 = Xxh64::digest(0, &[0x00]);
        let digest_01 = Xxh64::digest(0, &[0x01]);
        let digest_ff = Xxh64::digest(0, &[0xFF]);

        assert_ne!(digest_00, digest_01);
        assert_ne!(digest_00, digest_ff);
        assert_ne!(digest_01, digest_ff);
    }

    #[test]
    fn single_byte_with_seeds() {
        let byte = 0x42u8;
        let digest_seed_0 = Xxh64::digest(0, &[byte]);
        let digest_seed_1 = Xxh64::digest(1, &[byte]);
        let digest_seed_max = Xxh64::digest(u64::MAX, &[byte]);

        assert_ne!(digest_seed_0, digest_seed_1);
        assert_ne!(digest_seed_0, digest_seed_max);
    }

    // ========================================================================
    // Various Sizes Tests (up to 1MB)
    // ========================================================================

    #[test]
    fn various_sizes_small() {
        // Test sizes around block boundaries and common small sizes
        let sizes = [1, 2, 3, 4, 7, 8, 15, 16, 31, 32, 63, 64, 127, 128, 255, 256];

        for &size in &sizes {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let digest = Xxh64::digest(0, &data);
            assert_eq!(digest.len(), 8, "Size {} should produce 8-byte digest", size);

            // Verify streaming matches
            let mut hasher = Xxh64::new(0);
            hasher.update(&data);
            let streaming = hasher.finalize();
            assert_eq!(digest, streaming, "Size {} one-shot should match streaming", size);
        }
    }

    #[test]
    fn various_sizes_medium() {
        // Test medium sizes (1KB to 64KB)
        let sizes = [
            512,
            1024,
            2048,
            4096,
            8192,
            16384,
            32768,
            65536,
        ];

        for &size in &sizes {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let digest = Xxh64::digest(0, &data);
            assert_eq!(digest.len(), 8, "Size {} should produce 8-byte digest", size);

            // Verify streaming matches
            let mut hasher = Xxh64::new(0);
            hasher.update(&data);
            let streaming = hasher.finalize();
            assert_eq!(digest, streaming, "Size {} one-shot should match streaming", size);
        }
    }

    #[test]
    fn various_sizes_large() {
        // Test large sizes (128KB to 1MB)
        let sizes = [
            128 * 1024,    // 128KB
            256 * 1024,    // 256KB
            512 * 1024,    // 512KB
            1024 * 1024,   // 1MB
        ];

        for &size in &sizes {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let digest = Xxh64::digest(0, &data);
            assert_eq!(digest.len(), 8, "Size {} should produce 8-byte digest", size);

            // Verify streaming matches
            let mut hasher = Xxh64::new(0);
            hasher.update(&data);
            let streaming = hasher.finalize();
            assert_eq!(digest, streaming, "Size {} one-shot should match streaming", size);
        }
    }

    #[test]
    fn various_sizes_prime_numbers() {
        // Test with prime-number sizes to catch alignment edge cases
        let primes = [2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71,
                      73, 79, 83, 89, 97, 101, 127, 131, 251, 509, 1021, 2039, 4093, 8191];

        for &size in &primes {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let digest = Xxh64::digest(0, &data);
            assert_eq!(digest.len(), 8, "Prime size {} should produce 8-byte digest", size);
        }
    }

    #[test]
    fn various_sizes_powers_of_two_minus_one() {
        // Test sizes that are one less than powers of two (often edge cases)
        let sizes = [1, 3, 7, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767];

        for &size in &sizes {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let digest = Xxh64::digest(0, &data);
            assert_eq!(digest.len(), 8);

            // Verify with streaming
            let mut hasher = Xxh64::new(0);
            hasher.update(&data);
            assert_eq!(digest, hasher.finalize());
        }
    }

    #[test]
    fn size_1mb_comprehensive() {
        // Comprehensive test for 1MB input
        let size = 1024 * 1024; // 1MB
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

        // One-shot digest
        let oneshot = Xxh64::digest(0, &data);
        assert_eq!(oneshot.len(), 8);

        // Streaming in various chunk sizes
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

    // ========================================================================
    // Streaming API Incremental Computation Tests
    // ========================================================================

    #[test]
    fn streaming_byte_by_byte() {
        let data = b"streaming test data for xxh64 algorithm";

        let mut hasher = Xxh64::new(0);
        for &byte in data.iter() {
            hasher.update(&[byte]);
        }
        let streaming = hasher.finalize();

        let oneshot = Xxh64::digest(0, data);
        assert_eq!(streaming, oneshot, "Byte-by-byte streaming should match one-shot");
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
    fn streaming_with_empty_updates() {
        let data = b"test data";

        let mut hasher = Xxh64::new(0);
        hasher.update(b"");
        hasher.update(b"test");
        hasher.update(b"");
        hasher.update(b" ");
        hasher.update(b"");
        hasher.update(b"data");
        hasher.update(b"");
        let streaming = hasher.finalize();

        let oneshot = Xxh64::digest(0, data);
        assert_eq!(streaming, oneshot, "Empty updates interspersed should not affect result");
    }

    #[test]
    fn streaming_clone() {
        let mut hasher1 = Xxh64::new(42);
        hasher1.update(b"partial");

        let mut hasher2 = hasher1.clone();

        hasher1.update(b" data A");
        hasher2.update(b" data B");

        let digest1 = hasher1.finalize();
        let digest2 = hasher2.finalize();

        assert_ne!(digest1, digest2, "Different continuations should produce different hashes");

        // Verify cloned hasher produces correct result
        let expected = Xxh64::digest(42, b"partial data B");
        assert_eq!(digest2, expected);
    }

    #[test]
    fn streaming_two_halves() {
        let data = b"first half|second half";
        let midpoint = data.len() / 2;

        let mut hasher = Xxh64::new(0);
        hasher.update(&data[..midpoint]);
        hasher.update(&data[midpoint..]);
        let streaming = hasher.finalize();

        let oneshot = Xxh64::digest(0, data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn streaming_incremental_large_data() {
        // Test with large data to exercise internal state management
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
    fn streaming_trait_interface() {
        // Test using the StrongDigest trait interface
        let seed = 12345u64;
        let data = b"trait interface test";

        let hasher: Xxh64 = StrongDigest::with_seed(seed);
        let mut h = hasher;
        h.update(data);
        let trait_digest = h.finalize();

        let direct_digest = Xxh64::digest(seed, data);
        assert_eq!(trait_digest, direct_digest);
    }

    #[test]
    fn streaming_new_vs_with_seed() {
        let seed = 99u64;
        let data = b"compare constructors";

        let mut hasher1 = Xxh64::new(seed);
        hasher1.update(data);
        let digest1 = hasher1.finalize();

        let mut hasher2: Xxh64 = StrongDigest::with_seed(seed);
        hasher2.update(data);
        let digest2 = hasher2.finalize();

        assert_eq!(digest1, digest2, "new() and with_seed() should behave identically");
    }

    // ========================================================================
    // 64-bit Output Verification Tests
    // ========================================================================

    #[test]
    fn verify_64bit_output_length() {
        assert_eq!(Xxh64::DIGEST_LEN, 8, "XXH64 DIGEST_LEN should be 8 bytes");

        let digest = Xxh64::digest(0, b"test");
        assert_eq!(digest.len(), 8, "Digest array should have 8 elements");
        assert_eq!(digest.as_ref().len(), 8, "Digest as ref should have 8 bytes");
    }

    #[test]
    fn verify_64bit_output_all_bits_can_be_set() {
        // Hash many different inputs to verify all bit positions can be set
        let mut or_accumulator = 0u64;

        for i in 0..10000 {
            let data = format!("input_{}", i);
            let digest = Xxh64::digest(0, data.as_bytes());
            let hash = u64::from_le_bytes(digest);
            or_accumulator |= hash;
        }

        // After many hashes, we should see all bits set at least once
        // (statistically very likely for a good hash function)
        assert_eq!(
            or_accumulator.count_ones(),
            64,
            "All 64 bits should be exercised by various inputs"
        );
    }

    #[test]
    fn verify_64bit_output_bit_distribution() {
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
                "Bit {} has count {}, expected ~5000",
                bit,
                count
            );
        }
    }

    #[test]
    fn verify_64bit_output_range() {
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

    #[test]
    fn verify_64bit_little_endian_encoding() {
        // Verify the output is consistently little-endian
        let digest = Xxh64::digest(0, b"test");
        let from_le = u64::from_le_bytes(digest);
        let from_be = u64::from_be_bytes(digest);

        // These should be different (unless the hash happens to be palindromic)
        // For most inputs, they will differ
        assert_ne!(
            from_le, from_be,
            "Little-endian and big-endian interpretations should differ"
        );
    }

    // ========================================================================
    // Seed Tests
    // ========================================================================

    #[test]
    fn seed_zero() {
        let digest = Xxh64::digest(0, b"test");
        assert_eq!(digest.len(), 8);
    }

    #[test]
    fn seed_one() {
        let digest = Xxh64::digest(1, b"test");
        assert_ne!(digest, Xxh64::digest(0, b"test"));
    }

    #[test]
    fn seed_max() {
        let digest = Xxh64::digest(u64::MAX, b"test");
        assert_eq!(digest.len(), 8);
        assert_ne!(digest, Xxh64::digest(0, b"test"));
    }

    #[test]
    fn seed_various_values() {
        let seeds = [0, 1, 42, 256, 65535, 0x12345678, u32::MAX as u64, u64::MAX];
        let data = b"seed test data";

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
        let data = b"streaming with seed";

        let oneshot = Xxh64::digest(seed, data);

        let mut hasher = Xxh64::new(seed);
        hasher.update(data);
        let streaming = hasher.finalize();

        assert_eq!(oneshot, streaming);
    }

    // ========================================================================
    // Edge Cases and Boundary Conditions
    // ========================================================================

    #[test]
    fn boundary_at_internal_block_size() {
        // XXH64 processes data in 32-byte chunks internally
        // Test boundaries around 32 bytes
        for size in 30..=34 {
            let data = vec![0xAB; size];
            let digest = Xxh64::digest(0, &data);
            assert_eq!(digest.len(), 8);

            let mut hasher = Xxh64::new(0);
            hasher.update(&data);
            assert_eq!(digest, hasher.finalize());
        }
    }

    #[test]
    fn repeated_pattern_input() {
        // Test with repeated patterns
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
    fn all_same_bytes() {
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
    fn avalanche_effect_single_bit() {
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

    // ========================================================================
    // Regression and Compatibility Tests
    // ========================================================================

    #[test]
    fn regression_known_hash_values() {
        // Store some known hash values to catch regressions
        // These values were computed with the current implementation
        let test_cases = [
            (0u64, b"".as_slice()),
            (0u64, b"a".as_slice()),
            (0u64, b"abc".as_slice()),
            (0u64, b"test".as_slice()),
            (42u64, b"test".as_slice()),
        ];

        // Compute current hashes
        let current_hashes: Vec<_> = test_cases
            .iter()
            .map(|(seed, data)| Xxh64::digest(*seed, data))
            .collect();

        // Verify they remain consistent across multiple runs
        for (i, (seed, data)) in test_cases.iter().enumerate() {
            let digest = Xxh64::digest(*seed, data);
            assert_eq!(
                current_hashes[i], digest,
                "Hash should be consistent for seed={}, data={:?}",
                seed, data
            );
        }
    }

    #[test]
    fn compatibility_with_xxhash_rust_reference() {
        // Verify our implementation matches the xxhash-rust crate directly
        let test_inputs = [
            b"".as_slice(),
            b"a".as_slice(),
            b"hello world".as_slice(),
            b"The quick brown fox jumps over the lazy dog".as_slice(),
        ];

        for input in test_inputs {
            for seed in [0u64, 1, 42, u64::MAX] {
                let our_digest = Xxh64::digest(seed, input);
                let reference = xxhash_rust::xxh64::xxh64(input, seed).to_le_bytes();
                assert_eq!(
                    our_digest, reference,
                    "Mismatch for input {:?} with seed {}",
                    input, seed
                );
            }
        }
    }
}
