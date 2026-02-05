//! Comprehensive tests for MD4 checksum algorithm.
//!
//! This module provides thorough testing of the MD4 implementation including:
//! - RFC 1320 test vectors
//! - Empty input handling
//! - Single byte handling
//! - Various sizes up to 1MB
//! - Streaming API incremental computation
//! - Comparison with reference implementation (md4 crate)

#[cfg(test)]
mod tests {
    use crate::strong::{Md4, StrongDigest};
    use digest::Digest;

    /// Convert bytes to hexadecimal string for readable assertions.
    fn to_hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            write!(&mut out, "{byte:02x}").expect("write! to String cannot fail");
        }
        out
    }

    // ========================================================================
    // RFC 1320 Test Vectors
    // ========================================================================
    // Test vectors from RFC 1320 "The MD4 Message-Digest Algorithm"
    // https://www.rfc-editor.org/rfc/rfc1320

    #[test]
    fn md4_rfc1320_empty() {
        // MD4 ("") = 31d6cfe0d16ae931b73c59d7e0c089c0
        let digest = Md4::digest(b"");
        assert_eq!(
            to_hex(&digest),
            "31d6cfe0d16ae931b73c59d7e0c089c0",
            "RFC 1320: MD4 of empty string"
        );
    }

    #[test]
    fn md4_rfc1320_a() {
        // MD4 ("a") = bde52cb31de33e46245e05fbdbd6fb24
        let digest = Md4::digest(b"a");
        assert_eq!(
            to_hex(&digest),
            "bde52cb31de33e46245e05fbdbd6fb24",
            "RFC 1320: MD4 of 'a'"
        );
    }

    #[test]
    fn md4_rfc1320_abc() {
        // MD4 ("abc") = a448017aaf21d8525fc10ae87aa6729d
        let digest = Md4::digest(b"abc");
        assert_eq!(
            to_hex(&digest),
            "a448017aaf21d8525fc10ae87aa6729d",
            "RFC 1320: MD4 of 'abc'"
        );
    }

    #[test]
    fn md4_rfc1320_message_digest() {
        // MD4 ("message digest") = d9130a8164549fe818874806e1c7014b
        let digest = Md4::digest(b"message digest");
        assert_eq!(
            to_hex(&digest),
            "d9130a8164549fe818874806e1c7014b",
            "RFC 1320: MD4 of 'message digest'"
        );
    }

    #[test]
    fn md4_rfc1320_lowercase_alphabet() {
        // MD4 ("abcdefghijklmnopqrstuvwxyz") = d79e1c308aa5bbcdeea8ed63df412da9
        let digest = Md4::digest(b"abcdefghijklmnopqrstuvwxyz");
        assert_eq!(
            to_hex(&digest),
            "d79e1c308aa5bbcdeea8ed63df412da9",
            "RFC 1320: MD4 of lowercase alphabet"
        );
    }

    #[test]
    fn md4_rfc1320_alphanumeric() {
        // MD4 ("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789")
        //    = 043f8582f241db351ce627e153e7f0e4
        let digest =
            Md4::digest(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789");
        assert_eq!(
            to_hex(&digest),
            "043f8582f241db351ce627e153e7f0e4",
            "RFC 1320: MD4 of alphanumeric"
        );
    }

    #[test]
    fn md4_rfc1320_numeric_repeated() {
        // MD4 ("12345678901234567890123456789012345678901234567890123456789012345678901234567890")
        //    = e33b4ddc9c38f2199c3e7b164fcc0536
        let digest = Md4::digest(
            b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
        );
        assert_eq!(
            to_hex(&digest),
            "e33b4ddc9c38f2199c3e7b164fcc0536",
            "RFC 1320: MD4 of repeated digits"
        );
    }

    // ========================================================================
    // Empty Input Tests
    // ========================================================================

    #[test]
    fn md4_empty_input_one_shot() {
        let digest = Md4::digest(b"");
        assert_eq!(digest.len(), 16, "MD4 digest should be 16 bytes");
        // Verify against known value
        assert_eq!(to_hex(&digest), "31d6cfe0d16ae931b73c59d7e0c089c0");
    }

    #[test]
    fn md4_empty_input_streaming() {
        let hasher = Md4::new();
        let digest = hasher.finalize();
        assert_eq!(digest.len(), 16);
        assert_eq!(to_hex(&digest), "31d6cfe0d16ae931b73c59d7e0c089c0");
    }

    #[test]
    fn md4_empty_input_with_empty_updates() {
        let mut hasher = Md4::new();
        hasher.update(b"");
        hasher.update(b"");
        hasher.update(b"");
        let digest = hasher.finalize();
        assert_eq!(to_hex(&digest), "31d6cfe0d16ae931b73c59d7e0c089c0");
    }

    // ========================================================================
    // Single Byte Tests
    // ========================================================================

    #[test]
    fn md4_single_byte_zero() {
        let digest = Md4::digest(&[0x00]);
        assert_eq!(digest.len(), 16);
        // Verify determinism
        assert_eq!(Md4::digest(&[0x00]), digest);
    }

    #[test]
    fn md4_single_byte_max() {
        let digest = Md4::digest(&[0xFF]);
        assert_eq!(digest.len(), 16);
        // Should differ from 0x00
        assert_ne!(Md4::digest(&[0xFF]), Md4::digest(&[0x00]));
    }

    #[test]
    fn md4_single_byte_a() {
        // Single 'a' character (0x61)
        let digest = Md4::digest(&[0x61]);
        assert_eq!(to_hex(&digest), "bde52cb31de33e46245e05fbdbd6fb24");
    }

    #[test]
    fn md4_all_single_bytes_produce_unique_digests() {
        use std::collections::HashSet;
        let mut digests = HashSet::new();
        for byte in 0u8..=255 {
            let digest = Md4::digest(&[byte]);
            digests.insert(digest);
        }
        // All 256 single-byte inputs should produce unique digests
        assert_eq!(digests.len(), 256);
    }

    // ========================================================================
    // Various Size Tests (up to 1MB)
    // ========================================================================

    #[test]
    fn md4_small_sizes() {
        // Test sizes from 1 to 128 bytes
        for size in 1..=128 {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let digest = Md4::digest(&data);
            assert_eq!(digest.len(), 16, "MD4 digest should always be 16 bytes");
        }
    }

    #[test]
    fn md4_block_boundary_sizes() {
        // MD4 processes 64-byte blocks internally
        // Test sizes around block boundaries
        for &size in &[
            63, 64, 65, // First block boundary
            127, 128, 129, // Second block boundary
            191, 192, 193, // Third block boundary
            255, 256, 257, // Fourth block boundary
        ] {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let digest = Md4::digest(&data);
            assert_eq!(digest.len(), 16, "Size {size}: MD4 digest should be 16 bytes");
        }
    }

    #[test]
    fn md4_power_of_two_sizes() {
        for power in 0..=10 {
            // 1 byte to 1024 bytes
            let size = 1 << power;
            let data = vec![0xAB_u8; size];
            let digest = Md4::digest(&data);
            assert_eq!(digest.len(), 16);
        }
    }

    #[test]
    fn md4_1kb() {
        let data = vec![0xCD_u8; 1024];
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn md4_10kb() {
        let data = vec![0xEF_u8; 10 * 1024];
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn md4_100kb() {
        let data = vec![0x12_u8; 100 * 1024];
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn md4_1mb() {
        let data = vec![0x34_u8; 1024 * 1024];
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
        // Verify determinism
        assert_eq!(digest, Md4::digest(&data));
    }

    #[test]
    fn md4_1mb_incremental_equals_oneshot() {
        let data = vec![0x56_u8; 1024 * 1024];
        let oneshot = Md4::digest(&data);

        // Compute incrementally with 64KB chunks
        let mut hasher = Md4::new();
        for chunk in data.chunks(64 * 1024) {
            hasher.update(chunk);
        }
        let incremental = hasher.finalize();

        assert_eq!(oneshot, incremental);
    }

    // ========================================================================
    // Streaming API Incremental Computation Tests
    // ========================================================================

    #[test]
    fn md4_streaming_byte_by_byte() {
        let data = b"The quick brown fox jumps over the lazy dog";
        let oneshot = Md4::digest(data);

        let mut hasher = Md4::new();
        for &byte in data.iter() {
            hasher.update(&[byte]);
        }
        let streaming = hasher.finalize();

        assert_eq!(oneshot, streaming);
    }

    #[test]
    fn md4_streaming_various_chunk_sizes() {
        let data: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
        let expected = Md4::digest(&data);

        // Test with various chunk sizes
        for chunk_size in [1, 2, 3, 5, 7, 13, 17, 31, 64, 100, 256, 500] {
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
    fn md4_streaming_alternating_sizes() {
        let data = b"abcdefghijklmnopqrstuvwxyz";
        let expected = Md4::digest(data);

        let mut hasher = Md4::new();
        let mut pos = 0;
        let sizes = [1, 3, 2, 5, 4, 7, 4]; // Total = 26
        for &size in sizes.iter().cycle() {
            if pos >= data.len() {
                break;
            }
            let end = (pos + size).min(data.len());
            hasher.update(&data[pos..end]);
            pos = end;
        }
        let result = hasher.finalize();

        assert_eq!(result, expected);
    }

    #[test]
    fn md4_streaming_split_at_every_position() {
        let data = b"0123456789";
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
    fn md4_streaming_with_empty_updates_between() {
        let data = b"test data";
        let expected = Md4::digest(data);

        let mut hasher = Md4::new();
        hasher.update(b"test");
        hasher.update(b"");
        hasher.update(b" ");
        hasher.update(b"");
        hasher.update(b"");
        hasher.update(b"data");
        hasher.update(b"");
        let result = hasher.finalize();

        assert_eq!(result, expected);
    }

    #[test]
    fn md4_clone_during_streaming() {
        let mut hasher = Md4::new();
        hasher.update(b"first part");

        // Clone the hasher mid-stream
        let cloned = hasher.clone();

        // Continue with original
        hasher.update(b" second part");
        let original_result = hasher.finalize();

        // Continue with clone differently
        let mut cloned2 = cloned.clone();
        cloned2.update(b" different");
        let cloned_result = cloned2.finalize();

        // Results should differ
        assert_ne!(original_result, cloned_result);

        // Verify original is correct
        assert_eq!(original_result, Md4::digest(b"first part second part"));

        // Verify clone is correct
        assert_eq!(cloned_result, Md4::digest(b"first part different"));
    }

    // ========================================================================
    // Comparison with Reference Implementation (md4 crate)
    // ========================================================================

    #[test]
    fn md4_matches_reference_impl_empty() {
        let our_digest = Md4::digest(b"");
        let ref_digest: [u8; 16] = md4::Md4::digest(b"").into();
        assert_eq!(our_digest, ref_digest);
    }

    #[test]
    fn md4_matches_reference_impl_small_inputs() {
        let test_inputs: &[&[u8]] = &[
            b"",
            b"a",
            b"abc",
            b"message digest",
            b"abcdefghijklmnopqrstuvwxyz",
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
        ];

        for &input in test_inputs {
            let our_digest = Md4::digest(input);
            let ref_digest: [u8; 16] = md4::Md4::digest(input).into();
            assert_eq!(
                our_digest, ref_digest,
                "Mismatch for input: {:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn md4_matches_reference_impl_binary_data() {
        // Test with binary data (all byte values)
        let data: Vec<u8> = (0..=255).collect();
        let our_digest = Md4::digest(&data);
        let ref_digest: [u8; 16] = md4::Md4::digest(&data).into();
        assert_eq!(our_digest, ref_digest);
    }

    #[test]
    fn md4_matches_reference_impl_large_data() {
        // 64KB of patterned data
        let data: Vec<u8> = (0..65536).map(|i| (i % 256) as u8).collect();
        let our_digest = Md4::digest(&data);
        let ref_digest: [u8; 16] = md4::Md4::digest(&data).into();
        assert_eq!(our_digest, ref_digest);
    }

    #[test]
    fn md4_matches_reference_impl_streaming() {
        let data = b"streaming comparison test with multiple updates";

        // Our implementation
        let mut our_hasher = Md4::new();
        our_hasher.update(&data[..10]);
        our_hasher.update(&data[10..30]);
        our_hasher.update(&data[30..]);
        let our_digest = our_hasher.finalize();

        // Reference implementation
        let mut ref_hasher = md4::Md4::new();
        ref_hasher.update(&data[..10]);
        ref_hasher.update(&data[10..30]);
        ref_hasher.update(&data[30..]);
        let ref_digest: [u8; 16] = ref_hasher.finalize().into();

        assert_eq!(our_digest, ref_digest);
    }

    #[test]
    fn md4_matches_reference_impl_random_data() {
        use rand::Rng;
        let mut rng = rand::thread_rng();

        for size in [0, 1, 16, 63, 64, 65, 127, 128, 129, 255, 256, 1000, 10000] {
            let data: Vec<u8> = (0..size).map(|_| rng.r#gen()).collect();
            let our_digest = Md4::digest(&data);
            let ref_digest: [u8; 16] = md4::Md4::digest(&data).into();
            assert_eq!(our_digest, ref_digest, "Mismatch for size {size}");
        }
    }

    // ========================================================================
    // StrongDigest Trait Tests
    // ========================================================================

    #[test]
    fn md4_strong_digest_trait_digest_len() {
        assert_eq!(Md4::DIGEST_LEN, 16);
    }

    #[test]
    fn md4_strong_digest_trait_new() {
        let hasher: Md4 = StrongDigest::new();
        let mut h = hasher;
        h.update(b"test");
        let digest = h.finalize();
        assert_eq!(digest, Md4::digest(b"test"));
    }

    #[test]
    fn md4_strong_digest_trait_with_seed() {
        // MD4 seed type is (), so with_seed should behave like new
        let hasher: Md4 = StrongDigest::with_seed(());
        let mut h = hasher;
        h.update(b"test");
        let digest = h.finalize();
        assert_eq!(digest, Md4::digest(b"test"));
    }

    #[test]
    fn md4_strong_digest_trait_digest() {
        let trait_digest = <Md4 as StrongDigest>::digest(b"trait test");
        let inherent_digest = Md4::digest(b"trait test");
        assert_eq!(trait_digest, inherent_digest);
    }

    #[test]
    fn md4_strong_digest_trait_digest_with_seed() {
        let digest = <Md4 as StrongDigest>::digest_with_seed((), b"seeded");
        assert_eq!(digest, Md4::digest(b"seeded"));
    }

    // ========================================================================
    // Default and Debug Trait Tests
    // ========================================================================

    #[test]
    fn md4_default_trait() {
        let default_hasher = Md4::default();
        let new_hasher = Md4::new();

        let mut d = default_hasher;
        let mut n = new_hasher;
        d.update(b"test");
        n.update(b"test");

        assert_eq!(d.finalize(), n.finalize());
    }

    #[test]
    fn md4_debug_trait() {
        let hasher = Md4::new();
        let debug_str = format!("{:?}", hasher);
        assert!(debug_str.contains("Md4"));
        // Should indicate backend type
        assert!(debug_str.contains("backend"));
    }

    // ========================================================================
    // Batch Digest Tests
    // ========================================================================

    #[test]
    fn md4_batch_digest_empty_inputs() {
        let inputs: &[&[u8]] = &[];
        let results = crate::strong::md4_digest_batch(inputs);
        assert!(results.is_empty());
    }

    #[test]
    fn md4_batch_digest_single_input() {
        let inputs: &[&[u8]] = &[b"single"];
        let results = crate::strong::md4_digest_batch(inputs);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], Md4::digest(b"single"));
    }

    #[test]
    fn md4_batch_digest_multiple_inputs() {
        let inputs: &[&[u8]] = &[
            b"",
            b"a",
            b"abc",
            b"message digest",
            b"abcdefghijklmnopqrstuvwxyz",
        ];
        let results = crate::strong::md4_digest_batch(inputs);

        assert_eq!(results.len(), inputs.len());
        for (i, result) in results.iter().enumerate() {
            assert_eq!(*result, Md4::digest(inputs[i]), "Mismatch at index {i}");
        }
    }

    #[test]
    fn md4_batch_digest_large_batch() {
        let inputs: Vec<Vec<u8>> = (0..100).map(|i| vec![(i % 256) as u8; i + 1]).collect();
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let results = crate::strong::md4_digest_batch(&input_refs);

        assert_eq!(results.len(), 100);
        for (i, result) in results.iter().enumerate() {
            assert_eq!(*result, Md4::digest(&inputs[i]), "Mismatch at index {i}");
        }
    }

    // ========================================================================
    // Edge Cases and Corner Cases
    // ========================================================================

    #[test]
    fn md4_exactly_55_bytes() {
        // 55 bytes is the maximum that fits in one block with padding
        let data = vec![0xAA_u8; 55];
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);

        // Verify streaming matches
        let mut hasher = Md4::new();
        hasher.update(&data);
        assert_eq!(hasher.finalize(), digest);
    }

    #[test]
    fn md4_exactly_56_bytes() {
        // 56 bytes requires two blocks due to padding
        let data = vec![0xBB_u8; 56];
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn md4_exactly_64_bytes() {
        // Exactly one full block
        let data = vec![0xCC_u8; 64];
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn md4_repeated_same_input() {
        // Verify determinism
        let data = b"determinism test";
        let digest1 = Md4::digest(data);
        let digest2 = Md4::digest(data);
        let digest3 = Md4::digest(data);
        assert_eq!(digest1, digest2);
        assert_eq!(digest2, digest3);
    }

    #[test]
    fn md4_different_inputs_different_digests() {
        let inputs: &[&[u8]] = &[
            b"input1",
            b"input2",
            b"INPUT1",
            b"input1 ",
            b" input1",
            b"1input",
        ];

        let digests: Vec<[u8; 16]> = inputs.iter().map(|i| Md4::digest(*i)).collect();

        // All digests should be unique
        for i in 0..digests.len() {
            for j in (i + 1)..digests.len() {
                assert_ne!(
                    digests[i], digests[j],
                    "Inputs {} and {} should produce different digests",
                    i, j
                );
            }
        }
    }

    #[test]
    fn md4_null_bytes_in_input() {
        let data = b"before\x00after";
        let digest = Md4::digest(data);
        assert_eq!(digest.len(), 16);

        // Should differ from version without null
        assert_ne!(digest, Md4::digest(b"beforeafter"));
    }

    #[test]
    fn md4_all_zeros() {
        let data = vec![0u8; 1000];
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn md4_all_ones() {
        let data = vec![0xFF_u8; 1000];
        let digest = Md4::digest(&data);
        assert_eq!(digest.len(), 16);
    }
}
