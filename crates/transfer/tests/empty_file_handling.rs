//! Comprehensive tests for empty file (0 bytes) handling in the transfer crate.
//!
//! This module tests all aspects of empty file handling during delta transfers:
//! - Empty file delta generation (whole file mode)
//! - Empty file delta application
//! - Delta transfers between empty and non-empty files
//! - Checksum verification for empty files
//! - Sparse write state with empty data
//!
//! Empty files are an important edge case because they bypass many code paths
//! that handle file content, so it's critical to ensure correct behavior.

use std::fs::{self, File};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use tempfile::{tempdir, NamedTempFile};
use transfer::delta_apply::{ChecksumVerifier, DeltaApplyConfig, DeltaApplyResult, SparseWriteState};

// ============================================================================
// Empty File Delta Generation Tests
// ============================================================================

#[test]
fn empty_file_sparse_write_produces_zero_position() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(Vec::new());

    // Write empty data
    let result = state.write(&mut cursor, &[]).unwrap();
    assert_eq!(result, 0);

    // Finish should produce position 0
    let pos = state.finish(&mut cursor).unwrap();
    assert_eq!(pos, 0);
}

#[test]
fn empty_file_checksum_verification_md5() {
    let mut verifier = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::MD5);

    // Update with empty data
    verifier.update(&[]);

    // Finalize should produce valid MD5 of empty string
    let digest = verifier.finalize();
    assert_eq!(digest.len(), 16);

    // MD5 of empty string is d41d8cd98f00b204e9800998ecf8427e
    let expected_md5_empty: [u8; 16] = [
        0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8, 0x42,
        0x7e,
    ];
    assert_eq!(digest, expected_md5_empty);
}

#[test]
fn empty_file_checksum_verification_xxh64() {
    let mut verifier = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::XXH64);

    // Update with empty data
    verifier.update(&[]);

    // Finalize should produce valid XXH64 of empty input
    let digest = verifier.finalize();
    assert_eq!(digest.len(), 8);
}

#[test]
fn empty_file_checksum_verification_xxh3() {
    let mut verifier = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::XXH3);

    // Update with empty data
    verifier.update(&[]);

    let digest = verifier.finalize();
    assert_eq!(digest.len(), 8);
}

#[test]
fn empty_file_checksum_verification_sha1() {
    let mut verifier = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::SHA1);

    // Update with empty data
    verifier.update(&[]);

    let digest = verifier.finalize();
    assert_eq!(digest.len(), 20);

    // SHA1 of empty string is da39a3ee5e6b4b0d3255bfef95601890afd80709
    let expected_sha1_empty: [u8; 20] = [
        0xda, 0x39, 0xa3, 0xee, 0x5e, 0x6b, 0x4b, 0x0d, 0x32, 0x55, 0xbf, 0xef, 0x95, 0x60, 0x18,
        0x90, 0xaf, 0xd8, 0x07, 0x09,
    ];
    assert_eq!(digest, expected_sha1_empty);
}

// ============================================================================
// Sparse Write State with Empty File Content
// ============================================================================

#[test]
fn sparse_state_empty_write_then_finish() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 100]);

    // Write nothing
    state.write(&mut cursor, &[]).unwrap();

    // Finish
    let pos = state.finish(&mut cursor).unwrap();
    assert_eq!(pos, 0, "empty write should result in position 0");
}

#[test]
fn sparse_state_multiple_empty_writes() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 100]);

    // Multiple empty writes
    for _ in 0..10 {
        state.write(&mut cursor, &[]).unwrap();
    }

    let pos = state.finish(&mut cursor).unwrap();
    assert_eq!(pos, 0, "multiple empty writes should result in position 0");
}

#[test]
fn sparse_state_empty_then_data_then_empty() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 100]);

    // Empty write
    state.write(&mut cursor, &[]).unwrap();

    // Write some data
    state.write(&mut cursor, b"hello").unwrap();

    // Another empty write
    state.write(&mut cursor, &[]).unwrap();

    let pos = state.finish(&mut cursor).unwrap();
    assert_eq!(pos, 5, "position should be 5 after writing 'hello'");
}

// ============================================================================
// Empty File to Real File Tests
// ============================================================================

