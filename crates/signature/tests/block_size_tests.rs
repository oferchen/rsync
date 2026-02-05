//! Comprehensive tests for signature block size selection and behavior.
//!
//! These tests validate the block size selection heuristics and verify that
//! block size choices produce correct and rsync-compatible signatures with
//! appropriate accuracy characteristics.
//!
//! ## Test Coverage
//!
//! ### Block Size Selection
//! - Automatic block size derivation based on file size
//! - Small files using default block size (700 bytes)
//! - Medium files with scaled block sizes
//! - Large files approaching maximum block sizes
//! - Protocol-dependent maximum block size enforcement
//!
//! ### Forced Block Sizes
//! - Overriding automatic selection with fixed block sizes
//! - Very small block sizes (high granularity)
//! - Very large block sizes (low granularity)
//! - Block sizes larger than file size
//!
//! ### Signature Accuracy
//! - Small blocks detect fine-grained changes
//! - Large blocks may miss small changes
//! - Block boundaries affect change detection
//! - Trade-off between signature size and accuracy
//!
//! ### Round-Trip Verification
//! - Signature generation with various block sizes
//! - Layout reconstruction from signature
//! - File size calculation from layout
//! - Consistency across different configurations
//!
//! ## Upstream Reference
//!
//! Block sizing follows upstream rsync's behavior from:
//! - `generator.c:sum_sizes_sqroot()` - Block size heuristics
//! - Protocol documentation for block size limits

use checksums::RollingDigest;
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};
use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

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

// ============================================================================
// Block Size Selection Based on File Size
// ============================================================================

mod block_size_selection {
    //! Tests for automatic block size selection heuristics.

    use super::*;

    /// Very small files use the default block size of 700 bytes.
    ///
    /// For files smaller than the default block size, rsync uses 700 bytes
    /// as the block size, resulting in a single block.
    #[test]
    fn very_small_file_uses_default_block_size() {
        let file_sizes = [1u64, 10, 100, 500, 699];

        for size in file_sizes {
            let params = layout_params(size, 16);
            let layout = calculate_signature_layout(params).expect("layout");

            assert_eq!(
                layout.block_length().get(),
                700,
                "file size {size} should use default block size 700"
            );
            assert_eq!(layout.block_count(), 1, "should fit in one block");
            assert_eq!(
                layout.remainder(),
                size as u32,
                "entire file is remainder"
            );
        }
    }

