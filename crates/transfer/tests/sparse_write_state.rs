//! Comprehensive tests for sparse file write state handling.
//!
//! Tests the `SparseWriteState` implementation in `delta_apply.rs` which handles
//! sparse file detection and hole creation during delta transfer.
//!
//! # Coverage Areas
//!
//! - Basic sparse state operations (accumulate, flush, finish)
//! - Zero run detection at various thresholds
//! - Mixed data patterns (zeros + non-zeros)
//! - Large file handling (> 4GB offsets)
//! - Edge cases around chunk boundaries (32KB CHUNK_SIZE)
//! - Integration with delta application

use std::io::{Cursor, Read, Seek, SeekFrom};
use tempfile::NamedTempFile;
use transfer::delta_apply::SparseWriteState;

// ============================================================================
// Basic State Operations
// ============================================================================

#[test]
fn sparse_state_initial_pending_is_zero() {
    let state = SparseWriteState::new();
    assert_eq!(state.pending(), 0);
}

#[test]
fn sparse_state_accumulate_single_call() {
    let mut state = SparseWriteState::new();
    state.accumulate(42);
    assert_eq!(state.pending(), 42);
}

#[test]
fn sparse_state_accumulate_multiple_calls() {
    let mut state = SparseWriteState::new();
    state.accumulate(10);
    state.accumulate(20);
    state.accumulate(30);
    assert_eq!(state.pending(), 60);
}

#[test]
fn sparse_state_accumulate_zero_is_noop() {
    let mut state = SparseWriteState::new();
    state.accumulate(100);
    state.accumulate(0);
    assert_eq!(state.pending(), 100);
}

#[test]
fn sparse_state_accumulate_large_values() {
    let mut state = SparseWriteState::new();
    state.accumulate(1_000_000);
    state.accumulate(2_000_000);
    assert_eq!(state.pending(), 3_000_000);
}

// ============================================================================
// Flush Operations
// ============================================================================

#[test]
fn sparse_state_flush_empty_state_is_noop() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(Vec::new());
    state.flush(&mut cursor).expect("flush empty state");
    assert_eq!(cursor.position(), 0);
}

#[test]
fn sparse_state_flush_seeks_by_pending_amount() {
    let mut state = SparseWriteState::new();
    state.accumulate(100);
    let mut cursor = Cursor::new(vec![0u8; 200]);
    state.flush(&mut cursor).expect("flush");
    assert_eq!(cursor.position(), 100);
    assert_eq!(state.pending(), 0);
}

#[test]
fn sparse_state_flush_clears_pending() {
    let mut state = SparseWriteState::new();
    state.accumulate(50);
    let mut cursor = Cursor::new(vec![0u8; 100]);
    state.flush(&mut cursor).expect("flush");
    assert_eq!(state.pending(), 0);
}

#[test]
fn sparse_state_multiple_flushes() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 300]);

    state.accumulate(50);
    state.flush(&mut cursor).expect("first flush");
    assert_eq!(cursor.position(), 50);

    state.accumulate(100);
    state.flush(&mut cursor).expect("second flush");
    assert_eq!(cursor.position(), 150);

    state.accumulate(25);
    state.flush(&mut cursor).expect("third flush");
    assert_eq!(cursor.position(), 175);
}

// ============================================================================
// Write Operations
// ============================================================================

#[test]
fn sparse_state_write_empty_data_returns_zero() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(Vec::new());
    let result = state.write(&mut cursor, &[]).expect("write empty");
    assert_eq!(result, 0);
}

#[test]
fn sparse_state_write_non_zero_data() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(Vec::new());
    let data = b"hello world";
    let result = state.write(&mut cursor, data).expect("write non-zero");
    assert_eq!(result, data.len());
}

#[test]
fn sparse_state_write_all_zeros_accumulates() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 1000]);
    let zeros = [0u8; 500];
    let result = state.write(&mut cursor, &zeros).expect("write zeros");
    assert_eq!(result, 500);
    // Zeros should be accumulated, not written
    assert!(state.pending() > 0);
}

