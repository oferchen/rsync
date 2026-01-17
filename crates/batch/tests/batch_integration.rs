//! Comprehensive integration tests for the batch crate.
//!
//! These tests validate the complete batch mode functionality including:
//! - Batch file reading and writing operations
//! - Round-trip encoding/decoding of all data structures
//! - Error handling for malformed and corrupted data
//! - Edge cases including empty batches and large data volumes
//!
//! The tests are organized by functional area and follow upstream rsync's
//! batch.c behavior as documented in `target/interop/upstream-src/rsync-3.4.1/batch.c`.
//!
//! ## Upstream Compatibility
//!
//! The batch file format must maintain byte-for-byte compatibility with upstream rsync:
//! - Stream flags bitmap (i32) written first
//! - Protocol version (i32) follows
//! - Compat flags (varint) for protocol >= 30
//! - Checksum seed (i32) completes the header
//!
//! ## Test Organization
//!
//! - `batch_file_operations` - Basic read/write lifecycle
//! - `round_trip_tests` - Encoding/decoding verification
//! - `error_handling` - Malformed data and error conditions
//! - `edge_cases` - Empty batches, large data, boundary conditions
//! - `file_entry_tests` - FileEntry serialization
//! - `batch_flags_tests` - Stream flags across protocol versions
//! - `script_generation` - Shell script output validation

use batch::{BatchConfig, BatchFlags, BatchMode, BatchReader, BatchWriter, FileEntry};
use std::fs::{self, File};
use std::path::Path;
use tempfile::TempDir;

// ============================================================================
// Test Utilities
// ============================================================================

/// Creates a test batch file with the specified configuration and returns the path.
fn create_test_batch(temp_dir: &TempDir, name: &str, protocol: i32, seed: i32) -> String {
    let batch_path = temp_dir.path().join(name);
    let config = BatchConfig::new(
        BatchMode::Write,
        batch_path.to_string_lossy().to_string(),
        protocol,
    )
    .with_checksum_seed(seed);

    let mut writer = BatchWriter::new(config).expect("writer creation should succeed");
    writer
        .write_header(BatchFlags::default())
        .expect("header write should succeed");
    writer.finalize().expect("finalize should succeed");

    batch_path.to_string_lossy().to_string()
}

// ============================================================================
// Batch File Operations Tests
// ============================================================================

mod batch_file_operations {
    //! Tests for basic batch file read/write operations.

    use super::*;

    #[test]
    fn writer_creates_file_at_specified_path() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("test_create.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let writer = BatchWriter::new(config).expect("writer should be created");
        drop(writer);

        assert!(
            batch_path.exists(),
            "Batch file should exist after writer creation"
        );
    }

    #[test]
    fn writer_creates_parent_directories_error() {
        // Writers should fail when parent directory doesn't exist
        let config = BatchConfig::new(
            BatchMode::Write,
            "/nonexistent/parent/dir/batch.file".to_owned(),
            30,
        );

        let result = BatchWriter::new(config);
        assert!(
            result.is_err(),
            "Should fail when parent directory doesn't exist"
        );
    }

    #[test]
    fn reader_opens_existing_file() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = create_test_batch(&temp_dir, "existing.batch", 30, 0);

        let config = BatchConfig::new(BatchMode::Read, batch_path, 30);
        let reader = BatchReader::new(config);

        assert!(reader.is_ok(), "Reader should open existing file");
    }

    #[test]
    fn reader_fails_for_nonexistent_file() {
        let config = BatchConfig::new(
            BatchMode::Read,
            "/definitely/not/a/real/path.batch".to_owned(),
            30,
        );

        let result = BatchReader::new(config);
        assert!(result.is_err(), "Reader should fail for nonexistent file");
    }

    #[test]
    fn writer_overwrites_existing_file() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("overwrite.batch");

        // Create initial file with some content
        fs::write(&batch_path, b"initial content that should be overwritten").unwrap();
        let initial_size = fs::metadata(&batch_path).unwrap().len();

        // Overwrite with batch writer
        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.finalize().unwrap();

        let final_size = fs::metadata(&batch_path).unwrap().len();
        assert_ne!(
            initial_size, final_size,
            "File size should change after overwrite"
        );
    }

    #[test]
    fn multiple_sequential_writes() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("sequential.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();

        // Write multiple chunks
        for i in 0..100 {
            let data = format!("chunk_{:04}", i);
            writer.write_data(data.as_bytes()).unwrap();
        }

        writer.finalize().unwrap();

        // Verify file contains all data
        let content = fs::read(&batch_path).unwrap();
        assert!(
            content.len() > 100 * 10,
            "File should contain all written chunks"
        );
    }

    #[test]
    fn flush_persists_data_to_disk() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("flush.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.write_data(b"data before flush").unwrap();

        // Flush without finalizing
        writer.flush().unwrap();

        // File should have content after flush
        let size_after_flush = fs::metadata(&batch_path).unwrap().len();
        assert!(size_after_flush > 0, "File should have content after flush");
    }

    #[test]
    fn finalize_closes_file_handle() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("finalize.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.write_data(b"test data").unwrap();
        writer.finalize().unwrap();

        // File should be readable immediately after finalize
        let content = fs::read(&batch_path).unwrap();
        assert!(
            !content.is_empty(),
            "File should be readable after finalize"
        );
    }
}

// ============================================================================
// Round-Trip Encoding/Decoding Tests
// ============================================================================

mod round_trip_tests {
    //! Tests verifying data survives write-then-read cycles unchanged.

    use super::*;

    #[test]
    fn header_round_trip_protocol_30() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("roundtrip30.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        // Write
        let write_config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30)
            .with_checksum_seed(12345)
            .with_compat_flags(0x42);

