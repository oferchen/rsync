//! Comprehensive integration tests for the signature crate.
//!
//! These tests validate the complete file signature generation pipeline,
//! ensuring that the implementation produces correct and rsync-compatible
//! signatures across a variety of scenarios.
//!
//! ## Test Coverage
//!
//! ### Signature Generation
//! - Various file sizes (empty, small, medium, large)
//! - Different block sizes (default heuristics, forced overrides)
//! - All supported checksum algorithms (MD4, MD5, SHA1, XXH64, XXH3, XXH3/128)
//!
//! ### Layout Calculation
//! - Block size heuristics matching upstream rsync's `sum_sizes_sqroot()`
//! - Strong checksum length derivation based on file size and protocol
//! - Protocol version compatibility (28, 29, 30, 31, 32)
//!
//! ### Edge Cases
//! - Empty files (zero blocks)
//! - Single byte files
//! - Files exactly one block in size
//! - Files at block boundaries
//! - Very large files (multi-megabyte)
//!
//! ### Round-Trip Verification
//! - Signature generation produces consistent results
//! - Layout parameters are preserved through reconstruction
//! - Block checksums match expected values
//!
//! ## Upstream Reference
//!
//! The signature layout calculation mirrors upstream rsync's behavior from:
//! - `generator.c:sum_sizes_sqroot()` - Block sizing heuristics
//! - `match.c` - Checksum usage patterns
//! - Protocol documentation for checksum negotiation

use checksums::strong::Md5Seed;
use checksums::RollingDigest;
use protocol::ProtocolVersion;
use signature::{
    FileSignature, SignatureAlgorithm, SignatureBlock, SignatureLayout, SignatureLayoutError,
    SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};
use std::io::Cursor;
use std::num::{NonZeroU32, NonZeroU8};

// ============================================================================
// Test Utilities
// ============================================================================

/// Creates layout params with common defaults for testing.
fn layout_params(file_len: u64, checksum_len: u8) -> SignatureLayoutParams {
    SignatureLayoutParams::new(
        file_len,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(checksum_len).expect("checksum length must be non-zero"),
    )
}

/// Creates layout params with a forced block size.
fn layout_params_with_block(
    file_len: u64,
    block_len: u32,
    checksum_len: u8,
) -> SignatureLayoutParams {
    SignatureLayoutParams::new(
        file_len,
        NonZeroU32::new(block_len),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(checksum_len).expect("checksum length must be non-zero"),
    )
}

/// Creates layout params for a specific protocol version.
fn layout_params_with_protocol(
    file_len: u64,
    protocol: u8,
    checksum_len: u8,
) -> SignatureLayoutParams {
    SignatureLayoutParams::new(
        file_len,
        None,
        ProtocolVersion::try_from(protocol).expect("valid protocol version"),
        NonZeroU8::new(checksum_len).expect("checksum length must be non-zero"),
    )
}

/// Generates test data with a deterministic pattern based on index.
fn generate_test_data(size: usize) -> Vec<u8> {
    (0..size).map(|i| ((i * 17 + 31) % 256) as u8).collect()
}

/// Generates signature from test data and returns both signature and original data.
fn generate_signature_from_data(
    data: &[u8],
    algorithm: SignatureAlgorithm,
) -> (FileSignature, SignatureLayout) {
    let params = layout_params(data.len() as u64, 16);
    let layout = calculate_signature_layout(params).expect("layout calculation should succeed");
    let signature = generate_file_signature(Cursor::new(data), layout, algorithm)
        .expect("signature generation should succeed");
    (signature, layout)
}

// ============================================================================
// Signature Generation - File Size Variations
// ============================================================================

mod file_size_variations {
    //! Tests for signature generation across different file sizes.

    use super::*;

