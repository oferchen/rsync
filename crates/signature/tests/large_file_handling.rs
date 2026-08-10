//! Tests for large file handling (>4GB) in signature generation.
//!
//! These tests verify that signature layout calculation and generation
//! correctly handles files larger than 4GB (2^32 bytes).
//!
//! The tests use simulated/mock approaches to avoid actually creating
//! multi-gigabyte files during testing.

use std::num::{NonZeroU8, NonZeroU32};

use protocol::ProtocolVersion;
use signature::{SignatureLayoutError, SignatureLayoutParams, calculate_signature_layout};

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
                "Signature layout calculation should succeed for {size} byte file"
            );

            let layout = layout.unwrap();
            assert!(
                layout.block_count() > 0,
                "Block count should be positive for {size} byte file"
            );

            assert_eq!(
                layout.file_size(),
                size,
                "File size reconstruction failed for {size} byte file"
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
                "Signature layout should work for {size} byte file"
            );

            let layout = layout.unwrap();
            assert!(
                layout.block_count() < i32::MAX as u64,
                "Block count should not overflow i32 for {size} byte file"
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
            err => panic!("Expected FileTooLarge error, got: {err:?}"),
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

        // Acceptable outcomes: layout succeeds with a valid block count, or fails gracefully.
        let result = calculate_signature_layout(params);
        if let Ok(layout) = result {
            assert!(layout.block_count() > 0);
            assert!(layout.block_count() <= i32::MAX as u64);
        }
    }
}

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
                assert!(
                    layout.block_length().get() >= 32 * 1024,
                    "Block size {} should be >= 32KB for {} byte file",
                    layout.block_length().get(),
                    size
                );
            }

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

        // Max-block * i32::MAX is the upper bound before block_count overflow.
        let max_block_size = 128 * 1024u64;
        let max_blocks = i32::MAX as u64;
        let _max_file_before_overflow = max_blocks * max_block_size;

        let large_valid = 100 * ONE_TB; // 100TB - well within limits
        let params = SignatureLayoutParams::new(large_valid, None, protocol, checksum_length);

        let layout = calculate_signature_layout(params);
        assert!(layout.is_ok(), "Should handle {large_valid} byte file");

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

        // Protocol 30+ caps at 128 KB (1 << 17); legacy caps at 512 MB (1 << 29).
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

mod memory_bounds {
    use super::*;

    /// Tests that signature block memory is bounded for large files.
    #[test]
    fn signature_memory_bounded() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

        // 1 TB with 128 KB blocks produces ~8M blocks * (4 rolling + 16 strong) = ~160 MB.
        let file_size = ONE_TB;
        let params = SignatureLayoutParams::new(file_size, None, protocol, checksum_length);
        let layout = calculate_signature_layout(params).expect("Layout should succeed");

        let block_count = layout.block_count();
        let bytes_per_block = 4 + layout.strong_sum_length().get() as u64;
        let total_signature_bytes = block_count * bytes_per_block;

        assert!(
            total_signature_bytes < 1024 * 1024 * 1024,
            "Signature memory {total_signature_bytes} bytes too high for {file_size} byte file"
        );
    }

    /// Tests memory estimation for various file sizes.
    #[test]
    fn memory_estimation_various_sizes() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

        let test_sizes = [
            (FOUR_GB, 200 * 1024 * 1024),          // 4GB file -> < 200MB signatures
            (ONE_TB, 512 * 1024 * 1024),           // 1TB file -> < 512MB signatures
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
                "Signature size {total_signature_bytes} for {file_size} byte file exceeds limit {max_sig_bytes}"
            );
        }
    }
}

mod file_size_reconstruction {
    use super::*;

    /// Tests that file size can be accurately reconstructed from layout.
    #[test]
    fn file_size_roundtrip() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

        let test_sizes = [
            1u64,             // Minimum
            1024,             // 1KB
            1024 * 1024,      // 1MB
            FOUR_GB - 1,      // Just under 4GB
            FOUR_GB,          // Exactly 4GB
            FOUR_GB + 1,      // Just over 4GB
            ONE_TB,           // 1TB
            ONE_TB + 12345,   // Non-aligned size
            HUNDRED_TB - 999, // Large non-aligned
        ];

        for size in test_sizes {
            let params = SignatureLayoutParams::new(size, None, protocol, checksum_length);
            let layout = calculate_signature_layout(params).expect("Layout should succeed");

            assert_eq!(
                layout.file_size(),
                size,
                "File size reconstruction failed for {size}"
            );
        }
    }

    /// Tests file size reconstruction with remainder blocks.
    #[test]
    fn file_size_with_remainder() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

        // Sizes chosen so file_size % block_size != 0.
        let test_sizes = [
            FOUR_GB + 1,
            FOUR_GB + 100,
            FOUR_GB + 65535, // Just under one block extra
            ONE_TB + 1,
        ];

        for size in test_sizes {
            let params = SignatureLayoutParams::new(size, None, protocol, checksum_length);
            let layout = calculate_signature_layout(params).expect("Layout should succeed");

            assert!(
                layout.remainder() > 0 || size % layout.block_length().get() as u64 == 0,
                "Expected remainder for size {size}"
            );

            assert_eq!(layout.file_size(), size);
        }
    }
}

mod edge_cases {
    use super::*;

    /// Tests exactly at i64::MAX.
    #[test]
    fn exactly_at_i64_max() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

        let size = I64_MAX;
        let params = SignatureLayoutParams::new(size, None, protocol, checksum_length);

        // May succeed or fail depending on block count overflow, but must not panic.
        let _result = calculate_signature_layout(params);
    }

    /// Tests just above i64::MAX (should fail).
    #[test]
    fn just_above_i64_max() {
        let protocol = ProtocolVersion::NEWEST;
        let checksum_length = NonZeroU8::new(16).unwrap();

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

        let size = 100u64;
        let params =
            SignatureLayoutParams::new(size, Some(forced_block), protocol, checksum_length);

        let layout = calculate_signature_layout(params).expect("Should succeed");
        assert_eq!(layout.block_count(), 1);
        assert_eq!(layout.remainder(), 100);
        assert_eq!(layout.file_size(), 100);
    }
}