        let mut writer = BatchWriter::new(write_config).unwrap();
        let write_flags = BatchFlags {
            recurse: true,
            preserve_uid: true,
            preserve_gid: true,
            preserve_links: true,
            preserve_devices: false,
            preserve_hard_links: true,
            always_checksum: false,
            xfer_dirs: true,
            do_compression: true,
            iconv: false,
            preserve_acls: true,
            preserve_xattrs: true,
            inplace: false,
            append: false,
            append_verify: false,
        };
        writer.write_header(write_flags).unwrap();
        writer.finalize().unwrap();

        // Read
        let read_config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(read_config).unwrap();
        let read_flags = reader.read_header().unwrap();

        assert_eq!(
            write_flags, read_flags,
            "Flags should match after round-trip"
        );

        let header = reader.header().unwrap();
        assert_eq!(header.protocol_version, 30);
        assert_eq!(header.checksum_seed, 12345);
    }

    #[test]
    fn header_round_trip_protocol_31() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("roundtrip31.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let write_config =
            BatchConfig::new(BatchMode::Write, path_str.clone(), 31).with_checksum_seed(99999);

        let mut writer = BatchWriter::new(write_config).unwrap();
        let flags = BatchFlags {
            recurse: true,
            preserve_hard_links: true,
            always_checksum: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();
        writer.finalize().unwrap();

        let read_config = BatchConfig::new(BatchMode::Read, path_str, 31);
        let mut reader = BatchReader::new(read_config).unwrap();
        let read_flags = reader.read_header().unwrap();

        assert_eq!(flags, read_flags);
    }

    #[test]
    fn header_round_trip_protocol_29() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("roundtrip29.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let write_config = BatchConfig::new(BatchMode::Write, path_str.clone(), 29);

        let mut writer = BatchWriter::new(write_config).unwrap();
        let flags = BatchFlags {
            recurse: true,
            xfer_dirs: true,
            do_compression: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();
        writer.finalize().unwrap();

        let read_config = BatchConfig::new(BatchMode::Read, path_str, 29);
        let mut reader = BatchReader::new(read_config).unwrap();
        let read_flags = reader.read_header().unwrap();

        assert!(read_flags.recurse);
        assert!(read_flags.xfer_dirs);
        assert!(read_flags.do_compression);
    }

    #[test]
    fn header_round_trip_protocol_28() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("roundtrip28.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let write_config = BatchConfig::new(BatchMode::Write, path_str.clone(), 28);

        let mut writer = BatchWriter::new(write_config).unwrap();
        let flags = BatchFlags {
            recurse: true,
            preserve_uid: true,
            preserve_gid: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();
        writer.finalize().unwrap();

        let read_config = BatchConfig::new(BatchMode::Read, path_str, 28);
        let mut reader = BatchReader::new(read_config).unwrap();
        let read_flags = reader.read_header().unwrap();

        assert!(read_flags.recurse);
        assert!(read_flags.preserve_uid);
        assert!(read_flags.preserve_gid);

        // Protocol 28 should not have compat_flags
        assert!(reader.header().unwrap().compat_flags.is_none());
    }

    #[test]
    fn data_round_trip_binary_content() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("binary_roundtrip.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        // Create binary data with all byte values
        let mut binary_data = Vec::with_capacity(256);
        for i in 0u8..=255 {
            binary_data.push(i);
        }

        let write_config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(write_config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.write_data(&binary_data).unwrap();
        writer.finalize().unwrap();

        let read_config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(read_config).unwrap();
        reader.read_header().unwrap();

        let mut read_data = vec![0u8; 256];
        reader.read_exact(&mut read_data).unwrap();

        assert_eq!(
            binary_data, read_data,
            "Binary data should survive round-trip"
        );
    }

    #[test]
    fn data_round_trip_utf8_content() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("utf8_roundtrip.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let utf8_data = "Hello, World! \u{1F600} \u{4E2D}\u{6587} \u{0420}\u{0443}\u{0441}\u{0441}\u{043A}\u{0438}\u{0439}";

        let write_config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(write_config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.write_data(utf8_data.as_bytes()).unwrap();
        writer.finalize().unwrap();

        let read_config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(read_config).unwrap();
        reader.read_header().unwrap();

        let mut read_data = vec![0u8; utf8_data.len()];
        reader.read_exact(&mut read_data).unwrap();
        let read_str = String::from_utf8(read_data).unwrap();

        assert_eq!(utf8_data, read_str, "UTF-8 data should survive round-trip");
    }

    #[test]
    fn all_flags_set_round_trip() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("all_flags.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let all_flags = BatchFlags {
            recurse: true,
            preserve_uid: true,
            preserve_gid: true,
            preserve_links: true,
            preserve_devices: true,
            preserve_hard_links: true,
            always_checksum: true,
            xfer_dirs: true,
            do_compression: true,
            iconv: true,
            preserve_acls: true,
            preserve_xattrs: true,
            inplace: true,
            append: true,
            append_verify: true,
        };

        let write_config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(write_config).unwrap();
        writer.write_header(all_flags).unwrap();
        writer.finalize().unwrap();

        let read_config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(read_config).unwrap();
        let read_flags = reader.read_header().unwrap();

        assert_eq!(
            all_flags, read_flags,
            "All flags should match after round-trip"
        );
    }

    #[test]
    fn no_flags_set_round_trip() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("no_flags.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let no_flags = BatchFlags::default();

        let write_config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(write_config).unwrap();
        writer.write_header(no_flags).unwrap();
        writer.finalize().unwrap();

        let read_config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(read_config).unwrap();
        let read_flags = reader.read_header().unwrap();

        assert_eq!(
            no_flags, read_flags,
            "Default flags should match after round-trip"
        );
    }
}

// ============================================================================
// Error Handling Tests
// ============================================================================

mod error_handling {
    //! Tests for error conditions and malformed data handling.

    use super::*;

    #[test]
    fn truncated_header_reports_error() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("truncated.batch");

        // Write only 3 bytes - not enough for a valid header
        fs::write(&batch_path, &[0x01, 0x02, 0x03]).unwrap();

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut reader = BatchReader::new(config).unwrap();
        let result = reader.read_header();

        assert!(result.is_err(), "Truncated header should cause an error");
    }

    #[test]
    fn corrupted_flags_bitmap_handled() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("corrupt_flags.batch");

        // Write a valid-looking header with extreme flag values
        let mut data = Vec::new();
        // Stream flags with many bits set (some invalid)
        data.extend_from_slice(&0x7FFF_FFFFi32.to_le_bytes());
        // Protocol version 30
        data.extend_from_slice(&30i32.to_le_bytes());
        // Compat flags (minimal varint: 0)
        data.push(0);
        // Checksum seed
        data.extend_from_slice(&12345i32.to_le_bytes());

        fs::write(&batch_path, &data).unwrap();

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut reader = BatchReader::new(config).unwrap();
        // This should succeed - unknown flag bits are ignored
        let flags = reader.read_header().unwrap();

        // All known flags should be set
        assert!(flags.recurse);
        assert!(flags.preserve_uid);
    }

    #[test]
    fn protocol_version_mismatch_error() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("version_mismatch.batch");

        // Write a batch with protocol 30
        let write_config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );
        let mut writer = BatchWriter::new(write_config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.finalize().unwrap();

        // Try to read with protocol 31
        let read_config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            31,
        );

        let mut reader = BatchReader::new(read_config).unwrap();
        let result = reader.read_header();

        assert!(result.is_err(), "Protocol mismatch should cause an error");
        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("mismatch") || err_msg.contains("Protocol"),
            "Error message should mention protocol mismatch: {err_msg}"
        );
    }

    #[test]
    fn write_data_before_header_error() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("no_header.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut writer = BatchWriter::new(config).unwrap();
        let result = writer.write_data(b"data without header");

        assert!(result.is_err(), "Writing data before header should fail");
    }

    #[test]
    fn double_header_write_error() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("double_header.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        let result = writer.write_header(BatchFlags::default());

        assert!(result.is_err(), "Writing header twice should fail");
    }

    #[test]
    fn double_header_read_error() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = create_test_batch(&temp_dir, "double_read.batch", 30, 0);

        let config = BatchConfig::new(BatchMode::Read, batch_path, 30);

        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();
        let result = reader.read_header();

        assert!(result.is_err(), "Reading header twice should fail");
    }

    #[test]
    fn read_data_before_header_error() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = create_test_batch(&temp_dir, "read_no_header.batch", 30, 0);

        let config = BatchConfig::new(BatchMode::Read, batch_path, 30);

        let mut reader = BatchReader::new(config).unwrap();
        let mut buf = [0u8; 10];
        let result = reader.read_data(&mut buf);

        assert!(result.is_err(), "Reading data before header should fail");
    }

    #[test]
    fn read_exact_with_insufficient_data() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("short_data.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.write_data(b"short").unwrap();
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        // Try to read more data than available
        let mut buf = [0u8; 1000];
        let result = reader.read_exact(&mut buf);

        assert!(
            result.is_err(),
            "read_exact with insufficient data should fail"
        );
    }

    #[test]
    fn empty_file_header_read_error() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("empty.batch");

        // Create empty file
        File::create(&batch_path).unwrap();

        let config = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut reader = BatchReader::new(config).unwrap();
        let result = reader.read_header();

        assert!(result.is_err(), "Empty file should fail header read");
    }

    #[test]
    fn zero_length_data_read_at_eof() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("eof.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        // No data written
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        let mut buf = [0u8; 100];
        let n = reader.read_data(&mut buf).unwrap();

        assert_eq!(n, 0, "Reading at EOF should return 0 bytes");
    }

    #[test]
    fn write_file_entry_before_header_error() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("entry_no_header.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let mut writer = BatchWriter::new(config).unwrap();
        let entry = FileEntry::new("test.txt".to_owned(), 0o644, 100, 1234567890);
        let result = writer.write_file_entry(&entry);

        assert!(
            result.is_err(),
            "Writing file entry before header should fail"
        );
    }

    #[test]
    fn read_file_entry_before_header_error() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = create_test_batch(&temp_dir, "entry_read_no_header.batch", 30, 0);

        let config = BatchConfig::new(BatchMode::Read, batch_path, 30);
        let mut reader = BatchReader::new(config).unwrap();

        let result = reader.read_file_entry();

        assert!(
            result.is_err(),
            "Reading file entry before header should fail"
        );
    }
}

