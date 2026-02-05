//! Comprehensive MD4 checksum tests.
//!
//! This test module validates the MD4 implementation against:
//! 1. RFC 1320 official test vectors
//! 2. Edge cases (empty input, single byte)
//! 3. Various input sizes (1 byte, 55 bytes, 56 bytes, 64 bytes boundary cases)
//! 4. Large inputs up to 1MB
//! 5. Incremental hashing (update multiple times)

use checksums::strong::{Md4, StrongDigest};

/// Convert a byte slice to a lowercase hex string.
fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut out, "{byte:02x}").expect("write! to String cannot fail");
    }
    out
}

// ============================================================================
// RFC 1320 Official Test Vectors
// ============================================================================

/// RFC 1320 Section A.5 defines the official MD4 test suite.
/// These vectors are authoritative for validating MD4 implementations.
/// Reference: https://www.rfc-editor.org/rfc/rfc1320
mod rfc1320_test_vectors {
    use super::*;

    #[test]
    fn rfc1320_empty_string() {
        // MD4("") = 31d6cfe0d16ae931b73c59d7e0c089c0
        let digest = Md4::digest(b"");
        assert_eq!(to_hex(&digest), "31d6cfe0d16ae931b73c59d7e0c089c0");
    }

    #[test]
    fn rfc1320_single_char_a() {
        // MD4("a") = bde52cb31de33e46245e05fbdbd6fb24
        let digest = Md4::digest(b"a");
        assert_eq!(to_hex(&digest), "bde52cb31de33e46245e05fbdbd6fb24");
    }

    #[test]
    fn rfc1320_abc() {
        // MD4("abc") = a448017aaf21d8525fc10ae87aa6729d
        let digest = Md4::digest(b"abc");
        assert_eq!(to_hex(&digest), "a448017aaf21d8525fc10ae87aa6729d");
    }

    #[test]
    fn rfc1320_message_digest() {
        // MD4("message digest") = d9130a8164549fe818874806e1c7014b
        let digest = Md4::digest(b"message digest");
        assert_eq!(to_hex(&digest), "d9130a8164549fe818874806e1c7014b");
    }

    #[test]
    fn rfc1320_lowercase_alphabet() {
        // MD4("abcdefghijklmnopqrstuvwxyz") = d79e1c308aa5bbcdeea8ed63df412da9
        let digest = Md4::digest(b"abcdefghijklmnopqrstuvwxyz");
        assert_eq!(to_hex(&digest), "d79e1c308aa5bbcdeea8ed63df412da9");
    }

    #[test]
    fn rfc1320_alphanumeric_mixed_case() {
        // MD4("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789")
        // = 043f8582f241db351ce627e153e7f0e4
        let digest = Md4::digest(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789");
        assert_eq!(to_hex(&digest), "043f8582f241db351ce627e153e7f0e4");
    }

    #[test]
    fn rfc1320_numeric_sequence() {
        // MD4("12345678901234567890123456789012345678901234567890123456789012345678901234567890")
        // = e33b4ddc9c38f2199c3e7b164fcc0536
        let digest = Md4::digest(
            b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
        );
        assert_eq!(to_hex(&digest), "e33b4ddc9c38f2199c3e7b164fcc0536");
    }
}

// ============================================================================
// Empty Input Tests
// ============================================================================

mod empty_input {
    use super::*;

    #[test]
    fn empty_slice_produces_known_digest() {
        let digest = Md4::digest(b"");
        assert_eq!(to_hex(&digest), "31d6cfe0d16ae931b73c59d7e0c089c0");
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn empty_streaming_produces_same_digest() {
        let hasher = Md4::new();
        // No update calls - immediately finalize
        let digest = hasher.finalize();
        assert_eq!(to_hex(&digest), "31d6cfe0d16ae931b73c59d7e0c089c0");
    }

    #[test]
    fn empty_streaming_with_empty_updates() {
        let mut hasher = Md4::new();
        hasher.update(&[]);
        hasher.update(&[]);
        hasher.update(&[]);
        let digest = hasher.finalize();
        assert_eq!(to_hex(&digest), "31d6cfe0d16ae931b73c59d7e0c089c0");
    }

    #[test]
    fn empty_one_shot_equals_streaming() {
        let one_shot = Md4::digest(b"");
        let streaming = Md4::new().finalize();
        assert_eq!(one_shot, streaming);
    }
}

// ============================================================================
// Various Input Sizes Tests
// ============================================================================

mod various_sizes {
    use super::*;

