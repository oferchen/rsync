//! Tests for large file handling (>4GB) in the protocol layer.
//!
//! These tests verify that the rsync protocol implementation correctly handles
//! files larger than 4GB (2^32 bytes), which is a critical boundary for 32-bit
//! integer representations.
//!
//! The tests use simulated/mock approaches to avoid actually creating
//! multi-gigabyte files during testing.

use std::io::Cursor;
use std::path::PathBuf;

// =============================================================================
// Constants for Large File Testing
// =============================================================================

/// 4GB boundary - the critical threshold for 32-bit overflow
const FOUR_GB: u64 = 4 * 1024 * 1024 * 1024;

/// Slightly over 4GB - tests proper u64 handling
const OVER_FOUR_GB: u64 = FOUR_GB + 1024 * 1024;

/// Maximum positive i32 value - boundary for signed integer handling
const I32_MAX: u64 = i32::MAX as u64;

/// Maximum i64 value - the hard limit for rsync file sizes
const I64_MAX: u64 = i64::MAX as u64;

/// 1TB test size - represents realistic large files
const ONE_TB: u64 = 1024 * 1024 * 1024 * 1024;

/// 100TB - tests multi-petabyte edge cases
const HUNDRED_TB: u64 = 100 * ONE_TB;

// =============================================================================
// File Size Representation Tests
// =============================================================================

mod file_size_representation {
    use super::*;
    use protocol::flist::FileEntry;

    /// Tests that FileEntry correctly stores u64 file sizes.
    #[test]
    fn file_entry_stores_u64_size() {
        // Test 4GB boundary
        let entry = FileEntry::new_file(PathBuf::from("large.bin"), FOUR_GB, 0o644);
        assert_eq!(entry.size(), FOUR_GB);

        // Test over 4GB
        let entry = FileEntry::new_file(PathBuf::from("larger.bin"), OVER_FOUR_GB, 0o644);
        assert_eq!(entry.size(), OVER_FOUR_GB);

        // Test 1TB
        let entry = FileEntry::new_file(PathBuf::from("terabyte.bin"), ONE_TB, 0o644);
        assert_eq!(entry.size(), ONE_TB);

        // Test near i64::MAX (rsync's limit)
        let near_max = I64_MAX - 1000;
        let entry = FileEntry::new_file(PathBuf::from("near_max.bin"), near_max, 0o644);
        assert_eq!(entry.size(), near_max);
    }

    /// Tests that file sizes at u64 boundaries are handled correctly.
    #[test]
    fn file_size_boundary_values() {
        // Powers of 2 boundaries that might cause issues
        let boundaries = [
            1u64 << 31,          // 2GB - i32 sign bit
            (1u64 << 31) - 1,    // Just under 2GB
            1u64 << 32,          // 4GB - u32 overflow
            (1u64 << 32) - 1,    // Max u32
            (1u64 << 32) + 1,    // Just over 4GB
            1u64 << 40,          // 1TB
            1u64 << 50,          // ~1PB
            i64::MAX as u64 - 1, // Near max allowed size
        ];

        for size in boundaries {
            let entry = FileEntry::new_file(PathBuf::from("boundary.bin"), size, 0o644);
            assert_eq!(
                entry.size(),
                size,
                "File size {size} was not stored correctly"
            );
        }
    }

    /// Tests that i64 vs u64 conversion is handled safely.
    #[test]
    fn i64_u64_conversion_safety() {
        // Maximum safe size (i64::MAX as u64)
        let max_safe: u64 = i64::MAX as u64;
        assert!(max_safe > 0);

        // Verify that sizes below i64::MAX work
        let size: u64 = max_safe;
        let as_i64: i64 = size as i64;
        assert!(
            as_i64 > 0,
            "Max safe size should be positive when cast to i64"
        );

        // Verify size can be converted back
        let back_to_u64: u64 = as_i64 as u64;
        assert_eq!(
            size, back_to_u64,
            "Round-trip conversion should be lossless"
        );
    }