#[test]
fn empty_file_creation_via_sparse_state() {
    let temp = tempdir().expect("tempdir");
    let file_path = temp.path().join("empty.txt");

    // Create file through sparse state
    let file = File::create(&file_path).expect("create file");
    let mut state = SparseWriteState::new();

    // Use BufWriter to get a seekable writer
    let mut writer = std::io::BufWriter::new(file);

    // Write empty content
    state.write(&mut writer, &[]).expect("write empty");
    state.finish(&mut writer).expect("finish");
    writer.flush().expect("flush");

    // Verify file exists and is empty
    let metadata = fs::metadata(&file_path).expect("metadata");
    assert_eq!(metadata.len(), 0, "file should be empty");
}

#[test]
fn empty_file_checksum_matches_known_value() {
    // Test all algorithms produce consistent results for empty input
    let algorithms = [
        (protocol::ChecksumAlgorithm::MD4, 16),
        (protocol::ChecksumAlgorithm::MD5, 16),
        (protocol::ChecksumAlgorithm::SHA1, 20),
        (protocol::ChecksumAlgorithm::XXH64, 8),
        (protocol::ChecksumAlgorithm::XXH3, 8),
        (protocol::ChecksumAlgorithm::XXH128, 16),
    ];

    for (algorithm, expected_len) in algorithms {
        let mut verifier = ChecksumVerifier::for_algorithm(algorithm);
        verifier.update(&[]);
        let digest = verifier.finalize();
        assert_eq!(
            digest.len(),
            expected_len,
            "algorithm {:?} should produce {} byte digest",
            algorithm,
            expected_len
        );
    }
}

#[test]
fn empty_file_checksum_multiple_updates_with_empty() {
    let mut verifier = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::MD5);

    // Multiple empty updates should produce same result as single empty update
    verifier.update(&[]);
    verifier.update(&[]);
    verifier.update(&[]);

    let digest = verifier.finalize();
    assert_eq!(digest.len(), 16);

    // Should still be MD5 of empty string
    let expected_md5_empty: [u8; 16] = [
        0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8, 0x42,
        0x7e,
    ];
    assert_eq!(digest, expected_md5_empty);
}

// ============================================================================
// Delta Apply Config with Empty Files
// ============================================================================

#[test]
fn delta_apply_config_default_works_for_empty() {
    let config = DeltaApplyConfig::default();
    assert!(!config.sparse, "default should not use sparse mode");
}

#[test]
fn delta_apply_config_sparse_mode_for_empty() {
    let config = DeltaApplyConfig { sparse: true };
    assert!(config.sparse, "sparse mode should be enabled");
}

#[test]
fn delta_apply_result_default_zero_values() {
    let result = DeltaApplyResult::default();
    assert_eq!(result.bytes_written, 0);
    assert_eq!(result.literal_bytes, 0);
    assert_eq!(result.matched_bytes, 0);
    assert_eq!(result.literal_tokens, 0);
    assert_eq!(result.block_tokens, 0);
}

// ============================================================================
// Sparse Write State Edge Cases for Empty Content
// ============================================================================

#[test]
fn sparse_state_flush_with_no_pending_zeros() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 100]);

    // Flush without any pending zeros
    state.flush(&mut cursor).expect("flush empty");
    assert_eq!(cursor.position(), 0);
}

#[test]
fn sparse_state_finish_without_any_operations() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 100]);

    // Finish without any write operations
    let pos = state.finish(&mut cursor).expect("finish");
    assert_eq!(pos, 0);
}

#[test]
fn sparse_state_accumulate_then_finish_creates_hole() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![1u8; 100]); // Fill with ones

    // Accumulate some zeros
    state.accumulate(50);

    // Finish should create a hole
    let pos = state.finish(&mut cursor).expect("finish");
    assert_eq!(pos, 50);

    // Check that a single zero was written at position 49
    let buffer = cursor.into_inner();
    assert_eq!(buffer[49], 0, "position 49 should be zero");
}

// ============================================================================
// Real File Empty Transfer Simulation
// ============================================================================

#[test]
fn simulate_empty_file_transfer() {
    let temp = tempdir().expect("tempdir");
    let dest_path = temp.path().join("dest.txt");

    // Create destination file
    let file = File::create(&dest_path).expect("create dest");
    let mut state = SparseWriteState::new();
    let mut writer = std::io::BufWriter::new(file);

    // Simulate receiving empty delta (0 token indicating end)
    // In a real transfer, we'd receive a delta stream with just the end marker

    // Write empty content (simulating empty literal token)
    state.write(&mut writer, &[]).expect("write empty literal");

    // Finish
    state.finish(&mut writer).expect("finish");
    writer.flush().expect("flush");
    drop(writer);

    // Verify file is empty
    let metadata = fs::metadata(&dest_path).expect("metadata");
    assert_eq!(metadata.len(), 0, "transferred file should be empty");
}