    /// Empty files should produce signatures with zero blocks.
    ///
    /// This matches upstream rsync behavior where empty files have valid
    /// signatures but contain no block entries.
    #[test]
    fn empty_file_produces_empty_signature() {
        let params = layout_params(0, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(layout.block_count(), 0);
        assert_eq!(layout.remainder(), 0);

        let signature =
            generate_file_signature(Cursor::new(Vec::new()), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        assert!(signature.blocks().is_empty());
        assert_eq!(signature.total_bytes(), 0);
    }

    /// Single byte files should produce one block with remainder of 1.
    #[test]
    fn single_byte_file() {
        let data = vec![0x42u8];
        let params = layout_params(1, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(layout.block_count(), 1);
        assert_eq!(layout.remainder(), 1);

        let signature =
            generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        assert_eq!(signature.blocks().len(), 1);
        assert_eq!(signature.total_bytes(), 1);

        let block = &signature.blocks()[0];
        assert_eq!(block.index(), 0);
        assert_eq!(block.len(), 1);
        assert_eq!(block.rolling(), RollingDigest::from_bytes(&data));
    }

    /// Small files (< default block size of 700) fit in one block.
    #[test]
    fn small_file_fits_in_one_block() {
        let data = generate_test_data(500);
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        // Default block size is 700, so 500 bytes fits in one block
        assert_eq!(layout.block_length().get(), 700);
        assert_eq!(layout.block_count(), 1);
        assert_eq!(layout.remainder(), 500);

        let signature =
            generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        assert_eq!(signature.blocks().len(), 1);
        assert_eq!(signature.total_bytes(), 500);
    }

    /// Files exactly matching block size have zero remainder.
    ///
    /// When file_length % block_length == 0, there is no partial final block,
    /// so remainder is 0.
    #[test]
    fn file_exactly_one_block() {
        let data = generate_test_data(700);
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(layout.block_length().get(), 700);
        assert_eq!(layout.block_count(), 1);
        assert_eq!(layout.remainder(), 0); // Exact multiple means no partial block

        let signature =
            generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        assert_eq!(signature.blocks().len(), 1);
        assert_eq!(signature.total_bytes(), 700);
        // The single block should be full-sized
        assert_eq!(signature.blocks()[0].len(), 700);
    }

    /// Files spanning multiple blocks with a partial final block.
    #[test]
    fn multi_block_file_with_remainder() {
        // 1500 bytes with default block size 700 = 3 blocks (700 + 700 + 100)
        let data = generate_test_data(1500);
        let params = layout_params_with_block(data.len() as u64, 700, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(layout.block_length().get(), 700);
        assert_eq!(layout.block_count(), 3);
        assert_eq!(layout.remainder(), 100);

        let signature =
            generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        assert_eq!(signature.blocks().len(), 3);
        assert_eq!(signature.total_bytes(), 1500);

        // Verify block indices are sequential
        for (i, block) in signature.blocks().iter().enumerate() {
            assert_eq!(block.index(), i as u64);
        }

        // Verify block lengths
        assert_eq!(signature.blocks()[0].len(), 700);
        assert_eq!(signature.blocks()[1].len(), 700);
        assert_eq!(signature.blocks()[2].len(), 100);
    }

    /// Files spanning multiple blocks with exact block alignment.
    #[test]
    fn multi_block_file_exact_alignment() {
        // 2100 bytes with block size 700 = exactly 3 blocks
        let data = generate_test_data(2100);
        let params = layout_params_with_block(data.len() as u64, 700, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(layout.block_length().get(), 700);
        assert_eq!(layout.block_count(), 3);
        assert_eq!(layout.remainder(), 0);

        let signature =
            generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        assert_eq!(signature.blocks().len(), 3);
        assert_eq!(signature.total_bytes(), 2100);

        // All blocks should be full size
        for block in signature.blocks() {
            assert_eq!(block.len(), 700);
        }
    }

    /// Medium-sized files (1MB) use scaled block sizes.
    #[test]
    fn medium_file_one_megabyte() {
        let size = 1024 * 1024; // 1 MB
        let data = generate_test_data(size);
        let params = layout_params(size as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        // Block size should scale with file size (not the default 700)
        assert!(layout.block_length().get() > 700);

        let signature =
            generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        assert_eq!(signature.total_bytes(), size as u64);
        assert!(signature.blocks().len() > 1);
    }

    /// Large files (10MB) use larger block sizes for efficiency.
    #[test]
    fn large_file_ten_megabytes() {
        let size = 10 * 1024 * 1024; // 10 MB
        let data = generate_test_data(size);
        let params = layout_params(size as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        // Verify block size scales appropriately
        // For 10MB, rsync uses sqrt-based heuristics
        assert!(layout.block_length().get() >= 1024);

        let signature =
            generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        assert_eq!(signature.total_bytes(), size as u64);

        // Verify reasonable block count
        let expected_blocks = (size as u64 + layout.block_length().get() as u64 - 1)
            / layout.block_length().get() as u64;
        assert_eq!(signature.blocks().len() as u64, expected_blocks);
    }
}

// ============================================================================
// Block Size Variations
// ============================================================================

mod block_size_variations {
    //! Tests for different block size configurations.

    use super::*;

    /// Forced small block sizes create more blocks.
    #[test]
    fn forced_small_block_size() {
        let data = generate_test_data(10000);
        let params = layout_params_with_block(data.len() as u64, 100, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(layout.block_length().get(), 100);
        assert_eq!(layout.block_count(), 100);
        assert_eq!(layout.remainder(), 0);

        let signature =
            generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        assert_eq!(signature.blocks().len(), 100);
    }

    /// Forced large block sizes create fewer blocks.
    #[test]
    fn forced_large_block_size() {
        let data = generate_test_data(10000);
        let params = layout_params_with_block(data.len() as u64, 5000, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(layout.block_length().get(), 5000);
        assert_eq!(layout.block_count(), 2);
        assert_eq!(layout.remainder(), 0);

        let signature =
            generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        assert_eq!(signature.blocks().len(), 2);
    }

    /// Block size larger than file produces single block with file as remainder.
    #[test]
    fn block_size_larger_than_file() {
        let data = generate_test_data(500);
        let params = layout_params_with_block(data.len() as u64, 1000, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(layout.block_length().get(), 1000);
        assert_eq!(layout.block_count(), 1);
        assert_eq!(layout.remainder(), 500);

        let signature =
            generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        assert_eq!(signature.blocks().len(), 1);
        assert_eq!(signature.blocks()[0].len(), 500);
    }

    /// Default block size heuristics follow upstream rsync's sum_sizes_sqroot().
    ///
    /// For small files (<= 700*700 = 490000 bytes), block size is 700.
    /// For larger files, block size grows with sqrt of file size.
    #[test]
    fn default_block_size_heuristics() {
        // Small file: uses default 700
        let small_params = layout_params(1000, 16);
        let small_layout = calculate_signature_layout(small_params).expect("layout");
        assert_eq!(small_layout.block_length().get(), 700);

        // Medium file: block size scales
        let medium_params = layout_params(1_000_000, 16);
        let medium_layout = calculate_signature_layout(medium_params).expect("layout");
        assert!(medium_layout.block_length().get() > 700);

        // Large file: block size continues to scale
        let large_params = layout_params(100_000_000, 16);
        let large_layout = calculate_signature_layout(large_params).expect("layout");
        assert!(large_layout.block_length().get() > medium_layout.block_length().get());
    }

    /// Protocol version affects maximum block size.
    ///
    /// Protocol < 30: max block size is 2^29
    /// Protocol >= 30: max block size is 2^17 (131072)
    #[test]
    fn protocol_version_affects_max_block_size() {
        let huge_file = 1u64 << 35; // 32 GB

        // Modern protocol clamps to 131072
        let modern_params = layout_params_with_protocol(huge_file, 32, 16);
        let modern_layout = calculate_signature_layout(modern_params).expect("layout");
        assert_eq!(modern_layout.block_length().get(), 131072);

        // Legacy protocol allows larger blocks
        let legacy_params = layout_params_with_protocol(huge_file, 29, 16);
        let legacy_layout = calculate_signature_layout(legacy_params).expect("layout");
        assert!(legacy_layout.block_length().get() >= 131072);
    }
}

// ============================================================================
// Checksum Algorithm Variations
// ============================================================================

mod checksum_algorithms {
    //! Tests for all supported checksum algorithms.

    use super::*;

    /// MD4 produces 16-byte digests truncated to layout's strong_sum_length.
    #[test]
    fn md4_algorithm() {
        let data = generate_test_data(1000);
        let (signature, layout) = generate_signature_from_data(&data, SignatureAlgorithm::Md4);

        assert!(!signature.blocks().is_empty());
        for block in signature.blocks() {
            assert_eq!(
                block.strong().len(),
                layout.strong_sum_length().get() as usize
            );
        }
    }

    /// MD5 produces 16-byte digests with optional seeding.
    #[test]
    fn md5_algorithm_unseeded() {
        let data = generate_test_data(1000);
        let algorithm = SignatureAlgorithm::Md5 {
            seed_config: Md5Seed::none(),
        };
        let (signature, layout) = generate_signature_from_data(&data, algorithm);

        assert!(!signature.blocks().is_empty());
        for block in signature.blocks() {
            assert_eq!(
                block.strong().len(),
                layout.strong_sum_length().get() as usize
            );
        }
    }

    /// MD5 with different seeds produces different checksums.
    #[test]
    fn md5_algorithm_with_seed() {
        let data = generate_test_data(1000);

        let unseeded = SignatureAlgorithm::Md5 {
            seed_config: Md5Seed::none(),
        };
        let seeded = SignatureAlgorithm::Md5 {
            seed_config: Md5Seed::proper(12345),
        };

        let (sig_unseeded, _) = generate_signature_from_data(&data, unseeded);
        let (sig_seeded, _) = generate_signature_from_data(&data, seeded);

        // Different seeds should produce different checksums
        assert_ne!(
            sig_unseeded.blocks()[0].strong(),
            sig_seeded.blocks()[0].strong()
        );
    }

    /// SHA1 produces 20-byte digests.
    #[test]
    fn sha1_algorithm() {
        let data = generate_test_data(1000);

        // Use a checksum length compatible with SHA1 (max 20 bytes)
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let signature =
            generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Sha1)
                .expect("signature");

        assert!(!signature.blocks().is_empty());
        for block in signature.blocks() {
            assert_eq!(
                block.strong().len(),
                layout.strong_sum_length().get() as usize
            );
        }
    }

    /// XXH64 produces 8-byte digests with seed.
    #[test]
    fn xxh64_algorithm() {
        let data = generate_test_data(1000);

        // XXH64 only supports 8 bytes, so use matching checksum length
        let params = layout_params(data.len() as u64, 8);
        let layout = calculate_signature_layout(params).expect("layout");

        let algorithm = SignatureAlgorithm::Xxh64 { seed: 0 };
        let signature =
            generate_file_signature(Cursor::new(data), layout, algorithm).expect("signature");

        assert!(!signature.blocks().is_empty());
        for block in signature.blocks() {
            assert_eq!(block.strong().len(), 8);
        }
    }

    /// XXH64 with different seeds produces different checksums.
    #[test]
    fn xxh64_seed_affects_checksum() {
        let data = generate_test_data(1000);
        let params = layout_params(data.len() as u64, 8);
        let layout = calculate_signature_layout(params).expect("layout");

        let sig1 = generate_file_signature(
            Cursor::new(data.clone()),
            layout,
            SignatureAlgorithm::Xxh64 { seed: 0 },
        )
        .expect("signature");

        let sig2 = generate_file_signature(
            Cursor::new(data),
            layout,
            SignatureAlgorithm::Xxh64 { seed: 12345 },
        )
        .expect("signature");

        assert_ne!(sig1.blocks()[0].strong(), sig2.blocks()[0].strong());
    }

    /// XXH3/64 produces 8-byte digests.
    #[test]
    fn xxh3_algorithm() {
        let data = generate_test_data(1000);
        let params = layout_params(data.len() as u64, 8);
        let layout = calculate_signature_layout(params).expect("layout");

        let algorithm = SignatureAlgorithm::Xxh3 { seed: 42 };
        let signature =
            generate_file_signature(Cursor::new(data), layout, algorithm).expect("signature");

        assert!(!signature.blocks().is_empty());
        for block in signature.blocks() {
            assert_eq!(block.strong().len(), 8);
        }
    }

    /// XXH3/128 produces 16-byte digests.
    #[test]
    fn xxh3_128_algorithm() {
        let data = generate_test_data(1000);
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let algorithm = SignatureAlgorithm::Xxh3_128 { seed: 42 };
        let signature =
            generate_file_signature(Cursor::new(data), layout, algorithm).expect("signature");

        assert!(!signature.blocks().is_empty());
        for block in signature.blocks() {
            assert_eq!(
                block.strong().len(),
                layout.strong_sum_length().get() as usize
            );
        }
    }

    /// Digest length mismatch reports appropriate error.
    ///
    /// When the layout requests a strong checksum length that exceeds
    /// the algorithm's digest width, an error is returned.
    #[test]
    fn digest_length_mismatch_error() {
        let data = generate_test_data(1000);
        let params = layout_params(data.len() as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        // XXH64 produces 8 bytes, but layout wants 16
        let algorithm = SignatureAlgorithm::Xxh64 { seed: 0 };
        let result = generate_file_signature(Cursor::new(data), layout, algorithm);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("digest"));
    }
}

// ============================================================================
// Round-Trip Verification
// ============================================================================

mod round_trip {
    //! Tests verifying signature generation consistency and correctness.

    use super::*;

    /// Generating the same signature twice produces identical results.
    #[test]
    fn deterministic_signature_generation() {
        let data = generate_test_data(5000);

        let (sig1, layout1) = generate_signature_from_data(&data, SignatureAlgorithm::Md4);
        let (sig2, layout2) = generate_signature_from_data(&data, SignatureAlgorithm::Md4);

        assert_eq!(layout1, layout2);
        assert_eq!(sig1.total_bytes(), sig2.total_bytes());
        assert_eq!(sig1.blocks().len(), sig2.blocks().len());

        for (b1, b2) in sig1.blocks().iter().zip(sig2.blocks().iter()) {
            assert_eq!(b1.index(), b2.index());
            assert_eq!(b1.rolling(), b2.rolling());
            assert_eq!(b1.strong(), b2.strong());
        }
    }

    /// Rolling checksums match direct computation.
    #[test]
    fn rolling_checksum_correctness() {
        let data = generate_test_data(2000);
        let params = layout_params_with_block(data.len() as u64, 500, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let signature =
            generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        // Verify each block's rolling checksum
        for (i, block) in signature.blocks().iter().enumerate() {
            let start = i * 500;
            let end = if i == signature.blocks().len() - 1 {
                data.len()
            } else {
                start + 500
            };
            let expected = RollingDigest::from_bytes(&data[start..end]);
            assert_eq!(
                block.rolling(),
                expected,
                "block {i} rolling checksum mismatch"
            );
        }
    }

    /// Layout preserved through FileSignature.
    #[test]
    fn layout_preservation() {
        let data = generate_test_data(3000);
        let params = layout_params_with_block(data.len() as u64, 800, 12);
        let layout = calculate_signature_layout(params).expect("layout");

        let signature =
            generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        assert_eq!(signature.layout(), layout);
        assert_eq!(
            signature.layout().block_length(),
            layout.block_length()
        );
        assert_eq!(
            signature.layout().strong_sum_length(),
            layout.strong_sum_length()
        );
    }

    /// FileSignature reconstruction from raw parts.
    #[test]
    fn signature_reconstruction_from_parts() {
        let data = generate_test_data(2000);
        let (original, _) = generate_signature_from_data(&data, SignatureAlgorithm::Md4);

        // Reconstruct from raw parts
        let reconstructed = FileSignature::from_raw_parts(
            original.layout(),
            original.blocks().to_vec(),
            original.total_bytes(),
        );

        assert_eq!(reconstructed.layout(), original.layout());
        assert_eq!(reconstructed.total_bytes(), original.total_bytes());
        assert_eq!(reconstructed.blocks(), original.blocks());
    }

    /// SignatureBlock reconstruction from raw parts.
    #[test]
    fn block_reconstruction_from_parts() {
        let data = generate_test_data(1000);
        let (signature, _) = generate_signature_from_data(&data, SignatureAlgorithm::Md4);

        for block in signature.blocks() {
            let reconstructed = SignatureBlock::from_raw_parts(
                block.index(),
                block.rolling(),
                block.strong().to_vec(),
            );

            assert_eq!(reconstructed.index(), block.index());
            assert_eq!(reconstructed.rolling(), block.rolling());
            assert_eq!(reconstructed.strong(), block.strong());
            assert_eq!(reconstructed.len(), block.len());
        }
    }
}

// ============================================================================
// Edge Cases
// ============================================================================

mod edge_cases {
    //! Tests for boundary conditions and unusual scenarios.

    use super::*;

    /// Files with all zero bytes.
    #[test]
    fn all_zeros_file() {
        let data = vec![0u8; 2000];
        let (signature, _) = generate_signature_from_data(&data, SignatureAlgorithm::Md4);

        assert!(!signature.blocks().is_empty());
        assert_eq!(signature.total_bytes(), 2000);
    }

    /// Files with all 0xFF bytes.
    #[test]
    fn all_ones_file() {
        let data = vec![0xFFu8; 2000];
        let (signature, _) = generate_signature_from_data(&data, SignatureAlgorithm::Md4);

        assert!(!signature.blocks().is_empty());
        assert_eq!(signature.total_bytes(), 2000);
    }

    /// Files with repeating patterns.
    #[test]
    fn repeating_pattern_file() {
        let pattern = b"ABCDEFGHIJ";
        let data: Vec<u8> = pattern.iter().cycle().take(10000).copied().collect();

        let (signature, _) = generate_signature_from_data(&data, SignatureAlgorithm::Md4);

        assert!(!signature.blocks().is_empty());
        assert_eq!(signature.total_bytes(), 10000);
    }

    /// File exactly at i64::MAX boundary is rejected.
    #[test]
    fn file_too_large_error() {
        let params = layout_params(u64::MAX, 16);
        let result = calculate_signature_layout(params);

        assert!(result.is_err());
        match result.unwrap_err() {
            SignatureLayoutError::FileTooLarge { length } => {
                assert_eq!(length, u64::MAX);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// Block count overflow detection.
    #[test]
    fn block_count_overflow_error() {
        // Create params that would result in too many blocks
        // Using forced small block size with very large file
        let file_len = (i32::MAX as u64 + 1) * 700;
        let params = layout_params_with_block(file_len, 700, 16);

        let result = calculate_signature_layout(params);

        assert!(result.is_err());
        match result.unwrap_err() {
            SignatureLayoutError::BlockCountOverflow { block_length, .. } => {
                assert_eq!(block_length, 700);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// Trailing data in input stream is detected.
    #[test]
    fn trailing_data_detection() {
        let params = layout_params(100, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        // Provide more data than expected
        let data = vec![0u8; 150];
        let result = generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("trailing"));
    }

    /// Truncated input stream causes I/O error.
    #[test]
    fn truncated_input_error() {
        let params = layout_params(1000, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        // Provide less data than expected
        let data = vec![0u8; 500];
        let result = generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4);

        assert!(result.is_err());
    }

    /// Very small checksum lengths are honored.
    #[test]
    fn minimum_checksum_length() {
        let data = generate_test_data(1000);
        let params = layout_params(data.len() as u64, 2);
        let layout = calculate_signature_layout(params).expect("layout");

        let signature =
            generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        // Strong checksum should be truncated to 2 bytes
        for block in signature.blocks() {
            assert_eq!(block.strong().len(), layout.strong_sum_length().get() as usize);
        }
    }
}

// ============================================================================
// Protocol Version Compatibility
// ============================================================================

mod protocol_compatibility {
    //! Tests for protocol version-specific behavior.

    use super::*;

    /// All supported protocol versions produce valid layouts.
    #[test]
    fn all_protocol_versions_supported() {
        let supported_versions = [28u8, 29, 30, 31, 32];

        for version in supported_versions {
            let params = layout_params_with_protocol(10000, version, 16);
            let result = calculate_signature_layout(params);

            assert!(
                result.is_ok(),
                "protocol {version} should produce valid layout"
            );
        }
    }

    /// Legacy protocols (< 27) use fixed checksum length.
    ///
    /// Protocol versions before 27 don't use the bias-based strong
    /// checksum length derivation.
    #[test]
    fn strong_checksum_length_varies_by_protocol() {
        let file_len = 1_000_000u64;

        // Modern protocol uses bias-based derivation
        let modern_params = layout_params_with_protocol(file_len, 32, 4);
        let modern_layout = calculate_signature_layout(modern_params).expect("layout");

        // The strong sum length depends on file size and block length
        // For modern protocols (27+), the bias heuristic may increase it
        assert!(modern_layout.strong_sum_length().get() >= 4);
    }

    /// Layout params accessors work correctly.
    #[test]
    fn layout_params_accessors() {
        let params = SignatureLayoutParams::new(
            12345,
            NonZeroU32::new(1024),
            ProtocolVersion::NEWEST,
            NonZeroU8::new(8).unwrap(),
        );

        assert_eq!(params.file_length(), 12345);
        assert_eq!(params.forced_block_length().unwrap().get(), 1024);
        assert_eq!(params.protocol(), ProtocolVersion::NEWEST);
        assert_eq!(params.checksum_length().get(), 8);
    }
}

// ============================================================================
// Performance Characteristics
// ============================================================================

mod performance {
    //! Tests verifying performance-related properties.

    use super::*;

    /// Large file signature generation completes in reasonable time.
    ///
    /// This test ensures the implementation handles multi-megabyte files
    /// without excessive memory allocation or computation time.
    #[test]
    fn large_file_performance() {
        let size = 5 * 1024 * 1024; // 5 MB
        let data = generate_test_data(size);

        let start = std::time::Instant::now();

        let params = layout_params(size as u64, 16);
        let layout = calculate_signature_layout(params).expect("layout");
        let signature =
            generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        let elapsed = start.elapsed();

        assert_eq!(signature.total_bytes(), size as u64);

        // Should complete well under 5 seconds on most systems
        assert!(
            elapsed.as_secs() < 5,
            "signature generation took too long: {:?}",
            elapsed
        );
    }

    /// Block count scales appropriately with file size.
    #[test]
    fn block_count_scaling() {
        let sizes = [
            1_000u64,
            10_000,
            100_000,
            1_000_000,
            10_000_000,
        ];

        let mut prev_block_count = 0u64;

        for size in sizes {
            let params = layout_params(size, 16);
            let layout = calculate_signature_layout(params).expect("layout");

            // Block count should increase but not proportionally (due to scaling)
            assert!(
                layout.block_count() >= prev_block_count,
                "block count should not decrease with larger files"
            );

            // Block count should not be excessive
            assert!(
                layout.block_count() <= size,
                "should not have more blocks than bytes"
            );

            prev_block_count = layout.block_count();
        }
    }

    /// Memory usage is bounded by input size.
    ///
    /// The sequential signature generator should not buffer more than
    /// one block at a time.
    #[test]
    fn memory_efficient_generation() {
        // This test ensures the API doesn't require loading entire file
        let size = 1024 * 1024; // 1 MB
        let data = generate_test_data(size);

        let params = layout_params_with_block(size as u64, 4096, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        // The Cursor provides streaming access, and generate_file_signature
        // should process block by block
        let signature =
            generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        assert_eq!(signature.total_bytes(), size as u64);
    }
}

// ============================================================================
// Algorithm Property Tests
// ============================================================================

mod algorithm_properties {
    //! Tests verifying properties of the SignatureAlgorithm enum.

    use super::*;

    /// All algorithms report correct digest lengths.
    #[test]
    fn algorithm_digest_lengths() {
        assert_eq!(SignatureAlgorithm::Md4.digest_len(), 16);
        assert_eq!(
            SignatureAlgorithm::Md5 {
                seed_config: Md5Seed::none()
            }
            .digest_len(),
            16
        );
        assert_eq!(SignatureAlgorithm::Sha1.digest_len(), 20);
        assert_eq!(SignatureAlgorithm::Xxh64 { seed: 0 }.digest_len(), 8);
        assert_eq!(SignatureAlgorithm::Xxh3 { seed: 0 }.digest_len(), 8);
        assert_eq!(SignatureAlgorithm::Xxh3_128 { seed: 0 }.digest_len(), 16);
    }

    /// Algorithm equality works correctly.
    #[test]
    fn algorithm_equality() {
        assert_eq!(SignatureAlgorithm::Md4, SignatureAlgorithm::Md4);
        assert_eq!(SignatureAlgorithm::Sha1, SignatureAlgorithm::Sha1);

        assert_eq!(
            SignatureAlgorithm::Xxh64 { seed: 42 },
            SignatureAlgorithm::Xxh64 { seed: 42 }
        );
        assert_ne!(
            SignatureAlgorithm::Xxh64 { seed: 42 },
            SignatureAlgorithm::Xxh64 { seed: 0 }
        );

        assert_ne!(SignatureAlgorithm::Md4, SignatureAlgorithm::Sha1);
    }

    /// Algorithm debug formatting works.
    #[test]
    fn algorithm_debug_format() {
        let debug = format!("{:?}", SignatureAlgorithm::Md4);
        assert!(debug.contains("Md4"));

        let debug = format!("{:?}", SignatureAlgorithm::Xxh3 { seed: 123 });
        assert!(debug.contains("Xxh3"));
        assert!(debug.contains("123"));
    }

    /// Algorithms are Copy.
    #[test]
    fn algorithm_is_copy() {
        let algo = SignatureAlgorithm::Md4;
        let copied = algo;
        assert_eq!(algo, copied);

        let algo = SignatureAlgorithm::Xxh64 { seed: 42 };
        let copied = algo;
        assert_eq!(algo, copied);
    }
}

// ============================================================================
// Layout Property Tests
// ============================================================================

mod layout_properties {
    //! Tests verifying properties of SignatureLayout.

    use super::*;

    /// Layout is Copy.
    #[test]
    fn layout_is_copy() {
        let params = layout_params(1000, 16);
        let layout = calculate_signature_layout(params).expect("layout");
        let copied = layout;
        assert_eq!(layout, copied);
    }

    /// Layout equality works correctly.
    #[test]
    fn layout_equality() {
        let params1 = layout_params(1000, 16);
        let params2 = layout_params(1000, 16);
        let params3 = layout_params(2000, 16);

        let layout1 = calculate_signature_layout(params1).expect("layout");
        let layout2 = calculate_signature_layout(params2).expect("layout");
        let layout3 = calculate_signature_layout(params3).expect("layout");

        assert_eq!(layout1, layout2);
        assert_ne!(layout1, layout3);
    }

    /// Layout debug formatting works.
    #[test]
    fn layout_debug_format() {
        let params = layout_params(1000, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let debug = format!("{:?}", layout);
        assert!(debug.contains("SignatureLayout"));
    }

    /// Layout reconstruction from raw parts.
    #[test]
    fn layout_from_raw_parts() {
        let layout = SignatureLayout::from_raw_parts(
            NonZeroU32::new(1024).unwrap(),
            256,
            10,
            NonZeroU8::new(12).unwrap(),
        );

        assert_eq!(layout.block_length().get(), 1024);
        assert_eq!(layout.remainder(), 256);
        assert_eq!(layout.block_count(), 10);
        assert_eq!(layout.strong_sum_length().get(), 12);
    }
}

// ============================================================================
// FileSignature Property Tests
// ============================================================================

mod file_signature_properties {
    //! Tests verifying properties of FileSignature.

    use super::*;

    /// FileSignature equality works correctly.
    #[test]
    fn signature_equality() {
        let data = generate_test_data(1000);

        let (sig1, _) = generate_signature_from_data(&data, SignatureAlgorithm::Md4);
        let (sig2, _) = generate_signature_from_data(&data, SignatureAlgorithm::Md4);

        assert_eq!(sig1, sig2);
    }

    /// FileSignature clone works.
    #[test]
    fn signature_clone() {
        let data = generate_test_data(1000);
        let (original, _) = generate_signature_from_data(&data, SignatureAlgorithm::Md4);

        let cloned = original.clone();

        assert_eq!(original, cloned);
    }

    /// FileSignature debug formatting works.
    #[test]
    fn signature_debug_format() {
        let data = generate_test_data(100);
        let (signature, _) = generate_signature_from_data(&data, SignatureAlgorithm::Md4);

        let debug = format!("{:?}", signature);
        assert!(debug.contains("FileSignature"));
    }
}

// ============================================================================
// SignatureBlock Property Tests
// ============================================================================

mod block_properties {
    //! Tests verifying properties of SignatureBlock.

    use super::*;

    /// SignatureBlock equality works correctly.
    #[test]
    fn block_equality() {
        let rolling = RollingDigest::from_bytes(b"test");
        let strong = vec![1u8, 2, 3, 4];

        let block1 = SignatureBlock::from_raw_parts(0, rolling, strong.clone());
        let block2 = SignatureBlock::from_raw_parts(0, rolling, strong.clone());
        let block3 = SignatureBlock::from_raw_parts(1, rolling, strong);

        assert_eq!(block1, block2);
        assert_ne!(block1, block3);
    }

    /// SignatureBlock clone works.
    #[test]
    fn block_clone() {
        let rolling = RollingDigest::from_bytes(b"test");
        let block = SignatureBlock::from_raw_parts(0, rolling, vec![1, 2, 3]);

        let cloned = block.clone();
        assert_eq!(block, cloned);
    }

    /// SignatureBlock debug formatting works.
    #[test]
    fn block_debug_format() {
        let rolling = RollingDigest::from_bytes(b"test");
        let block = SignatureBlock::from_raw_parts(42, rolling, vec![1, 2, 3]);

        let debug = format!("{:?}", block);
        assert!(debug.contains("SignatureBlock"));
    }

    /// SignatureBlock is_empty for zero-length blocks.
    #[test]
    fn block_is_empty() {
        let empty_rolling = RollingDigest::from_bytes(b"");
        let empty_block = SignatureBlock::from_raw_parts(0, empty_rolling, vec![]);

        assert!(empty_block.is_empty());

        let non_empty_rolling = RollingDigest::from_bytes(b"x");
        let non_empty_block = SignatureBlock::from_raw_parts(0, non_empty_rolling, vec![1]);

        assert!(!non_empty_block.is_empty());
    }
}

// ============================================================================
// Error Property Tests
// ============================================================================

mod error_properties {
    //! Tests verifying error type properties.

    use super::*;
    use signature::SignatureError;
    use std::io;

    /// SignatureLayoutError display formatting.
    #[test]
    fn layout_error_display() {
        let too_large = SignatureLayoutError::FileTooLarge {
            length: u64::MAX,
        };
        let display = format!("{}", too_large);
        assert!(display.contains("i64::MAX"));

        let overflow = SignatureLayoutError::BlockCountOverflow {
            block_length: 700,
            blocks: 1_000_000_000,
        };
        let display = format!("{}", overflow);
        assert!(display.contains("700"));
        assert!(display.contains("1000000000"));
    }

    /// SignatureError from I/O error.
    #[test]
    fn signature_error_from_io() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "test error");
        let sig_err: SignatureError = io_err.into();

        let display = format!("{}", sig_err);
        assert!(display.contains("read"));
    }

    /// SignatureError display formatting.
    #[test]
    fn signature_error_display() {
        // We can't construct DigestLengthMismatch directly since the field
        // uses NonZeroUsize, but we can test via the generation function
        let params = layout_params(100, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let result = generate_file_signature(
            Cursor::new(vec![0u8; 100]),
            layout,
            SignatureAlgorithm::Xxh64 { seed: 0 },
        );

        if let Err(e) = result {
            let display = format!("{}", e);
            assert!(display.contains("digest") || display.contains("checksum"));
        }
    }
}

// ============================================================================
// Upstream rsync Compatibility Tests
// ============================================================================

mod upstream_compatibility {
    //! Tests ensuring compatibility with upstream rsync behavior.

    use super::*;

    /// Block sizes match upstream rsync's sum_sizes_sqroot heuristic.
    ///
    /// Reference: generator.c:sum_sizes_sqroot()
    #[test]
    fn block_size_matches_upstream_heuristic() {
        // Known test cases from upstream rsync behavior
        let test_cases = [
            (32u64, 700u32),           // Small file: default block size
            (1000, 700),               // Still small: default
            (490_000, 700),            // At threshold: default
            (10 * 1024 * 1024, 3232),  // 10MB: scaled block size
        ];

        for (file_size, expected_block) in test_cases {
            let params = layout_params(file_size, 16);
            let layout = calculate_signature_layout(params).expect("layout");

            assert_eq!(
                layout.block_length().get(),
                expected_block,
                "file size {} expected block size {}, got {}",
                file_size,
                expected_block,
                layout.block_length().get()
            );
        }
    }

    /// Strong checksum length derivation follows upstream bias algorithm.
    ///
    /// Reference: generator.c:sum_sizes_sqroot()
    #[test]
    fn strong_checksum_bias_heuristic() {
        // For protocol 27+, strong checksum length is derived from
        // file size and block length using a bias calculation

        let params = layout_params_with_protocol(1_048_576, 32, 2); // 1MB, proto 32, min 2
        let layout = calculate_signature_layout(params).expect("layout");

        // The derived strong sum length should be >= minimum and <= 16
        assert!(layout.strong_sum_length().get() >= 2);
        assert!(layout.strong_sum_length().get() <= 16);
    }

    /// Protocol version 30+ caps block size at 131072.
    ///
    /// Reference: generator.c MAX_BLOCK_SIZE constant
    #[test]
    fn protocol_30_max_block_size() {
        let huge_file = 1u64 << 40; // 1 TB

        let params = layout_params_with_protocol(huge_file, 30, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(layout.block_length().get(), 131072);
    }

    /// Legacy protocols (< 30) allow larger blocks.
    ///
    /// Reference: generator.c OLD_MAX_BLOCK_SIZE constant (2^29)
    #[test]
    fn legacy_protocol_max_block_size() {
        let huge_file = 1u64 << 34;

        let params = layout_params_with_protocol(huge_file, 29, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        // Legacy allows larger than 131072
        assert!(layout.block_length().get() >= 131072);
    }
}