    /// Tests sizes that would overflow if treated as signed 32-bit.
    #[test]
    fn sizes_above_i32_max() {
        // These sizes would be negative if truncated to i32
        let sizes_above_i32_max = [
            I32_MAX + 1,
            I32_MAX + 1000,
            u32::MAX as u64,
            u32::MAX as u64 + 1,
            FOUR_GB,
        ];

        for size in sizes_above_i32_max {
            let entry = FileEntry::new_file(PathBuf::from("file.bin"), size, 0o644);
            assert_eq!(entry.size(), size);
            // Verify it's not accidentally truncated to i32
            assert!(entry.size() > I32_MAX);
        }
    }
}

// =============================================================================
// Varint/Varlong Encoding Tests for Large File Sizes
// =============================================================================

mod varint_large_files {
    use super::*;
    use protocol::{read_longint, read_varlong, write_longint, write_varlong};

    /// Tests that varlong encoding handles large file sizes correctly.
    ///
    /// Note: With min_bytes=3, varlong can encode up to ~72 petabytes (2^56 - 1).
    /// For even larger values, use min_bytes=4 or higher.
    #[test]
    fn varlong_encodes_large_sizes() {
        // Test various large file sizes (within practical limits for min_bytes=3)
        // max for min_bytes=3 is approximately 2^56 - 1 = 72,057,594,037,927,935
        let large_sizes: &[i64] = &[
            FOUR_GB as i64,
            OVER_FOUR_GB as i64,
            ONE_TB as i64,
            HUNDRED_TB as i64,
            1000 * ONE_TB as i64, // 1 PB
        ];

        for &size in large_sizes {
            let mut buffer = Vec::new();
            write_varlong(&mut buffer, size, 3).expect("write_varlong should succeed");

            let mut cursor = Cursor::new(&buffer);
            let decoded = read_varlong(&mut cursor, 3).expect("read_varlong should succeed");

            assert_eq!(decoded, size, "Varlong round-trip failed for size {size}");
        }
    }

    /// Tests varlong practical limit documentation.
    ///
    /// The varlong encoding has a practical limit of ~64 PB (2^56 - 1) for
    /// standard encoding. This is sufficient for any realistic file size,
    /// as the largest commercial storage systems are in the exabyte range.
    #[test]
    fn varlong_practical_limits() {
        // Maximum safely encodable value: approximately 64 PB
        let max_practical: i64 = (1i64 << 56) - 1;

        let mut buffer = Vec::new();
        write_varlong(&mut buffer, max_practical, 3).expect("write_varlong should succeed");

        let mut cursor = Cursor::new(&buffer);
        let decoded = read_varlong(&mut cursor, 3).expect("read_varlong should succeed");

        assert_eq!(decoded, max_practical);

        // This is approximately 64 petabytes - far exceeding any practical file size
        // Integer division: 72,057,594,037,927,935 / 1,125,899,906,842,624 = 63
        let petabytes = max_practical / (1024 * 1024 * 1024 * 1024 * 1024);
        assert!(
            petabytes >= 63,
            "Should support at least 63 PB, got {petabytes}"
        );
    }

    /// Tests that longint encoding (protocol < 30) handles large sizes.
    #[test]
    fn longint_encodes_large_sizes() {
        let large_sizes: &[i64] = &[
            0x7FFF_FFFF,     // Max that fits in first format
            0x7FFF_FFFF + 1, // First size requiring extended format
            FOUR_GB as i64,
            ONE_TB as i64,
            i64::MAX - 1,
        ];

        for &size in large_sizes {
            let mut buffer = Vec::new();
            write_longint(&mut buffer, size).expect("write_longint should succeed");

            let mut cursor = Cursor::new(&buffer);
            let decoded = read_longint(&mut cursor).expect("read_longint should succeed");

            assert_eq!(decoded, size, "Longint round-trip failed for size {size}");
        }
    }