    /// Files exactly at default block size have no remainder.
    #[test]
    fn file_exactly_default_block_size() {
        let params = layout_params(700, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(layout.block_length().get(), 700);
        assert_eq!(layout.block_count(), 1);
        assert_eq!(layout.remainder(), 0, "exact size means no remainder");
    }

    /// Small files up to 700*700 bytes use default block size.
    ///
    /// Reference: rsync's sum_sizes_sqroot() uses block size 700 for files
    /// up to 490,000 bytes (700 * 700).
    #[test]
    fn small_files_use_default_block_size() {
        let test_cases = [
            (1_000u64, 700u32),
            (10_000, 700),
            (100_000, 700),
            (490_000, 700), // At threshold
        ];

        for (file_size, expected_block_size) in test_cases {
            let params = layout_params(file_size, 16);
            let layout = calculate_signature_layout(params).expect("layout");

            assert_eq!(
                layout.block_length().get(),
                expected_block_size,
                "file size {file_size} should use block size {expected_block_size}"
            );
        }
    }

    /// Medium files scale block size with sqrt heuristic.
    ///
    /// Once file size exceeds 700*700 bytes, block size increases
    /// following a square-root-based heuristic to maintain reasonable
    /// signature sizes. We verify the block size grows appropriately
    /// without asserting exact values (which depend on implementation details).
    #[test]
    fn medium_files_scale_block_size() {
        let test_cases = [
            (500_000u64, 700u32, 800u32),       // Just over threshold
            (1_048_576, 900, 1200),             // 1 MB
            (10_485_760, 3000, 3500),           // 10 MB
            (104_857_600, 10_000, 11_000),      // 100 MB
        ];

        for (file_size, min_block, max_block) in test_cases {
            let params = layout_params(file_size, 16);
            let layout = calculate_signature_layout(params).expect("layout");
            let block_size = layout.block_length().get();

            assert!(
                block_size >= min_block && block_size <= max_block,
                "file size {file_size}: block size {block_size} should be in range [{min_block}, {max_block}]"
            );
        }
    }

    /// Large files continue scaling but respect maximum limits.
    #[test]
    fn large_files_scale_within_limits() {
        let test_cases = [
            (1_073_741_824u64, 30_000u32, 35_000u32), // 1 GB
            (10_737_418_240, 100_000, 110_000),       // 10 GB
        ];

        for (file_size, min_block, max_block) in test_cases {
            let params = layout_params(file_size, 16);
            let layout = calculate_signature_layout(params).expect("layout");
            let block_size = layout.block_length().get();

            assert!(
                block_size >= min_block && block_size <= max_block,
                "file size {file_size}: block size {block_size} should be in range [{min_block}, {max_block}]"
            );

            // Verify it doesn't exceed protocol maximum (2^17 = 131072 for protocol >= 30)
            assert!(
                block_size <= 131_072,
                "block size should not exceed protocol maximum"
            );
        }
    }

    /// Block size increases monotonically with file size.
    ///
    /// This property ensures predictability: larger files never get
    /// smaller block sizes than smaller files.
    #[test]
    fn block_size_increases_monotonically() {
        let file_sizes = [
            1_000u64,
            10_000,
            100_000,
            1_000_000,
            10_000_000,
            100_000_000,
        ];

        let mut prev_block_size = 0u32;

        for size in file_sizes {
            let params = layout_params(size, 16);
            let layout = calculate_signature_layout(params).expect("layout");
            let block_size = layout.block_length().get();

            assert!(
                block_size >= prev_block_size,
                "block size should not decrease: file size {size} has block size {block_size}, \
                 previous was {prev_block_size}"
            );

            prev_block_size = block_size;
        }
    }

    /// Block count remains reasonable for all file sizes.
    ///
    /// The sqrt-based heuristic ensures that block count grows
    /// sub-linearly with file size, keeping signatures manageable.
    #[test]
    fn block_count_grows_sublinearly() {
        let test_cases = [
            (1_000_000u64, 1_000u64),      // 1 MB -> 1K blocks
            (10_000_000, 3_165),           // 10 MB -> ~3K blocks
            (100_000_000, 10_000),         // 100 MB -> ~10K blocks
            (1_000_000_000, 31_630),       // 1 GB -> ~32K blocks
        ];

        for (file_size, expected_blocks) in test_cases {
            let params = layout_params(file_size, 16);
            let layout = calculate_signature_layout(params).expect("layout");

            assert_eq!(
                layout.block_count(),
                expected_blocks,
                "file size {file_size} should have ~{expected_blocks} blocks"
            );

            // Verify block count is much less than file size
            assert!(
                layout.block_count() < file_size / 100,
                "block count should be much less than file size in bytes"
            );
        }
    }
}

// ============================================================================
// Minimum and Maximum Block Sizes
// ============================================================================

mod block_size_limits {
    //! Tests for block size boundaries and constraints.

    use super::*;

    /// Minimum effective block size is 1 byte.
    #[test]
    fn minimum_block_size_one_byte() {
        let data = vec![0x42u8];
        let params = layout_params_with_block(1, 1, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(layout.block_length().get(), 1);
        assert_eq!(layout.block_count(), 1);
        assert_eq!(layout.remainder(), 0);

        let signature = generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4)
            .expect("signature");

        assert_eq!(signature.blocks().len(), 1);
        assert_eq!(signature.blocks()[0].len(), 1);
    }

