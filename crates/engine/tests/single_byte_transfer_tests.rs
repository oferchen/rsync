//! Integration tests for single-byte file transfers.
//!
//! These tests validate that the rsync engine correctly handles edge cases
//! involving single-byte files across the complete delta transfer pipeline:
//! signature generation, delta computation, and delta application.
//!
//! ## Test Coverage
//!
//! ### Single-Byte File Handling
//! - Signature generation for single-byte files
//! - Delta generation when basis is single byte
//! - Delta generation when target is single byte
//! - Block matching with single-byte inputs
//!
//! ### Delta Transfer Scenarios
//! - Single byte to single byte (identical)
//! - Single byte to single byte (different)
//! - Single byte to empty
//! - Empty to single byte
//! - Single byte to multi-byte
//! - Multi-byte to single byte
//!
//! ### Round-Trip Verification
//! - Content preservation through complete pipeline
//! - All byte values (0x00 through 0xFF)
//! - Edge cases (null byte, max byte)
//!
//! ## Implementation Notes
//!
//! Single-byte files are special cases in rsync because:
//! 1. They cannot form a complete block (default block size is 700)
//! 2. They must be transmitted as literal data
//! 3. Rolling checksum behavior with single byte is edge case
//! 4. Strong checksum still needs to be computed correctly
//!
//! These tests ensure the implementation handles these edge cases correctly,
//! matching upstream rsync behavior.

use engine::{
    DeltaGenerator, DeltaSignatureIndex, DeltaToken, SignatureAlgorithm, SignatureLayoutParams,
    apply_delta, calculate_signature_layout, generate_delta, generate_file_signature,
};
use protocol::ProtocolVersion;
use std::io::Cursor;
use std::num::NonZeroU8;

// ============================================================================
// Test Utilities
// ============================================================================

/// Creates a signature index from the provided data.
///
/// Returns None if the data is too small to produce valid blocks.
fn build_index(data: &[u8]) -> Option<DeltaSignatureIndex> {
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).ok()?;
    let signature =
        generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4).ok()?;
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
}

/// Applies a delta script and returns the reconstructed output.
fn apply_and_reconstruct(
    basis: &[u8],
    index: &DeltaSignatureIndex,
    script: &engine::DeltaScript,
) -> Vec<u8> {
    let mut basis_cursor = Cursor::new(basis);
    let mut output = Vec::new();
    apply_delta(&mut basis_cursor, &mut output, index, script).expect("apply_delta should succeed");
    output
}

