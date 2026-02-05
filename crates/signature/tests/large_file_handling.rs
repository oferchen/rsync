//! Tests for large file handling (>4GB) in signature generation.
//!
//! These tests verify that signature layout calculation and generation
//! correctly handles files larger than 4GB (2^32 bytes).
//!
//! The tests use simulated/mock approaches to avoid actually creating
//! multi-gigabyte files during testing.

use std::num::{NonZeroU32, NonZeroU8};

use protocol::ProtocolVersion;
use signature::{SignatureLayoutError, SignatureLayoutParams, calculate_signature_layout};

// =============================================================================
// Constants for Large File Testing
// =============================================================================

/// 4GB boundary - the critical threshold for 32-bit overflow
const FOUR_GB: u64 = 4 * 1024 * 1024 * 1024;

/// Slightly over 4GB - tests proper u64 handling
const OVER_FOUR_GB: u64 = FOUR_GB + 1024 * 1024;

/// Maximum i64 value - the hard limit for rsync file sizes
const I64_MAX: u64 = i64::MAX as u64;

/// 1TB test size - represents realistic large files
const ONE_TB: u64 = 1024 * 1024 * 1024 * 1024;

/// 100TB - tests multi-petabyte edge cases
const HUNDRED_TB: u64 = 100 * ONE_TB;

// =============================================================================
// Signature Layout Tests for Large Files
// =============================================================================

mod signature_layout {
    use super::*;

    /// Tests signature layout calculation for files at the 4GB boundary.
    #[test]
    fn layout_at_4gb_boundary() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

        // Files around 4GB boundary
        let sizes = [FOUR_GB - 1, FOUR_GB, FOUR_GB + 1, OVER_FOUR_GB];

        for size in sizes {
            let params = SignatureLayoutParams::new(size, None, protocol, checksum_length);

            let layout = calculate_signature_layout(params);
            assert!(
                layout.is_ok(),
                "Signature layout calculation should succeed for {} byte file",
                size
            );

            let layout = layout.unwrap();
            assert!(
                layout.block_count() > 0,
                "Block count should be positive for {} byte file",
                size
            );

            // Verify file size can be reconstructed
            assert_eq!(
                layout.file_size(),
                size,
                "File size reconstruction failed for {} byte file",
                size
            );
        }
    }

    /// Tests signature layout with terabyte-scale files.
    #[test]
    fn layout_terabyte_files() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

        // Various large file sizes
        let sizes = [ONE_TB, ONE_TB * 10, HUNDRED_TB];

        for size in sizes {
            let params = SignatureLayoutParams::new(size, None, protocol, checksum_length);

            let layout = calculate_signature_layout(params);
            assert!(
                layout.is_ok(),
                "Signature layout should work for {} byte file",
                size
            );

            let layout = layout.unwrap();
            // Block count should be reasonable (not overflowed)
            assert!(
                layout.block_count() < i32::MAX as u64,
                "Block count should not overflow i32 for {} byte file",
                size
            );
        }
    }

    /// Tests that files exceeding i64::MAX are rejected.
    #[test]
    fn layout_rejects_files_exceeding_i64_max() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

        // File size > i64::MAX
        let too_large = u64::MAX;
        let params = SignatureLayoutParams::new(too_large, None, protocol, checksum_length);

        let result = calculate_signature_layout(params);
        assert!(
            result.is_err(),
            "Should reject file size exceeding i64::MAX"
        );

        match result.unwrap_err() {
            SignatureLayoutError::FileTooLarge { length } => {
                assert_eq!(length, too_large);
            }
            err => panic!("Expected FileTooLarge error, got: {:?}", err),
        }
    }

    /// Tests signature layout with forced block sizes for large files.
    #[test]
    fn layout_forced_block_size_large_files() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();
        let forced_block = NonZeroU32::new(65536).unwrap(); // 64KB blocks

        // 1TB file with forced 64KB blocks
        let size = ONE_TB;
        let params =
            SignatureLayoutParams::new(size, Some(forced_block), protocol, checksum_length);

        let layout = calculate_signature_layout(params).expect("Layout should succeed");

        // With 64KB blocks, 1TB = approximately 16 million blocks
        let expected_blocks = size / 65536 + if size % 65536 > 0 { 1 } else { 0 };
        assert_eq!(layout.block_count(), expected_blocks);
        assert_eq!(layout.block_length().get(), 65536);
    }

    /// Tests that block count doesn't overflow for large files.
    #[test]
    fn block_count_no_overflow_large_files() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

        // Near-maximum file size
        let near_max = I64_MAX - 1000;
        let params = SignatureLayoutParams::new(near_max, None, protocol, checksum_length);

        // This should either succeed with valid block count or fail gracefully
        let result = calculate_signature_layout(params);
        if let Ok(layout) = result {
            // If it succeeds, block count should be valid
            assert!(layout.block_count() > 0);
            assert!(layout.block_count() <= i32::MAX as u64);
        }
        // Otherwise, it's acceptable to reject very large files
    }
}