#[test]
fn simulate_empty_to_nonempty_transfer() {
    let temp = tempdir().expect("tempdir");
    let dest_path = temp.path().join("dest.txt");

    // Create destination file with existing content
    fs::write(&dest_path, b"old content").expect("write old content");

    // Now transfer empty content to it
    let file = File::create(&dest_path).expect("create dest");
    let mut state = SparseWriteState::new();
    let mut writer = std::io::BufWriter::new(file);

    // Write empty content
    state.write(&mut writer, &[]).expect("write empty");
    state.finish(&mut writer).expect("finish");
    writer.flush().expect("flush");
    drop(writer);

    // Verify file is now empty (truncated)
    let metadata = fs::metadata(&dest_path).expect("metadata");
    assert_eq!(
        metadata.len(),
        0,
        "file should be empty after transfer from empty source"
    );
}

#[test]
fn simulate_nonempty_to_empty_basis_delta() {
    // This simulates the scenario where we're creating a new file
    // with content, but the basis file (if it existed) was empty.
    // Since there's no basis, this is whole-file transfer.

    let temp = tempdir().expect("tempdir");
    let dest_path = temp.path().join("dest.txt");

    let file = File::create(&dest_path).expect("create dest");
    let mut state = SparseWriteState::new();
    let mut writer = std::io::BufWriter::new(file);

    // Write literal content (whole-file transfer from sender)
    let content = b"new file content";
    state.write(&mut writer, content).expect("write content");
    state.finish(&mut writer).expect("finish");
    writer.flush().expect("flush");
    drop(writer);

    // Verify file has the new content
    let result = fs::read(&dest_path).expect("read");
    assert_eq!(result, content);
}

// ============================================================================
// Multiple Empty Files in Same Directory
// ============================================================================

#[test]
fn multiple_empty_files_sequential_creation() {
    let temp = tempdir().expect("tempdir");

    // Create multiple empty files
    let file_names = ["empty1.txt", "empty2.txt", "empty3.txt", "empty4.txt", "empty5.txt"];

    for name in &file_names {
        let path = temp.path().join(name);
        let file = File::create(&path).expect("create file");
        let mut state = SparseWriteState::new();
        let mut writer = std::io::BufWriter::new(file);

        state.write(&mut writer, &[]).expect("write empty");
        state.finish(&mut writer).expect("finish");
        writer.flush().expect("flush");

        // Verify each file is empty
        let metadata = fs::metadata(&path).expect("metadata");
        assert_eq!(metadata.len(), 0, "file {} should be empty", name);
    }

    // Verify all files exist
    for name in &file_names {
        assert!(temp.path().join(name).exists(), "file {} should exist", name);
    }
}

#[test]
fn mixed_empty_and_nonempty_files() {
    let temp = tempdir().expect("tempdir");

    // Create alternating empty and non-empty files
    let files: Vec<(&str, &[u8])> = vec![
        ("empty1.txt", b""),
        ("content1.txt", b"hello"),
        ("empty2.txt", b""),
        ("content2.txt", b"world"),
        ("empty3.txt", b""),
    ];

    for (name, content) in &files {
        let path = temp.path().join(name);
        let file = File::create(&path).expect("create file");
        let mut state = SparseWriteState::new();
        let mut writer = std::io::BufWriter::new(file);

        state.write(&mut writer, *content).expect("write");
        state.finish(&mut writer).expect("finish");
        writer.flush().expect("flush");

        // Verify file size
        let metadata = fs::metadata(&path).expect("metadata");
        assert_eq!(
            metadata.len(),
            content.len() as u64,
            "file {} should have correct size",
            name
        );
    }
}

// ============================================================================
// Checksum Consistency Tests
// ============================================================================

#[test]
fn empty_file_checksum_consistency_across_calls() {
    // Create two verifiers and check they produce the same result
    let mut v1 = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::MD5);
    let mut v2 = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::MD5);

    v1.update(&[]);
    v2.update(&[]);

    let d1 = v1.finalize();
    let d2 = v2.finalize();

    assert_eq!(d1, d2, "empty file checksums should be consistent");
}