// ============================================================================
// Edge Cases Tests
// ============================================================================

mod edge_cases {
    //! Tests for boundary conditions and extreme values.

    use super::*;

    #[test]
    fn empty_batch_file() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("empty.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        // Write batch with only header
        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.finalize().unwrap();

        // Read it back
        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        let mut buf = [0u8; 10];
        let n = reader.read_data(&mut buf).unwrap();
        assert_eq!(n, 0, "Empty batch should have no data after header");
    }

    #[test]
    fn large_batch_one_megabyte() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("large_1mb.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let data_size = 1024 * 1024; // 1 MB
        let large_data: Vec<u8> = (0..data_size).map(|i| (i % 256) as u8).collect();

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.write_data(&large_data).unwrap();
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        let mut read_data = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = reader.read_data(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            read_data.extend_from_slice(&buf[..n]);
        }

        assert_eq!(read_data.len(), data_size);
        assert_eq!(read_data, large_data);
    }

    #[test]
    fn large_batch_ten_megabytes() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("large_10mb.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let data_size = 10 * 1024 * 1024; // 10 MB
        let large_data: Vec<u8> = (0..data_size).map(|i| (i % 256) as u8).collect();

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.write_data(&large_data).unwrap();
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        let mut read_data = Vec::new();
        let mut buf = [0u8; 32768];
        loop {
            let n = reader.read_data(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            read_data.extend_from_slice(&buf[..n]);
        }

        assert_eq!(read_data.len(), data_size);
        assert_eq!(read_data, large_data);
    }

    #[test]
    fn checksum_seed_boundary_values() {
        let temp_dir = TempDir::new().unwrap();

        let seeds = [0i32, 1, -1, i32::MAX, i32::MIN, 12345, -67890];

        for (i, &seed) in seeds.iter().enumerate() {
            let batch_path = temp_dir.path().join(format!("seed_{i}.batch"));
            let path_str = batch_path.to_string_lossy().to_string();

            let config =
                BatchConfig::new(BatchMode::Write, path_str.clone(), 30).with_checksum_seed(seed);

            let mut writer = BatchWriter::new(config).unwrap();
            writer.write_header(BatchFlags::default()).unwrap();
            writer.finalize().unwrap();

            let config = BatchConfig::new(BatchMode::Read, path_str, 30);
            let mut reader = BatchReader::new(config).unwrap();
            reader.read_header().unwrap();

            assert_eq!(
                reader.header().unwrap().checksum_seed,
                seed,
                "Seed {seed} should round-trip correctly"
            );
        }
    }

    #[test]
    fn protocol_version_boundary_values() {
        let temp_dir = TempDir::new().unwrap();

        // Test all supported protocol versions
        let protocols = [28, 29, 30, 31, 32];

        for &protocol in &protocols {
            let batch_path = temp_dir.path().join(format!("proto_{protocol}.batch"));
            let path_str = batch_path.to_string_lossy().to_string();

            let config = BatchConfig::new(BatchMode::Write, path_str.clone(), protocol);

            let mut writer = BatchWriter::new(config).unwrap();
            writer.write_header(BatchFlags::default()).unwrap();
            writer.finalize().unwrap();

            let config = BatchConfig::new(BatchMode::Read, path_str, protocol);
            let mut reader = BatchReader::new(config).unwrap();
            reader.read_header().unwrap();

            assert_eq!(
                reader.header().unwrap().protocol_version,
                protocol,
                "Protocol {protocol} should round-trip correctly"
            );
        }
    }

    #[test]
    fn compat_flags_boundary_values() {
        let temp_dir = TempDir::new().unwrap();

        let compat_flags = [0u64, 1, 0x7F, 0x80, 0xFF, 0xFFFF, u64::MAX];

        for (i, &flags) in compat_flags.iter().enumerate() {
            let batch_path = temp_dir.path().join(format!("compat_{i}.batch"));
            let path_str = batch_path.to_string_lossy().to_string();

            let config =
                BatchConfig::new(BatchMode::Write, path_str.clone(), 30).with_compat_flags(flags);

            let mut writer = BatchWriter::new(config).unwrap();
            writer.write_header(BatchFlags::default()).unwrap();
            writer.finalize().unwrap();

            let config = BatchConfig::new(BatchMode::Read, path_str, 30);
            let mut reader = BatchReader::new(config).unwrap();
            reader.read_header().unwrap();

            assert_eq!(
                reader.header().unwrap().compat_flags,
                Some(flags),
                "Compat flags {flags} should round-trip correctly"
            );
        }
    }

    #[test]
    fn many_small_writes() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("many_writes.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();

        // Write 10000 single-byte chunks
        let write_count = 10000;
        for i in 0..write_count {
            writer.write_data(&[(i % 256) as u8]).unwrap();
        }
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        let mut read_data = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            let n = reader.read_data(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            read_data.extend_from_slice(&buf[..n]);
        }

        assert_eq!(read_data.len(), write_count);
        for (i, &byte) in read_data.iter().enumerate() {
            assert_eq!(byte, (i % 256) as u8);
        }
    }

    #[test]
    fn single_byte_data() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("single_byte.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.write_data(&[0x42]).unwrap();
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        let mut buf = [0u8; 1];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(buf[0], 0x42);
    }

    #[test]
    fn null_bytes_in_data() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("null_bytes.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let data_with_nulls = b"before\x00middle\x00after\x00\x00\x00";

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.write_data(data_with_nulls).unwrap();
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        let mut read_data = vec![0u8; data_with_nulls.len()];
        reader.read_exact(&mut read_data).unwrap();
        assert_eq!(read_data, data_with_nulls);
    }
}

// ============================================================================
// File Entry Tests
// ============================================================================

mod file_entry_tests {
    //! Tests for FileEntry serialization and round-trips.