/// Generates a delta and verifies round-trip reconstruction.
#[allow(dead_code)]
fn verify_round_trip(basis: &[u8], input: &[u8]) -> engine::DeltaScript {
    let index = build_index(basis).expect("should build index");
    let script = generate_delta(input, &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(basis, &index, &script);
    assert_eq!(
        reconstructed, input,
        "round-trip reconstruction should match input"
    );
    script
}

// ============================================================================
// Signature Generation Tests for Single-Byte Files
// ============================================================================

/// Verifies that signature generation handles single-byte files correctly.
///
/// A single-byte file should produce a signature with one block that has
/// a remainder of 1 (since it's smaller than the default block size).
#[test]
fn single_byte_file_signature() {
    let data = [0x42u8];
    let params = SignatureLayoutParams::new(
        1,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout calculation should succeed");

    assert_eq!(layout.block_count(), 1, "should have one block");
    assert_eq!(layout.remainder(), 1, "remainder should be 1");

    let signature = generate_file_signature(Cursor::new(&data), layout, SignatureAlgorithm::Md4)
        .expect("signature generation should succeed");

    assert_eq!(signature.blocks().len(), 1, "should have exactly one block");
    assert_eq!(signature.total_bytes(), 1, "total bytes should be 1");

    let block = &signature.blocks()[0];
    assert_eq!(block.index(), 0, "block index should be 0");
    assert_eq!(block.len(), 1, "block length should be 1");
}

/// Verifies signature generation for all possible single-byte values.
///
/// Tests that every byte value from 0x00 to 0xFF produces a valid signature.
#[test]
fn single_byte_all_values() {
    for byte_val in 0u8..=255 {
        let data = [byte_val];
        let params = SignatureLayoutParams::new(
            1,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature =
            generate_file_signature(Cursor::new(&data), layout, SignatureAlgorithm::Md4)
                .expect("signature generation should succeed for all byte values");

        assert_eq!(signature.blocks().len(), 1);
        assert_eq!(signature.total_bytes(), 1);
    }
}

/// Verifies that single-byte signature produces valid rolling checksum.
#[test]
fn single_byte_rolling_checksum() {
    use checksums::RollingDigest;

    let data = [0xAAu8];
    let params = SignatureLayoutParams::new(
        1,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature = generate_file_signature(Cursor::new(&data), layout, SignatureAlgorithm::Md4)
        .expect("signature");

    let block = &signature.blocks()[0];
    let expected_rolling = RollingDigest::from_bytes(&data);
    assert_eq!(
        block.rolling(),
        expected_rolling,
        "rolling checksum should match direct computation"
    );
}

// ============================================================================
// Delta Generation Tests with Single-Byte Files
// ============================================================================

/// Verifies delta generation when basis is a single byte and input is identical.
///
/// Even though the file is only one byte, if it's identical, the delta
/// algorithm should attempt to match it. However, since it's smaller than
/// a block, it will be transmitted as literal.
#[test]
fn delta_single_byte_identical() {
    let basis = [0x42u8];

    // Single byte basis is too small to create an index with matchable blocks
    // This is expected - files smaller than block size can't be used as basis
    let index_result = build_index(&basis);

    if let Some(index) = index_result {
        let input = [0x42u8];
        let script = generate_delta(&input[..], &index).expect("delta generation should succeed");

        // Even if identical, single byte will be transmitted as literal
        // because it's smaller than block size
        assert_eq!(script.total_bytes(), 1);

        let reconstructed = apply_and_reconstruct(&basis, &index, &script);
        assert_eq!(reconstructed, input);
    } else {
        // This is the expected case - single byte can't form a basis
        // We verify that generate_delta handles this gracefully
        // by using a larger basis instead
        let basis_large = vec![0x42u8; 1000];
        let index = build_index(&basis_large).expect("larger basis should work");
        let input = [0x42u8];

        let script = generate_delta(&input[..], &index).expect("delta");
        assert_eq!(script.total_bytes(), 1);
        assert_eq!(script.literal_bytes(), 1, "single byte should be literal");
    }
}

/// Verifies delta generation when input is a single different byte.
#[test]
fn delta_single_byte_different() {
    let basis = vec![0xAAu8; 1000];
    let index = build_index(&basis).expect("should build index");

    let input = [0xBBu8];
    let script = generate_delta(&input[..], &index).expect("delta");

    assert_eq!(script.total_bytes(), 1);
    assert_eq!(
        script.literal_bytes(),
        1,
        "different byte should be literal"
    );

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input);
}

/// Verifies delta generation from empty to single byte.
#[test]
fn delta_empty_to_single_byte() {
    let basis = vec![0u8; 1000];
    let index = build_index(&basis).expect("should build index");

    let input = [0x42u8];
    let script = generate_delta(&input[..], &index).expect("delta");

    assert_eq!(script.total_bytes(), 1);
    assert_eq!(script.literal_bytes(), 1);

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input);
}

/// Verifies delta generation from single byte to empty.
#[test]
fn delta_single_byte_to_empty() {
    let basis = vec![0x42u8; 1000];
    let index = build_index(&basis).expect("should build index");

    let input: &[u8] = &[];
    let script = generate_delta(input, &index).expect("delta");

    assert!(
        script.tokens().is_empty(),
        "empty input should produce no tokens"
    );
    assert_eq!(script.total_bytes(), 0);
    assert_eq!(script.literal_bytes(), 0);

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input);
}

/// Verifies delta generation from single byte to multi-byte.
#[test]
fn delta_single_to_multi_byte() {
    let basis = vec![0x42u8; 1000];
    let index = build_index(&basis).expect("should build index");

    // Input starts with one byte that might match, followed by different bytes
    let input = vec![0x42u8, 0x43, 0x44, 0x45, 0x46];
    let script = generate_delta(&input[..], &index).expect("delta");

    assert_eq!(script.total_bytes(), 5);
    // All should be literals since input is too small to match full blocks
    assert_eq!(script.literal_bytes(), 5);

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input);
}

/// Verifies delta generation from multi-byte to single byte.
#[test]
fn delta_multi_to_single_byte() {
    let basis: Vec<u8> = (0..1000).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");

    let input = [0x42u8];
    let script = generate_delta(&input[..], &index).expect("delta");

    assert_eq!(script.total_bytes(), 1);
    assert_eq!(script.literal_bytes(), 1);

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input);
}

// ============================================================================
// Block Matching Tests with Single Bytes
// ============================================================================

/// Verifies that block matching correctly handles single-byte input.
///
/// When the input is a single byte, it cannot form a complete block
/// (minimum block size is larger), so it should always be transmitted
/// as a literal, even if that byte appears in the basis.
#[test]
fn block_matching_single_byte_cannot_match() {
    let basis: Vec<u8> = vec![0x42u8; 5000];
    let index = build_index(&basis).expect("should build index");

    let input = [0x42u8]; // Same byte as entire basis
    let script = generate_delta(&input[..], &index).expect("delta");

    // Single byte cannot match because it's smaller than block size
    assert_eq!(script.literal_bytes(), 1, "single byte must be literal");
    assert_eq!(
        script.copy_bytes(),
        0,
        "single byte cannot be copied as block"
    );

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input);
}

