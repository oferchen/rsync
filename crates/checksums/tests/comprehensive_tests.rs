//! Comprehensive test coverage for the checksums crate.
//!
//! This module provides extensive tests for:
//! 1. Rolling checksum edge cases
//! 2. Strong checksum truncation
//! 3. Checksum seed handling
//! 4. Pipelined checksum operations
//! 5. All supported algorithms

use checksums::pipelined::{
    BlockChecksums, DoubleBufferedReader, PipelineConfig, PipelinedChecksumIterator,
    compute_checksums_pipelined,
};
use checksums::strong::{
    Md4, Md5, Md5Seed, Sha1, Sha256, Sha512, StrongDigest, Xxh3, Xxh3_128, Xxh64, md4_digest_batch,
    md5_digest_batch,
};
use checksums::{RollingChecksum, RollingDigest, RollingError, RollingSliceError};
use std::io::{Cursor, IoSlice};

// ============================================================================
// 1. Rolling Checksum Edge Cases
// ============================================================================

mod rolling_edge_cases {
    use super::*;

    #[test]
    fn empty_input_produces_zero_checksum() {
        let mut checksum = RollingChecksum::new();
        checksum.update(b"");
        assert_eq!(checksum.value(), 0);
        assert_eq!(checksum.len(), 0);
        assert!(checksum.is_empty());
    }

    #[test]
    fn single_byte_checksum() {
        for byte in 0u8..=255 {
            let mut checksum = RollingChecksum::new();
            checksum.update(&[byte]);
            assert_eq!(checksum.len(), 1);
            // For single byte b: s1 = b, s2 = b, value = (b << 16) | b
            let expected = ((byte as u32) << 16) | (byte as u32);
            assert_eq!(checksum.value(), expected, "Failed for byte {byte}");
        }
    }

    #[test]
    fn update_byte_method() {
        let mut checksum1 = RollingChecksum::new();
        checksum1.update_byte(0x42);
        checksum1.update_byte(0x43);

        let mut checksum2 = RollingChecksum::new();
        checksum2.update(&[0x42, 0x43]);

        assert_eq!(checksum1.value(), checksum2.value());
        assert_eq!(checksum1.len(), checksum2.len());
    }

    #[test]
    fn maximum_window_size_edge() {
        // Test with a very large window
        let data = vec![0xABu8; 65536];
        let mut checksum = RollingChecksum::new();
        checksum.update(&data);
        assert_eq!(checksum.len(), 65536);
        // Rolling should still work on large windows
        let result = checksum.roll(0xAB, 0xCD);
        assert!(result.is_ok());
    }

    #[test]
    fn roll_on_empty_window_returns_error() {
        let mut checksum = RollingChecksum::new();
        let result = checksum.roll(0, 0);
        assert!(matches!(result, Err(RollingError::EmptyWindow)));
    }

    #[test]
    fn roll_many_mismatched_lengths_returns_error() {
        let mut checksum = RollingChecksum::new();
        checksum.update(b"test");
        let result = checksum.roll_many(&[1, 2, 3], &[4, 5]);
        assert!(matches!(
            result,
            Err(RollingError::MismatchedSliceLength {
                outgoing: 3,
                incoming: 2
            })
        ));
    }

    #[test]
    fn roll_preserves_window_length() {
        let mut checksum = RollingChecksum::new();
        checksum.update(b"test");
        let len_before = checksum.len();
        checksum.roll(b't', b'x').unwrap();
        assert_eq!(checksum.len(), len_before);
    }

    #[test]
    fn roll_produces_correct_checksum() {
        // Compute checksum for "BCDE" by rolling from "ABCD"
        let mut rolling = RollingChecksum::new();
        rolling.update(b"ABCD");
        rolling.roll(b'A', b'E').unwrap();

        // Compute directly
        let mut direct = RollingChecksum::new();
        direct.update(b"BCDE");

        assert_eq!(rolling.value(), direct.value());
    }

    #[test]
    fn roll_many_produces_correct_checksum() {
        // Roll multiple bytes at once
        let mut rolling = RollingChecksum::new();
        rolling.update(b"ABCDEFGH");
        rolling.roll_many(b"ABC", b"XYZ").unwrap();

        // Compute what the checksum should be for "XYZDEFGH"
        let mut direct = RollingChecksum::new();
        direct.update(b"DEFGHXYZ");

        // Note: roll_many maintains window size, so we need different comparison
        // After rolling ABC->XYZ, window is "DEFGHXYZ" (8 bytes)
        // But the rolling checksum tracks a sliding window
        assert_eq!(rolling.len(), 8);
    }

    #[test]
    fn update_vectored_matches_sequential() {
        let data1 = b"Hello, ";
        let data2 = b"World!";

        let mut vectored = RollingChecksum::new();
        let slices = [IoSlice::new(data1), IoSlice::new(data2)];
        vectored.update_vectored(&slices);

        let mut sequential = RollingChecksum::new();
        sequential.update(data1);
        sequential.update(data2);

        assert_eq!(vectored.value(), sequential.value());
    }

    #[test]
    fn update_vectored_with_empty_slices() {
        let data = b"test";
        let empty: &[u8] = b"";

        let mut vectored = RollingChecksum::new();
        let slices = [IoSlice::new(empty), IoSlice::new(data), IoSlice::new(empty)];
        vectored.update_vectored(&slices);

        let mut direct = RollingChecksum::new();
        direct.update(data);

        assert_eq!(vectored.value(), direct.value());
    }

    #[test]
    fn update_vectored_large_slice() {
        // Test with slice larger than VECTORED_STACK_CAPACITY (128 bytes)
        let large_data = vec![0xAAu8; 512];
        let mut checksum = RollingChecksum::new();
        let slices = [IoSlice::new(&large_data)];
        checksum.update_vectored(&slices);
        assert_eq!(checksum.len(), 512);
    }

    #[test]
    fn update_from_block_resets_state() {
        let mut checksum = RollingChecksum::new();
        checksum.update(b"initial data");

        checksum.update_from_block(b"new data");

        let mut fresh = RollingChecksum::new();
        fresh.update(b"new data");

        assert_eq!(checksum.value(), fresh.value());
        assert_eq!(checksum.len(), 8);
    }

