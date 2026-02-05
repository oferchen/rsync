//! Comprehensive MD5 checksum tests.
//!
//! This test module validates the MD5 implementation against:
//! 1. RFC 1321 official test vectors
//! 2. Edge cases (empty input, single byte)
//! 3. Various sizes up to 1MB
//! 4. Streaming API incremental computation
//! 5. Comparison with system md5sum command

use checksums::strong::{Md5, Md5Seed, StrongDigest};
use std::io::Write;
use std::process::{Command, Stdio};

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
// RFC 1321 Official Test Vectors
// ============================================================================

/// RFC 1321 Section A.5 defines the official MD5 test suite.
/// These vectors are authoritative for validating MD5 implementations.
mod rfc1321_test_vectors {
    use super::*;

    #[test]
    fn rfc1321_empty_string() {
        // MD5("") = d41d8cd98f00b204e9800998ecf8427e
        let digest = Md5::digest(b"");
        assert_eq!(to_hex(&digest), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn rfc1321_single_char_a() {
        // MD5("a") = 0cc175b9c0f1b6a831c399e269772661
        let digest = Md5::digest(b"a");
        assert_eq!(to_hex(&digest), "0cc175b9c0f1b6a831c399e269772661");
    }

    #[test]
    fn rfc1321_abc() {
        // MD5("abc") = 900150983cd24fb0d6963f7d28e17f72
        let digest = Md5::digest(b"abc");
        assert_eq!(to_hex(&digest), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn rfc1321_message_digest() {
        // MD5("message digest") = f96b697d7cb7938d525a2f31aaf161d0
        let digest = Md5::digest(b"message digest");
        assert_eq!(to_hex(&digest), "f96b697d7cb7938d525a2f31aaf161d0");
    }

    #[test]
    fn rfc1321_lowercase_alphabet() {
        // MD5("abcdefghijklmnopqrstuvwxyz") = c3fcd3d76192e4007dfb496cca67e13b
        let digest = Md5::digest(b"abcdefghijklmnopqrstuvwxyz");
        assert_eq!(to_hex(&digest), "c3fcd3d76192e4007dfb496cca67e13b");
    }

    #[test]
    fn rfc1321_alphanumeric_mixed_case() {
        // MD5("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789")
        // = d174ab98d277d9f5a5611c2c9f419d9f
        let digest = Md5::digest(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789");
        assert_eq!(to_hex(&digest), "d174ab98d277d9f5a5611c2c9f419d9f");
    }

    #[test]
    fn rfc1321_numeric_sequence() {
        // MD5("12345678901234567890123456789012345678901234567890123456789012345678901234567890")
        // = 57edf4a22be3c955ac49da2e2107b67a
        let digest = Md5::digest(
            b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
        );
        assert_eq!(to_hex(&digest), "57edf4a22be3c955ac49da2e2107b67a");
    }

    /// Additional well-known test vector for padding boundary testing.
    #[test]
    fn rfc1321_55_bytes_padding_boundary() {
        // 55 bytes: one byte short of requiring an extra 64-byte block
        let input = b"0123456789012345678901234567890123456789012345678901234";
        assert_eq!(input.len(), 55);
        let digest = Md5::digest(input);
        // Verified with: echo -n "0123456789012345678901234567890123456789012345678901234" | md5sum
        assert_eq!(to_hex(&digest), "6e7a4fc92eb1c3f6e652425bcc8d44b5");
    }

    #[test]
    fn rfc1321_56_bytes_padding_boundary() {
        // 56 bytes: exactly at padding boundary, requires extra block
        let input = b"01234567890123456789012345678901234567890123456789012345";
        assert_eq!(input.len(), 56);
        let digest = Md5::digest(input);
        // Verified with: echo -n "01234567890123456789012345678901234567890123456789012345" | md5sum
        assert_eq!(to_hex(&digest), "8af270b2847610e742b0791b53648c09");
    }

    #[test]
    fn rfc1321_64_bytes_exactly_one_block() {
        // 64 bytes: exactly one MD5 block
        let input = b"0123456789012345678901234567890123456789012345678901234567890123";
        assert_eq!(input.len(), 64);
        let digest = Md5::digest(input);
        // Pre-calculated value
        assert_eq!(to_hex(&digest), "7f7bfd348709deeaace19e3f535f8c54");
    }

    #[test]
    fn rfc1321_57_bytes_padding_boundary() {
        // 57 bytes: just past the 56-byte boundary
        let input = b"012345678901234567890123456789012345678901234567890123456";
        assert_eq!(input.len(), 57);
        let digest = Md5::digest(input);
        // Verified with: echo -n "012345678901234567890123456789012345678901234567890123456" | md5sum
        assert_eq!(to_hex(&digest), "c620bace4cde41bc45a14cfa62ee3487");
    }

    #[test]
    fn rfc1321_63_bytes_just_under_block() {
        // 63 bytes: one byte short of a full block
        let input = b"012345678901234567890123456789012345678901234567890123456789012";
        assert_eq!(input.len(), 63);
        let digest = Md5::digest(input);
        // Verified with: echo -n "012345678901234567890123456789012345678901234567890123456789012" | md5sum
        assert_eq!(to_hex(&digest), "c5e256437e758092dbfe06283e489019");
    }

    #[test]
    fn rfc1321_119_bytes_two_block_padding_boundary() {
        // 119 bytes: 55 + 64, padding fits in one block after two full blocks
        let input: Vec<u8> = (0..119).map(|i| b'0' + (i % 10) as u8).collect();
        assert_eq!(input.len(), 119);
        let digest = Md5::digest(&input);
        // Verify streaming matches
        let mut hasher = Md5::new();
        hasher.update(&input);
        assert_eq!(hasher.finalize(), digest);
    }

    #[test]
    fn rfc1321_120_bytes_two_block_padding_boundary() {
        // 120 bytes: 56 + 64, padding requires extra block
        let input: Vec<u8> = (0..120).map(|i| b'0' + (i % 10) as u8).collect();
        assert_eq!(input.len(), 120);
        let digest = Md5::digest(&input);
        // Verify streaming matches
        let mut hasher = Md5::new();
        hasher.update(&input);
        assert_eq!(hasher.finalize(), digest);
    }
}

// ============================================================================
// Empty Input Tests
// ============================================================================

mod empty_input {
    use super::*;

    #[test]
    fn empty_slice_produces_known_digest() {
        let digest = Md5::digest(b"");
        assert_eq!(to_hex(&digest), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn empty_streaming_produces_same_digest() {
        let hasher = Md5::new();
        // No update calls
        let digest = hasher.finalize();
        assert_eq!(to_hex(&digest), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn empty_streaming_with_empty_updates() {
        let mut hasher = Md5::new();
        hasher.update(&[]);
        hasher.update(&[]);
        hasher.update(&[]);
        let digest = hasher.finalize();
        assert_eq!(to_hex(&digest), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn empty_with_seed_proper_order() {
        let seed = Md5Seed::proper(0x12345678);
        let hasher = Md5::with_seed(seed);
        // No data updates
        let seeded = hasher.finalize();

        // Should equal hash of just the seed bytes
        let mut manual = Md5::new();
        manual.update(&0x12345678_i32.to_le_bytes());
        assert_eq!(seeded, manual.finalize());
    }

    #[test]
    fn empty_with_seed_legacy_order() {
        let seed = Md5Seed::legacy(0x12345678);
        let hasher = Md5::with_seed(seed);
        // No data updates
        let seeded = hasher.finalize();

        // Should equal hash of just the seed bytes (added after data)
        let mut manual = Md5::new();
        manual.update(&0x12345678_i32.to_le_bytes());
        assert_eq!(seeded, manual.finalize());
    }
}

// ============================================================================
// Single Byte Tests
// ============================================================================

mod single_byte {
    use super::*;

    #[test]
    fn single_byte_zero() {
        let digest = Md5::digest(&[0x00]);
        assert_eq!(to_hex(&digest), "93b885adfe0da089cdf634904fd59f71");
    }

    #[test]
    fn single_byte_one() {
        let digest = Md5::digest(&[0x01]);
        assert_eq!(to_hex(&digest), "55a54008ad1ba589aa210d2629c1df41");
    }

    #[test]
    fn single_byte_max() {
        let digest = Md5::digest(&[0xFF]);
        // Verified with: printf '\xff' | md5sum
        assert_eq!(to_hex(&digest), "00594fd4f42ba43fc1ca0427a0576295");
    }

    #[test]
    fn single_byte_a() {
        // Same as RFC vector
        let digest = Md5::digest(b"a");
        assert_eq!(to_hex(&digest), "0cc175b9c0f1b6a831c399e269772661");
    }

    #[test]
    fn single_byte_streaming() {
        let mut hasher = Md5::new();
        hasher.update(&[0x42]);
        let streaming = hasher.finalize();

        let oneshot = Md5::digest(&[0x42]);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn all_256_single_bytes_unique() {
        let mut digests = std::collections::HashSet::new();
        for byte in 0u8..=255 {
            let digest = Md5::digest(&[byte]);
            assert!(
                digests.insert(digest),
                "Collision detected for single byte {byte}"
            );
        }
        assert_eq!(digests.len(), 256);
    }
}

// ============================================================================
// Various Sizes Tests (up to 1MB)
// ============================================================================

mod various_sizes {
    use super::*;

    /// Helper to generate deterministic test data.
    fn generate_data(size: usize) -> Vec<u8> {
        (0..size).map(|i| (i % 256) as u8).collect()
    }

    #[test]
    fn size_1_byte() {
        let data = generate_data(1);
        let digest = Md5::digest(&data);
        assert_eq!(digest.len(), 16);
        // Verify streaming matches
        let mut hasher = Md5::new();
        hasher.update(&data);
        assert_eq!(hasher.finalize(), digest);
    }

    #[test]
    fn size_16_bytes() {
        let data = generate_data(16);
        let digest = Md5::digest(&data);
        assert_eq!(digest.len(), 16);
        // Verify streaming matches
        let mut hasher = Md5::new();
        hasher.update(&data);
        assert_eq!(hasher.finalize(), digest);
    }

    #[test]
    fn size_63_bytes() {
        // Just under one block
        let data = generate_data(63);
        let digest = Md5::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_64_bytes() {
        // Exactly one block
        let data = generate_data(64);
        let digest = Md5::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_65_bytes() {
        // Just over one block
        let data = generate_data(65);
        let digest = Md5::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_127_bytes() {
        let data = generate_data(127);
        let digest = Md5::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_128_bytes() {
        // Two blocks
        let data = generate_data(128);
        let digest = Md5::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_256_bytes() {
        let data = generate_data(256);
        let digest = Md5::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_1kb() {
        let data = generate_data(1024);
        let digest = Md5::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_4kb() {
        let data = generate_data(4 * 1024);
        let digest = Md5::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_64kb() {
        let data = generate_data(64 * 1024);
        let digest = Md5::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_256kb() {
        let data = generate_data(256 * 1024);
        let digest = Md5::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_512kb() {
        let data = generate_data(512 * 1024);
        let digest = Md5::digest(&data);
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn size_1mb() {
        let data = generate_data(1024 * 1024);
        let digest = Md5::digest(&data);
        assert_eq!(digest.len(), 16);

        // Verify streaming matches
        let mut hasher = Md5::new();
        hasher.update(&data);
        assert_eq!(hasher.finalize(), digest);
    }

    #[test]
    fn size_1mb_chunked() {
        // Hash 1MB in 4KB chunks
        let data = generate_data(1024 * 1024);
        let mut hasher = Md5::new();
        for chunk in data.chunks(4096) {
            hasher.update(chunk);
        }
        let chunked = hasher.finalize();

        let oneshot = Md5::digest(&data);
        assert_eq!(chunked, oneshot);
    }

    #[test]
    fn sizes_near_block_boundaries() {
        // Test sizes around 64-byte block boundaries
        for offset in [-3_i32, -2, -1, 0, 1, 2, 3] {
            for multiplier in [1, 2, 4, 8, 16] {
                let base_size = 64 * multiplier;
                let size = (base_size + offset).max(0) as usize;
                let data = generate_data(size);

                let oneshot = Md5::digest(&data);
                let mut hasher = Md5::new();
                hasher.update(&data);
                let streaming = hasher.finalize();

                assert_eq!(
                    oneshot, streaming,
                    "Mismatch at size {size} (base={base_size}, offset={offset})"
                );
            }
        }
    }
}

// ============================================================================
// Streaming API Incremental Computation Tests
// ============================================================================

mod streaming_api {
    use super::*;

    #[test]
    fn streaming_byte_by_byte() {
        let data = b"The quick brown fox jumps over the lazy dog";
        let mut hasher = Md5::new();
        for &byte in data.iter() {
            hasher.update(&[byte]);
        }
        let streaming = hasher.finalize();

        let oneshot = Md5::digest(data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn streaming_two_halves() {
        let data = b"abcdefghijklmnopqrstuvwxyz";
        let mid = data.len() / 2;

        let mut hasher = Md5::new();
        hasher.update(&data[..mid]);
        hasher.update(&data[mid..]);
        let streaming = hasher.finalize();

        let oneshot = Md5::digest(data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn streaming_random_chunk_sizes() {
        let data: Vec<u8> = (0..1000).map(|i| (i * 17 % 256) as u8).collect();

        // Use varied chunk sizes
        let chunk_sizes = [1, 3, 7, 13, 31, 63, 127, 255];
        let mut hasher = Md5::new();
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
        let oneshot = Md5::digest(&data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn streaming_many_empty_updates() {
        let data = b"test data";
        let mut hasher = Md5::new();

        hasher.update(&[]);
        hasher.update(b"test");
        hasher.update(&[]);
        hasher.update(&[]);
        hasher.update(b" ");
        hasher.update(&[]);
        hasher.update(b"data");
        hasher.update(&[]);

        let streaming = hasher.finalize();
        let oneshot = Md5::digest(data);
        assert_eq!(streaming, oneshot);
    }

    #[test]
    fn streaming_clone_mid_computation() {
        let data = b"hello world";
        let mut hasher = Md5::new();
        hasher.update(b"hello");

        // Clone and continue with different data
        let cloned = hasher.clone();

        hasher.update(b" world");
        let full = hasher.finalize();

        let mut cloned_hasher = cloned;
        cloned_hasher.update(b" world");
        let cloned_full = cloned_hasher.finalize();

        assert_eq!(full, cloned_full);
        assert_eq!(full, Md5::digest(data));
    }

    #[test]
    fn streaming_large_data_various_chunk_sizes() {
        let data: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();
        let expected = Md5::digest(&data);

        for chunk_size in [1, 7, 13, 64, 128, 1000, 4096, 8192] {
            let mut hasher = Md5::new();
            for chunk in data.chunks(chunk_size) {
                hasher.update(chunk);
            }
            let result = hasher.finalize();
            assert_eq!(result, expected, "Mismatch with chunk_size={chunk_size}");
        }
    }

    #[test]
    fn streaming_with_seeded_proper_order() {
        let seed = Md5Seed::proper(0xDEADBEEF_u32 as i32);
        let data = b"seeded streaming test";

        let mut hasher = Md5::with_seed(seed);
        hasher.update(b"seeded ");
        hasher.update(b"streaming ");
        hasher.update(b"test");
        let streaming = hasher.finalize();

        // Compare with single-shot seeded
        let mut single = Md5::with_seed(seed);
        single.update(data);
        let single_result = single.finalize();

        assert_eq!(streaming, single_result);
    }

    #[test]
    fn streaming_with_seeded_legacy_order() {
        let seed = Md5Seed::legacy(0xCAFEBABE_u32 as i32);
        let data = b"legacy seeded test";

        let mut hasher = Md5::with_seed(seed);
        hasher.update(b"legacy ");
        hasher.update(b"seeded ");
        hasher.update(b"test");
        let streaming = hasher.finalize();

        // Compare with single-shot seeded
        let mut single = Md5::with_seed(seed);
        single.update(data);
        let single_result = single.finalize();

        assert_eq!(streaming, single_result);
    }

    #[test]
    fn trait_new_matches_inherent_new() {
        let mut trait_hasher: Md5 = StrongDigest::new();
        trait_hasher.update(b"trait test");
        let trait_result = trait_hasher.finalize();

        let mut inherent_hasher = Md5::new();
        inherent_hasher.update(b"trait test");
        let inherent_result = inherent_hasher.finalize();

        assert_eq!(trait_result, inherent_result);
    }

    #[test]
    fn trait_digest_matches_inherent_digest() {
        let trait_result = <Md5 as StrongDigest>::digest(b"quick test");
        let inherent_result = Md5::digest(b"quick test");
        assert_eq!(trait_result, inherent_result);
    }
}

// ============================================================================
// System md5sum Comparison Tests
// ============================================================================

mod system_md5sum_comparison {
    use super::*;

    /// Run system md5sum on the given data and return the hex digest.
    fn system_md5sum(data: &[u8]) -> Option<String> {
        let mut child = Command::new("md5sum")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        {
            let stdin = child.stdin.as_mut()?;
            stdin.write_all(data).ok()?;
        }

        let output = child.wait_with_output().ok()?;
        if !output.status.success() {
            return None;
        }

        // md5sum output format: "hash  -" or "hash  filename"
        let stdout = String::from_utf8(output.stdout).ok()?;
        stdout.split_whitespace().next().map(|s| s.to_lowercase())
    }

    #[test]
    fn compare_empty_with_system() {
        if let Some(system_hash) = system_md5sum(b"") {
            let our_hash = to_hex(&Md5::digest(b""));
            assert_eq!(
                our_hash, system_hash,
                "Empty string hash mismatch with system md5sum"
            );
        }
    }

    #[test]
    fn compare_single_byte_with_system() {
        if let Some(system_hash) = system_md5sum(&[0x42]) {
            let our_hash = to_hex(&Md5::digest(&[0x42]));
            assert_eq!(
                our_hash, system_hash,
                "Single byte hash mismatch with system md5sum"
            );
        }
    }

    #[test]
    fn compare_hello_world_with_system() {
        let data = b"Hello, World!";
        if let Some(system_hash) = system_md5sum(data) {
            let our_hash = to_hex(&Md5::digest(data));
            assert_eq!(
                our_hash, system_hash,
                "'Hello, World!' hash mismatch with system md5sum"
            );
        }
    }

    #[test]
    fn compare_rfc_vectors_with_system() {
        let test_cases: &[&[u8]] = &[
            b"",
            b"a",
            b"abc",
            b"message digest",
            b"abcdefghijklmnopqrstuvwxyz",
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
        ];

        for data in test_cases {
            if let Some(system_hash) = system_md5sum(data) {
                let our_hash = to_hex(&Md5::digest(data));
                assert_eq!(
                    our_hash,
                    system_hash,
                    "RFC vector {:?} hash mismatch with system md5sum",
                    String::from_utf8_lossy(data)
                );
            }
        }
    }

    #[test]
    fn compare_1kb_with_system() {
        let data: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();
        if let Some(system_hash) = system_md5sum(&data) {
            let our_hash = to_hex(&Md5::digest(&data));
            assert_eq!(
                our_hash, system_hash,
                "1KB data hash mismatch with system md5sum"
            );
        }
    }

    #[test]
    fn compare_64kb_with_system() {
        let data: Vec<u8> = (0..65536).map(|i| (i % 256) as u8).collect();
        if let Some(system_hash) = system_md5sum(&data) {
            let our_hash = to_hex(&Md5::digest(&data));
            assert_eq!(
                our_hash, system_hash,
                "64KB data hash mismatch with system md5sum"
            );
        }
    }

    #[test]
    fn compare_1mb_with_system() {
        let data: Vec<u8> = (0..1_048_576).map(|i| (i % 256) as u8).collect();
        if let Some(system_hash) = system_md5sum(&data) {
            let our_hash = to_hex(&Md5::digest(&data));
            assert_eq!(
                our_hash, system_hash,
                "1MB data hash mismatch with system md5sum"
            );
        }
    }

    #[test]
    fn compare_binary_data_with_system() {
        // Test with actual binary data including null bytes and all byte values
        let data: Vec<u8> = (0..=255).collect();
        if let Some(system_hash) = system_md5sum(&data) {
            let our_hash = to_hex(&Md5::digest(&data));
            assert_eq!(
                our_hash, system_hash,
                "Binary data (0-255) hash mismatch with system md5sum"
            );
        }
    }

    #[test]
    fn compare_repeated_patterns_with_system() {
        // Test repeated patterns of various sizes
        let patterns: &[&[u8]] = &[&[0xAA; 1000], &[0x00; 1000], &[0xFF; 1000]];

        for pattern in patterns {
            if let Some(system_hash) = system_md5sum(pattern) {
                let our_hash = to_hex(&Md5::digest(pattern));
                assert_eq!(
                    our_hash, system_hash,
                    "Repeated pattern hash mismatch with system md5sum"
                );
            }
        }
    }
}

// ============================================================================
// Additional Edge Case Tests
// ============================================================================

mod edge_cases {
    use super::*;

    #[test]
    fn digest_len_constant() {
        assert_eq!(Md5::DIGEST_LEN, 16);
    }

    #[test]
    fn digest_output_is_16_bytes() {
        let digest = Md5::digest(b"test");
        assert_eq!(digest.len(), 16);
        assert_eq!(digest.as_ref().len(), 16);
    }

    #[test]
    fn deterministic_output() {
        let data = b"determinism test";
        let d1 = Md5::digest(data);
        let d2 = Md5::digest(data);
        let d3 = Md5::digest(data);
        assert_eq!(d1, d2);
        assert_eq!(d2, d3);
    }

    #[test]
    fn different_inputs_different_outputs() {
        let d1 = Md5::digest(b"input1");
        let d2 = Md5::digest(b"input2");
        assert_ne!(d1, d2);
    }

    #[test]
    fn similar_inputs_different_outputs() {
        let d1 = Md5::digest(b"test");
        let d2 = Md5::digest(b"Test"); // Different case
        let d3 = Md5::digest(b"test "); // Trailing space
        let d4 = Md5::digest(b" test"); // Leading space

        assert_ne!(d1, d2);
        assert_ne!(d1, d3);
        assert_ne!(d1, d4);
        assert_ne!(d2, d3);
        assert_ne!(d2, d4);
        assert_ne!(d3, d4);
    }

    #[test]
    fn seed_value_affects_output() {
        let data = b"seeded test";

        let mut no_seed = Md5::with_seed(Md5Seed::none());
        no_seed.update(data);
        let no_seed_result = no_seed.finalize();

        let mut with_seed = Md5::with_seed(Md5Seed::proper(123));
        with_seed.update(data);
        let with_seed_result = with_seed.finalize();

        assert_ne!(no_seed_result, with_seed_result);
    }

    #[test]
    fn different_seeds_different_outputs() {
        let data = b"same data";

        let mut h1 = Md5::with_seed(Md5Seed::proper(1));
        h1.update(data);
        let r1 = h1.finalize();

        let mut h2 = Md5::with_seed(Md5Seed::proper(2));
        h2.update(data);
        let r2 = h2.finalize();

        assert_ne!(r1, r2);
    }

    #[test]
    fn proper_vs_legacy_order_different() {
        let data = b"order test";
        let seed_value = 0x12345678;

        let mut proper = Md5::with_seed(Md5Seed::proper(seed_value));
        proper.update(data);
        let proper_result = proper.finalize();

        let mut legacy = Md5::with_seed(Md5Seed::legacy(seed_value));
        legacy.update(data);
        let legacy_result = legacy.finalize();

        assert_ne!(proper_result, legacy_result);
    }

    #[test]
    fn debug_format_contains_md5() {
        let hasher = Md5::new();
        let debug = format!("{hasher:?}");
        assert!(debug.contains("Md5"));
    }

    #[test]
    fn default_equals_new() {
        let mut default_hasher = Md5::default();
        let mut new_hasher = Md5::new();

        default_hasher.update(b"test");
        new_hasher.update(b"test");

        assert_eq!(default_hasher.finalize(), new_hasher.finalize());
    }

    #[test]
    fn seed_default_equals_none() {
        let default_seed = Md5Seed::default();
        let none_seed = Md5Seed::none();
        assert_eq!(default_seed, none_seed);
        assert!(default_seed.value.is_none());
    }

    #[test]
    fn seed_accessors() {
        let proper = Md5Seed::proper(42);
        assert_eq!(proper.value, Some(42));
        assert!(proper.proper_order);

        let legacy = Md5Seed::legacy(-1);
        assert_eq!(legacy.value, Some(-1));
        assert!(!legacy.proper_order);

        let none = Md5Seed::none();
        assert!(none.value.is_none());
        assert!(none.proper_order);
    }

    #[test]
    fn all_zero_input_various_sizes() {
        for size in [0, 1, 16, 64, 128, 1024] {
            let data = vec![0u8; size];
            let digest = Md5::digest(&data);
            assert_eq!(digest.len(), 16);
        }
    }

    #[test]
    fn all_ones_input_various_sizes() {
        for size in [0, 1, 16, 64, 128, 1024] {
            let data = vec![0xFFu8; size];
            let digest = Md5::digest(&data);
            assert_eq!(digest.len(), 16);
        }
    }

    #[test]
    fn negative_seed_value() {
        let seed = Md5Seed::proper(-1);
        let mut hasher = Md5::with_seed(seed);
        hasher.update(b"test");
        let result = hasher.finalize();
        assert_eq!(result.len(), 16);
    }

    #[test]
    fn max_positive_seed_value() {
        let seed = Md5Seed::proper(i32::MAX);
        let mut hasher = Md5::with_seed(seed);
        hasher.update(b"test");
        let result = hasher.finalize();
        assert_eq!(result.len(), 16);
    }

    #[test]
    fn min_negative_seed_value() {
        let seed = Md5Seed::proper(i32::MIN);
        let mut hasher = Md5::with_seed(seed);
        hasher.update(b"test");
        let result = hasher.finalize();
        assert_eq!(result.len(), 16);
    }
}

// ============================================================================
// Batch API Tests
// ============================================================================

mod batch_api {
    use super::*;
    use checksums::strong::md5_digest_batch;

    #[test]
    fn batch_empty_inputs() {
        let inputs: &[&[u8]] = &[];
        let results = md5_digest_batch(inputs);
        assert!(results.is_empty());
    }

    #[test]
    fn batch_single_input() {
        let inputs: &[&[u8]] = &[b"single"];
        let results = md5_digest_batch(inputs);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], Md5::digest(b"single"));
    }

    #[test]
    fn batch_multiple_inputs() {
        let inputs: &[&[u8]] = &[b"a", b"b", b"c", b"abc"];
        let results = md5_digest_batch(inputs);
        assert_eq!(results.len(), 4);
        for (i, result) in results.iter().enumerate() {
            assert_eq!(*result, Md5::digest(inputs[i]));
        }
    }

    #[test]
    fn batch_matches_sequential() {
        let inputs: Vec<Vec<u8>> = (0..100)
            .map(|i| (0..((i % 50) + 1)).map(|j| (j % 256) as u8).collect())
            .collect();
        let input_refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();

        let batch_results = md5_digest_batch(&input_refs);
        let sequential_results: Vec<[u8; 16]> = inputs.iter().map(|v| Md5::digest(v)).collect();

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

        let results = md5_digest_batch(inputs);

        let expected = [
            "d41d8cd98f00b204e9800998ecf8427e",
            "0cc175b9c0f1b6a831c399e269772661",
            "900150983cd24fb0d6963f7d28e17f72",
            "f96b697d7cb7938d525a2f31aaf161d0",
            "c3fcd3d76192e4007dfb496cca67e13b",
        ];

        for (i, result) in results.iter().enumerate() {
            assert_eq!(
                to_hex(result),
                expected[i],
                "Batch result mismatch at index {i}"
            );
        }
    }
}