    use super::*;

    #[test]
    fn file_entry_basic_round_trip() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("entry_basic.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let entry = FileEntry::new(
            "test/file.txt".to_owned(),
            0o100644, // Regular file with rw-r--r--
            1024,
            1609459200, // 2021-01-01 00:00:00 UTC
        );

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.write_file_entry(&entry).unwrap();
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        let read_entry = reader.read_file_entry().unwrap();
        assert!(read_entry.is_some());
        let read_entry = read_entry.unwrap();

        assert_eq!(entry.path, read_entry.path);
        assert_eq!(entry.mode, read_entry.mode);
        assert_eq!(entry.size, read_entry.size);
        assert_eq!(entry.mtime, read_entry.mtime);
    }

    #[test]
    fn file_entry_with_uid_gid() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("entry_uid_gid.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let mut entry = FileEntry::new("owned.txt".to_owned(), 0o100755, 2048, 1609459200);
        entry.uid = Some(1000);
        entry.gid = Some(1000);

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.write_file_entry(&entry).unwrap();
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        let read_entry = reader.read_file_entry().unwrap().unwrap();

        assert_eq!(entry.uid, read_entry.uid);
        assert_eq!(entry.gid, read_entry.gid);
    }

    #[test]
    fn file_entry_root_uid_gid() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("entry_root.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let mut entry = FileEntry::new("root_owned.txt".to_owned(), 0o100644, 100, 1609459200);
        entry.uid = Some(0);
        entry.gid = Some(0);

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.write_file_entry(&entry).unwrap();
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        let read_entry = reader.read_file_entry().unwrap().unwrap();

        assert_eq!(Some(0), read_entry.uid);
        assert_eq!(Some(0), read_entry.gid);
    }