    /// Tests varlong with minimum bytes parameter for large file sizes.
    #[test]
    fn varlong_min_bytes_variants() {
        let large_size = ONE_TB as i64;

        // Test with different min_bytes values (common in protocol)
        for min_bytes in [3u8, 4, 5] {
            let mut buffer = Vec::new();
            write_varlong(&mut buffer, large_size, min_bytes)
                .expect("write_varlong should succeed");

            let mut cursor = Cursor::new(&buffer);
            let decoded =
                read_varlong(&mut cursor, min_bytes).expect("read_varlong should succeed");

            assert_eq!(
                decoded, large_size,
                "Varlong with min_bytes={min_bytes} failed"
            );
        }
    }

    /// Tests encoding sizes at specific bit boundaries.
    #[test]
    fn varlong_bit_boundaries() {
        // Test at powers of 2 that might cause issues
        let bit_boundaries: &[i64] = &[
            (1i64 << 30) - 1, // Just under 1GB
            1i64 << 30,       // 1GB
            (1i64 << 31) - 1, // Just under 2GB (i32 max)
            1i64 << 31,       // 2GB
            (1i64 << 32) - 1, // Just under 4GB (u32 max)
            1i64 << 32,       // 4GB
            (1i64 << 40) - 1, // Just under 1TB
            1i64 << 40,       // 1TB
            (1i64 << 50) - 1, // ~1PB
        ];

        for &size in bit_boundaries {
            let mut buffer = Vec::new();
            write_varlong(&mut buffer, size, 3).expect("write_varlong should succeed");

            let mut cursor = Cursor::new(&buffer);
            let decoded = read_varlong(&mut cursor, 3).expect("read_varlong should succeed");

            assert_eq!(decoded, size, "Varlong failed at bit boundary {size}");
        }
    }
}

// =============================================================================
// Delta Wire Format Tests for Large Files
// =============================================================================

mod delta_wire_format_large_files {
    use super::*;
    use protocol::wire::{CHUNK_SIZE, DeltaOp, write_token_literal, write_token_stream};

    /// Tests that delta operations can reference positions beyond 4GB.
    #[test]
    fn delta_ops_with_large_offsets() {
        // Create delta operations that would reference positions > 4GB
        let large_block_index = (FOUR_GB / 65536) as u32; // Block at 4GB if 64KB blocks

        let ops = [
            DeltaOp::Literal(vec![0; 1024]),
            DeltaOp::Copy {
                block_index: large_block_index,
                length: 65536,
            },
            DeltaOp::Literal(vec![0; 512]),
        ];

        // Verify the operations are created correctly
        match &ops[1] {
            DeltaOp::Copy {
                block_index,
                length,
            } => {
                assert_eq!(*block_index, large_block_index);
                assert_eq!(*length, 65536);
            }
            _ => panic!("Expected Copy operation"),
        }
    }

    /// Tests wire format encoding of delta operations for large files.
    #[test]
    fn delta_wire_format_large_blocks() {
        // Simulate delta for a file where blocks are at positions > 4GB
        let ops = vec![
            DeltaOp::Literal(vec![1, 2, 3, 4]),
            // Block index that represents position > 4GB
            DeltaOp::Copy {
                block_index: 70_000, // With 64KB blocks, this is ~4.5GB
                length: 65536,
            },
            DeltaOp::Literal(vec![5, 6, 7, 8]),
        ];

        let mut buffer = Vec::new();
        write_token_stream(&mut buffer, &ops).expect("Should encode delta ops");

        // Buffer should contain encoded data
        assert!(!buffer.is_empty());
    }

    /// Tests that delta token encoding handles large literal chunks.
    #[test]
    fn delta_literal_chunking() {
        // Test that large literals are properly chunked
        let large_literal = vec![0u8; CHUNK_SIZE * 3 + 100]; // 3+ chunks

        let mut buffer = Vec::new();
        write_token_literal(&mut buffer, &large_literal).expect("Should write literal");

        // Should have written multiple chunk headers
        // Each chunk is 4 bytes (i32 length) + data
        assert!(buffer.len() > large_literal.len());
    }