    /// Helper to generate deterministic test data.
    fn generate_data(size: usize) -> Vec<u8> {
        (0..size).map(|i| (i % 256) as u8).collect()
    }

    // 1 byte
    #[test]
    fn size_1_byte() {
        let data = generate_data(1);
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
        // Verify streaming matches
        let mut hasher = Md4::new();
        hasher.update(&data);
        assert_eq!(hasher.finalize(), digest);
    }

    // 55 bytes - maximum that fits in one block with padding
    // (64 - 8 length bytes - 1 padding byte = 55)
    #[test]
    fn size_55_bytes() {
        let data = generate_data(55);
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
        // Verify streaming matches
        let mut hasher = Md4::new();
        hasher.update(&data);
        assert_eq!(hasher.finalize(), digest);
    }

    // 56 bytes - exactly at padding boundary, requires extra block
    #[test]
    fn size_56_bytes() {
        let data = generate_data(56);
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
        // Verify streaming matches
        let mut hasher = Md4::new();
        hasher.update(&data);
        assert_eq!(hasher.finalize(), digest);
    }

    // 64 bytes - exactly one full MD4 block
    #[test]
    fn size_64_bytes() {
        let data = generate_data(64);
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
        // Verify streaming matches
        let mut hasher = Md4::new();
        hasher.update(&data);
        assert_eq!(hasher.finalize(), digest);
    }