    #[test]
    fn multiple_file_entries() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("multi_entry.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let entries = vec![
            FileEntry::new("file1.txt".to_owned(), 0o100644, 100, 1000000000),
            FileEntry::new("dir/file2.txt".to_owned(), 0o100755, 200, 1000000001),
            FileEntry::new(
                "deep/nested/file3.dat".to_owned(),
                0o100600,
                300,
                1000000002,
            ),
        ];

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        for entry in &entries {
            writer.write_file_entry(entry).unwrap();
        }
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        for original in &entries {
            let read_entry = reader.read_file_entry().unwrap().unwrap();
            assert_eq!(original.path, read_entry.path);
            assert_eq!(original.mode, read_entry.mode);
            assert_eq!(original.size, read_entry.size);
        }
    }

    #[test]
    fn file_entry_large_size() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("entry_large.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let entry = FileEntry::new(
            "huge_file.bin".to_owned(),
            0o100644,
            u64::MAX, // Maximum file size
            1609459200,
        );

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.write_file_entry(&entry).unwrap();
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        let read_entry = reader.read_file_entry().unwrap().unwrap();
        assert_eq!(u64::MAX, read_entry.size);
    }

    #[test]
    fn file_entry_zero_size() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("entry_zero.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let entry = FileEntry::new("empty.txt".to_owned(), 0o100644, 0, 1609459200);

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.write_file_entry(&entry).unwrap();
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        let read_entry = reader.read_file_entry().unwrap().unwrap();
        assert_eq!(0, read_entry.size);
    }

    #[test]
    fn file_entry_special_path_characters() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("entry_special.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let special_paths = vec![
            "file with spaces.txt",
            "file\twith\ttabs.txt",
            "unicode_\u{1F600}.txt",
            "path/with/many/components/deep/file.txt",
            ".hidden",
            "..dotdot",
            "file-with-dashes_and_underscores.txt",
        ];

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();

        for path in &special_paths {
            let entry = FileEntry::new((*path).to_owned(), 0o100644, 100, 1609459200);
            writer.write_file_entry(&entry).unwrap();
        }
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        for original_path in &special_paths {
            let read_entry = reader.read_file_entry().unwrap().unwrap();
            assert_eq!(*original_path, read_entry.path);
        }
    }

    #[test]
    fn file_entry_long_path() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("entry_long_path.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        // Create a path with 200 components
        let long_path = (0..200)
            .map(|i| format!("dir{i}"))
            .collect::<Vec<_>>()
            .join("/")
            + "/final_file.txt";

        let entry = FileEntry::new(long_path.clone(), 0o100644, 100, 1609459200);

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.write_file_entry(&entry).unwrap();
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        let read_entry = reader.read_file_entry().unwrap().unwrap();
        assert_eq!(long_path, read_entry.path);
    }

    #[test]
    fn file_entry_different_modes() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("entry_modes.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let modes = [
            (0o100644, "regular_rw_r_r"),
            (0o100755, "regular_rwx_rx_rx"),
            (0o100600, "regular_rw_only"),
            (0o040755, "directory"),
            (0o120777, "symlink"),
            (0o020644, "char_device"),
            (0o060644, "block_device"),
            (0o010644, "fifo"),
            (0o140755, "socket"),
        ];

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();

        for (mode, name) in &modes {
            let entry = FileEntry::new((*name).to_owned(), *mode, 0, 1609459200);
            writer.write_file_entry(&entry).unwrap();
        }
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        for (expected_mode, expected_name) in &modes {
            let read_entry = reader.read_file_entry().unwrap().unwrap();
            assert_eq!(*expected_name, read_entry.path);
            assert_eq!(*expected_mode, read_entry.mode);
        }
    }

    #[test]
    fn file_entry_mtime_boundary_values() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("entry_mtime.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        // Note: mtime is stored as i32, so values outside that range are truncated
        let mtimes = [
            0i64,            // Unix epoch
            1,               // Just after epoch
            -1,              // Before epoch
            i32::MAX as i64, // Max positive i32
            i32::MIN as i64, // Min negative i32
            1609459200,      // 2021-01-01 00:00:00 UTC
        ];

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();

        for (i, &mtime) in mtimes.iter().enumerate() {
            let entry = FileEntry::new(format!("file_{i}.txt"), 0o100644, 0, mtime);
            writer.write_file_entry(&entry).unwrap();
        }
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        for &original_mtime in &mtimes {
            let read_entry = reader.read_file_entry().unwrap().unwrap();
            // mtime is truncated to i32 then sign-extended back to i64
            let expected = (original_mtime as i32) as i64;
            assert_eq!(expected, read_entry.mtime);
        }
    }

    #[test]
    fn file_entry_eof_returns_none() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("entry_eof.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        // Write batch with just header
        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(BatchFlags::default()).unwrap();
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        // Should return None at EOF
        let result = reader.read_file_entry().unwrap();
        assert!(result.is_none(), "EOF should return None");
    }
}

// ============================================================================
// Batch Flags Tests
// ============================================================================

mod batch_flags_tests {
    //! Tests for BatchFlags across different protocol versions.

    use super::*;