    /// Very small forced block sizes work correctly.
    #[test]
    fn very_small_forced_block_sizes() {
        let block_sizes = [1u32, 2, 4, 8, 16, 32];

        for block_size in block_sizes {
            let data = generate_test_data(1000);
            let params = layout_params_with_block(data.len() as u64, block_size, 16);
            let layout = calculate_signature_layout(params).expect("layout");

            assert_eq!(layout.block_length().get(), block_size);

            let expected_blocks = (data.len() as u64).div_ceil(block_size as u64);
            assert_eq!(layout.block_count(), expected_blocks);

            let signature =
                generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                    .expect("signature");

            assert_eq!(signature.blocks().len(), expected_blocks as usize);
        }
    }

    /// Maximum block size for protocol 30+ is 2^17 (131072).
    #[test]
    fn maximum_block_size_modern_protocol() {
        let huge_file = 1u64 << 40; // 1 TB

        let params = layout_params_with_protocol(huge_file, 30, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(
            layout.block_length().get(),
            131_072,
            "protocol 30+ caps block size at 2^17"
        );

        let params = layout_params_with_protocol(huge_file, 31, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(layout.block_length().get(), 131_072);

        let params = layout_params_with_protocol(huge_file, 32, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(layout.block_length().get(), 131_072);
    }

    /// Maximum block size for protocol < 30 is 2^29 (536870912).
    #[test]
    fn maximum_block_size_legacy_protocol() {
        let huge_file = 1u64 << 40; // 1 TB

        let params = layout_params_with_protocol(huge_file, 28, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        // Should be larger than modern protocol limit
        assert!(
            layout.block_length().get() > 131_072,
            "legacy protocol allows larger blocks"
        );
        assert!(
            layout.block_length().get() <= (1 << 29),
            "but still capped at 2^29"
        );

        let params = layout_params_with_protocol(huge_file, 29, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert!(layout.block_length().get() > 131_072);
    }

    /// Forced block size can exceed automatic selection.
    #[test]
    fn forced_block_size_overrides_heuristic() {
        let file_size = 10_000u64;

        // Automatic selection would use 700
        let auto_params = layout_params(file_size, 16);
        let auto_layout = calculate_signature_layout(auto_params).expect("layout");
        assert_eq!(auto_layout.block_length().get(), 700);

        // Force a larger block size
        let forced_params = layout_params_with_block(file_size, 2048, 16);
        let forced_layout = calculate_signature_layout(forced_params).expect("layout");
        assert_eq!(forced_layout.block_length().get(), 2048);

        // Force a smaller block size
        let forced_params = layout_params_with_block(file_size, 128, 16);
        let forced_layout = calculate_signature_layout(forced_params).expect("layout");
        assert_eq!(forced_layout.block_length().get(), 128);
    }

    /// Forced block size still respects protocol maximum.
    #[test]
    fn forced_block_size_capped_by_protocol() {
        let file_size = 1_000_000u64;
        let excessive_block_size = 1 << 20; // 1 MB

        // Protocol 30+ should clamp to 131072
        let params = SignatureLayoutParams::new(
            file_size,
            NonZeroU32::new(excessive_block_size),
            ProtocolVersion::try_from(30).unwrap(),
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(
            layout.block_length().get(),
            131_072,
            "forced block size should be clamped to protocol maximum"
        );
    }

    /// Block size larger than file results in single block.
    #[test]
    fn block_size_larger_than_file() {
        let file_size = 1000u64;
        let block_size = 10_000u32;

        let params = layout_params_with_block(file_size, block_size, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        assert_eq!(layout.block_length().get(), block_size);
        assert_eq!(layout.block_count(), 1, "file fits in single block");
        assert_eq!(
            layout.remainder(),
            file_size as u32,
            "entire file is remainder"
        );
    }
}

// ============================================================================
// Block Size Affects Signature Accuracy
// ============================================================================

mod signature_accuracy {
    //! Tests demonstrating how block size affects change detection accuracy.

    use super::*;

    /// Smaller blocks detect smaller changes.
    ///
    /// With small block sizes, even minor changes affect only a few blocks,
    /// allowing more precise delta computation.
    #[test]
    fn small_blocks_detect_small_changes() {
        let original = generate_test_data(10_000);
        let mut modified = original.clone();

        // Modify 100 bytes in the middle
        for i in 5_000..5_100 {
            modified[i] = modified[i].wrapping_add(1);
        }

        // Use small 100-byte blocks
        let params = layout_params_with_block(original.len() as u64, 100, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let sig_original = generate_file_signature(
            Cursor::new(original.clone()),
            layout,
            SignatureAlgorithm::Md4,
        )
        .expect("signature");

        let sig_modified = generate_file_signature(
            Cursor::new(modified.clone()),
            layout,
            SignatureAlgorithm::Md4,
        )
        .expect("signature");

        // Count differing blocks
        let mut diff_count = 0;
        for (orig, modif) in sig_original.blocks().iter().zip(sig_modified.blocks().iter()) {
            if orig.strong() != modif.strong() {
                diff_count += 1;
            }
        }

        // With 100-byte blocks, we expect only ~1 block to differ
        assert_eq!(
            diff_count, 1,
            "small blocks should isolate changes to affected blocks"
        );
    }

    /// Larger blocks may miss or conflate small changes.
    ///
    /// With large block sizes, small changes can affect entire blocks,
    /// potentially making delta computation less efficient.
    #[test]
    fn large_blocks_less_granular() {
        let original = generate_test_data(10_000);
        let mut modified = original.clone();

        // Modify 100 bytes in the middle
        for i in 5_000..5_100 {
            modified[i] = modified[i].wrapping_add(1);
        }

        // Use large 5000-byte blocks
        let params = layout_params_with_block(original.len() as u64, 5_000, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let sig_original = generate_file_signature(
            Cursor::new(original.clone()),
            layout,
            SignatureAlgorithm::Md4,
        )
        .expect("signature");

        let sig_modified = generate_file_signature(
            Cursor::new(modified.clone()),
            layout,
            SignatureAlgorithm::Md4,
        )
        .expect("signature");

        // Count differing blocks
        let mut diff_count = 0;
        for (orig, modif) in sig_original.blocks().iter().zip(sig_modified.blocks().iter()) {
            if orig.strong() != modif.strong() {
                diff_count += 1;
            }
        }

        // With 5000-byte blocks, we expect 1 large block to differ
        // (the second block containing bytes 5000-9999)
        assert_eq!(
            diff_count, 1,
            "large blocks contain more data per block"
        );
    }

    /// Block boundaries matter for change detection.
    ///
    /// Changes that span block boundaries affect multiple blocks.
    #[test]
    fn changes_spanning_block_boundaries() {
        let original = generate_test_data(10_000);
        let mut modified = original.clone();

        // Modify 200 bytes spanning block boundary at 5000
        for i in 4_900..5_100 {
            modified[i] = modified[i].wrapping_add(1);
        }

        // Use 1000-byte blocks (boundary at 5000)
        let params = layout_params_with_block(original.len() as u64, 1_000, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let sig_original = generate_file_signature(
            Cursor::new(original.clone()),
            layout,
            SignatureAlgorithm::Md4,
        )
        .expect("signature");

        let sig_modified = generate_file_signature(
            Cursor::new(modified.clone()),
            layout,
            SignatureAlgorithm::Md4,
        )
        .expect("signature");

        // Count differing blocks
        let mut diff_count = 0;
        for (orig, modif) in sig_original.blocks().iter().zip(sig_modified.blocks().iter()) {
            if orig.strong() != modif.strong() {
                diff_count += 1;
            }
        }

        // Changes span blocks 4 (4000-4999) and 5 (5000-5999)
        assert_eq!(
            diff_count, 2,
            "changes spanning boundaries affect multiple blocks"
        );
    }

    /// Rolling checksums detect block-aligned vs misaligned content.
    ///
    /// The rolling checksum is sensitive to data position, so identical
    /// content at different positions produces different signatures.
    #[test]
    fn rolling_checksum_position_sensitivity() {
        let data1 = b"AAAA BBBB CCCC DDDD";
        let data2 = b"BBBB CCCC DDDD EEEE";

        let params = layout_params_with_block(data1.len() as u64, 5, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let sig1 = generate_file_signature(Cursor::new(data1), layout, SignatureAlgorithm::Md4)
            .expect("signature");

        let sig2 = generate_file_signature(Cursor::new(data2), layout, SignatureAlgorithm::Md4)
            .expect("signature");

        // Even though data2 contains much of data1's content shifted,
        // block boundaries cause different signatures
        assert_ne!(
            sig1.blocks()[0].rolling(),
            sig2.blocks()[0].rolling(),
            "different content at same position"
        );
    }

    /// Signature size scales with block count.
    ///
    /// Smaller blocks mean more blocks, resulting in larger signatures.
    /// This demonstrates the size/accuracy trade-off.
    #[test]
    fn signature_size_scales_with_block_count() {
        let data = generate_test_data(10_000);

        let test_cases = [(100u32, 100usize), (500, 20), (1_000, 10), (5_000, 2)];

        for (block_size, expected_block_count) in test_cases {
            let params = layout_params_with_block(data.len() as u64, block_size, 16);
            let layout = calculate_signature_layout(params).expect("layout");

            let signature =
                generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                    .expect("signature");

            assert_eq!(
                signature.blocks().len(),
                expected_block_count,
                "block size {block_size} should produce {expected_block_count} blocks"
            );

            // Each block has overhead (index, rolling, strong checksum)
            // Smaller blocks = more blocks = larger signature
        }
    }

    /// Identical files produce identical signatures regardless of block size.
    #[test]
    fn identical_files_identical_signatures() {
        let data = generate_test_data(5_000);

        let block_sizes = [100u32, 500, 1_000, 2_500];

        for block_size in block_sizes {
            let params = layout_params_with_block(data.len() as u64, block_size, 16);
            let layout = calculate_signature_layout(params).expect("layout");

            let sig1 =
                generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                    .expect("signature");

            let sig2 =
                generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                    .expect("signature");

            assert_eq!(
                sig1, sig2,
                "identical data should produce identical signatures with block size {block_size}"
            );
        }
    }
}

// ============================================================================
// Round-Trip with Different Block Sizes
// ============================================================================

mod round_trip {
    //! Tests verifying signature generation and reconstruction with various block sizes.

    use super::*;

    /// Layout calculation round-trips correctly.
    #[test]
    fn layout_round_trip_preserves_parameters() {
        let test_cases = [
            (1_000u64, None),
            (10_000, None),
            (100_000, None),
            (1_000, Some(128u32)),
            (10_000, Some(512)),
            (100_000, Some(4_096)),
        ];

        for (file_size, forced_block) in test_cases {
            let params = SignatureLayoutParams::new(
                file_size,
                forced_block.and_then(NonZeroU32::new),
                ProtocolVersion::NEWEST,
                NonZeroU8::new(16).unwrap(),
            );

            let layout = calculate_signature_layout(params).expect("layout");

            // Verify layout produces correct file size
            assert_eq!(
                layout.file_size(),
                file_size,
                "layout.file_size() should match input"
            );

            // Verify block length
            if let Some(forced) = forced_block {
                assert_eq!(
                    layout.block_length().get(),
                    forced,
                    "forced block size should be preserved"
                );
            }

            // Verify block count and remainder are consistent
            let expected_count = file_size.div_ceil(layout.block_length().get() as u64);
            assert_eq!(layout.block_count(), expected_count);

            let expected_remainder = (file_size % layout.block_length().get() as u64) as u32;
            if expected_remainder == 0 && file_size > 0 {
                // When file size is exact multiple, remainder is 0
                assert_eq!(layout.remainder(), 0);
            } else if file_size > 0 {
                assert_eq!(layout.remainder(), expected_remainder);
            }
        }
    }

    /// File size calculation from layout is accurate.
    #[test]
    fn file_size_calculation_from_layout() {
        let test_cases = [
            (0u64, 700u32),      // Empty file
            (1, 700),            // Single byte
            (700, 700),          // Exact block
            (701, 700),          // One byte over
            (1400, 700),         // Two exact blocks
            (1500, 700),         // Two blocks + remainder
            (10_000, 1_000),     // Ten exact blocks
            (10_500, 1_000),     // Ten blocks + remainder
        ];

        for (file_size, block_size) in test_cases {
            let params = layout_params_with_block(file_size, block_size, 16);
            let layout = calculate_signature_layout(params).expect("layout");

            let calculated_size = layout.file_size();

            assert_eq!(
                calculated_size, file_size,
                "layout.file_size() should match original file size \
                 (file={file_size}, block={block_size})"
            );
        }
    }

    /// Signature generation with various block sizes preserves total bytes.
    #[test]
    fn signature_preserves_total_bytes() {
        let data = generate_test_data(10_000);
        let block_sizes = [100u32, 250, 500, 700, 1_000, 2_000, 5_000];

        for block_size in block_sizes {
            let params = layout_params_with_block(data.len() as u64, block_size, 16);
            let layout = calculate_signature_layout(params).expect("layout");

            let signature =
                generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                    .expect("signature");

            assert_eq!(
                signature.total_bytes(),
                data.len() as u64,
                "signature should preserve total bytes with block size {block_size}"
            );
        }
    }

    /// Block checksums are deterministic for given block size.
    #[test]
    fn block_checksums_deterministic() {
        let data = generate_test_data(5_000);
        let block_size = 500u32;

        let params = layout_params_with_block(data.len() as u64, block_size, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        // Generate signature multiple times
        let sig1 =
            generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        let sig2 =
            generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                .expect("signature");

        let sig3 = generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4)
            .expect("signature");

        // All should be identical
        assert_eq!(sig1.blocks().len(), sig2.blocks().len());
        assert_eq!(sig2.blocks().len(), sig3.blocks().len());

        for i in 0..sig1.blocks().len() {
            assert_eq!(
                sig1.blocks()[i].rolling(),
                sig2.blocks()[i].rolling(),
                "rolling checksums should be deterministic"
            );
            assert_eq!(
                sig1.blocks()[i].strong(),
                sig2.blocks()[i].strong(),
                "strong checksums should be deterministic"
            );
            assert_eq!(
                sig2.blocks()[i].rolling(),
                sig3.blocks()[i].rolling()
            );
            assert_eq!(sig2.blocks()[i].strong(), sig3.blocks()[i].strong());
        }
    }

    /// Layout reconstruction from signature components.
    #[test]
    fn layout_reconstruction_from_signature() {
        let data = generate_test_data(7_500);
        let block_size = 1_000u32;

        let params = layout_params_with_block(data.len() as u64, block_size, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let signature = generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4)
            .expect("signature");

        // Verify we can reconstruct the layout from the signature
        let reconstructed_layout = signature.layout();

        assert_eq!(reconstructed_layout.block_length(), layout.block_length());
        assert_eq!(reconstructed_layout.block_count(), layout.block_count());
        assert_eq!(reconstructed_layout.remainder(), layout.remainder());
        assert_eq!(
            reconstructed_layout.strong_sum_length(),
            layout.strong_sum_length()
        );
        assert_eq!(reconstructed_layout.file_size(), layout.file_size());
    }

    /// Block indices are sequential regardless of block size.
    #[test]
    fn block_indices_sequential() {
        let data = generate_test_data(10_000);
        let block_sizes = [100u32, 500, 1_000, 2_500];

        for block_size in block_sizes {
            let params = layout_params_with_block(data.len() as u64, block_size, 16);
            let layout = calculate_signature_layout(params).expect("layout");

            let signature =
                generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                    .expect("signature");

            for (expected_idx, block) in signature.blocks().iter().enumerate() {
                assert_eq!(
                    block.index(),
                    expected_idx as u64,
                    "block indices should be sequential with block size {block_size}"
                );
            }
        }
    }

    /// Last block length matches remainder when present.
    #[test]
    fn last_block_matches_remainder() {
        let test_cases = [
            (1_500u64, 700u32),   // Remainder = 100
            (2_500, 1_000),       // Remainder = 500
            (10_100, 1_000),      // Remainder = 100
            (5_001, 1_000),       // Remainder = 1
        ];

        for (file_size, block_size) in test_cases {
            let params = layout_params_with_block(file_size, block_size, 16);
            let layout = calculate_signature_layout(params).expect("layout");

            let data = generate_test_data(file_size as usize);
            let signature =
                generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4)
                    .expect("signature");

            if layout.remainder() != 0 {
                let last_block = signature.blocks().last().expect("at least one block");
                assert_eq!(
                    last_block.len(),
                    layout.remainder() as usize,
                    "last block length should match remainder \
                     (file={file_size}, block={block_size}, remainder={})",
                    layout.remainder()
                );
            }
        }
    }

    /// All non-last blocks have full block size.
    #[test]
    fn non_last_blocks_full_size() {
        let file_size = 5_500u64;
        let block_size = 1_000u32;

        let params = layout_params_with_block(file_size, block_size, 16);
        let layout = calculate_signature_layout(params).expect("layout");

        let data = generate_test_data(file_size as usize);
        let signature = generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4)
            .expect("signature");

        let block_count = signature.blocks().len();

        for (i, block) in signature.blocks().iter().enumerate() {
            if i + 1 < block_count {
                assert_eq!(
                    block.len(),
                    block_size as usize,
                    "non-last block {i} should have full block size"
                );
            } else {
                // Last block should have remainder size
                assert_eq!(block.len(), layout.remainder() as usize);
            }
        }
    }

    /// Empty file round-trips correctly with any block size.
    #[test]
    fn empty_file_round_trip() {
        let block_sizes = [1u32, 100, 700, 1_000, 10_000];

        for block_size in block_sizes {
            let params = layout_params_with_block(0, block_size, 16);
            let layout = calculate_signature_layout(params).expect("layout");

            assert_eq!(layout.block_count(), 0);
            assert_eq!(layout.remainder(), 0);
            assert_eq!(layout.file_size(), 0);

            let signature =
                generate_file_signature(Cursor::new(Vec::new()), layout, SignatureAlgorithm::Md4)
                    .expect("signature");

            assert!(signature.blocks().is_empty());
            assert_eq!(signature.total_bytes(), 0);
            assert_eq!(signature.layout().file_size(), 0);
        }
    }

    /// Single byte file round-trips with various block sizes.
    #[test]
    fn single_byte_round_trip() {
        let data = vec![0x42u8];
        let block_sizes = [1u32, 10, 100, 700, 1_000];

        for block_size in block_sizes {
            let params = layout_params_with_block(1, block_size, 16);
            let layout = calculate_signature_layout(params).expect("layout");

            assert_eq!(layout.block_count(), 1);
            // Remainder is file_size % block_size: for block_size=1 it's 0, otherwise 1
            let expected_remainder = if block_size == 1 { 0 } else { 1 };
            assert_eq!(layout.remainder(), expected_remainder);
            assert_eq!(layout.file_size(), 1);

            let signature =
                generate_file_signature(Cursor::new(data.clone()), layout, SignatureAlgorithm::Md4)
                    .expect("signature");

            assert_eq!(signature.blocks().len(), 1);
            assert_eq!(signature.blocks()[0].len(), 1);
            assert_eq!(signature.total_bytes(), 1);
            assert_eq!(
                signature.blocks()[0].rolling(),
                RollingDigest::from_bytes(&data)
            );
        }
    }
}