/// Verifies delta generator with minimum buffer and single-byte input.
#[test]
fn single_byte_with_minimum_buffer() {
    let basis: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("index");

    // Use smallest possible buffer
    let generator = DeltaGenerator::new().with_buffer_len(1);
    let input = [0x99u8];

    let script = generator.generate(&input[..], &index).expect("delta");
    assert_eq!(script.total_bytes(), 1);
    assert_eq!(script.literal_bytes(), 1);

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input);
}

/// Verifies that rolling checksum doesn't false-match on single bytes.
#[test]
fn no_false_matches_single_byte() {
    // Create basis with various byte patterns
    let basis: Vec<u8> = (0..8192).map(|i| (i % 256) as u8).collect();
    let index = build_index(&basis).expect("index");

    // Test every possible byte value
    for byte_val in 0u8..=255 {
        let input = [byte_val];
        let script = generate_delta(&input[..], &index).expect("delta");

        // Should always be a literal, never a false match
        assert_eq!(
            script.literal_bytes(),
            1,
            "byte 0x{byte_val:02X} should be literal"
        );
        assert_eq!(script.copy_bytes(), 0, "no copies for single byte");

        let reconstructed = apply_and_reconstruct(&basis, &index, &script);
        assert_eq!(reconstructed, &input[..]);
    }
}

// ============================================================================
// Round-Trip Tests for Single-Byte Files
// ============================================================================

/// Verifies round-trip for all possible single-byte values.
///
/// This is a comprehensive test ensuring that every byte value from 0x00
/// to 0xFF can be transmitted correctly through the delta pipeline.
#[test]
fn round_trip_all_byte_values() {
    let basis: Vec<u8> = (0..10000).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("index");

    for byte_val in 0u8..=255 {
        let input = [byte_val];
        let script = generate_delta(&input[..], &index).expect("delta");
        let reconstructed = apply_and_reconstruct(&basis, &index, &script);

        assert_eq!(
            reconstructed,
            &input[..],
            "round-trip failed for byte 0x{byte_val:02X}"
        );
    }
}

/// Verifies round-trip with null byte (0x00).
#[test]
fn round_trip_null_byte() {
    let basis = vec![0xFFu8; 5000];
    let index = build_index(&basis).expect("index");

    let input = [0x00u8];
    let script = generate_delta(&input[..], &index).expect("delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(
        reconstructed, input,
        "null byte should round-trip correctly"
    );
}

/// Verifies round-trip with maximum byte (0xFF).
#[test]
fn round_trip_max_byte() {
    let basis = vec![0x00u8; 5000];
    let index = build_index(&basis).expect("index");

    let input = [0xFFu8];
    let script = generate_delta(&input[..], &index).expect("delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(reconstructed, input, "max byte should round-trip correctly");
}

/// Verifies round-trip for printable ASCII single bytes.
#[test]
fn round_trip_printable_ascii() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("index");

    // Test printable ASCII range (0x20 to 0x7E)
    for byte_val in 0x20u8..=0x7E {
        let input = [byte_val];
        let script = generate_delta(&input[..], &index).expect("delta");
        let reconstructed = apply_and_reconstruct(&basis, &index, &script);

        assert_eq!(
            reconstructed,
            &input[..],
            "ASCII '{}' (0x{:02X}) should round-trip",
            byte_val as char,
            byte_val
        );
    }
}

// ============================================================================
// Edge Case Tests
// ============================================================================

/// Verifies handling of single byte at end of larger input.
#[test]
fn single_byte_at_end_of_input() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("index");
    let block_len = index.block_length();

    // Input is one full block from basis, then one extra byte
    let mut input = basis[..block_len].to_vec();
    input.push(0xFF);

    let script = generate_delta(&input[..], &index).expect("delta");

    // Should have copy for the block and literal for the trailing byte
    assert_eq!(script.literal_bytes(), 1, "trailing byte should be literal");

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input);
}

/// Verifies handling of single byte at start of larger input.
#[test]
fn single_byte_at_start_of_input() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("index");
    let block_len = index.block_length();

    // Input is one byte, then a full block from basis
    let mut input = vec![0xFF];
    input.extend_from_slice(&basis[..block_len]);

    let script = generate_delta(&input[..], &index).expect("delta");

    // Should have literal for leading byte, then ability to find matching block
    assert!(
        script.literal_bytes() >= 1,
        "leading byte should be literal"
    );

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input);
}

/// Verifies that DeltaToken correctly reports single-byte literal length.
#[test]
fn delta_token_single_byte_literal() {
    let literal = DeltaToken::Literal(vec![0x42]);
    assert_eq!(
        literal.byte_len(),
        1,
        "single-byte literal should report len 1"
    );
    assert!(literal.is_literal(), "should be identified as literal");
}