    /// Tests block indices within the valid i32 range.
    ///
    /// Note: The wire format uses i32 for token encoding, so block indices
    /// are limited to i32::MAX - 1 (because token = -(block_index + 1)).
    /// Block indices >= i32::MAX will cause overflow.
    #[test]
    fn delta_ops_valid_block_index_range() {
        // Test block indices within valid range (< i32::MAX)
        let valid_block_indices = [
            0u32,
            1000,
            1_000_000,
            (i32::MAX - 1) as u32, // Maximum valid block index
        ];

        for block_index in valid_block_indices {
            let ops = vec![DeltaOp::Copy {
                block_index,
                length: 1024,
            }];

            let mut buffer = Vec::new();
            write_token_stream(&mut buffer, &ops).expect("Should encode delta op");

            // Verify encoding succeeded
            assert!(!buffer.is_empty());
        }
    }

    /// Tests that the maximum block index is limited by i32 range.
    ///
    /// With 128KB blocks (max for protocol 30+), the maximum file size
    /// that can be delta-transferred is approximately:
    /// (i32::MAX - 1) * 128KB = ~274TB
    ///
    /// This is a protocol limitation inherent in rsync's wire format.
    #[test]
    fn delta_max_file_size_calculation() {
        let max_block_size: u64 = 128 * 1024; // 128KB for protocol 30+
        let max_block_index: u64 = (i32::MAX - 1) as u64;

        // Maximum file size that can be delta-transferred
        let max_delta_file_size = max_block_index * max_block_size;

        // Should be approximately 274TB
        assert!(
            max_delta_file_size > 200 * ONE_TB,
            "Max delta file size {max_delta_file_size} should be > 200TB"
        );
        assert!(
            max_delta_file_size < 300 * ONE_TB,
            "Max delta file size {max_delta_file_size} should be < 300TB"
        );
    }
}

// =============================================================================
// File List Entry Tests for Large Files
// =============================================================================

mod file_list_large_files {
    use super::*;
    use protocol::flist::FileEntry;

    /// Tests file list entry encoding with large file sizes.
    #[test]
    fn file_list_entry_large_sizes() {
        // Create entries with various large sizes
        let entries = vec![
            FileEntry::new_file(PathBuf::from("4gb.bin"), FOUR_GB, 0o644),
            FileEntry::new_file(PathBuf::from("1tb.bin"), ONE_TB, 0o644),
            FileEntry::new_file(PathBuf::from("nearmax.bin"), I64_MAX - 1, 0o644),
        ];

        for entry in entries {
            // Verify sizes are stored correctly
            assert!(entry.size() > 0);
            // Verify size doesn't overflow when accessed
            let _ = entry.size().to_string();
        }
    }

    /// Tests that file entry comparison works with large sizes.
    #[test]
    fn file_entry_comparison_large_sizes() {
        let small = FileEntry::new_file(PathBuf::from("a.bin"), 1024, 0o644);
        let large = FileEntry::new_file(PathBuf::from("b.bin"), FOUR_GB, 0o644);
        let larger = FileEntry::new_file(PathBuf::from("c.bin"), ONE_TB, 0o644);

        assert!(small.size() < large.size());
        assert!(large.size() < larger.size());
    }

    /// Tests file entries at exact boundary sizes.
    #[test]
    fn file_entry_boundary_sizes() {
        let test_sizes = [
            (1u64 << 31) - 1, // Max positive i32
            1u64 << 31,       // i32 overflow
            (1u64 << 32) - 1, // Max u32
            1u64 << 32,       // u32 overflow
            (1u64 << 63) - 1, // Max i64
        ];

        for size in test_sizes {
            let entry = FileEntry::new_file(PathBuf::from("test.bin"), size, 0o644);
            assert_eq!(entry.size(), size, "Size {size} not stored correctly");
        }
    }
}