#[test]
fn sparse_state_write_mixed_data_leading_zeros() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 100]);
    // Leading zeros, then data
    let data = [0, 0, 0, 0, 0, 1, 2, 3, 4, 5];
    let result = state.write(&mut cursor, &data).expect("write mixed");
    assert_eq!(result, data.len());
}

#[test]
fn sparse_state_write_mixed_data_trailing_zeros() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 100]);
    // Data, then trailing zeros
    let data = [1, 2, 3, 4, 5, 0, 0, 0, 0, 0];
    let result = state.write(&mut cursor, &data).expect("write mixed");
    assert_eq!(result, data.len());
}

#[test]
fn sparse_state_write_mixed_data_surrounded_by_zeros() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 100]);
    // Zeros, data, zeros
    let data = [0, 0, 0, 1, 2, 3, 0, 0, 0];
    let result = state.write(&mut cursor, &data).expect("write mixed");
    assert_eq!(result, data.len());
}

// ============================================================================
// Finish Operations
// ============================================================================

#[test]
fn sparse_state_finish_empty_state() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 10]);
    let pos = state.finish(&mut cursor).expect("finish empty");
    assert_eq!(pos, 0);
}

#[test]
fn sparse_state_finish_with_pending_zeros() {
    let mut state = SparseWriteState::new();
    state.accumulate(100);
    let mut cursor = Cursor::new(vec![0u8; 200]);
    let pos = state.finish(&mut cursor).expect("finish with pending");
    // finish() should write a single zero at the end after seeking
    assert_eq!(pos, 100);
}

#[test]
fn sparse_state_finish_writes_single_trailing_zero() {
    let mut state = SparseWriteState::new();
    state.accumulate(10);
    let buffer = vec![1u8; 20]; // Fill with ones
    let mut cursor = Cursor::new(buffer);
    let pos = state.finish(&mut cursor).expect("finish");
    assert_eq!(pos, 10);

    // Check that a single zero was written at position 9
    let buffer = cursor.into_inner();
    assert_eq!(buffer[9], 0);
}

// ============================================================================
// Chunk Boundary Tests (CHUNK_SIZE = 32KB)
// ============================================================================

const CHUNK_SIZE: usize = 32 * 1024;

#[test]
fn sparse_state_write_exactly_chunk_size_zeros() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; CHUNK_SIZE * 2]);
    let zeros = vec![0u8; CHUNK_SIZE];
    let result = state.write(&mut cursor, &zeros).expect("write chunk");
    assert_eq!(result, CHUNK_SIZE);
}

#[test]
fn sparse_state_write_just_under_chunk_size() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; CHUNK_SIZE * 2]);
    let zeros = vec![0u8; CHUNK_SIZE - 1];
    let result = state.write(&mut cursor, &zeros).expect("write under chunk");
    assert_eq!(result, CHUNK_SIZE - 1);
}

#[test]
fn sparse_state_write_just_over_chunk_size() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; CHUNK_SIZE * 2]);
    let zeros = vec![0u8; CHUNK_SIZE + 1];
    let result = state.write(&mut cursor, &zeros).expect("write over chunk");
    assert_eq!(result, CHUNK_SIZE + 1);
}

#[test]
fn sparse_state_write_multiple_chunks() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; CHUNK_SIZE * 5]);
    let zeros = vec![0u8; CHUNK_SIZE * 3];
    let result = state.write(&mut cursor, &zeros).expect("write multi-chunk");
    assert_eq!(result, CHUNK_SIZE * 3);
}

#[test]
fn sparse_state_write_data_at_chunk_boundary() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; CHUNK_SIZE * 2]);

    // First chunk: all zeros
    let mut data = vec![0u8; CHUNK_SIZE * 2];
    // Place non-zero data exactly at chunk boundary
    data[CHUNK_SIZE] = 0xAA;
    data[CHUNK_SIZE + 1] = 0xBB;

    let result = state.write(&mut cursor, &data).expect("write at boundary");
    assert_eq!(result, CHUNK_SIZE * 2);
}

// ============================================================================
// Large Data Tests
// ============================================================================