    // Additional boundary cases
    #[test]
    fn size_63_bytes() {
        let data = generate_data(63);
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_65_bytes() {
        let data = generate_data(65);
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_127_bytes() {
        let data = generate_data(127);
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_128_bytes() {
        // Two full blocks
        let data = generate_data(128);
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_129_bytes() {
        let data = generate_data(129);
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_256_bytes() {
        let data = generate_data(256);
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn sizes_near_block_boundaries() {
        // Test sizes around 64-byte block boundaries
        for offset in [-3_i32, -2, -1, 0, 1, 2, 3] {
            for multiplier in [1, 2, 4, 8, 16] {
                let base_size = 64 * multiplier;
                let size = (base_size + offset).max(0) as usize;
                let data = generate_data(size);

                let oneshot = Md4::digest(&data);
                let mut hasher = Md4::new();
                hasher.update(&data);
                let streaming = hasher.finalize();

                assert_eq!(
                    oneshot, streaming,
                    "Mismatch at size {size} (base={base_size}, offset={offset})"
                );
            }
        }
    }

    // Test critical padding boundaries
    #[test]
    fn padding_boundary_54_bytes() {
        let data = generate_data(54);
        let oneshot = Md4::digest(&data);
        let mut streaming = Md4::new();
        streaming.update(&data);
        assert_eq!(oneshot, streaming.finalize());
    }

    #[test]
    fn padding_boundary_57_bytes() {
        let data = generate_data(57);
        let oneshot = Md4::digest(&data);
        let mut streaming = Md4::new();
        streaming.update(&data);
        assert_eq!(oneshot, streaming.finalize());
    }

    #[test]
    fn padding_boundary_119_bytes() {
        // 119 = 2*64 - 8 - 1 (boundary for 2 blocks)
        let data = generate_data(119);
        let oneshot = Md4::digest(&data);
        let mut streaming = Md4::new();
        streaming.update(&data);
        assert_eq!(oneshot, streaming.finalize());
    }

    #[test]
    fn padding_boundary_120_bytes() {
        // 120 = 2*64 - 8 (requires 3rd block for padding)
        let data = generate_data(120);
        let oneshot = Md4::digest(&data);
        let mut streaming = Md4::new();
        streaming.update(&data);
        assert_eq!(oneshot, streaming.finalize());
    }
}

// ============================================================================
// Large Input Tests
// ============================================================================

mod large_inputs {
    use super::*;

    /// Helper to generate deterministic test data.
    fn generate_data(size: usize) -> Vec<u8> {
        (0..size).map(|i| (i % 256) as u8).collect()
    }

    #[test]
    fn size_1kb() {
        let data = generate_data(1024);
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_4kb() {
        let data = generate_data(4 * 1024);
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_64kb() {
        let data = generate_data(64 * 1024);
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_256kb() {
        let data = generate_data(256 * 1024);
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_512kb() {
        let data = generate_data(512 * 1024);
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_1mb() {
        let data = generate_data(1024 * 1024);
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);

        // Verify streaming matches
        let mut hasher = Md4::new();
        hasher.update(&data);
        assert_eq!(hasher.finalize(), digest);
    }

    #[test]
    fn size_1mb_chunked() {
        // Hash 1MB in 4KB chunks
        let data = generate_data(1024 * 1024);
        let mut hasher = Md4::new();
        for chunk in data.chunks(4096) {
            hasher.update(chunk);
        }
        let chunked = hasher.finalize();

        let oneshot = Md4::digest(&data);
        assert_eq!(chunked, oneshot);
    }

    #[test]
    fn large_data_deterministic() {
        let data = generate_data(100_000);
        let d1 = Md4::digest(&data);
        let d2 = Md4::digest(&data);
        let d3 = Md4::digest(&data);
        assert_eq!(d1, d2);
        assert_eq!(d2, d3);
    }
}

// ============================================================================
// Incremental Hashing (Streaming API) Tests
// ============================================================================

mod incremental_hashing {
    use super::*;

    #[test]
    fn streaming_byte_by_byte() {
        let data = b"The quick brown fox jumps over the lazy dog";
        let mut hasher = Md4::new();
        for &byte in data.iter() {
            hasher.update(&[byte]);
        }
        let streaming = hasher.finalize();

        let oneshot = Md4::digest(data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn streaming_two_halves() {
        let data = b"abcdefghijklmnopqrstuvwxyz";
        let mid = data.len() / 2;

        let mut hasher = Md4::new();
        hasher.update(&data[..mid]);
        hasher.update(&data[mid..]);
        let streaming = hasher.finalize();

        let oneshot = Md4::digest(data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn streaming_three_parts() {
        let data = b"message digest";
        let mut hasher = Md4::new();
        hasher.update(b"mess");
        hasher.update(b"age ");
        hasher.update(b"digest");
        let streaming = hasher.finalize();

        let oneshot = Md4::digest(data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn streaming_various_chunk_sizes() {
        let data: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
        let expected = Md4::digest(&data);

        // Test with various chunk sizes
        for chunk_size in [1, 2, 3, 5, 7, 13, 17, 31, 63, 64, 65, 100, 256, 500] {
            let mut hasher = Md4::new();
            for chunk in data.chunks(chunk_size) {
                hasher.update(chunk);
            }
            let result = hasher.finalize();
            assert_eq!(
                result, expected,
                "Chunk size {chunk_size} should produce same result"
            );
        }
    }

    #[test]
    fn streaming_random_chunk_sizes() {
        let data: Vec<u8> = (0..1000).map(|i| (i * 17 % 256) as u8).collect();

        // Use varied chunk sizes
        let chunk_sizes = [1, 3, 7, 13, 31, 63, 127, 255];
        let mut hasher = Md4::new();
        let mut offset = 0;
        let mut chunk_idx = 0;

        while offset < data.len() {
            let chunk_size = chunk_sizes[chunk_idx % chunk_sizes.len()];
            let end = (offset + chunk_size).min(data.len());
            hasher.update(&data[offset..end]);
            offset = end;
            chunk_idx += 1;
        }

        let streaming = hasher.finalize();
        let oneshot = Md4::digest(&data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn streaming_with_empty_updates() {
        let data = b"test data";
        let mut hasher = Md4::new();

        hasher.update(&[]);
        hasher.update(b"test");
        hasher.update(&[]);
        hasher.update(&[]);
        hasher.update(b" ");
        hasher.update(&[]);
        hasher.update(b"data");
        hasher.update(&[]);

        let streaming = hasher.finalize();
        let oneshot = Md4::digest(data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn streaming_only_empty_updates() {
        let mut hasher = Md4::new();
        for _ in 0..100 {
            hasher.update(&[]);
        }
        let streaming = hasher.finalize();
        let oneshot = Md4::digest(b"");
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn streaming_clone_mid_computation() {
        let mut hasher = Md4::new();
        hasher.update(b"hello");

        // Clone and continue with different data
        let cloned = hasher.clone();

        hasher.update(b" world");
        let full = hasher.finalize();

        let mut cloned_hasher = cloned;
        cloned_hasher.update(b" world");
        let cloned_full = cloned_hasher.finalize();

        assert_eq!(full, cloned_full);
        assert_eq!(full, Md4::digest(b"hello world"));
    }

    #[test]
    fn streaming_clone_divergent_paths() {
        let mut hasher = Md4::new();
        hasher.update(b"prefix_");

        let clone1 = hasher.clone();
        let clone2 = hasher.clone();

        hasher.update(b"original");
        let mut c1 = clone1;
        c1.update(b"path_a");
        let mut c2 = clone2;
        c2.update(b"path_b");

        let r_orig = hasher.finalize();
        let r_a = c1.finalize();
        let r_b = c2.finalize();

        assert_eq!(r_orig, Md4::digest(b"prefix_original"));
        assert_eq!(r_a, Md4::digest(b"prefix_path_a"));
        assert_eq!(r_b, Md4::digest(b"prefix_path_b"));

        // All three should be different
        assert_ne!(r_orig, r_a);
        assert_ne!(r_orig, r_b);
        assert_ne!(r_a, r_b);
    }

    #[test]
    fn streaming_large_data_various_chunk_sizes() {
        let data: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();
        let expected = Md4::digest(&data);

        for chunk_size in [1, 7, 13, 64, 128, 1000, 4096, 8192] {
            let mut hasher = Md4::new();
            for chunk in data.chunks(chunk_size) {
                hasher.update(chunk);
            }
            let result = hasher.finalize();
            assert_eq!(result, expected, "Mismatch with chunk_size={chunk_size}");
        }
    }

    #[test]
    fn streaming_split_at_all_positions() {
        let data = b"0123456789abcdef"; // 16 bytes
        let expected = Md4::digest(data);

        for split_pos in 0..=data.len() {
            let mut hasher = Md4::new();
            hasher.update(&data[..split_pos]);
            hasher.update(&data[split_pos..]);
            let result = hasher.finalize();
            assert_eq!(
                result, expected,
                "Split at position {split_pos} should produce same result"
            );
        }
    }

    #[test]
    fn streaming_multiple_splits() {
        let data = b"The quick brown fox";
        let expected = Md4::digest(data);

        // Split into 5 parts
        let mut hasher = Md4::new();
        hasher.update(b"The ");
        hasher.update(b"qui");
        hasher.update(b"ck ");
        hasher.update(b"brown");
        hasher.update(b" fox");
        assert_eq!(hasher.finalize(), expected);
    }
}

// ============================================================================
// Single Byte Tests
// ============================================================================

mod single_byte {
    use super::*;

    #[test]
    fn single_byte_zero() {
        let digest = Md4::digest(&[0x00]);
        assert_eq!(digest.len(), 16);
        // Should be deterministic
        assert_eq!(Md4::digest(&[0x00]), digest);
    }

    #[test]
    fn single_byte_one() {
        let digest = Md4::digest(&[0x01]);
        assert_eq!(digest.len(), 16);
        // Different from zero
        assert_ne!(digest, Md4::digest(&[0x00]));
    }

    #[test]
    fn single_byte_max() {
        let digest = Md4::digest(&[0xFF]);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn single_byte_a() {
        // Same as RFC vector
        let digest = Md4::digest(b"a");
        assert_eq!(to_hex(&digest), "bde52cb31de33e46245e05fbdbd6fb24");
    }

    #[test]
    fn single_byte_streaming() {
        let mut hasher = Md4::new();
        hasher.update(&[0x42]);
        let streaming = hasher.finalize();

        let oneshot = Md4::digest(&[0x42]);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn all_256_single_bytes_unique() {
        let mut digests = std::collections::HashSet::new();
        for byte in 0u8..=255 {
            let digest = Md4::digest(&[byte]);
            assert!(
                digests.insert(digest),
                "Collision detected for single byte {byte}"
            );
        }
        assert_eq!(digests.len(), 256);
    }

    #[test]
    fn adjacent_byte_values_produce_different_digests() {
        for byte in 0u8..255 {
            let d1 = Md4::digest(&[byte]);
            let d2 = Md4::digest(&[byte + 1]);
            assert_ne!(
                d1,
                d2,
                "Adjacent bytes {} and {} should differ",
                byte,
                byte + 1
            );
        }
    }
}

// ============================================================================
// StrongDigest Trait Tests
// ============================================================================

mod strong_digest_trait {
    use super::*;

    #[test]
    fn trait_new_matches_inherent_new() {
        let mut trait_hasher: Md4 = StrongDigest::new();
        trait_hasher.update(b"trait test");
        let trait_result = trait_hasher.finalize();

        let mut inherent_hasher = Md4::new();
        inherent_hasher.update(b"trait test");
        let inherent_result = inherent_hasher.finalize();

        assert_eq!(trait_result, inherent_result);
    }

    #[test]
    fn trait_digest_matches_inherent_digest() {
        let trait_result = <Md4 as StrongDigest>::digest(b"quick test");
        let inherent_result = Md4::digest(b"quick test");
        assert_eq!(trait_result, inherent_result);
    }

    #[test]
    fn digest_len_constant() {
        assert_eq!(Md4::DIGEST_LEN, 16);
    }

    #[test]
    fn with_seed_matches_new() {
        // MD4 seed is (), so with_seed should behave like new
        let mut seeded: Md4 = StrongDigest::with_seed(());
        seeded.update(b"test");
        let seeded_result = seeded.finalize();

        let mut new_hasher = Md4::new();
        new_hasher.update(b"test");
        let new_result = new_hasher.finalize();

        assert_eq!(seeded_result, new_result);
    }

    #[test]
    fn digest_with_seed_matches_digest() {
        let seeded = <Md4 as StrongDigest>::digest_with_seed((), b"test");
        let unseeded = <Md4 as StrongDigest>::digest(b"test");
        assert_eq!(seeded, unseeded);
    }
}

// ============================================================================
// Edge Cases and Corner Cases
// ============================================================================

mod edge_cases {
    use super::*;

    #[test]
    fn digest_output_is_16_bytes() {
        let digest = Md4::digest(b"test");
        assert_eq!(digest.len(), 16);
        assert_eq!(digest.as_ref().len(), 16);
    }

    #[test]
    fn deterministic_output() {
        let data = b"determinism test";
        let d1 = Md4::digest(data);
        let d2 = Md4::digest(data);
        let d3 = Md4::digest(data);
        assert_eq!(d1, d2);
        assert_eq!(d2, d3);
    }

    #[test]
    fn different_inputs_different_outputs() {
        let d1 = Md4::digest(b"input1");
        let d2 = Md4::digest(b"input2");
        assert_ne!(d1, d2);
    }

    #[test]
    fn similar_inputs_different_outputs() {
        let d1 = Md4::digest(b"test");
        let d2 = Md4::digest(b"Test"); // Different case
        let d3 = Md4::digest(b"test "); // Trailing space
        let d4 = Md4::digest(b" test"); // Leading space

        assert_ne!(d1, d2);
        assert_ne!(d1, d3);
        assert_ne!(d1, d4);
        assert_ne!(d2, d3);
        assert_ne!(d2, d4);
        assert_ne!(d3, d4);
    }

    #[test]
    fn debug_format_contains_md4() {
        let hasher = Md4::new();
        let debug = format!("{hasher:?}");
        assert!(debug.contains("Md4"));
    }

    #[test]
    fn default_equals_new() {
        let mut default_hasher = Md4::default();
        let mut new_hasher = Md4::new();

        default_hasher.update(b"test");
        new_hasher.update(b"test");

        assert_eq!(default_hasher.finalize(), new_hasher.finalize());
    }

    #[test]
    fn all_zero_input_various_sizes() {
        for size in [0, 1, 16, 55, 56, 64, 128, 1024] {
            let data = vec![0u8; size];
            let digest = Md4::digest(&data);
            assert_eq!(digest.len(), 16, "Size {size}: digest should be 16 bytes");
        }
    }

    #[test]
    fn all_ones_input_various_sizes() {
        for size in [0, 1, 16, 55, 56, 64, 128, 1024] {
            let data = vec![0xFFu8; size];
            let digest = Md4::digest(&data);
            assert_eq!(digest.len(), 16, "Size {size}: digest should be 16 bytes");
        }
    }

    #[test]
    fn binary_data_with_null_bytes() {
        let data_with_null = b"before\x00after";
        let data_without_null = b"beforeafter";

        let d1 = Md4::digest(data_with_null);
        let d2 = Md4::digest(data_without_null);

        // Null byte should affect the hash
        assert_ne!(d1, d2);
    }

    #[test]
    fn all_byte_values() {
        // Test with data containing all possible byte values
        let data: Vec<u8> = (0..=255).collect();
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);

        // Verify streaming matches
        let mut hasher = Md4::new();
        hasher.update(&data);
        assert_eq!(hasher.finalize(), digest);
    }

    #[test]
    fn repeated_patterns() {
        // Test with repeated patterns of various sizes
        let patterns: &[&[u8]] = &[&[0xAA; 1000], &[0x00; 1000], &[0xFF; 1000], &[0x55; 1000]];

        let mut digests = Vec::new();
        for pattern in patterns {
            let digest = Md4::digest(pattern);
            assert_eq!(digest.len(), 16);
            digests.push(digest);
        }

        // All patterns should produce unique digests
        for i in 0..digests.len() {
            for j in (i + 1)..digests.len() {
                assert_ne!(digests[i], digests[j], "Patterns {i} and {j} should differ");
            }
        }
    }

    #[test]
    fn alternating_patterns() {
        let pattern1: Vec<u8> = (0..1000)
            .map(|i| if i % 2 == 0 { 0xAA } else { 0x55 })
            .collect();
        let pattern2: Vec<u8> = (0..1000)
            .map(|i| if i % 2 == 0 { 0x55 } else { 0xAA })
            .collect();

        let d1 = Md4::digest(&pattern1);
        let d2 = Md4::digest(&pattern2);

        assert_ne!(d1, d2);
    }
}

// ============================================================================
// Batch API Tests
// ============================================================================

mod batch_api {
    use super::*;
    use checksums::strong::md4_digest_batch;

    #[test]
    fn batch_empty_inputs() {
        let inputs: &[&[u8]] = &[];
        let results = md4_digest_batch(inputs);
        assert!(results.is_empty());
    }

    #[test]
    fn batch_single_input() {
        let inputs: &[&[u8]] = &[b"single"];
        let results = md4_digest_batch(inputs);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], Md4::digest(b"single"));
    }

    #[test]
    fn batch_multiple_inputs() {
        let inputs: &[&[u8]] = &[b"a", b"b", b"c", b"abc"];
        let results = md4_digest_batch(inputs);
        assert_eq!(results.len(), 4);
        for (i, result) in results.iter().enumerate() {
            assert_eq!(*result, Md4::digest(inputs[i]));
        }
    }

    #[test]
    fn batch_matches_sequential() {
        let inputs: Vec<Vec<u8>> = (0..100)
            .map(|i| (0..((i % 50) + 1)).map(|j| (j % 256) as u8).collect())
            .collect();
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let batch_results = md4_digest_batch(&input_refs);
        let sequential_results: Vec<[u8; 16]> = inputs.iter().map(|v| Md4::digest(v)).collect();

        assert_eq!(batch_results, sequential_results);
    }

    #[test]
    fn batch_rfc_vectors() {
        let inputs: &[&[u8]] = &[
            b"",
            b"a",
            b"abc",
            b"message digest",
            b"abcdefghijklmnopqrstuvwxyz",
        ];

        let results = md4_digest_batch(inputs);

        let expected = [
            "31d6cfe0d16ae931b73c59d7e0c089c0",
            "bde52cb31de33e46245e05fbdbd6fb24",
            "a448017aaf21d8525fc10ae87aa6729d",
            "d9130a8164549fe818874806e1c7014b",
            "d79e1c308aa5bbcdeea8ed63df412da9",
        ];

        for (i, result) in results.iter().enumerate() {
            assert_eq!(
                to_hex(result),
                expected[i],
                "Batch result mismatch at index {i}"
            );
        }
    }

    #[test]
    fn batch_large_batch() {
        let inputs: Vec<Vec<u8>> = (0..500)
            .map(|i| vec![(i % 256) as u8; (i % 100) + 1])
            .collect();
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let batch_results = md4_digest_batch(&input_refs);

        assert_eq!(batch_results.len(), 500);
        for (i, result) in batch_results.iter().enumerate() {
            assert_eq!(*result, Md4::digest(&inputs[i]), "Mismatch at index {i}");
        }
    }
}