// =============================================================================
// Statistics Accumulation Tests
// =============================================================================

mod statistics_large_files {
    use super::*;

    /// Tests statistics accumulation for large file transfers.
    #[test]
    fn statistics_accumulation_large_transfers() {
        // Simulate accumulating transfer stats for many large files
        struct TransferStats {
            total_bytes: u64,
            files_transferred: u64,
            matched_bytes: u64,
            literal_bytes: u64,
        }

        let mut stats = TransferStats {
            total_bytes: 0,
            files_transferred: 0,
            matched_bytes: 0,
            literal_bytes: 0,
        };

        // Add 100 files of 1TB each
        // Note: Use exact byte values to avoid floating point precision issues
        let file_size: u64 = ONE_TB;
        let matched_per_file: u64 = (file_size / 10) * 9; // 90%
        let literal_per_file: u64 = file_size - matched_per_file; // 10%

        for _ in 0..100 {
            stats.total_bytes = stats.total_bytes.saturating_add(file_size);
            stats.files_transferred += 1;
            stats.matched_bytes = stats.matched_bytes.saturating_add(matched_per_file);
            stats.literal_bytes = stats.literal_bytes.saturating_add(literal_per_file);
        }

        assert_eq!(stats.total_bytes, 100 * ONE_TB);
        assert_eq!(stats.files_transferred, 100);
        // 90% matched, 10% literal
        assert_eq!(stats.matched_bytes + stats.literal_bytes, stats.total_bytes);
    }

    /// Tests that byte counters don't overflow for petabyte-scale transfers.
    #[test]
    fn byte_counter_petabyte_scale() {
        let mut total_bytes: u64 = 0;

        // Simulate transferring 1000 TB (1 PB)
        for _ in 0..1000 {
            total_bytes = total_bytes
                .checked_add(ONE_TB)
                .expect("Should not overflow");
        }

        assert_eq!(total_bytes, 1000 * ONE_TB);

        // Verify it's still well under u64::MAX
        assert!(total_bytes < u64::MAX / 2);
    }
}

// =============================================================================
// Unsigned File Size Tests
// =============================================================================

mod unsigned_file_sizes {
    use super::*;
    use protocol::{read_varlong, write_varlong};

    /// Tests that file sizes (always non-negative) encode correctly.
    ///
    /// Note: The rsync protocol uses varlong for file sizes, which are always
    /// non-negative. The i64 type is used for compatibility with the C implementation,
    /// but actual file sizes should never be negative.
    ///
    /// With min_bytes=3 (common for file sizes), the practical limit is ~72 PB.
    #[test]
    fn varlong_file_sizes() {
        let file_sizes: &[i64] = &[
            0,
            1,
            1024,
            1024 * 1024,
            FOUR_GB as i64,
            ONE_TB as i64,
            HUNDRED_TB as i64,
            1000 * ONE_TB as i64, // 1 PB - practical max for min_bytes=3
        ];

        for &size in file_sizes {
            let mut buffer = Vec::new();
            write_varlong(&mut buffer, size, 3).expect("write_varlong should succeed");

            let mut cursor = Cursor::new(&buffer);
            let decoded = read_varlong(&mut cursor, 3).expect("read_varlong should succeed");

            assert_eq!(decoded, size, "File size {size} not preserved");
        }
    }

    /// Tests encoding at petabyte scale.
    #[test]
    fn varlong_petabyte_scale() {
        // 1 PB = 1024 TB
        let one_pb: i64 = (1024 * ONE_TB) as i64;

        let mut buffer = Vec::new();
        write_varlong(&mut buffer, one_pb, 3).expect("write_varlong should succeed");

        let mut cursor = Cursor::new(&buffer);
        let decoded = read_varlong(&mut cursor, 3).expect("read_varlong should succeed");

        assert_eq!(decoded, one_pb);
    }
}