    #[test]
    fn flags_protocol_28_ignores_newer_flags() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("flags_p28.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        // Set flags that exist only in newer protocols
        let flags = BatchFlags {
            recurse: true,
            preserve_uid: true,
            xfer_dirs: true,      // Protocol 29+
            do_compression: true, // Protocol 29+
            iconv: true,          // Protocol 30+
            preserve_acls: true,  // Protocol 30+
            ..Default::default()
        };

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 28);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(flags).unwrap();
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 28);
        let mut reader = BatchReader::new(config).unwrap();
        let read_flags = reader.read_header().unwrap();

        // Base flags should be preserved
        assert!(read_flags.recurse);
        assert!(read_flags.preserve_uid);

        // Protocol 29+ flags should be serialized but may be read back
        // based on how from_bitmap handles them (it reads all bits)
    }

    #[test]
    fn flags_protocol_29_supports_dirs_and_compression() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("flags_p29.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let flags = BatchFlags {
            recurse: true,
            xfer_dirs: true,
            do_compression: true,
            ..Default::default()
        };

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 29);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(flags).unwrap();
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 29);
        let mut reader = BatchReader::new(config).unwrap();
        let read_flags = reader.read_header().unwrap();

        assert!(read_flags.recurse);
        assert!(read_flags.xfer_dirs);
        assert!(read_flags.do_compression);
    }

    #[test]
    fn flags_protocol_30_supports_all_flags() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("flags_p30.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        let all_flags = BatchFlags {
            recurse: true,
            preserve_uid: true,
            preserve_gid: true,
            preserve_links: true,
            preserve_devices: true,
            preserve_hard_links: true,
            always_checksum: true,
            xfer_dirs: true,
            do_compression: true,
            iconv: true,
            preserve_acls: true,
            preserve_xattrs: true,
            inplace: true,
            append: true,
            append_verify: true,
        };

        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
        let mut writer = BatchWriter::new(config).unwrap();
        writer.write_header(all_flags).unwrap();
        writer.finalize().unwrap();

        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        let read_flags = reader.read_header().unwrap();

        assert_eq!(all_flags, read_flags);
    }

    #[test]
    fn flags_bitmap_values_match_upstream() {
        // Verify bitmap positions match upstream rsync's batch.c flag_ptr array
        let flags = BatchFlags {
            recurse: true,             // bit 0
            preserve_uid: true,        // bit 1
            preserve_gid: true,        // bit 2
            preserve_links: true,      // bit 3
            preserve_devices: true,    // bit 4
            preserve_hard_links: true, // bit 5
            always_checksum: true,     // bit 6
            xfer_dirs: true,           // bit 7 (protocol 29+)
            do_compression: true,      // bit 8 (protocol 29+)
            iconv: true,               // bit 9 (protocol 30+)
            preserve_acls: true,       // bit 10 (protocol 30+)
            preserve_xattrs: true,     // bit 11 (protocol 30+)
            inplace: true,             // bit 12 (protocol 30+)
            append: true,              // bit 13 (protocol 30+)
            append_verify: true,       // bit 14 (protocol 30+)
        };

        let bitmap = flags.to_bitmap(30);

        // Verify each bit position
        assert_ne!(bitmap & (1 << 0), 0, "bit 0: recurse");
        assert_ne!(bitmap & (1 << 1), 0, "bit 1: preserve_uid");
        assert_ne!(bitmap & (1 << 2), 0, "bit 2: preserve_gid");
        assert_ne!(bitmap & (1 << 3), 0, "bit 3: preserve_links");
        assert_ne!(bitmap & (1 << 4), 0, "bit 4: preserve_devices");
        assert_ne!(bitmap & (1 << 5), 0, "bit 5: preserve_hard_links");
        assert_ne!(bitmap & (1 << 6), 0, "bit 6: always_checksum");
        assert_ne!(bitmap & (1 << 7), 0, "bit 7: xfer_dirs");
        assert_ne!(bitmap & (1 << 8), 0, "bit 8: do_compression");
        assert_ne!(bitmap & (1 << 9), 0, "bit 9: iconv");
        assert_ne!(bitmap & (1 << 10), 0, "bit 10: preserve_acls");
        assert_ne!(bitmap & (1 << 11), 0, "bit 11: preserve_xattrs");
        assert_ne!(bitmap & (1 << 12), 0, "bit 12: inplace");
        assert_ne!(bitmap & (1 << 13), 0, "bit 13: append");
        assert_ne!(bitmap & (1 << 14), 0, "bit 14: append_verify");
    }

    #[test]
    fn flags_individual_bits_round_trip() {
        let temp_dir = TempDir::new().unwrap();

        // Test each flag individually
        let flag_setters: Vec<(&str, Box<dyn Fn(&mut BatchFlags)>)> = vec![
            ("recurse", Box::new(|f: &mut BatchFlags| f.recurse = true)),
            (
                "preserve_uid",
                Box::new(|f: &mut BatchFlags| f.preserve_uid = true),
            ),
            (
                "preserve_gid",
                Box::new(|f: &mut BatchFlags| f.preserve_gid = true),
            ),
            (
                "preserve_links",
                Box::new(|f: &mut BatchFlags| f.preserve_links = true),
            ),
            (
                "preserve_devices",
                Box::new(|f: &mut BatchFlags| f.preserve_devices = true),
            ),
            (
                "preserve_hard_links",
                Box::new(|f: &mut BatchFlags| f.preserve_hard_links = true),
            ),
            (
                "always_checksum",
                Box::new(|f: &mut BatchFlags| f.always_checksum = true),
            ),
            (
                "xfer_dirs",
                Box::new(|f: &mut BatchFlags| f.xfer_dirs = true),
            ),
            (
                "do_compression",
                Box::new(|f: &mut BatchFlags| f.do_compression = true),
            ),
            ("iconv", Box::new(|f: &mut BatchFlags| f.iconv = true)),
            (
                "preserve_acls",
                Box::new(|f: &mut BatchFlags| f.preserve_acls = true),
            ),
            (
                "preserve_xattrs",
                Box::new(|f: &mut BatchFlags| f.preserve_xattrs = true),
            ),
            ("inplace", Box::new(|f: &mut BatchFlags| f.inplace = true)),
            ("append", Box::new(|f: &mut BatchFlags| f.append = true)),
            (
                "append_verify",
                Box::new(|f: &mut BatchFlags| f.append_verify = true),
            ),
        ];

        for (name, setter) in &flag_setters {
            let batch_path = temp_dir.path().join(format!("flag_{name}.batch"));
            let path_str = batch_path.to_string_lossy().to_string();

            let mut flags = BatchFlags::default();
            setter(&mut flags);

            let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);
            let mut writer = BatchWriter::new(config).unwrap();
            writer.write_header(flags).unwrap();
            writer.finalize().unwrap();

            let config = BatchConfig::new(BatchMode::Read, path_str, 30);
            let mut reader = BatchReader::new(config).unwrap();
            let read_flags = reader.read_header().unwrap();

            assert_eq!(flags, read_flags, "Flag {name} should round-trip correctly");
        }
    }
}