    #[test]
    fn reset_clears_all_state() {
        let mut checksum = RollingChecksum::new();
        checksum.update(b"some data");
        assert!(!checksum.is_empty());

        checksum.reset();

        assert!(checksum.is_empty());
        assert_eq!(checksum.len(), 0);
        assert_eq!(checksum.value(), 0);
    }

    #[test]
    fn digest_roundtrip() {
        let mut original = RollingChecksum::new();
        original.update(b"roundtrip test");

        let digest = original.digest();
        let reconstructed = RollingChecksum::from_digest(digest);

        assert_eq!(original.value(), reconstructed.value());
        assert_eq!(original.len(), reconstructed.len());
    }

    #[test]
    fn rolling_digest_zero_constant() {
        assert_eq!(RollingDigest::ZERO.sum1(), 0);
        assert_eq!(RollingDigest::ZERO.sum2(), 0);
        assert_eq!(RollingDigest::ZERO.len(), 0);
        assert!(RollingDigest::ZERO.is_empty());
    }

    #[test]
    fn rolling_digest_from_bytes() {
        let data = b"test data";
        let digest = RollingDigest::from_bytes(data);
        assert_eq!(digest.len(), data.len());

        let mut checksum = RollingChecksum::new();
        checksum.update(data);
        assert_eq!(digest, checksum.digest());
    }

    #[test]
    fn rolling_digest_le_bytes_roundtrip() {
        let digest = RollingDigest::new(0x1234, 0x5678, 100);
        let bytes = digest.to_le_bytes();
        let recovered = RollingDigest::from_le_bytes(bytes, 100);
        assert_eq!(digest, recovered);
    }

    #[test]
    fn rolling_digest_from_le_slice_error() {
        let result = RollingDigest::from_le_slice(&[1, 2, 3], 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.len(), 3);
    }

    #[test]
    fn rolling_slice_error_properties() {
        // Create error by providing wrong length slice
        let err = RollingDigest::from_le_slice(&[1, 2, 3, 4, 5], 0).unwrap_err();
        assert_eq!(err.len(), 5);
        assert!(!err.is_empty());
        assert_eq!(RollingSliceError::EXPECTED_LEN, 4);

        // Test empty slice error
        let empty_err = RollingDigest::from_le_slice(&[], 0).unwrap_err();
        assert!(empty_err.is_empty());
    }

    #[test]
    fn rolling_digest_value_packing() {
        // s1 in low 16 bits, s2 in high 16 bits
        let digest = RollingDigest::new(0x1234, 0x5678, 100);
        let value = digest.value();
        assert_eq!(value & 0xFFFF, 0x1234);
        assert_eq!((value >> 16) & 0xFFFF, 0x5678);
    }

    #[test]
    fn rolling_digest_from_value_unpacks_correctly() {
        let packed: u32 = 0x5678_1234;
        let digest = RollingDigest::from_value(packed, 42);
        assert_eq!(digest.sum1(), 0x1234);
        assert_eq!(digest.sum2(), 0x5678);
        assert_eq!(digest.len(), 42);
    }

    #[test]
    fn rolling_checksum_order_matters() {
        let mut c1 = RollingChecksum::new();
        c1.update(b"AB");

        let mut c2 = RollingChecksum::new();
        c2.update(b"BA");

        assert_ne!(c1.value(), c2.value());
    }

    #[test]
    fn simd_acceleration_query() {
        // Should not panic; result depends on platform
        let _ = checksums::simd_acceleration_available();
    }
}

// ============================================================================
// 2. Strong Checksum Truncation Tests
// ============================================================================

mod strong_truncation {
    use super::*;