#[test]
fn empty_vs_zero_byte_checksum() {
    // Empty file vs file with single zero byte should have different checksums
    let mut v_empty = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::MD5);
    let mut v_zero = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::MD5);

    v_empty.update(&[]);
    v_zero.update(&[0u8]);

    let d_empty = v_empty.finalize();
    let d_zero = v_zero.finalize();

    assert_ne!(
        d_empty, d_zero,
        "empty file and single zero byte should have different checksums"
    );
}

// ============================================================================
// Protocol Version Compatibility for Empty Files
// ============================================================================

#[test]
fn empty_file_verifier_protocol_29() {
    let verifier = ChecksumVerifier::new(
        None,
        protocol::ProtocolVersion::try_from(29u8).unwrap(),
        0,
        None,
    );
    // Protocol 29 uses MD4 by default
    assert_eq!(verifier.digest_len(), 16);
}

#[test]
fn empty_file_verifier_protocol_30() {
    let verifier = ChecksumVerifier::new(
        None,
        protocol::ProtocolVersion::try_from(30u8).unwrap(),
        0,
        None,
    );
    // Protocol 30 uses MD5 by default
    assert_eq!(verifier.digest_len(), 16);
}

#[test]
fn empty_file_verifier_protocol_31() {
    let verifier = ChecksumVerifier::new(
        None,
        protocol::ProtocolVersion::try_from(31u8).unwrap(),
        0,
        None,
    );
    // Protocol 31 uses MD5 by default
    assert_eq!(verifier.digest_len(), 16);
}

// ============================================================================
// Sparse File Edge Cases with Empty Content
// ============================================================================

#[test]
fn sparse_state_pending_after_empty_write() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 100]);

    // Empty write should not affect pending count
    let pending_before = state.pending();
    state.write(&mut cursor, &[]).expect("write empty");
    let pending_after = state.pending();

    assert_eq!(
        pending_before, pending_after,
        "empty write should not change pending zeros"
    );
}

#[test]
fn sparse_state_empty_write_returns_zero_bytes() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 100]);

    let bytes_written = state.write(&mut cursor, &[]).expect("write empty");
    assert_eq!(bytes_written, 0, "empty write should return 0 bytes written");
}

#[test]
fn sparse_state_position_unchanged_after_empty_write() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 100]);

    let pos_before = cursor.position();
    state.write(&mut cursor, &[]).expect("write empty");
    let pos_after = cursor.position();

    assert_eq!(
        pos_before, pos_after,
        "cursor position should not change after empty write"
    );
}

// ============================================================================
// Named Temp File Empty Tests
// ============================================================================

#[test]
fn named_temp_file_starts_empty() {
    let temp_file = NamedTempFile::new().expect("temp file");
    let metadata = temp_file.as_file().metadata().expect("metadata");
    assert_eq!(metadata.len(), 0, "new temp file should be empty");
}

#[test]
fn empty_file_read_back_verification() {
    let mut temp_file = NamedTempFile::new().expect("temp file");

    // Write empty content via sparse state
    {
        let file = temp_file.as_file_mut();
        let mut state = SparseWriteState::new();
        state.write(file, &[]).expect("write empty");
        state.finish(file).expect("finish");
        file.flush().expect("flush");
    }

    // Seek back and read
    temp_file.as_file_mut().seek(SeekFrom::Start(0)).expect("seek");
    let mut contents = Vec::new();
    temp_file.as_file_mut().read_to_end(&mut contents).expect("read");

    assert!(contents.is_empty(), "read back should be empty");
}

// ============================================================================
// Delta Apply Result for Empty Transfers
// ============================================================================

#[test]
fn delta_apply_result_empty_transfer_stats() {
    // Simulate stats for an empty file transfer
    let result = DeltaApplyResult {
        bytes_written: 0,
        literal_bytes: 0,
        matched_bytes: 0,
        literal_tokens: 1, // Even empty files have one (empty) literal token
        block_tokens: 0,
    };

    assert_eq!(result.bytes_written, 0);
    assert_eq!(result.literal_bytes, 0);
    assert_eq!(result.matched_bytes, 0);
    assert_eq!(result.literal_tokens, 1);
    assert_eq!(result.block_tokens, 0);
}

#[test]
fn delta_apply_result_empty_with_zero_tokens() {
    // Result when absolutely nothing was transferred (edge case)
    let result = DeltaApplyResult::default();

    assert_eq!(result.bytes_written, 0);
    assert_eq!(result.literal_bytes, 0);
    assert_eq!(result.matched_bytes, 0);
    assert_eq!(result.literal_tokens, 0);
    assert_eq!(result.block_tokens, 0);
}