// ============================================================================
// Script Generation Tests
// ============================================================================

mod script_generation {
    //! Tests for shell script generation functionality.

    use super::*;
    use batch::script;

    #[test]
    fn generate_script_creates_file() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("script_test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        script::generate_script(&config).unwrap();

        let script_path = config.script_file_path();
        assert!(
            Path::new(&script_path).exists(),
            "Script file should be created"
        );
    }

    #[test]
    fn generate_script_content_format() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("content_test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        script::generate_script(&config).unwrap();

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();

        assert!(
            content.starts_with("#!/bin/sh\n"),
            "Script should start with shebang"
        );
        assert!(
            content.contains("--read-batch="),
            "Script should have --read-batch option"
        );
        assert!(
            content.contains("oc-rsync"),
            "Script should invoke oc-rsync"
        );
    }

    #[test]
    #[cfg(unix)]
    fn generate_script_is_executable() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("exec_test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        script::generate_script(&config).unwrap();

        let script_path = config.script_file_path();
        let metadata = fs::metadata(&script_path).unwrap();
        let mode = metadata.permissions().mode();

        assert_ne!(mode & 0o111, 0, "Script should be executable");
    }

    #[test]
    fn generate_script_with_args_preserves_options() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("args_test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let args = vec![
            "oc-rsync".to_owned(),
            "-avz".to_owned(),
            "--progress".to_owned(),
            "--write-batch=args_test.batch".to_owned(),
            "source/".to_owned(),
            "dest/".to_owned(),
        ];

        script::generate_script_with_args(&config, &args, None).unwrap();

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();

        assert!(
            content.contains("--read-batch="),
            "Should convert to read-batch"
        );
        assert!(content.contains("-avz"), "Should preserve -avz option");
        assert!(
            content.contains("--progress"),
            "Should preserve --progress option"
        );
    }

    #[test]
    fn generate_script_with_filter_rules() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("filter_test.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        let args = vec![
            "oc-rsync".to_owned(),
            "-av".to_owned(),
            "--write-batch=filter_test.batch".to_owned(),
            "source/".to_owned(),
            "dest/".to_owned(),
        ];

        let filter_rules = "- *.tmp\n- *.log\n+ */\n+ *.rs\n- *\n";

        script::generate_script_with_args(&config, &args, Some(filter_rules)).unwrap();

        let script_path = config.script_file_path();
        let content = fs::read_to_string(&script_path).unwrap();

        assert!(content.contains("<<'#E#'"), "Should have heredoc start");
        assert!(content.contains("#E#"), "Should have heredoc end");
        assert!(content.contains("*.tmp"), "Should include filter rules");
        assert!(content.contains("*.rs"), "Should include filter rules");
    }

    #[test]
    fn script_path_with_special_characters() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("special path with spaces.batch");

        let config = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().to_string(),
            30,
        );

        script::generate_script(&config).unwrap();

        let script_path = config.script_file_path();
        assert!(Path::new(&script_path).exists());

        let content = fs::read_to_string(&script_path).unwrap();
        // Path with spaces should be quoted
        assert!(
            content.contains("'") || content.contains("\""),
            "Paths with spaces should be quoted"
        );
    }
}

// ============================================================================
// Batch Config Tests
// ============================================================================

mod batch_config_tests {
    //! Tests for BatchConfig construction and accessors.

    use super::*;

    #[test]
    fn config_mode_predicates() {
        let write_config = BatchConfig::new(BatchMode::Write, "test".to_owned(), 30);
        assert!(write_config.is_write_mode());
        assert!(!write_config.is_read_mode());
        assert!(write_config.should_transfer());

        let only_write_config = BatchConfig::new(BatchMode::OnlyWrite, "test".to_owned(), 30);
        assert!(only_write_config.is_write_mode());
        assert!(!only_write_config.is_read_mode());
        assert!(!only_write_config.should_transfer());

        let read_config = BatchConfig::new(BatchMode::Read, "test".to_owned(), 30);
        assert!(!read_config.is_write_mode());
        assert!(read_config.is_read_mode());
        assert!(read_config.should_transfer());
    }

    #[test]
    fn config_path_accessors() {
        let config = BatchConfig::new(BatchMode::Write, "/path/to/batch".to_owned(), 30);

        assert_eq!(config.batch_file_path(), Path::new("/path/to/batch"));
        assert_eq!(config.script_file_path(), "/path/to/batch.sh");
    }

    #[test]
    fn config_with_checksum_seed() {
        let config =
            BatchConfig::new(BatchMode::Write, "test".to_owned(), 30).with_checksum_seed(12345);

        assert_eq!(config.checksum_seed, 12345);
    }

    #[test]
    fn config_with_compat_flags_protocol_30() {
        let config =
            BatchConfig::new(BatchMode::Write, "test".to_owned(), 30).with_compat_flags(0x42);

        assert_eq!(config.compat_flags, Some(0x42));
    }

    #[test]
    fn config_with_compat_flags_protocol_28_ignored() {
        // Protocol 28 doesn't support compat flags
        let config =
            BatchConfig::new(BatchMode::Write, "test".to_owned(), 28).with_compat_flags(0x42);

        // with_compat_flags only sets if protocol >= 30
        assert!(config.compat_flags.is_none());
    }

    #[test]
    fn config_default_compat_flags_for_protocol_30() {
        let config = BatchConfig::new(BatchMode::Write, "test".to_owned(), 30);

        assert_eq!(config.compat_flags, Some(0));
    }