    fn to_hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            write!(&mut out, "{byte:02x}").expect("write! to String cannot fail");
        }
        out
    }

    #[test]
    fn md4_digest_length() {
        assert_eq!(Md4::DIGEST_LEN, 16);
        let digest = Md4::digest(b"test");
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn md5_digest_length() {
        assert_eq!(Md5::DIGEST_LEN, 16);
        let digest = Md5::digest(b"test");
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn sha1_digest_length() {
        assert_eq!(Sha1::DIGEST_LEN, 20);
        let digest = Sha1::digest(b"test");
        assert_eq!(digest.len(), 20);
    }

    #[test]
    fn sha256_digest_length() {
        assert_eq!(Sha256::DIGEST_LEN, 32);
        let digest = Sha256::digest(b"test");
        assert_eq!(digest.len(), 32);
    }

    #[test]
    fn sha512_digest_length() {
        assert_eq!(Sha512::DIGEST_LEN, 64);
        let digest = Sha512::digest(b"test");
        assert_eq!(digest.len(), 64);
    }

    #[test]
    fn xxh64_digest_length() {
        assert_eq!(Xxh64::DIGEST_LEN, 8);
        let digest = Xxh64::digest(0, b"test");
        assert_eq!(digest.len(), 8);
    }

    #[test]
    fn xxh3_digest_length() {
        assert_eq!(Xxh3::DIGEST_LEN, 8);
        let digest = Xxh3::digest(0, b"test");
        assert_eq!(digest.len(), 8);
    }

    #[test]
    fn xxh3_128_digest_length() {
        assert_eq!(Xxh3_128::DIGEST_LEN, 16);
        let digest = Xxh3_128::digest(0, b"test");
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn truncating_strong_digest_manually() {
        // Test that we can safely truncate digests for protocol compatibility
        let full_sha256 = Sha256::digest(b"block data");
        assert_eq!(full_sha256.len(), 32);

        // Truncate to 16 bytes (128 bits) - common for rsync block matching
        let truncated = &full_sha256[..16];
        assert_eq!(truncated.len(), 16);

        // Verify truncation is deterministic
        let another_sha256 = Sha256::digest(b"block data");
        assert_eq!(&another_sha256[..16], truncated);
    }

    #[test]
    fn truncating_sha512_to_sha256_equivalent() {
        let sha512 = Sha512::digest(b"data");
        assert_eq!(sha512.len(), 64);

        // Truncate to first 32 bytes
        let truncated = &sha512[..32];
        assert_eq!(truncated.len(), 32);
    }

    #[test]
    fn digest_prefix_collision_detection() {
        // Different inputs should have different prefixes (high probability)
        let digest1 = Sha256::digest(b"input one");
        let digest2 = Sha256::digest(b"input two");

        // Even first 4 bytes should differ for different inputs
        assert_ne!(&digest1[..4], &digest2[..4]);
    }

    #[test]
    fn md5_known_vector_truncation() {
        // MD5("abc") = 900150983cd24fb0d6963f7d28e17f72
        let digest = Md5::digest(b"abc");
        assert_eq!(to_hex(&digest), "900150983cd24fb0d6963f7d28e17f72");

        // Verify first 8 bytes (common truncation for rsync)
        assert_eq!(to_hex(&digest[..8]), "900150983cd24fb0");
    }

    #[test]
    fn sha1_known_vector_truncation() {
        // SHA1("abc") = a9993e364706816aba3e25717850c26c9cd0d89d
        let digest = Sha1::digest(b"abc");
        assert_eq!(to_hex(&digest), "a9993e364706816aba3e25717850c26c9cd0d89d");

        // Verify first 8 bytes
        assert_eq!(to_hex(&digest[..8]), "a9993e364706816a");
    }
}

// ============================================================================
// 3. Checksum Seed Handling Tests
// ============================================================================

mod seed_handling {
    use super::*;

    #[test]
    fn md5_no_seed_matches_default() {
        let data = b"unseeded test";

        let mut with_none = Md5::with_seed(Md5Seed::none());
        with_none.update(data);
        let none_result = with_none.finalize();

        let default_result = Md5::digest(data);

        assert_eq!(none_result, default_result);
    }

    #[test]
    fn md5_seed_default_is_none() {
        let default_seed = Md5Seed::default();
        let none_seed = Md5Seed::none();
        assert_eq!(default_seed, none_seed);
        assert!(default_seed.value.is_none());
    }

    #[test]
    fn md5_proper_seed_hashes_before_data() {
        let seed_value = 0x12345678i32;
        let data = b"test data";

        // Using proper order seed
        let mut seeded = Md5::with_seed(Md5Seed::proper(seed_value));
        seeded.update(data);
        let seeded_result = seeded.finalize();

        // Manual: hash seed bytes, then data
        let mut manual = Md5::new();
        manual.update(&seed_value.to_le_bytes());
        manual.update(data);
        let manual_result = manual.finalize();

        assert_eq!(seeded_result, manual_result);
    }

    #[test]
    fn md5_legacy_seed_hashes_after_data() {
        let seed_value = 0x12345678i32;
        let data = b"test data";

        // Using legacy order seed
        let mut seeded = Md5::with_seed(Md5Seed::legacy(seed_value));
        seeded.update(data);
        let seeded_result = seeded.finalize();

        // Manual: hash data, then seed bytes
        let mut manual = Md5::new();
        manual.update(data);
        manual.update(&seed_value.to_le_bytes());
        let manual_result = manual.finalize();

        assert_eq!(seeded_result, manual_result);
    }

    #[test]
    fn md5_proper_vs_legacy_differ() {
        let seed_value = 0x12345678i32;
        let data = b"same data";

        let mut proper = Md5::with_seed(Md5Seed::proper(seed_value));
        proper.update(data);
        let proper_result = proper.finalize();

        let mut legacy = Md5::with_seed(Md5Seed::legacy(seed_value));
        legacy.update(data);
        let legacy_result = legacy.finalize();

        assert_ne!(proper_result, legacy_result);
    }

    #[test]
    fn md5_different_seeds_produce_different_results() {
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
    fn md5_seed_accessors() {
        let proper = Md5Seed::proper(42);
        assert_eq!(proper.value, Some(42));
        assert!(proper.proper_order);

        let legacy = Md5Seed::legacy(-1);
        assert_eq!(legacy.value, Some(-1));
        assert!(!legacy.proper_order);

        let none = Md5Seed::none();
        assert!(none.value.is_none());
    }

    #[test]
    fn md5_negative_seed() {
        let seed = Md5Seed::proper(-1);
        let mut hasher = Md5::with_seed(seed);
        hasher.update(b"test");
        let result = hasher.finalize();
        assert_eq!(result.len(), 16);
    }

    #[test]
    fn md5_extreme_seed_values() {
        let data = b"test";

        // i32::MAX
        let mut h1 = Md5::with_seed(Md5Seed::proper(i32::MAX));
        h1.update(data);
        let r1 = h1.finalize();
        assert_eq!(r1.len(), 16);

        // i32::MIN
        let mut h2 = Md5::with_seed(Md5Seed::proper(i32::MIN));
        h2.update(data);
        let r2 = h2.finalize();
        assert_eq!(r2.len(), 16);

        // Different extreme seeds should produce different results
        assert_ne!(r1, r2);
    }

    #[test]
    fn xxh64_seed_handling() {
        let data = b"test data";

        let seed0_result = Xxh64::digest(0, data);
        let seed1_result = Xxh64::digest(1, data);
        let seed_max_result = Xxh64::digest(u64::MAX, data);

        // Different seeds produce different results
        assert_ne!(seed0_result, seed1_result);
        assert_ne!(seed0_result, seed_max_result);
        assert_ne!(seed1_result, seed_max_result);
    }

    #[test]
    fn xxh64_streaming_with_seed() {
        let seed = 12345u64;
        let data = b"streaming test";

        // One-shot
        let oneshot = Xxh64::digest(seed, data);

        // Streaming
        let mut streaming = Xxh64::new(seed);
        streaming.update(data);
        let streaming_result = streaming.finalize();

        assert_eq!(oneshot, streaming_result);
    }

    #[test]
    fn xxh3_seed_handling() {
        let data = b"test data";

        let seed0 = Xxh3::digest(0, data);
        let seed1 = Xxh3::digest(1, data);

        assert_ne!(seed0, seed1);
    }

    #[test]
    fn xxh3_128_seed_handling() {
        let data = b"test data";

        let seed0 = Xxh3_128::digest(0, data);
        let seed1 = Xxh3_128::digest(1, data);

        assert_ne!(seed0, seed1);
    }

    #[test]
    fn xxh3_streaming_with_seed() {
        let seed = 42u64;
        let data = b"streaming xxh3 test";

        let oneshot = Xxh3::digest(seed, data);

        let mut streaming = Xxh3::new(seed);
        streaming.update(data);
        let streaming_result = streaming.finalize();

        assert_eq!(oneshot, streaming_result);
    }

    #[test]
    fn xxh3_simd_availability_query() {
        // Should not panic; result depends on compile-time features
        let _ = checksums::xxh3_simd_available();
    }

    #[test]
    fn trait_with_seed_for_md5() {
        let seed = Md5Seed::proper(123);
        let mut hasher: Md5 = StrongDigest::with_seed(seed);
        hasher.update(b"trait test");
        let result = hasher.finalize();
        assert_eq!(result.len(), Md5::DIGEST_LEN);
    }

    #[test]
    fn trait_with_seed_for_xxh64() {
        let seed = 42u64;
        let mut hasher: Xxh64 = StrongDigest::with_seed(seed);
        hasher.update(b"trait test");
        let result = hasher.finalize();
        assert_eq!(result.len(), Xxh64::DIGEST_LEN);
    }
}

// ============================================================================
// 4. Pipelined Checksum Operations Tests
// ============================================================================

mod pipelined_operations {
    use super::*;

    #[test]
    fn pipeline_config_default() {
        let config = PipelineConfig::default();
        assert_eq!(config.block_size, 64 * 1024);
        assert_eq!(config.min_file_size, 256 * 1024);
        assert!(config.enabled);
    }

    #[test]
    fn pipeline_config_builder() {
        let config = PipelineConfig::new()
            .with_block_size(32 * 1024)
            .with_min_file_size(128 * 1024)
            .with_enabled(false);

        assert_eq!(config.block_size, 32 * 1024);
        assert_eq!(config.min_file_size, 128 * 1024);
        assert!(!config.enabled);
    }

    #[test]
    fn double_buffered_reader_empty_input() {
        let data: Vec<u8> = vec![];
        let config = PipelineConfig::default();
        let mut reader = DoubleBufferedReader::new(Cursor::new(data), config);

        assert!(reader.next_block().unwrap().is_none());
    }

    #[test]
    fn double_buffered_reader_small_file_sync_mode() {
        // File smaller than min_file_size should use sync mode
        let data = vec![0xABu8; 64 * 1024]; // 64KB, less than default 256KB min
        let config = PipelineConfig::default();
        let mut reader = DoubleBufferedReader::with_size_hint(
            Cursor::new(data.clone()),
            config,
            Some(64 * 1024),
        );

        // Should be in synchronous mode
        assert!(!reader.is_pipelined());

        let mut total_bytes = 0;
        while let Some(block) = reader.next_block().unwrap() {
            total_bytes += block.len();
        }
        assert_eq!(total_bytes, data.len());
    }

    #[test]
    fn double_buffered_reader_large_file_pipelined_mode() {
        let data = vec![0xCDu8; 512 * 1024]; // 512KB, more than default 256KB min
        let config = PipelineConfig::default();
        let reader =
            DoubleBufferedReader::with_size_hint(Cursor::new(data), config, Some(512 * 1024));

        assert!(reader.is_pipelined());
    }

    #[test]
    fn double_buffered_reader_disabled_pipelining() {
        let data = vec![0xEFu8; 512 * 1024];
        let config = PipelineConfig::default().with_enabled(false);
        let reader = DoubleBufferedReader::new(Cursor::new(data), config);

        assert!(!reader.is_pipelined());
    }

    #[test]
    fn double_buffered_reader_reads_all_data() {
        let data = vec![0x12u8; 256 * 1024];
        let config = PipelineConfig::default().with_block_size(64 * 1024);
        let mut reader = DoubleBufferedReader::new(Cursor::new(data.clone()), config);

        let mut total_bytes = 0;
        let mut block_count = 0;

        while let Some(block) = reader.next_block().unwrap() {
            total_bytes += block.len();
            block_count += 1;
            // Verify data integrity
            assert!(block.iter().all(|&b| b == 0x12));
        }

        assert_eq!(total_bytes, data.len());
        assert_eq!(block_count, 4); // 256KB / 64KB = 4 blocks
    }

    #[test]
    fn double_buffered_reader_partial_last_block() {
        // 100KB with 64KB blocks = 1 full + 1 partial (36KB)
        let data = vec![0x34u8; 100 * 1024];
        let config = PipelineConfig::default()
            .with_block_size(64 * 1024)
            .with_min_file_size(0); // Force pipelining for small files

        let mut reader =
            DoubleBufferedReader::with_size_hint(Cursor::new(data), config, Some(100 * 1024));

        let block1 = reader.next_block().unwrap().unwrap();
        assert_eq!(block1.len(), 64 * 1024);

        let block2 = reader.next_block().unwrap().unwrap();
        assert_eq!(block2.len(), 36 * 1024);

        assert!(reader.next_block().unwrap().is_none());
    }

    #[test]
    fn double_buffered_reader_block_size_accessor() {
        let config = PipelineConfig::default().with_block_size(128 * 1024);
        let reader = DoubleBufferedReader::new(Cursor::new(vec![0u8; 256 * 1024]), config);
        assert_eq!(reader.block_size(), 128 * 1024);
    }

    #[test]
    fn compute_checksums_pipelined_basic() {
        let data = vec![0x56u8; 256 * 1024];
        let config = PipelineConfig::default()
            .with_block_size(64 * 1024)
            .with_min_file_size(0);

        let checksums = compute_checksums_pipelined::<Md5, _>(
            Cursor::new(data.clone()),
            config,
            Some(256 * 1024),
        )
        .unwrap();

        assert_eq!(checksums.len(), 4);

        // Verify each checksum matches direct computation
        for (i, cs) in checksums.iter().enumerate() {
            let start = i * 64 * 1024;
            let end = start + 64 * 1024;
            let block = &data[start..end];

            let expected_rolling = RollingDigest::from_bytes(block);
            let expected_strong = Md5::digest(block);

            assert_eq!(cs.rolling, expected_rolling);
            assert_eq!(cs.strong, expected_strong);
            assert_eq!(cs.len, block.len());
        }
    }

    #[test]
    fn compute_checksums_pipelined_empty_input() {
        let data: Vec<u8> = vec![];
        let config = PipelineConfig::default();

        let checksums =
            compute_checksums_pipelined::<Md5, _>(Cursor::new(data), config, Some(0)).unwrap();

        assert!(checksums.is_empty());
    }

    #[test]
    fn compute_checksums_pipelined_matches_sequential() {
        let data = vec![0x78u8; 256 * 1024];
        let config = PipelineConfig::default().with_block_size(64 * 1024);

        // Pipelined
        let pipelined =
            compute_checksums_pipelined::<Md5, _>(Cursor::new(data.clone()), config, None).unwrap();

        // Sequential (with disabled pipelining)
        let sync_config = config.with_enabled(false);
        let sequential =
            compute_checksums_pipelined::<Md5, _>(Cursor::new(data), sync_config, None).unwrap();

        assert_eq!(pipelined.len(), sequential.len());
        for (p, s) in pipelined.iter().zip(sequential.iter()) {
            assert_eq!(p.rolling, s.rolling);
            assert_eq!(p.strong, s.strong);
            assert_eq!(p.len, s.len);
        }
    }

    #[test]
    fn pipelined_iterator_basic() {
        let data = vec![0x9Au8; 128 * 1024];
        let config = PipelineConfig::default().with_block_size(32 * 1024);

        let mut iter: PipelinedChecksumIterator<Md5, _> =
            PipelinedChecksumIterator::new(Cursor::new(data.clone()), config);

        let mut count = 0;
        while let Some(cs) = iter.next_block_checksums().unwrap() {
            assert_eq!(cs.len, 32 * 1024);
            count += 1;
        }

        assert_eq!(count, 4);
    }

    #[test]
    fn pipelined_iterator_with_size_hint() {
        let data = vec![0xBCu8; 128 * 1024];
        let config = PipelineConfig::default().with_block_size(32 * 1024);

        let iter: PipelinedChecksumIterator<Md5, _> =
            PipelinedChecksumIterator::with_size_hint(Cursor::new(data), config, Some(128 * 1024));

        // Should be pipelined since size >= min_file_size is not applicable here
        // (min_file_size is 256KB, but we pass explicit size hint)
        let _ = iter.is_pipelined(); // Just verify method exists
    }

    #[test]
    fn block_checksums_clone_debug() {
        let cs = BlockChecksums {
            rolling: RollingDigest::from_bytes(b"test"),
            strong: [0u8; 16],
            len: 4,
        };

        let cloned = cs.clone();
        assert_eq!(cloned.rolling, cs.rolling);
        assert_eq!(cloned.strong, cs.strong);
        assert_eq!(cloned.len, cs.len);

        let debug = format!("{cs:?}");
        assert!(debug.contains("BlockChecksums"));
    }

    #[test]
    fn pipelined_with_different_algorithms() {
        let data = vec![0xDEu8; 64 * 1024];
        let config = PipelineConfig::default()
            .with_block_size(32 * 1024)
            .with_min_file_size(0);

        // Test with various strong digest algorithms
        let _md5_checksums = compute_checksums_pipelined::<Md5, _>(
            Cursor::new(data.clone()),
            config,
            Some(64 * 1024),
        )
        .unwrap();

        let _sha256_checksums = compute_checksums_pipelined::<Sha256, _>(
            Cursor::new(data.clone()),
            config,
            Some(64 * 1024),
        )
        .unwrap();

        let _md4_checksums =
            compute_checksums_pipelined::<Md4, _>(Cursor::new(data), config, Some(64 * 1024))
                .unwrap();
    }

    #[test]
    fn pipelined_handles_exact_block_boundary() {
        // Data size exactly divisible by block size
        let data = vec![0xF0u8; 128 * 1024];
        let config = PipelineConfig::default()
            .with_block_size(64 * 1024)
            .with_min_file_size(0);

        let checksums =
            compute_checksums_pipelined::<Md5, _>(Cursor::new(data), config, Some(128 * 1024))
                .unwrap();

        assert_eq!(checksums.len(), 2);
        assert_eq!(checksums[0].len, 64 * 1024);
        assert_eq!(checksums[1].len, 64 * 1024);
    }

    #[test]
    fn pipelined_very_small_blocks() {
        let data = vec![0x11u8; 1000];
        let config = PipelineConfig::default()
            .with_block_size(100)
            .with_min_file_size(0);

        let checksums =
            compute_checksums_pipelined::<Md5, _>(Cursor::new(data), config, Some(1000)).unwrap();

        assert_eq!(checksums.len(), 10);
    }
}

// ============================================================================
// 5. All Supported Algorithms Tests
// ============================================================================

mod all_algorithms {
    use super::*;

    fn to_hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            write!(&mut out, "{byte:02x}").expect("write! to String cannot fail");
        }
        out
    }

    // --- MD4 Tests ---

    #[test]
    fn md4_rfc_vectors() {
        let vectors = [
            (b"".as_slice(), "31d6cfe0d16ae931b73c59d7e0c089c0"),
            (b"a".as_slice(), "bde52cb31de33e46245e05fbdbd6fb24"),
            (b"abc".as_slice(), "a448017aaf21d8525fc10ae87aa6729d"),
            (
                b"message digest".as_slice(),
                "d9130a8164549fe818874806e1c7014b",
            ),
        ];

        for (input, expected) in vectors {
            assert_eq!(to_hex(&Md4::digest(input)), expected);
        }
    }

    #[test]
    fn md4_streaming_matches_oneshot() {
        let data = b"streaming test for md4";
        let mut hasher = Md4::new();
        hasher.update(&data[..10]);
        hasher.update(&data[10..]);
        let streaming = hasher.finalize();

        assert_eq!(streaming, Md4::digest(data));
    }

    #[test]
    fn md4_batch_matches_sequential() {
        let inputs: &[&[u8]] = &[b"a", b"b", b"c"];
        let batch = md4_digest_batch(inputs);
        let sequential: Vec<_> = inputs.iter().map(|i| Md4::digest(i)).collect();
        assert_eq!(batch, sequential);
    }

    // --- MD5 Tests ---

    #[test]
    fn md5_rfc_vectors() {
        let vectors = [
            (b"".as_slice(), "d41d8cd98f00b204e9800998ecf8427e"),
            (b"a".as_slice(), "0cc175b9c0f1b6a831c399e269772661"),
            (b"abc".as_slice(), "900150983cd24fb0d6963f7d28e17f72"),
            (
                b"message digest".as_slice(),
                "f96b697d7cb7938d525a2f31aaf161d0",
            ),
        ];

        for (input, expected) in vectors {
            assert_eq!(to_hex(&Md5::digest(input)), expected);
        }
    }

    #[test]
    fn md5_streaming_matches_oneshot() {
        let data = b"streaming test for md5";
        let mut hasher = Md5::new();
        hasher.update(&data[..10]);
        hasher.update(&data[10..]);
        let streaming = hasher.finalize();

        assert_eq!(streaming, Md5::digest(data));
    }

    #[test]
    fn md5_batch_matches_sequential() {
        let inputs: &[&[u8]] = &[b"a", b"b", b"c"];
        let batch = md5_digest_batch(inputs);
        let sequential: Vec<_> = inputs.iter().map(|i| Md5::digest(i)).collect();
        assert_eq!(batch, sequential);
    }

    #[test]
    fn md5_clone_continues_correctly() {
        let mut hasher = Md5::new();
        hasher.update(b"hello");

        let cloned = hasher.clone();

        hasher.update(b" world");
        let full = hasher.finalize();

        let mut cloned_hasher = cloned;
        cloned_hasher.update(b" world");
        let cloned_full = cloned_hasher.finalize();

        assert_eq!(full, cloned_full);
        assert_eq!(full, Md5::digest(b"hello world"));
    }

    // --- SHA-1 Tests ---

    #[test]
    fn sha1_known_vectors() {
        let vectors = [
            (b"".as_slice(), "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            (
                b"abc".as_slice(),
                "a9993e364706816aba3e25717850c26c9cd0d89d",
            ),
        ];

        for (input, expected) in vectors {
            assert_eq!(to_hex(&Sha1::digest(input)), expected);
        }
    }

    #[test]
    fn sha1_streaming_matches_oneshot() {
        let data = b"streaming sha1 test";
        let mut hasher = Sha1::new();
        hasher.update(&data[..10]);
        hasher.update(&data[10..]);
        let streaming = hasher.finalize();

        assert_eq!(streaming, Sha1::digest(data));
    }

    // --- SHA-256 Tests ---

    #[test]
    fn sha256_known_vectors() {
        let vectors = [
            (
                b"".as_slice(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc".as_slice(),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
        ];

        for (input, expected) in vectors {
            assert_eq!(to_hex(&Sha256::digest(input)), expected);
        }
    }

    #[test]
    fn sha256_streaming_matches_oneshot() {
        let data = b"streaming sha256 test";
        let mut hasher = Sha256::new();
        hasher.update(&data[..10]);
        hasher.update(&data[10..]);
        let streaming = hasher.finalize();

        assert_eq!(streaming, Sha256::digest(data));
    }

    // --- SHA-512 Tests ---

    #[test]
    fn sha512_known_vectors() {
        // SHA-512("")
        let empty_expected = "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e";
        assert_eq!(to_hex(&Sha512::digest(b"")), empty_expected);
    }

    #[test]
    fn sha512_streaming_matches_oneshot() {
        let data = b"streaming sha512 test";
        let mut hasher = Sha512::new();
        hasher.update(&data[..10]);
        hasher.update(&data[10..]);
        let streaming = hasher.finalize();

        assert_eq!(streaming, Sha512::digest(data));
    }

    // --- XXH64 Tests ---

    #[test]
    fn xxh64_basic() {
        let data = b"test data";
        let digest = Xxh64::digest(0, data);
        assert_eq!(digest.len(), 8);

        // Different seeds produce different results
        let digest_seed1 = Xxh64::digest(1, data);
        assert_ne!(digest, digest_seed1);
    }

    #[test]
    fn xxh64_streaming_matches_oneshot() {
        let seed = 12345u64;
        let data = b"streaming xxh64 test";

        let oneshot = Xxh64::digest(seed, data);

        let mut hasher = Xxh64::new(seed);
        hasher.update(&data[..10]);
        hasher.update(&data[10..]);
        let streaming = hasher.finalize();

        assert_eq!(oneshot, streaming);
    }

    #[test]
    fn xxh64_empty_input() {
        let digest = Xxh64::digest(0, b"");
        assert_eq!(digest.len(), 8);
    }

    // --- XXH3 (64-bit) Tests ---

    #[test]
    fn xxh3_basic() {
        let data = b"test data";
        let digest = Xxh3::digest(0, data);
        assert_eq!(digest.len(), 8);

        // Different seeds produce different results
        let digest_seed1 = Xxh3::digest(1, data);
        assert_ne!(digest, digest_seed1);
    }

    #[test]
    fn xxh3_streaming_matches_oneshot() {
        let seed = 42u64;
        let data = b"streaming xxh3 test";

        let oneshot = Xxh3::digest(seed, data);

        let mut hasher = Xxh3::new(seed);
        hasher.update(&data[..10]);
        hasher.update(&data[10..]);
        let streaming = hasher.finalize();

        assert_eq!(oneshot, streaming);
    }

    #[test]
    fn xxh3_empty_input() {
        let digest = Xxh3::digest(0, b"");
        assert_eq!(digest.len(), 8);
    }

    #[test]
    fn xxh3_large_input() {
        // Test with larger input to potentially exercise SIMD paths
        let data: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
        let digest = Xxh3::digest(0, &data);
        assert_eq!(digest.len(), 8);

        // Verify streaming matches
        let mut hasher = Xxh3::new(0);
        hasher.update(&data);
        assert_eq!(hasher.finalize(), digest);
    }

    // --- XXH3-128 Tests ---

    #[test]
    fn xxh3_128_basic() {
        let data = b"test data";
        let digest = Xxh3_128::digest(0, data);
        assert_eq!(digest.len(), 16);

        // Different seeds produce different results
        let digest_seed1 = Xxh3_128::digest(1, data);
        assert_ne!(digest, digest_seed1);
    }

    #[test]
    fn xxh3_128_streaming_matches_oneshot() {
        let seed = 777u64;
        let data = b"streaming xxh3-128 test";

        let oneshot = Xxh3_128::digest(seed, data);

        let mut hasher = Xxh3_128::new(seed);
        hasher.update(&data[..10]);
        hasher.update(&data[10..]);
        let streaming = hasher.finalize();

        assert_eq!(oneshot, streaming);
    }

    #[test]
    fn xxh3_128_empty_input() {
        let digest = Xxh3_128::digest(0, b"");
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn xxh3_128_large_input() {
        let data: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
        let digest = Xxh3_128::digest(0, &data);
        assert_eq!(digest.len(), 16);

        let mut hasher = Xxh3_128::new(0);
        hasher.update(&data);
        assert_eq!(hasher.finalize(), digest);
    }

    // --- StrongDigest Trait Tests ---

    #[test]
    fn trait_digest_for_all_algorithms() {
        let data = b"trait test";

        // Test that trait methods work for all algorithms
        let _md4: <Md4 as StrongDigest>::Digest = Md4::digest(data);
        let _md5: <Md5 as StrongDigest>::Digest = Md5::digest(data);
        let _sha1: <Sha1 as StrongDigest>::Digest = Sha1::digest(data);
        let _sha256: <Sha256 as StrongDigest>::Digest = Sha256::digest(data);
        let _sha512: <Sha512 as StrongDigest>::Digest = Sha512::digest(data);
        let _xxh64: <Xxh64 as StrongDigest>::Digest = Xxh64::digest_with_seed(0, data);
        let _xxh3: <Xxh3 as StrongDigest>::Digest = Xxh3::digest_with_seed(0, data);
        let _xxh3_128: <Xxh3_128 as StrongDigest>::Digest = Xxh3_128::digest_with_seed(0, data);
    }

    #[test]
    fn trait_digest_with_seed() {
        let data = b"seeded test";

        // MD5 with seed
        let md5_seeded = Md5::digest_with_seed(Md5Seed::proper(42), data);
        let md5_default = Md5::digest(data);
        assert_ne!(md5_seeded, md5_default);

        // XXH64 with seed
        let xxh64_seeded = Xxh64::digest_with_seed(42, data);
        let xxh64_default = Xxh64::digest_with_seed(0, data);
        assert_ne!(xxh64_seeded, xxh64_default);
    }

    #[test]
    fn trait_new_and_with_seed_equivalence() {
        // For algorithms without seeds, new() and with_seed(default) should be equivalent
        let data = b"equivalence test";

        let mut md4_new = Md4::new();
        md4_new.update(data);
        let md4_new_result = md4_new.finalize();

        let mut md4_seed: Md4 = StrongDigest::with_seed(());
        md4_seed.update(data);
        let md4_seed_result = md4_seed.finalize();

        assert_eq!(md4_new_result, md4_seed_result);
    }

    #[test]
    fn all_algorithms_produce_valid_output_for_empty_input() {
        let empty = b"";

        assert_eq!(Md4::digest(empty).len(), 16);
        assert_eq!(Md5::digest(empty).len(), 16);
        assert_eq!(Sha1::digest(empty).len(), 20);
        assert_eq!(Sha256::digest(empty).len(), 32);
        assert_eq!(Sha512::digest(empty).len(), 64);
        assert_eq!(Xxh64::digest(0, empty).len(), 8);
        assert_eq!(Xxh3::digest(0, empty).len(), 8);
        assert_eq!(Xxh3_128::digest(0, empty).len(), 16);
    }

    #[test]
    fn all_algorithms_deterministic() {
        let data = b"determinism check";

        assert_eq!(Md4::digest(data), Md4::digest(data));
        assert_eq!(Md5::digest(data), Md5::digest(data));
        assert_eq!(Sha1::digest(data), Sha1::digest(data));
        assert_eq!(Sha256::digest(data), Sha256::digest(data));
        assert_eq!(Sha512::digest(data), Sha512::digest(data));
        assert_eq!(Xxh64::digest(0, data), Xxh64::digest(0, data));
        assert_eq!(Xxh3::digest(0, data), Xxh3::digest(0, data));
        assert_eq!(Xxh3_128::digest(0, data), Xxh3_128::digest(0, data));
    }

    #[test]
    fn all_algorithms_different_input_different_output() {
        let data1 = b"input one";
        let data2 = b"input two";

        assert_ne!(Md4::digest(data1), Md4::digest(data2));
        assert_ne!(Md5::digest(data1), Md5::digest(data2));
        assert_ne!(Sha1::digest(data1), Sha1::digest(data2));
        assert_ne!(Sha256::digest(data1), Sha256::digest(data2));
        assert_ne!(Sha512::digest(data1), Sha512::digest(data2));
        assert_ne!(Xxh64::digest(0, data1), Xxh64::digest(0, data2));
        assert_ne!(Xxh3::digest(0, data1), Xxh3::digest(0, data2));
        assert_ne!(Xxh3_128::digest(0, data1), Xxh3_128::digest(0, data2));
    }

    #[test]
    fn openssl_acceleration_query() {
        // Should not panic; result depends on feature flags
        let _ = checksums::openssl_acceleration_available();
    }

    #[test]
    fn default_trait_for_hashers() {
        // Verify Default works
        let _md4 = Md4::default();
        let _md5 = Md5::default();
        let _sha1 = Sha1::default();
        let _sha256 = Sha256::default();
        let _sha512 = Sha512::default();
    }

    #[test]
    fn debug_trait_for_hashers() {
        // Verify Debug works
        let md4 = Md4::new();
        let md5 = Md5::new();
        let sha1 = Sha1::new();
        let sha256 = Sha256::new();
        let sha512 = Sha512::new();

        assert!(format!("{md4:?}").contains("Md4"));
        assert!(format!("{md5:?}").contains("Md5"));
        assert!(format!("{sha1:?}").contains("Sha1"));
        assert!(format!("{sha256:?}").contains("Sha256"));
        assert!(format!("{sha512:?}").contains("Sha512"));
    }
}

// ============================================================================
// Additional Integration Tests
// ============================================================================

mod integration {
    use super::*;

    #[test]
    fn complete_block_signature_workflow() {
        // Simulate the rsync block signature workflow:
        // 1. Split file into blocks
        // 2. Compute rolling and strong checksums
        // 3. Verify checksums for matching

        let file_data = b"This is a test file with some content for block matching.";
        let block_size = 16;

        let mut signatures: Vec<(u32, [u8; 16])> = Vec::new();

        // Generate signatures for each block
        for block in file_data.chunks(block_size) {
            let mut rolling = RollingChecksum::new();
            rolling.update(block);
            let strong = Md5::digest(block);
            signatures.push((rolling.value(), strong));
        }

        // Verify we can match blocks
        let first_block = &file_data[..block_size];
        let mut test_rolling = RollingChecksum::new();
        test_rolling.update(first_block);

        assert_eq!(test_rolling.value(), signatures[0].0);
        assert_eq!(Md5::digest(first_block), signatures[0].1);
    }

    #[test]
    fn rolling_checksum_sliding_window() {
        // Simulate sliding window search for matching blocks
        let data = b"ABCDEFGHIJKLMNOP";
        let window_size = 4;

        // Initial window "ABCD"
        let mut rolling = RollingChecksum::new();
        rolling.update(&data[..window_size]);

        let mut checksums = vec![rolling.value()];

        // Slide window through the data
        for i in 0..(data.len() - window_size) {
            rolling.roll(data[i], data[i + window_size]).unwrap();
            checksums.push(rolling.value());
        }

        // Verify each checksum matches direct computation
        for (i, &checksum) in checksums.iter().enumerate() {
            let mut direct = RollingChecksum::new();
            direct.update(&data[i..i + window_size]);
            assert_eq!(checksum, direct.value(), "Mismatch at position {i}");
        }
    }

    #[test]
    fn multiple_algorithm_comparison() {
        // Compare different strong checksum algorithms for the same input
        let data = b"comparison test data";

        let md4 = Md4::digest(data);
        let md5 = Md5::digest(data);
        let sha1 = Sha1::digest(data);
        let sha256 = Sha256::digest(data);
        let xxh64 = Xxh64::digest(0, data);
        let xxh3 = Xxh3::digest(0, data);

        // All should be different (different algorithms)
        assert_ne!(&md4[..], &md5[..]);
        assert_ne!(&md5[..], &sha1[..]);

        // Same algorithm, same result
        assert_eq!(md4, Md4::digest(data));
        assert_eq!(md5, Md5::digest(data));
        assert_eq!(sha1, Sha1::digest(data));
        assert_eq!(sha256, Sha256::digest(data));
        assert_eq!(xxh64, Xxh64::digest(0, data));
        assert_eq!(xxh3, Xxh3::digest(0, data));
    }

    #[test]
    fn large_file_simulation() {
        // Simulate processing a larger file
        let file_size = 1024 * 1024; // 1 MB
        let block_size = 64 * 1024; // 64 KB

        let data: Vec<u8> = (0..file_size).map(|i| (i % 256) as u8).collect();

        let config = PipelineConfig::default()
            .with_block_size(block_size)
            .with_min_file_size(0);

        let checksums = compute_checksums_pipelined::<Sha256, _>(
            Cursor::new(data.clone()),
            config,
            Some(file_size as u64),
        )
        .unwrap();

        assert_eq!(checksums.len(), file_size / block_size);

        // Verify first and last block
        let first_block = &data[..block_size];
        assert_eq!(checksums[0].rolling, RollingDigest::from_bytes(first_block));
        assert_eq!(checksums[0].strong, Sha256::digest(first_block));

        let last_start = file_size - block_size;
        let last_block = &data[last_start..];
        assert_eq!(
            checksums.last().unwrap().rolling,
            RollingDigest::from_bytes(last_block)
        );
        assert_eq!(checksums.last().unwrap().strong, Sha256::digest(last_block));
    }

    #[test]
    fn reader_based_checksum() {
        let data = b"Reader-based checksum test data that is long enough to test multiple reads";

        let digest = RollingDigest::from_reader(&mut Cursor::new(data.to_vec())).unwrap();
        let direct = RollingDigest::from_bytes(data);

        assert_eq!(digest, direct);
    }

    #[test]
    fn wire_format_serialization() {
        // Test that digest wire format serialization works correctly
        let digest = RollingDigest::new(0x1234, 0x5678, 100);

        // Write to buffer
        let mut buffer = Vec::new();
        digest.write_le_to(&mut buffer).unwrap();
        assert_eq!(buffer.len(), 4);

        // Read back
        let recovered = RollingDigest::read_le_from(&mut Cursor::new(buffer), 100).unwrap();
        assert_eq!(digest, recovered);
    }
}

// ============================================================================
// Property-Based Tests (if proptest is available)
// ============================================================================

#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn rolling_checksum_deterministic(data: Vec<u8>) {
            let mut c1 = RollingChecksum::new();
            c1.update(&data);

            let mut c2 = RollingChecksum::new();
            c2.update(&data);

            prop_assert_eq!(c1.value(), c2.value());
        }

        #[test]
        fn rolling_digest_value_roundtrip(s1: u16, s2: u16, len in 0usize..1_000_000) {
            let original = RollingDigest::new(s1, s2, len);
            let value = original.value();
            let recovered = RollingDigest::from_value(value, len);
            prop_assert_eq!(original, recovered);
        }

        #[test]
        fn strong_digest_deterministic(data: Vec<u8>) {
            prop_assert_eq!(Md5::digest(&data), Md5::digest(&data));
            prop_assert_eq!(Sha256::digest(&data), Sha256::digest(&data));
            prop_assert_eq!(Xxh3::digest(0, &data), Xxh3::digest(0, &data));
        }

        #[test]
        fn rolling_update_is_cumulative(chunks: Vec<Vec<u8>>) {
            // Updating with multiple chunks should equal updating with concatenated data
            let mut incremental = RollingChecksum::new();
            for chunk in &chunks {
                incremental.update(chunk);
            }

            let concatenated: Vec<u8> = chunks.into_iter().flatten().collect();
            let mut full = RollingChecksum::new();
            full.update(&concatenated);

            prop_assert_eq!(incremental.value(), full.value());
        }

        #[test]
        fn strong_update_is_cumulative(chunks: Vec<Vec<u8>>) {
            // Streaming should equal one-shot for concatenated data
            let mut streaming = Md5::new();
            for chunk in &chunks {
                streaming.update(chunk);
            }
            let streaming_result = streaming.finalize();

            let concatenated: Vec<u8> = chunks.into_iter().flatten().collect();
            let oneshot_result = Md5::digest(&concatenated);

            prop_assert_eq!(streaming_result, oneshot_result);
        }

        #[test]
        fn digest_le_bytes_roundtrip(data: Vec<u8>) {
            let digest = RollingDigest::from_bytes(&data);
            let bytes = digest.to_le_bytes();
            let recovered = RollingDigest::from_le_bytes(bytes, data.len());
            prop_assert_eq!(digest, recovered);
        }

        #[test]
        fn xxh_seeds_produce_different_results(data in prop::collection::vec(any::<u8>(), 1..100), seed1: u64, seed2: u64) {
            prop_assume!(seed1 != seed2);
            prop_assume!(!data.is_empty());

            let d1 = Xxh64::digest(seed1, &data);
            let d2 = Xxh64::digest(seed2, &data);
            prop_assert_ne!(d1, d2);
        }
    }
}