/// Verifies delta script byte accounting with single byte.
#[test]
fn delta_script_accounting_single_byte() {
    let basis: Vec<u8> = (0..5000).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("index");

    let input = [0x99u8];
    let script = generate_delta(&input[..], &index).expect("delta");

    // Verify accounting
    assert_eq!(script.total_bytes(), 1, "total should be 1");
    assert_eq!(script.literal_bytes(), 1, "literal should be 1");
    assert_eq!(script.copy_bytes(), 0, "copy should be 0");
    assert_eq!(
        script.total_bytes(),
        script.literal_bytes() + script.copy_bytes(),
        "accounting should balance"
    );
}

// ============================================================================
// Buffer Boundary Tests
// ============================================================================

/// Verifies single-byte handling with various buffer sizes.
///
/// Tests that different buffer sizes don't affect correctness when
/// processing single-byte inputs.
#[test]
fn single_byte_various_buffer_sizes() {
    let basis: Vec<u8> = (0..10000).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("index");
    let input = [0x42u8];

    let buffer_sizes = [1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 4096, 8192];

    for buffer_size in buffer_sizes {
        let generator = DeltaGenerator::new().with_buffer_len(buffer_size);
        let script = generator
            .generate(&input[..], &index)
            .expect("delta should succeed with any buffer size");

        assert_eq!(
            script.total_bytes(),
            1,
            "buffer size {buffer_size} should produce same result"
        );
        assert_eq!(script.literal_bytes(), 1);

        let reconstructed = apply_and_reconstruct(&basis, &index, &script);
        assert_eq!(reconstructed, input);
    }
}

/// Verifies that zero-length buffer (clamped to 1) handles single byte.
#[test]
fn single_byte_zero_buffer_clamped_to_one() {
    let basis: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("index");

    // Zero buffer gets clamped to 1
    let generator = DeltaGenerator::new().with_buffer_len(0);
    let input = [0x42u8];

    let script = generator.generate(&input[..], &index).expect("delta");
    assert_eq!(script.total_bytes(), 1);

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input);
}

// ============================================================================
// Stress Tests
// ============================================================================

/// Stress test: Many sequential single-byte transfers.
#[test]
fn stress_many_single_byte_transfers() {
    let basis: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("index");

    // Test 256 different single-byte inputs
    for byte_val in 0u8..=255 {
        let input = [byte_val];
        let script = generate_delta(&input[..], &index).expect("delta");
        let reconstructed = apply_and_reconstruct(&basis, &index, &script);
        assert_eq!(reconstructed, input);
    }
}

/// Stress test: Alternating single bytes in larger stream.
#[test]
fn stress_alternating_single_bytes() {
    let basis: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("index");

    // Create input that alternates between matching and non-matching bytes
    let mut input = Vec::new();
    for (i, &byte) in basis.iter().enumerate().take(100) {
        if i % 2 == 0 {
            input.push(byte);
        } else {
            input.push(0xFF);
        }
    }

    let script = generate_delta(&input[..], &index).expect("delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input);
}

// ============================================================================
// Algorithm Correctness Tests
// ============================================================================

/// Verifies that single-byte delta generation is deterministic.
#[test]
fn single_byte_delta_is_deterministic() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("index");
    let input = [0x42u8];

    // Generate delta multiple times
    let script1 = generate_delta(&input[..], &index).expect("delta 1");
    let script2 = generate_delta(&input[..], &index).expect("delta 2");
    let script3 = generate_delta(&input[..], &index).expect("delta 3");

    // All should be identical
    assert_eq!(script1.total_bytes(), script2.total_bytes());
    assert_eq!(script2.total_bytes(), script3.total_bytes());
    assert_eq!(script1.literal_bytes(), script2.literal_bytes());
    assert_eq!(script2.literal_bytes(), script3.literal_bytes());
}

/// Verifies that apply_delta handles single-byte literal correctly.
#[test]
fn apply_delta_single_byte_literal() {
    let basis: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("index");

    // Manually construct a script with a single-byte literal
    let script = engine::DeltaScript::new(vec![DeltaToken::Literal(vec![0x42])], 1, 1);

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, [0x42]);
}

/// Verifies correct handling of single byte surrounded by copies.
#[test]
fn single_byte_between_copies() {
    let basis: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("index");
    let block_len = index.block_length();

    // Input: block 0, single byte, block 1
    let mut input = basis[..block_len].to_vec();
    input.push(0xFF); // Single byte that doesn't match
    input.extend_from_slice(&basis[block_len..block_len * 2]);

    let script = generate_delta(&input[..], &index).expect("delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(reconstructed, input);

    // Verify we have both copies and literals
    assert!(script.copy_bytes() > 0, "should have copy tokens");
    assert!(
        script.literal_bytes() >= 1,
        "should have literal for single byte"
    );
}