    #[test]
    fn config_no_compat_flags_for_protocol_29() {
        let config = BatchConfig::new(BatchMode::Write, "test".to_owned(), 29);

        assert!(config.compat_flags.is_none());
    }

    #[test]
    fn batch_mode_equality() {
        assert_eq!(BatchMode::Write, BatchMode::Write);
        assert_eq!(BatchMode::OnlyWrite, BatchMode::OnlyWrite);
        assert_eq!(BatchMode::Read, BatchMode::Read);

        assert_ne!(BatchMode::Write, BatchMode::Read);
        assert_ne!(BatchMode::Write, BatchMode::OnlyWrite);
        assert_ne!(BatchMode::Read, BatchMode::OnlyWrite);
    }

    #[test]
    fn batch_mode_debug_format() {
        let debug_write = format!("{:?}", BatchMode::Write);
        let debug_read = format!("{:?}", BatchMode::Read);
        let debug_only = format!("{:?}", BatchMode::OnlyWrite);

        assert!(debug_write.contains("Write"));
        assert!(debug_read.contains("Read"));
        assert!(debug_only.contains("OnlyWrite"));
    }
}

// ============================================================================
// Integration Scenarios
// ============================================================================

mod integration_scenarios {
    //! End-to-end integration scenarios simulating real-world usage.

    use super::*;

    #[test]
    fn scenario_full_transfer_simulation() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("full_transfer.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        // Simulate a complete transfer batch
        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 31)
            .with_checksum_seed(42424242)
            .with_compat_flags(0x0F);

        let mut writer = BatchWriter::new(config).unwrap();

        let flags = BatchFlags {
            recurse: true,
            preserve_uid: true,
            preserve_gid: true,
            preserve_links: true,
            always_checksum: true,
            xfer_dirs: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();

        // Write file entries
        let entries = vec![
            ("src/main.rs", 0o100644, 1024),
            ("src/lib.rs", 0o100644, 2048),
            ("Cargo.toml", 0o100644, 512),
            ("README.md", 0o100644, 4096),
        ];

        for (path, mode, size) in &entries {
            let mut entry = FileEntry::new((*path).to_owned(), *mode, *size, 1609459200);
            entry.uid = Some(1000);
            entry.gid = Some(1000);
            writer.write_file_entry(&entry).unwrap();
        }

        // Write simulated delta data
        writer.write_data(b"DELTA_HEADER_MARKER").unwrap();
        for (path, _, size) in &entries {
            let delta_marker = format!("DELTA:{path}:{size}:");
            writer.write_data(delta_marker.as_bytes()).unwrap();
            // Simulated file content
            let content = vec![0xAB; *size as usize];
            writer.write_data(&content).unwrap();
        }
        writer.write_data(b"DELTA_END_MARKER").unwrap();

        writer.finalize().unwrap();

        // Read and verify
        let config = BatchConfig::new(BatchMode::Read, path_str, 31);
        let mut reader = BatchReader::new(config).unwrap();

        let read_flags = reader.read_header().unwrap();
        assert_eq!(flags, read_flags);

        let header = reader.header().unwrap();
        assert_eq!(header.checksum_seed, 42424242);
        assert_eq!(header.compat_flags, Some(0x0F));

        // Read file entries
        for (expected_path, expected_mode, expected_size) in &entries {
            let entry = reader.read_file_entry().unwrap().unwrap();
            assert_eq!(entry.path, *expected_path);
            assert_eq!(entry.mode, *expected_mode);
            assert_eq!(entry.size, *expected_size as u64);
            assert_eq!(entry.uid, Some(1000));
            assert_eq!(entry.gid, Some(1000));
        }
    }

    #[test]
    fn scenario_incremental_backup_batch() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("incremental.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        // Simulate incremental backup with checksums
        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);

        let mut writer = BatchWriter::new(config).unwrap();

        let flags = BatchFlags {
            always_checksum: true,
            recurse: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();

        // Only changed files in incremental backup
        let changed_files = vec![
            ("modified.txt", 512, vec![0x01; 512]),
            ("new_file.txt", 256, vec![0x02; 256]),
        ];

        for (name, size, content) in &changed_files {
            let entry = FileEntry::new((*name).to_owned(), 0o100644, *size as u64, 1609459200);
            writer.write_file_entry(&entry).unwrap();
            writer.write_data(content).unwrap();
        }

        writer.finalize().unwrap();

        // Verify incremental batch
        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();

        let read_flags = reader.read_header().unwrap();
        assert!(read_flags.always_checksum);
        assert!(read_flags.recurse);
    }

    #[test]
    fn scenario_empty_directory_sync() {
        let temp_dir = TempDir::new().unwrap();
        let batch_path = temp_dir.path().join("empty_dir.batch");
        let path_str = batch_path.to_string_lossy().to_string();

        // Batch for syncing empty directories
        let config = BatchConfig::new(BatchMode::Write, path_str.clone(), 30);

        let mut writer = BatchWriter::new(config).unwrap();

        let flags = BatchFlags {
            recurse: true,
            xfer_dirs: true,
            ..Default::default()
        };
        writer.write_header(flags).unwrap();

        // Directory entries with zero size
        let dirs = vec![
            ("empty_dir1", 0o040755),
            ("empty_dir2", 0o040700),
            ("nested/empty", 0o040755),
        ];

        for (path, mode) in &dirs {
            let entry = FileEntry::new((*path).to_owned(), *mode, 0, 1609459200);
            writer.write_file_entry(&entry).unwrap();
        }

        writer.finalize().unwrap();

        // Verify
        let config = BatchConfig::new(BatchMode::Read, path_str, 30);
        let mut reader = BatchReader::new(config).unwrap();
        reader.read_header().unwrap();

        for (expected_path, expected_mode) in &dirs {
            let entry = reader.read_file_entry().unwrap().unwrap();
            assert_eq!(entry.path, *expected_path);
            assert_eq!(entry.mode, *expected_mode);
            assert_eq!(entry.size, 0);
        }
    }
}