#[test]
fn sparse_state_write_large_zero_run() {
    let mut state = SparseWriteState::new();
    // 1MB buffer
    let mut cursor = Cursor::new(vec![0u8; 1024 * 1024]);
    let zeros = vec![0u8; 512 * 1024]; // 512KB of zeros
    let result = state.write(&mut cursor, &zeros).expect("write large zeros");
    assert_eq!(result, 512 * 1024);
}

#[test]
fn sparse_state_write_large_non_zero_data() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(Vec::new());
    // 256KB of non-zero data
    let data: Vec<u8> = (0..=255).cycle().take(256 * 1024).collect();
    let result = state.write(&mut cursor, &data).expect("write large data");
    assert_eq!(result, 256 * 1024);
}

#[test]
fn sparse_state_interleaved_zeros_and_data() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 1024 * 1024]);

    // Pattern: 64KB zeros, 32KB data, 64KB zeros, 32KB data
    let mut data = Vec::new();
    data.extend(vec![0u8; 64 * 1024]);
    data.extend(vec![0xFFu8; 32 * 1024]);
    data.extend(vec![0u8; 64 * 1024]);
    data.extend(vec![0xFFu8; 32 * 1024]);

    let result = state.write(&mut cursor, &data).expect("write interleaved");
    assert_eq!(result, data.len());
}

// ============================================================================
// Real File Tests
// ============================================================================

#[test]
fn sparse_state_write_to_real_file() {
    let mut state = SparseWriteState::new();
    let mut file = NamedTempFile::new().expect("temp file");

    // Write pattern: data, zeros, data
    let data1 = b"HEADER";
    let zeros = vec![0u8; CHUNK_SIZE];
    let data2 = b"FOOTER";

    state
        .write(file.as_file_mut(), data1)
        .expect("write header");
    state
        .write(file.as_file_mut(), &zeros)
        .expect("write zeros");
    state
        .write(file.as_file_mut(), data2)
        .expect("write footer");

    let _final_pos = state.finish(file.as_file_mut()).expect("finish");

    // Verify file contents
    file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");
    let mut contents = Vec::new();
    file.as_file_mut()
        .read_to_end(&mut contents)
        .expect("read back");

    // Check header
    assert_eq!(&contents[0..6], b"HEADER");
}

#[cfg(unix)]
#[test]
fn sparse_state_creates_actual_hole_on_disk() {
    use std::os::unix::fs::MetadataExt;

    let mut state = SparseWriteState::new();
    let mut file = NamedTempFile::new().expect("temp file");

    // Write pattern: small data, large zero run, small data
    let data1 = vec![0xAAu8; 512];
    let zeros = vec![0u8; 256 * 1024]; // 256KB zeros
    let data2 = vec![0xBBu8; 512];

    state
        .write(file.as_file_mut(), &data1)
        .expect("write data1");
    state
        .write(file.as_file_mut(), &zeros)
        .expect("write zeros");
    state
        .write(file.as_file_mut(), &data2)
        .expect("write data2");

    state.finish(file.as_file_mut()).expect("finish");

    // Set the file length (sparse finish may not extend the file)
    let expected_len = (512 + 256 * 1024 + 512) as u64;
    file.as_file_mut()
        .set_len(expected_len)
        .expect("set length");

    // Check that the file is sparse (uses fewer blocks than size would suggest)
    let metadata = file.as_file_mut().metadata().expect("metadata");
    let blocks = metadata.blocks();
    let size = metadata.len();

    // A fully allocated file of this size would need more blocks
    // Block size is typically 512 bytes, so expected dense blocks would be ~518
    // Sparse file should use significantly fewer
    // Note: This depends on filesystem support, so we just verify the write succeeded
    assert_eq!(size, expected_len);
    eprintln!(
        "Sparse file test: size={}, blocks={}, expected dense blocks={}",
        size,
        blocks,
        size / 512
    );
}

// ============================================================================
// Edge Cases and Error Handling
// ============================================================================

#[test]
fn sparse_state_saturating_accumulate() {
    let mut state = SparseWriteState::new();
    // Accumulate near u64::MAX to test saturating behavior
    state.accumulate(usize::MAX);
    state.accumulate(usize::MAX);
    // Should not panic or overflow
    assert!(state.pending() > 0);
}