// =============================================================================
// Block Size Heuristics for Large Files
// =============================================================================

mod block_size_heuristics {
    use super::*;

    /// Tests that block sizes scale appropriately for large files.
    #[test]
    fn block_size_scaling_large_files() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

        // Block sizes should increase with file size (sqrt heuristic)
        // but be capped at MAX_BLOCK_SIZE (128KB for protocol 30+)
        let max_block_size = 1 << 17; // 128KB

        let test_cases = vec![
            (1024 * 1024, false),       // 1MB - small block
            (1024 * 1024 * 1024, true), // 1GB - larger block
            (FOUR_GB, true),            // 4GB - near max block
            (ONE_TB, true),             // 1TB - should hit max
        ];

        for (size, should_be_large) in test_cases {
            let params = SignatureLayoutParams::new(size, None, protocol, checksum_length);
            let layout = calculate_signature_layout(params).expect("Layout should succeed");

            if should_be_large {
                // Block size should be at or near maximum
                assert!(
                    layout.block_length().get() >= 32 * 1024,
                    "Block size {} should be >= 32KB for {} byte file",
                    layout.block_length().get(),
                    size
                );
            }

            // Block size should never exceed maximum
            assert!(
                layout.block_length().get() <= max_block_size,
                "Block size {} exceeds max {} for {} byte file",
                layout.block_length().get(),
                max_block_size,
                size
            );
        }
    }

    /// Tests block count calculations for files near the max size.
    #[test]
    fn block_count_near_max_size() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

        // With max block size 128KB, calculate max file size before block count overflows i32
        let max_block_size = 128 * 1024u64;
        let max_blocks = i32::MAX as u64;
        let _max_file_before_overflow = max_blocks * max_block_size;

        // Test a large but valid file size
        let large_valid = 100 * ONE_TB; // 100TB - well within limits
        let params = SignatureLayoutParams::new(large_valid, None, protocol, checksum_length);

        let layout = calculate_signature_layout(params);
        assert!(layout.is_ok(), "Should handle {} byte file", large_valid);

        if let Ok(layout) = layout {
            assert!(
                layout.block_count() <= max_blocks,
                "Block count {} should not exceed i32::MAX",
                layout.block_count()
            );
        }
    }

    /// Tests block size with different protocol versions for large files.
    #[test]
    fn block_size_protocol_versions() {
        let checksum_length = NonZeroU8::new(16).unwrap();

        // Protocol 30+ has max block size of 128KB
        // Protocol < 30 has max block size of 512MB (1 << 29)
        let protocols_and_max = [
            (ProtocolVersion::try_from(30u8).unwrap(), 1u32 << 17),
            (ProtocolVersion::try_from(31u8).unwrap(), 1u32 << 17),
            (ProtocolVersion::try_from(32u8).unwrap(), 1u32 << 17),
        ];

        for (protocol, expected_max) in protocols_and_max {
            let params = SignatureLayoutParams::new(ONE_TB, None, protocol, checksum_length);
            let layout = calculate_signature_layout(params).expect("Layout should succeed");

            assert!(
                layout.block_length().get() <= expected_max,
                "Protocol {} should have max block size <= {}",
                protocol.as_u8(),
                expected_max
            );
        }
    }
}

// =============================================================================
// Memory Usage Bounds for Large Files
// =============================================================================

mod memory_bounds {
    use super::*;

    /// Tests that signature block memory is bounded for large files.
    #[test]
    fn signature_memory_bounded() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

        // For a 1TB file with 128KB blocks, we'd have ~8 million blocks
        // Each SignatureBlock is typically 4 bytes (rolling) + 16 bytes (strong) = 20 bytes
        // Total: ~160MB for signatures - reasonable

        let file_size = ONE_TB;
        let params = SignatureLayoutParams::new(file_size, None, protocol, checksum_length);
        let layout = calculate_signature_layout(params).expect("Layout should succeed");

        let block_count = layout.block_count();
        let bytes_per_block = 4 + layout.strong_sum_length().get() as u64; // rolling + strong
        let total_signature_bytes = block_count * bytes_per_block;