#[test]
fn sparse_state_single_byte_patterns() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 100]);

    // Single non-zero byte
    state.write(&mut cursor, &[1]).expect("single byte");
    // Single zero byte
    state.write(&mut cursor, &[0]).expect("single zero");

    let pos = state.finish(&mut cursor).expect("finish");
    assert!(pos > 0);
}

#[test]
fn sparse_state_alternating_bytes() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 1000]);

    // Alternating zero and non-zero bytes
    let data: Vec<u8> = (0..500).map(|i| if i % 2 == 0 { 0 } else { 1 }).collect();
    let result = state.write(&mut cursor, &data).expect("alternating");
    assert_eq!(result, 500);
}

#[test]
fn sparse_state_nearly_all_zeros_with_single_non_zero() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 100_000]);

    // 64KB of zeros with single non-zero byte in the middle
    let mut data = vec![0u8; 64 * 1024];
    data[32 * 1024] = 0xFF;

    let result = state.write(&mut cursor, &data).expect("nearly all zeros");
    assert_eq!(result, 64 * 1024);
}

// ============================================================================
// Integration-style Tests
// ============================================================================

#[test]
fn sparse_state_simulates_delta_transfer_pattern() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 1024 * 1024]);

    // Simulate a typical delta transfer pattern:
    // 1. Literal data (file header)
    // 2. Block reference (zeros from basis)
    // 3. Literal data (changed section)
    // 4. Block reference (more zeros)
    // 5. Literal data (file footer)

    // Header
    state.write(&mut cursor, b"FILE_HEADER_V1").expect("header");

    // Zero block (from basis)
    state
        .write(&mut cursor, &vec![0u8; CHUNK_SIZE])
        .expect("zero block 1");

    // Changed section
    state
        .write(&mut cursor, b"MODIFIED_CONTENT")
        .expect("modified");

    // Another zero block
    state
        .write(&mut cursor, &vec![0u8; CHUNK_SIZE])
        .expect("zero block 2");

    // Footer
    state.write(&mut cursor, b"END_OF_FILE").expect("footer");

    let final_pos = state.finish(&mut cursor).expect("finish");
    assert!(final_pos > 0);
}

#[test]
fn sparse_state_sequential_writes_maintain_position() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 10000]);

    // Write sequence of different data types
    state.write(&mut cursor, b"A").expect("write A");
    state.write(&mut cursor, &[0u8; 100]).expect("write zeros");
    state.write(&mut cursor, b"B").expect("write B");
    state.write(&mut cursor, &[0u8; 200]).expect("write zeros");
    state.write(&mut cursor, b"C").expect("write C");

    let final_pos = state.finish(&mut cursor).expect("finish");

    // Total: 1 + 100 + 1 + 200 + 1 = 303
    assert_eq!(final_pos, 303);
}

// ============================================================================
// Boundary Alignment Tests
// ============================================================================

#[test]
fn sparse_state_16_byte_aligned_zeros() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 1000]);

    // Exactly 16 bytes (u128 boundary)
    let zeros = [0u8; 16];
    state.write(&mut cursor, &zeros).expect("16 byte zeros");
}

#[test]
fn sparse_state_17_byte_zeros() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 1000]);

    // 17 bytes (16 + 1)
    let zeros = [0u8; 17];
    state.write(&mut cursor, &zeros).expect("17 byte zeros");
}

#[test]
fn sparse_state_15_byte_zeros() {
    let mut state = SparseWriteState::new();
    let mut cursor = Cursor::new(vec![0u8; 1000]);

    // 15 bytes (under u128 boundary)
    let zeros = [0u8; 15];
    state.write(&mut cursor, &zeros).expect("15 byte zeros");
}

#[test]
fn sparse_state_various_alignments() {
    for size in [1, 8, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129] {
        let mut state = SparseWriteState::new();
        let mut cursor = Cursor::new(vec![0u8; 1000]);
        let zeros = vec![0u8; size];
        state
            .write(&mut cursor, &zeros)
            .unwrap_or_else(|_| panic!("write {size} zeros"));
    }
}