        // Should be < 1GB for reasonable memory usage
        assert!(
            total_signature_bytes < 1024 * 1024 * 1024,
            "Signature memory {} bytes too high for {} byte file",
            total_signature_bytes,
            file_size
        );
    }

    /// Tests memory estimation for various file sizes.
    #[test]
    fn memory_estimation_various_sizes() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

        let test_sizes = [
            (FOUR_GB, 200 * 1024 * 1024),    // 4GB file -> < 200MB signatures
            (ONE_TB, 512 * 1024 * 1024),     // 1TB file -> < 512MB signatures
            (HUNDRED_TB, 50 * 1024 * 1024 * 1024), // 100TB -> < 50GB signatures
        ];

        for (file_size, max_sig_bytes) in test_sizes {
            let params = SignatureLayoutParams::new(file_size, None, protocol, checksum_length);
            let layout = calculate_signature_layout(params).expect("Layout should succeed");

            let block_count = layout.block_count();
            let bytes_per_block = 4 + layout.strong_sum_length().get() as u64;
            let total_signature_bytes = block_count * bytes_per_block;

            assert!(
                total_signature_bytes < max_sig_bytes,
                "Signature size {} for {} byte file exceeds limit {}",
                total_signature_bytes,
                file_size,
                max_sig_bytes
            );
        }
    }
}

// =============================================================================
// File Size Reconstruction Tests
// =============================================================================

mod file_size_reconstruction {
    use super::*;

    /// Tests that file size can be accurately reconstructed from layout.
    #[test]
    fn file_size_roundtrip() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

        // Test various file sizes including edge cases
        let test_sizes = [
            1u64,              // Minimum
            1024,              // 1KB
            1024 * 1024,       // 1MB
            FOUR_GB - 1,       // Just under 4GB
            FOUR_GB,           // Exactly 4GB
            FOUR_GB + 1,       // Just over 4GB
            ONE_TB,            // 1TB
            ONE_TB + 12345,    // Non-aligned size
            HUNDRED_TB - 999,  // Large non-aligned
        ];

        for size in test_sizes {
            let params = SignatureLayoutParams::new(size, None, protocol, checksum_length);
            let layout = calculate_signature_layout(params).expect("Layout should succeed");

            assert_eq!(
                layout.file_size(),
                size,
                "File size reconstruction failed for {}",
                size
            );
        }
    }

    /// Tests file size reconstruction with remainder blocks.
    #[test]
    fn file_size_with_remainder() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

        // Files that don't divide evenly into blocks
        let test_sizes = [
            FOUR_GB + 1,
            FOUR_GB + 100,
            FOUR_GB + 65535, // Just under one block extra
            ONE_TB + 1,
        ];

        for size in test_sizes {
            let params = SignatureLayoutParams::new(size, None, protocol, checksum_length);
            let layout = calculate_signature_layout(params).expect("Layout should succeed");

            // Should have a non-zero remainder
            assert!(
                layout.remainder() > 0 || size % layout.block_length().get() as u64 == 0,
                "Expected remainder for size {}",
                size
            );

            // File size should still reconstruct correctly
            assert_eq!(layout.file_size(), size);
        }
    }
}

// =============================================================================
// Edge Case Tests
// =============================================================================

mod edge_cases {
    use super::*;

    /// Tests exactly at i64::MAX.
    #[test]
    fn exactly_at_i64_max() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

        // i64::MAX should work (it's the limit)
        let size = I64_MAX;
        let params = SignatureLayoutParams::new(size, None, protocol, checksum_length);

        // This may or may not succeed depending on block count overflow
        // but should not panic
        let _result = calculate_signature_layout(params);
    }

    /// Tests just above i64::MAX (should fail).
    #[test]
    fn just_above_i64_max() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

        // i64::MAX + 1 should fail
        let size = I64_MAX + 1;
        let params = SignatureLayoutParams::new(size, None, protocol, checksum_length);

        let result = calculate_signature_layout(params);
        assert!(result.is_err(), "Should reject size > i64::MAX");
    }

    /// Tests zero file size.
    #[test]
    fn zero_file_size() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

        let params = SignatureLayoutParams::new(0, None, protocol, checksum_length);
        let layout = calculate_signature_layout(params).expect("Zero size should work");

        assert_eq!(layout.block_count(), 0);
        assert_eq!(layout.file_size(), 0);
    }

    /// Tests forced block size larger than file.
    #[test]
    fn forced_block_larger_than_file() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();
        let forced_block = NonZeroU32::new(1024 * 1024).unwrap(); // 1MB blocks

        // 100 byte file with 1MB block size
        let size = 100u64;
        let params =
            SignatureLayoutParams::new(size, Some(forced_block), protocol, checksum_length);

        let layout = calculate_signature_layout(params).expect("Should succeed");
        assert_eq!(layout.block_count(), 1);
        assert_eq!(layout.remainder(), 100);
        assert_eq!(layout.file_size(), 100);
    }
}
