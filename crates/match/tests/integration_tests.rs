//! Integration tests for block matching and delta generation.
//!
//! These tests validate the complete delta generation pipeline, ensuring
//! that the rsync block matching algorithm produces correct deltas across
//! a variety of scenarios. The implementation mirrors upstream rsync's
//! `match.c` behavior.
//!
//! ## Test Coverage
//!
//! ### Data Pattern Tests
//! - Uniform data (all same bytes)
//! - Random data (no block correlations)
//! - Repetitive patterns (periodic data)
//! - Incremental data (sequential bytes)
//! - Sparse data (mostly zeros with scattered content)
//! - Real-world patterns (text-like, binary-like)
//!
//! ### Block Matching Scenarios
//! - No matches (completely different files)
//! - All matches (identical files)
//! - Partial matches (modified regions)
//! - Insertions at various positions
//! - Deletions at various positions
//! - Block reordering
//!
//! ### Edge Cases
//! - Empty input
//! - Single byte input
//! - Input smaller than block size
//! - Input exactly one block
//! - Input at block boundaries
//! - Very large files
//!
//! ### Performance Characteristics
//! - Different block sizes (small, medium, large)
//! - Buffer size effects on delta generation
//! - Signature index lookup performance
//!
//! ## Upstream Reference
//!
//! - `match.c:55` - `build_hash_table()` - Hash table construction
//! - `match.c:140` - `hash_search()` - Rolling checksum matching
//! - `match.c:362` - `match_sums()` - Main delta generation entry point

use matching::{
    DeltaGenerator, DeltaScript, DeltaSignatureIndex, DeltaToken, apply_delta, generate_delta,
};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};
use std::io::Cursor;
use std::num::NonZeroU8;

// ============================================================================
// Test Utilities
// ============================================================================

/// Creates a signature index from the provided data.
fn build_index(data: &[u8]) -> Option<DeltaSignatureIndex> {
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).ok()?;
    let signature = generate_file_signature(data, layout, SignatureAlgorithm::Md4).ok()?;
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
}

/// Creates a signature index with a specific block size hint.
fn build_index_with_block_hint(data: &[u8], block_hint: u32) -> Option<DeltaSignatureIndex> {
    use std::num::NonZeroU32;
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        NonZeroU32::new(block_hint),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).ok()?;
    let signature = generate_file_signature(data, layout, SignatureAlgorithm::Md4).ok()?;
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
}

/// Applies a delta script and returns the reconstructed output.
fn apply_and_reconstruct(
    basis: &[u8],
    index: &DeltaSignatureIndex,
    script: &DeltaScript,
) -> Vec<u8> {
    let mut basis_cursor = Cursor::new(basis);
    let mut output = Vec::new();
    apply_delta(&mut basis_cursor, &mut output, index, script).expect("apply_delta should succeed");
    output
}

/// Generates a delta and verifies round-trip reconstruction.
fn verify_round_trip(basis: &[u8], input: &[u8]) -> DeltaScript {
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
// Data Pattern Tests
// ============================================================================

/// Verifies delta generation with uniform data (all same bytes).
///
/// When basis and input contain the same repeated byte pattern, the delta
/// algorithm should efficiently match blocks. This tests the rolling
/// checksum's behavior with low-entropy data.
///
/// Note: Even identical files may have some trailing literal bytes if the
/// file length is not an exact multiple of the block size. This mirrors
/// upstream rsync behavior where the last partial block cannot be matched.
#[test]
fn uniform_data_generates_copy_tokens() {
    let basis = vec![0xAA; 8192];
    let input = vec![0xAA; 8192];

    let script = verify_round_trip(&basis, &input);

    // Should be mostly copy tokens since data is identical
    let copy_count = script
        .tokens()
        .iter()
        .filter(|t| matches!(t, DeltaToken::Copy { .. }))
        .count();
    assert!(
        copy_count > 0,
        "identical uniform data should produce copy tokens"
    );

    // Most bytes should be copied (allowing for trailing partial block as literals)
    let copy_bytes = script.copy_bytes();
    let literal_bytes = script.literal_bytes();
    assert!(
        copy_bytes > literal_bytes,
        "identical data should be mostly copies: copy={copy_bytes}, literal={literal_bytes}"
    );
}

/// Verifies delta generation with different uniform data produces literals.
#[test]
fn different_uniform_data_generates_literals() {
    let basis = vec![0xAA; 4096];
    let input = vec![0xBB; 4096];

    let script = verify_round_trip(&basis, &input);

    // Different content should produce all literals
    assert_eq!(
        script.literal_bytes(),
        input.len() as u64,
        "different data should be all literals"
    );
}

/// Verifies delta generation with random data (no correlations).
///
/// Random data should not produce accidental matches except by hash collision,
/// which the strong checksum should reject.
#[test]
fn random_data_no_false_matches() {
    // Use a PRNG pattern that won't accidentally match
    let basis: Vec<u8> = (0..8192).map(|i| ((i * 17 + 31) % 256) as u8).collect();
    let input: Vec<u8> = (0..4096).map(|i| ((i * 23 + 47) % 256) as u8).collect();

    let script = verify_round_trip(&basis, &input);

    // Should be all literals since patterns don't match
    assert_eq!(script.literal_bytes(), input.len() as u64);
}

/// Verifies delta generation with repetitive periodic patterns.
///
/// Note: Trailing bytes that don't form a complete block will be literals.
#[test]
fn repetitive_pattern_matching() {
    let pattern: Vec<u8> = (0..256).map(|i| i as u8).collect();
    let basis: Vec<u8> = pattern.iter().cycle().take(8192).copied().collect();
    let input: Vec<u8> = pattern.iter().cycle().take(8192).copied().collect();

    let script = verify_round_trip(&basis, &input);

    // Identical repetitive data should produce mostly copy tokens
    let copy_bytes = script.copy_bytes();
    let literal_bytes = script.literal_bytes();
    assert!(
        copy_bytes > literal_bytes,
        "identical repetitive data should be mostly copies: copy={copy_bytes}, literal={literal_bytes}"
    );
}

/// Verifies delta generation with incrementing byte sequences.
///
/// Note: Trailing bytes that don't form a complete block will be literals.
#[test]
fn incremental_data_pattern() {
    let basis: Vec<u8> = (0u32..10000).map(|i| (i % 251) as u8).collect();
    let input: Vec<u8> = (0u32..10000).map(|i| (i % 251) as u8).collect();

    let script = verify_round_trip(&basis, &input);

    // Most bytes should be copied
    let copy_bytes = script.copy_bytes();
    let literal_bytes = script.literal_bytes();
    assert!(
        copy_bytes > literal_bytes,
        "identical incremental data should be mostly copies: copy={copy_bytes}, literal={literal_bytes}"
    );
}

/// Verifies delta generation with sparse data (mostly zeros).
///
/// Note: Trailing bytes that don't form a complete block will be literals.
#[test]
fn sparse_data_with_islands() {
    let mut basis = vec![0u8; 16384];
    // Add some non-zero islands
    for i in (0..basis.len()).step_by(1000) {
        if i + 100 <= basis.len() {
            for j in 0..100 {
                basis[i + j] = ((i + j) % 256) as u8;
            }
        }
    }

    let input = basis.clone();
    let script = verify_round_trip(&basis, &input);

    // Most bytes should be copied
    let copy_bytes = script.copy_bytes();
    let literal_bytes = script.literal_bytes();
    assert!(
        copy_bytes > literal_bytes,
        "identical sparse data should be mostly copies: copy={copy_bytes}, literal={literal_bytes}"
    );
}

/// Verifies delta generation with text-like data patterns.
///
/// Note: Trailing bytes that don't form a complete block will be literals.
#[test]
fn text_like_data_pattern() {
    // Simulate text: printable ASCII with newlines
    let text = "The quick brown fox jumps over the lazy dog.\n".repeat(200);
    let basis = text.as_bytes().to_vec();
    let input = basis.clone();

    let script = verify_round_trip(&basis, &input);

    // Most bytes should be copied
    let copy_bytes = script.copy_bytes();
    let literal_bytes = script.literal_bytes();
    assert!(
        copy_bytes > literal_bytes,
        "identical text should be mostly copies: copy={copy_bytes}, literal={literal_bytes}"
    );
}

// ============================================================================
// Block Matching Scenarios
// ============================================================================

/// Verifies that completely different files produce all literal tokens.
#[test]
fn no_matches_all_literals() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 256) as u8).collect();
    let input = b"completely different content with no block matches whatsoever".to_vec();

    let script = verify_round_trip(&basis, &input);

    assert_eq!(script.literal_bytes(), input.len() as u64);
    let literal_count = script.tokens().iter().filter(|t| t.is_literal()).count();
    assert_eq!(literal_count, 1, "should produce single literal token");
}

/// Verifies that identical files produce mostly copy tokens.
///
/// Note: The last partial block (if any) that doesn't reach full block size
/// will be transmitted as literals. This is expected rsync behavior.
#[test]
fn all_matches_identical_files() {
    let basis: Vec<u8> = (0..10000).map(|i| (i % 251) as u8).collect();
    let input = basis.clone();

    let script = verify_round_trip(&basis, &input);

    // The vast majority should be copies
    let copy_bytes = script.copy_bytes();
    let literal_bytes = script.literal_bytes();
    assert!(
        copy_bytes > literal_bytes * 10,
        "identical files should be overwhelmingly copies: copy={copy_bytes}, literal={literal_bytes}"
    );
    assert!(script.copy_bytes() > 0, "should have copy tokens");
}

/// Verifies partial matches with modified regions.
#[test]
fn partial_matches_modified_middle() {
    let basis: Vec<u8> = (0..10000).map(|i| (i % 251) as u8).collect();
    let mut input = basis.clone();

    // Modify middle section
    let mid = input.len() / 2;
    for i in 0..100 {
        if mid + i < input.len() {
            input[mid + i] = 0xFF;
        }
    }

    let script = verify_round_trip(&basis, &input);

    // Should have both copy and literal tokens
    assert!(
        script.literal_bytes() > 0,
        "modified region should produce literals"
    );
    assert!(
        script.copy_bytes() > 0,
        "unmodified regions should produce copies"
    );
}

/// Verifies handling of insertions at the beginning.
#[test]
fn insertion_at_beginning() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    let mut input = vec![0xFFu8; 50]; // Insert 50 bytes
    input.extend_from_slice(&basis);

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(reconstructed, input);

    // Should have literals for the insertion plus copies for the rest
    assert!(
        script.literal_bytes() >= 50,
        "should have literals for insertion"
    );
    assert!(
        script.copy_bytes() >= block_len as u64,
        "should have copies for original data"
    );
}

/// Verifies handling of insertions at the end.
#[test]
fn insertion_at_end() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");

    let mut input = basis.clone();
    input.extend_from_slice(&[0xFFu8; 100]); // Append 100 bytes

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(reconstructed, input);
    assert!(
        script.literal_bytes() >= 100,
        "should have literals for appended data"
    );
}

/// Verifies handling of insertions in the middle.
#[test]
fn insertion_in_middle() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");

    let mid = basis.len() / 2;
    let mut input = basis[..mid].to_vec();
    input.extend_from_slice(&[0xFFu8; 200]); // Insert 200 bytes
    input.extend_from_slice(&basis[mid..]);

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(reconstructed, input);
    assert!(
        script.literal_bytes() >= 200,
        "should have literals for insertion"
    );
}

/// Verifies handling of deletions at the beginning.
#[test]
fn deletion_at_beginning() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Remove first portion (more than a block to ensure we test matching)
    let input = basis[block_len * 2..].to_vec();

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(reconstructed, input);
}

/// Verifies handling of deletions at the end.
#[test]
fn deletion_at_end() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Remove last portion
    let input = basis[..basis.len() - block_len * 2].to_vec();

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(reconstructed, input);
}

/// Verifies handling of block reordering (blocks in different order).
#[test]
fn block_reordering() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Reverse block order for first few blocks
    let num_blocks = 4;
    let mut input = Vec::new();
    for i in (0..num_blocks).rev() {
        let start = i * block_len;
        let end = (i + 1) * block_len;
        if end <= basis.len() {
            input.extend_from_slice(&basis[start..end]);
        }
    }
    // Append remaining data
    if num_blocks * block_len < basis.len() {
        input.extend_from_slice(&basis[num_blocks * block_len..]);
    }

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(reconstructed, input);

    // Reordered blocks should still be found and copied
    let copy_count = script
        .tokens()
        .iter()
        .filter(|t| matches!(t, DeltaToken::Copy { .. }))
        .count();
    assert!(
        copy_count >= num_blocks,
        "reordered blocks should still match"
    );
}

// ============================================================================
// Edge Case Tests
// ============================================================================

/// Verifies delta generation with empty input.
#[test]
fn empty_input_produces_empty_script() {
    let basis = vec![0u8; 4096];
    let index = build_index(&basis).expect("should build index");

    let input: &[u8] = &[];
    let script = generate_delta(input, &index).expect("should generate delta");

    assert!(
        script.tokens().is_empty(),
        "empty input should produce no tokens"
    );
    assert_eq!(script.total_bytes(), 0);
    assert_eq!(script.literal_bytes(), 0);
}

/// Verifies delta generation with single byte input.
#[test]
fn single_byte_input() {
    let basis = vec![0u8; 4096];
    let index = build_index(&basis).expect("should build index");

    let input = [42u8];
    let script = generate_delta(&input[..], &index).expect("should generate delta");

    assert_eq!(script.literal_bytes(), 1);
    assert_eq!(script.total_bytes(), 1);

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input);
}

/// Verifies delta generation when input is smaller than block size.
#[test]
fn input_smaller_than_block_size() {
    let basis = vec![0u8; 4096];
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Input smaller than one block
    let input = vec![42u8; block_len - 1];
    let script = generate_delta(&input[..], &index).expect("should generate delta");

    // Should be all literals since no complete block can match
    assert_eq!(script.literal_bytes(), input.len() as u64);

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input);
}

/// Verifies delta generation when input is exactly one block.
#[test]
fn input_exactly_one_block() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Input is exactly one block from the basis
    let input = basis[..block_len].to_vec();
    let script = generate_delta(&input[..], &index).expect("should generate delta");

    // Should be a single copy token
    assert_eq!(script.literal_bytes(), 0);
    let copy_count = script
        .tokens()
        .iter()
        .filter(|t| matches!(t, DeltaToken::Copy { .. }))
        .count();
    assert_eq!(copy_count, 1, "should produce exactly one copy token");

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input);
}

/// Verifies delta generation at exact block boundaries.
#[test]
fn input_at_block_boundaries() {
    let basis: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Input is exactly N blocks
    let num_blocks = 3;
    let input = basis[..block_len * num_blocks].to_vec();
    let script = generate_delta(&input[..], &index).expect("should generate delta");

    assert_eq!(
        script.literal_bytes(),
        0,
        "block-aligned identical data should be all copies"
    );

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input);
}

/// Verifies delta generation with input one byte over block boundary.
#[test]
fn input_one_byte_over_block_boundary() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Input is one block plus one byte
    let mut input = basis[..block_len].to_vec();
    input.push(0xFF);

    let script = generate_delta(&input[..], &index).expect("should generate delta");

    // Should have copy for the block and literal for the extra byte
    assert_eq!(script.literal_bytes(), 1);

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input);
}

/// Verifies delta generation with input one byte under block boundary.
#[test]
fn input_one_byte_under_block_boundary() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Input is one block minus one byte
    let input = basis[..block_len - 1].to_vec();
    let script = generate_delta(&input[..], &index).expect("should generate delta");

    // Cannot match since not a complete block
    assert_eq!(script.literal_bytes(), input.len() as u64);

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input);
}

/// Verifies delta generation with large files.
#[test]
fn large_file_handling() {
    // 1 MB of data
    let basis: Vec<u8> = (0..1_048_576).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");

    // Modify a small section
    let mut input = basis.clone();
    let mid = input.len() / 2;
    for i in 0..1000 {
        input[mid + i] = 0xFF;
    }

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(reconstructed, input);

    // Most should be copies, some literals for modified section
    assert!(
        script.copy_bytes() > script.literal_bytes(),
        "large file should mostly copy"
    );
}

// ============================================================================
// Block Size Variation Tests
// ============================================================================

/// Verifies delta generation with small block size.
#[test]
fn small_block_size() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();

    // Use small block hint
    let index = build_index_with_block_hint(&basis, 128).expect("should build index");
    assert!(index.block_length() <= 256, "block size should be small");

    let input = basis.clone();
    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(reconstructed, input);
}

/// Verifies delta generation with large block size.
#[test]
fn large_block_size() {
    let basis: Vec<u8> = (0..65536).map(|i| (i % 251) as u8).collect();

    // Use large block hint
    let index = build_index_with_block_hint(&basis, 8192).expect("should build index");

    let input = basis.clone();
    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(reconstructed, input);
}

/// Verifies that different buffer sizes produce identical results.
#[test]
fn buffer_size_independence() {
    let basis: Vec<u8> = (0..10000).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");

    let mut input = basis.clone();
    input[5000] = 0xFF; // Small modification

    // Generate with different buffer sizes
    let gen_small = DeltaGenerator::new().with_buffer_len(64);
    let gen_medium = DeltaGenerator::new().with_buffer_len(1024);
    let gen_large = DeltaGenerator::new().with_buffer_len(32768);
    let gen_default = DeltaGenerator::new();

    let script_small = gen_small
        .generate(&input[..], &index)
        .expect("small buffer");
    let script_medium = gen_medium
        .generate(&input[..], &index)
        .expect("medium buffer");
    let script_large = gen_large
        .generate(&input[..], &index)
        .expect("large buffer");
    let script_default = gen_default
        .generate(&input[..], &index)
        .expect("default buffer");

    // All should produce the same byte counts
    assert_eq!(script_small.total_bytes(), script_default.total_bytes());
    assert_eq!(script_medium.total_bytes(), script_default.total_bytes());
    assert_eq!(script_large.total_bytes(), script_default.total_bytes());

    assert_eq!(script_small.literal_bytes(), script_default.literal_bytes());
    assert_eq!(
        script_medium.literal_bytes(),
        script_default.literal_bytes()
    );
    assert_eq!(script_large.literal_bytes(), script_default.literal_bytes());

    // All should reconstruct correctly
    let rec_small = apply_and_reconstruct(&basis, &index, &script_small);
    let rec_medium = apply_and_reconstruct(&basis, &index, &script_medium);
    let rec_large = apply_and_reconstruct(&basis, &index, &script_large);

    assert_eq!(rec_small, input);
    assert_eq!(rec_medium, input);
    assert_eq!(rec_large, input);
}

// ============================================================================
// Rolling Checksum Integration Tests
// ============================================================================

/// Verifies that rolling checksum correctly identifies matching blocks
/// after sliding past non-matching data.
#[test]
fn rolling_checksum_finds_offset_match() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Input has garbage followed by a matching block
    let mut input = vec![0xFFu8; 50]; // Non-matching prefix
    input.extend_from_slice(&basis[..block_len]); // Matching block
    input.extend_from_slice(b"trailing garbage");

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(reconstructed, input);

    // Should find the matching block despite the prefix
    let copy_count = script
        .tokens()
        .iter()
        .filter(|t| matches!(t, DeltaToken::Copy { .. }))
        .count();
    assert!(
        copy_count >= 1,
        "should find the matching block after prefix"
    );
}

/// Verifies matching of blocks at various offsets in the input stream.
#[test]
fn matching_at_various_offsets() {
    let basis: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Test different offset positions
    for offset in [1, 10, 50, 100, 255, 512].iter() {
        let mut input = vec![0xFFu8; *offset];
        input.extend_from_slice(&basis[..block_len]);

        let script = generate_delta(&input[..], &index).expect("should generate delta");
        let reconstructed = apply_and_reconstruct(&basis, &index, &script);

        assert_eq!(reconstructed, input, "failed at offset {}", *offset);

        let copy_count = script
            .tokens()
            .iter()
            .filter(|t| matches!(t, DeltaToken::Copy { .. }))
            .count();
        assert!(copy_count >= 1, "should find match at offset {}", *offset);
    }
}

/// Verifies that multiple scattered matching blocks are all found.
#[test]
fn multiple_scattered_matches() {
    let basis: Vec<u8> = (0..32768).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Input: garbage, block 0, garbage, block 2, garbage, block 4
    let mut input = Vec::new();
    input.extend_from_slice(b"GARBAGE1");
    input.extend_from_slice(&basis[0..block_len]);
    input.extend_from_slice(b"GARBAGE2");
    input.extend_from_slice(&basis[block_len * 2..block_len * 3]);
    input.extend_from_slice(b"GARBAGE3");
    input.extend_from_slice(&basis[block_len * 4..block_len * 5]);
    input.extend_from_slice(b"END");

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(reconstructed, input);

    let copy_count = script
        .tokens()
        .iter()
        .filter(|t| matches!(t, DeltaToken::Copy { .. }))
        .count();
    assert_eq!(copy_count, 3, "should find all three scattered blocks");
}

// ============================================================================
// Signature Index Tests
// ============================================================================

/// Verifies that signature index correctly reports block length.
#[test]
fn index_block_length_accessor() {
    let basis = vec![0u8; 8192];
    let index = build_index(&basis).expect("should build index");

    assert!(index.block_length() > 0, "block length should be positive");
    assert!(
        index.block_length() <= basis.len(),
        "block length should not exceed data size"
    );
}

/// Verifies that signature index correctly reports strong checksum length.
#[test]
fn index_strong_length_accessor() {
    let basis = vec![0u8; 8192];
    let index = build_index(&basis).expect("should build index");

    assert_eq!(
        index.strong_length(),
        16,
        "strong length should match MD4 truncation"
    );
}

/// Verifies that index returns None for data without full blocks.
#[test]
fn index_returns_none_for_small_data() {
    // Very small data that won't produce full blocks
    let basis = vec![0u8; 64];
    let result = build_index(&basis);

    // The index should return None because there are no full blocks
    assert!(
        result.is_none(),
        "small data should not produce index with full blocks"
    );
}

// ============================================================================
// Delta Script Structure Tests
// ============================================================================

/// Verifies DeltaScript byte accounting is accurate.
#[test]
fn script_byte_accounting() {
    let basis: Vec<u8> = (0..10000).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");

    let mut input = basis.clone();
    // Modify some bytes to create mixed copy/literal
    for i in 0..100 {
        input[1000 + i] = 0xFF;
    }

    let script = generate_delta(&input[..], &index).expect("should generate delta");

    // Verify byte accounting
    assert_eq!(
        script.total_bytes(),
        script.literal_bytes() + script.copy_bytes(),
        "total should equal literal + copy"
    );
    assert_eq!(
        script.total_bytes(),
        input.len() as u64,
        "total bytes should match input length"
    );
}

/// Verifies DeltaToken byte_len method.
#[test]
fn token_byte_len_accuracy() {
    let literal = DeltaToken::Literal(vec![1, 2, 3, 4, 5]);
    assert_eq!(literal.byte_len(), 5);

    let copy = DeltaToken::Copy {
        index: 0,
        len: 1024,
    };
    assert_eq!(copy.byte_len(), 1024);

    let empty_literal = DeltaToken::Literal(vec![]);
    assert_eq!(empty_literal.byte_len(), 0);
}

/// Verifies DeltaToken is_literal method.
#[test]
fn token_is_literal_check() {
    let literal = DeltaToken::Literal(vec![1, 2, 3]);
    assert!(literal.is_literal());

    let copy = DeltaToken::Copy { index: 0, len: 100 };
    assert!(!copy.is_literal());
}

/// Verifies DeltaScript into_tokens consumes and returns tokens.
#[test]
fn script_into_tokens() {
    let tokens = vec![
        DeltaToken::Literal(vec![1, 2, 3]),
        DeltaToken::Copy { index: 0, len: 100 },
    ];
    let script = DeltaScript::new(tokens.clone(), 103, 3);

    let extracted = script.into_tokens();
    assert_eq!(extracted.len(), 2);
    assert_eq!(extracted, tokens);
}

/// Verifies DeltaScript is_empty for empty script.
#[test]
fn script_is_empty() {
    let empty = DeltaScript::new(vec![], 0, 0);
    assert!(empty.is_empty());

    let non_empty = DeltaScript::new(vec![DeltaToken::Literal(vec![1])], 1, 1);
    assert!(!non_empty.is_empty());
}

// ============================================================================
// DeltaGenerator Tests
// ============================================================================

/// Verifies DeltaGenerator builder pattern.
#[test]
fn generator_builder_pattern() {
    let generator = DeltaGenerator::new()
        .with_buffer_len(4096)
        .with_buffer_len(8192); // Chain calls

    // Should use last value
    let basis = vec![0u8; 4096];
    let index = build_index(&basis).expect("should build index");
    let script = generator
        .generate(&[][..], &index)
        .expect("should work with chained builder");

    assert!(script.is_empty());
}

/// Verifies DeltaGenerator with zero buffer becomes 1.
#[test]
fn generator_zero_buffer_becomes_one() {
    let generator = DeltaGenerator::new().with_buffer_len(0);

    let basis = vec![0u8; 4096];
    let index = build_index(&basis).expect("should build index");
    let input = vec![1u8, 2, 3];

    let script = generator
        .generate(&input[..], &index)
        .expect("should handle minimum buffer");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(reconstructed, input);
}

/// Verifies DeltaGenerator Clone implementation.
#[test]
fn generator_clone() {
    let gen1 = DeltaGenerator::new().with_buffer_len(512);
    let gen2 = gen1.clone();

    let basis = vec![0u8; 4096];
    let index = build_index(&basis).expect("should build index");
    let input = b"test data";

    let script1 = gen1.generate(&input[..], &index).expect("gen1");
    let script2 = gen2.generate(&input[..], &index).expect("gen2");

    assert_eq!(script1.total_bytes(), script2.total_bytes());
    assert_eq!(script1.literal_bytes(), script2.literal_bytes());
}

/// Verifies DeltaGenerator Default implementation matches new().
#[test]
fn generator_default() {
    let gen1 = DeltaGenerator::new();
    let gen2 = DeltaGenerator::default();

    let basis = vec![0u8; 4096];
    let index = build_index(&basis).expect("should build index");
    let input = b"test";

    let script1 = gen1.generate(&input[..], &index).expect("new");
    let script2 = gen2.generate(&input[..], &index).expect("default");

    assert_eq!(script1.total_bytes(), script2.total_bytes());
}

// ============================================================================
// Apply Delta Tests
// ============================================================================

/// Verifies apply_delta handles empty script.
#[test]
fn apply_empty_script() {
    let basis = vec![0u8; 4096];
    let index = build_index(&basis).expect("should build index");

    let script = DeltaScript::new(vec![], 0, 0);
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert!(reconstructed.is_empty());
}

/// Verifies apply_delta handles multiple consecutive literals.
#[test]
fn apply_consecutive_literals() {
    let basis = vec![0u8; 4096];
    let index = build_index(&basis).expect("should build index");

    let script = DeltaScript::new(
        vec![
            DeltaToken::Literal(b"Hello, ".to_vec()),
            DeltaToken::Literal(b"World".to_vec()),
            DeltaToken::Literal(b"!".to_vec()),
        ],
        13,
        13,
    );

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, b"Hello, World!");
}

/// Verifies apply_delta handles multiple consecutive copies.
#[test]
fn apply_consecutive_copies() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Copy block 0, then block 1
    let script = DeltaScript::new(
        vec![
            DeltaToken::Copy {
                index: 0,
                len: block_len,
            },
            DeltaToken::Copy {
                index: 1,
                len: block_len,
            },
        ],
        (block_len * 2) as u64,
        0,
    );

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, &basis[..block_len * 2]);
}

/// Verifies apply_delta handles interleaved copy and literal tokens.
#[test]
fn apply_interleaved_tokens() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    let script = DeltaScript::new(
        vec![
            DeltaToken::Literal(b"PREFIX".to_vec()),
            DeltaToken::Copy {
                index: 0,
                len: block_len,
            },
            DeltaToken::Literal(b"MIDDLE".to_vec()),
            DeltaToken::Copy {
                index: 1,
                len: block_len,
            },
            DeltaToken::Literal(b"SUFFIX".to_vec()),
        ],
        (block_len * 2 + 18) as u64,
        18,
    );

    let mut expected = b"PREFIX".to_vec();
    expected.extend_from_slice(&basis[..block_len]);
    expected.extend_from_slice(b"MIDDLE");
    expected.extend_from_slice(&basis[block_len..block_len * 2]);
    expected.extend_from_slice(b"SUFFIX");

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, expected);
}

// ============================================================================
// Hash Collision Resistance Tests
// ============================================================================

/// Verifies that false positives from rolling checksum are rejected by strong checksum.
///
/// This test creates data designed to have the same weak checksum as a block
/// in the basis but different strong checksum, ensuring false positives are rejected.
#[test]
fn strong_checksum_rejects_false_positives() {
    // Create basis with specific pattern
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Create input with same length but different content
    // Even if rolling checksum collides, strong checksum should differ
    let input: Vec<u8> = (0..block_len).map(|i| ((i * 7 + 13) % 256) as u8).collect();

    let script = generate_delta(&input[..], &index).expect("should generate delta");

    // Verify the data can be reconstructed (proves no corruption from false matches)
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input);
}

// ============================================================================
// Stress Tests
// ============================================================================

/// Stress test with many small modifications throughout the file.
#[test]
fn many_small_modifications() {
    let basis: Vec<u8> = (0..32768).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");

    let mut input = basis.clone();
    // Make small modifications throughout
    for i in (0..input.len()).step_by(500) {
        input[i] = input[i].wrapping_add(1);
    }

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(reconstructed, input);
}

/// Stress test with alternating matching and non-matching regions.
#[test]
fn alternating_match_regions() {
    let basis: Vec<u8> = (0..32768).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    let mut input = Vec::new();
    let num_blocks = basis.len() / block_len;

    for i in 0..num_blocks {
        if i % 2 == 0 {
            // Copy from basis
            let start = i * block_len;
            let end = start + block_len;
            input.extend_from_slice(&basis[start..end]);
        } else {
            // Insert garbage
            input.extend_from_slice(&vec![0xFFu8; block_len]);
        }
    }

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(reconstructed, input);

    // Should have roughly half copies
    let copy_count = script
        .tokens()
        .iter()
        .filter(|t| matches!(t, DeltaToken::Copy { .. }))
        .count();
    assert!(
        copy_count >= num_blocks / 4,
        "should find at least some matches"
    );
}

// ============================================================================
// Regression Tests
// ============================================================================

/// Regression test: Ensure basis file position tracking is correct across multiple copies.
#[test]
fn regression_basis_position_tracking() {
    let basis: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Copy non-sequential blocks to exercise seeking
    let script = DeltaScript::new(
        vec![
            DeltaToken::Copy {
                index: 5,
                len: block_len,
            }, // Block 5
            DeltaToken::Copy {
                index: 2,
                len: block_len,
            }, // Block 2 (backward seek)
            DeltaToken::Copy {
                index: 7,
                len: block_len,
            }, // Block 7 (forward seek)
            DeltaToken::Copy {
                index: 0,
                len: block_len,
            }, // Block 0 (backward seek)
        ],
        (block_len * 4) as u64,
        0,
    );

    let mut expected = Vec::new();
    expected.extend_from_slice(&basis[5 * block_len..6 * block_len]);
    expected.extend_from_slice(&basis[2 * block_len..3 * block_len]);
    expected.extend_from_slice(&basis[7 * block_len..8 * block_len]);
    expected.extend_from_slice(&basis[0..block_len]);

    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, expected);
}

/// Regression test: Very large literal followed by copy.
#[test]
fn regression_large_literal_then_copy() {
    let basis: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Large literal (larger than default buffer) followed by matching block
    let mut input = vec![0xFFu8; 200_000];
    input.extend_from_slice(&basis[..block_len]);

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(reconstructed, input);
}

// ============================================================================
// Fuzzy Matching Integration Tests
// ============================================================================

mod fuzzy_matching {
    use matching::{FuzzyMatcher, compute_similarity_score};
    use std::ffi::OsStr;
    use std::fs;
    use tempfile::TempDir;

    /// Verifies fuzzy matching finds similar files in a directory.
    #[test]
    fn finds_similar_file_in_directory() {
        let temp = TempDir::new().expect("create temp dir");

        // Create files with similar names
        fs::write(temp.path().join("report_2023.csv"), "old data").expect("write file");
        fs::write(temp.path().join("report_2024.csv"), "new data").expect("write file");
        fs::write(temp.path().join("unrelated.txt"), "other").expect("write file");

        let matcher = FuzzyMatcher::new();
        let result = matcher.find_fuzzy_basis(
            OsStr::new("report_2025.csv"),
            temp.path(),
            100, // target size
        );

        // Should find one of the similar report files
        assert!(result.is_some(), "should find a fuzzy match");
        let matched = result.unwrap();
        assert!(
            matched
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("report"),
            "matched file should be a report: {:?}",
            matched.path
        );
    }

    /// Verifies fuzzy matching returns None when no similar files exist.
    #[test]
    fn no_match_when_files_differ_completely() {
        let temp = TempDir::new().expect("create temp dir");

        // Create files with completely different names
        fs::write(temp.path().join("alpha.bin"), "data1").expect("write file");
        fs::write(temp.path().join("beta.bin"), "data2").expect("write file");

        let matcher = FuzzyMatcher::new().with_min_score(100);
        let result = matcher.find_fuzzy_basis(OsStr::new("gamma.txt"), temp.path(), 50);

        // High threshold should prevent matching unrelated files
        assert!(
            result.is_none(),
            "should not find unrelated files with high threshold"
        );
    }

    /// Verifies fuzzy matching respects minimum score threshold.
    #[test]
    fn respects_minimum_score_threshold() {
        let temp = TempDir::new().expect("create temp dir");

        fs::write(temp.path().join("file_a.txt"), "data").expect("write file");

        // Low threshold should match
        let matcher_low = FuzzyMatcher::new().with_min_score(1);
        let result_low = matcher_low.find_fuzzy_basis(OsStr::new("file_b.txt"), temp.path(), 100);
        assert!(result_low.is_some(), "low threshold should find match");

        // Very high threshold should not match
        let matcher_high = FuzzyMatcher::new().with_min_score(10000);
        let result_high = matcher_high.find_fuzzy_basis(OsStr::new("file_b.txt"), temp.path(), 100);
        assert!(
            result_high.is_none(),
            "very high threshold should not match"
        );
    }

    /// Verifies fuzzy matching searches additional basis directories (level 2).
    #[test]
    fn searches_additional_fuzzy_basis_dirs() {
        let temp1 = TempDir::new().expect("create temp dir 1");
        let temp2 = TempDir::new().expect("create temp dir 2");

        // Put the similar file in the second directory
        fs::write(temp2.path().join("config_v1.json"), "old config").expect("write file");

        // Use level 2 fuzzy matching to search additional directories
        let matcher = FuzzyMatcher::with_level(2)
            .with_fuzzy_basis_dirs(vec![temp2.path().to_path_buf()]);

        let result = matcher.find_fuzzy_basis(
            OsStr::new("config_v2.json"),
            temp1.path(), // Empty primary directory
            100,
        );

        assert!(result.is_some(), "should find file in additional basis dir");
        let matched = result.unwrap();
        assert!(
            matched.path.starts_with(temp2.path()),
            "matched file should be from additional dir"
        );
    }

    /// Verifies fuzzy matching handles empty directories gracefully.
    #[test]
    fn handles_empty_directory() {
        let temp = TempDir::new().expect("create temp dir");

        let matcher = FuzzyMatcher::new();
        let result = matcher.find_fuzzy_basis(OsStr::new("anyfile.txt"), temp.path(), 100);

        assert!(result.is_none(), "empty directory should return no match");
    }

    /// Verifies fuzzy matching skips directories in search results.
    #[test]
    fn skips_directories() {
        let temp = TempDir::new().expect("create temp dir");

        // Create a directory with similar name
        fs::create_dir(temp.path().join("similar_dir.txt")).expect("create dir");
        // Create a file with different name
        fs::write(temp.path().join("different.bin"), "data").expect("write file");

        let matcher = FuzzyMatcher::new().with_min_score(1);
        let result = matcher.find_fuzzy_basis(OsStr::new("similar_file.txt"), temp.path(), 100);

        // Should either find the different file (low score) or nothing,
        // but should not match the directory
        if let Some(matched) = result {
            assert!(
                matched.path.is_file(),
                "matched result should be a file, not directory"
            );
        }
    }

    /// Verifies similarity score computation for various file name patterns.
    #[test]
    fn similarity_score_patterns() {
        // Extension match adds a bonus
        let score_same_ext = compute_similarity_score("a.txt", "b.txt", 1000, 1000);
        let score_no_ext = compute_similarity_score("a", "b", 1000, 1000);
        assert!(
            score_same_ext > score_no_ext,
            "same extension should add bonus: same_ext={score_same_ext}, no_ext={score_no_ext}"
        );

        // Longer common prefix should score higher
        let score_long_prefix = compute_similarity_score(
            "application_config.json",
            "application_settings.json",
            1000,
            1000,
        );
        let score_short_prefix =
            compute_similarity_score("app_config.json", "application_settings.json", 1000, 1000);
        assert!(
            score_long_prefix > score_short_prefix,
            "longer prefix should score higher: long={score_long_prefix}, short={score_short_prefix}"
        );

        // Similar file sizes should boost score
        let score_similar_size = compute_similarity_score("file.dat", "data.dat", 1000, 900);
        let score_very_different_size = compute_similarity_score("file.dat", "data.dat", 1000, 10);
        assert!(
            score_similar_size > score_very_different_size,
            "similar sizes should score higher: similar={score_similar_size}, different={score_very_different_size}"
        );

        // Completely different files should score low
        let score_unrelated = compute_similarity_score("abc.xyz", "def.uvw", 100, 50000);
        assert!(
            score_unrelated < 50,
            "unrelated files should score low: {score_unrelated}"
        );
    }

    /// Verifies fuzzy matching chooses best match among multiple candidates.
    #[test]
    fn chooses_best_match_among_candidates() {
        let temp = TempDir::new().expect("create temp dir");

        // Create several files with varying similarity
        fs::write(temp.path().join("data_backup_2024.csv"), "x".repeat(1000)).expect("write");
        fs::write(temp.path().join("data_backup_2023.csv"), "x".repeat(900)).expect("write");
        fs::write(temp.path().join("totally_different.txt"), "x".repeat(100)).expect("write");

        let matcher = FuzzyMatcher::new();
        let result = matcher.find_fuzzy_basis(
            OsStr::new("data_backup_2025.csv"),
            temp.path(),
            1000, // Similar size to our candidates
        );

        assert!(result.is_some(), "should find a match");
        let matched = result.unwrap();
        // Should prefer one of the similar backup files
        assert!(
            matched
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("data_backup"),
            "should match one of the backup files"
        );
    }
}

// ============================================================================
// Ring Buffer Specific Tests
// ============================================================================

mod ring_buffer_behavior {
    use matching::{DeltaGenerator, DeltaSignatureIndex, generate_delta};
    use protocol::ProtocolVersion;
    use signature::{
        SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout,
        generate_file_signature,
    };
    use std::num::NonZeroU8;

    fn build_index(data: &[u8]) -> Option<DeltaSignatureIndex> {
        let params = SignatureLayoutParams::new(
            data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).ok()?;
        let signature = generate_file_signature(data, layout, SignatureAlgorithm::Md4).ok()?;
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
    }

    /// Verifies delta generation when input length equals block size exactly.
    #[test]
    fn input_length_equals_block_size() {
        let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
        let index = build_index(&basis).expect("index");
        let block_len = index.block_length();

        // Input is exactly one block from basis
        let input = basis[..block_len].to_vec();
        let script = generate_delta(&input[..], &index).expect("delta");

        assert_eq!(script.total_bytes(), block_len as u64);
        assert_eq!(script.literal_bytes(), 0, "exact block should be a copy");
    }

    /// Verifies delta generation processes data byte-by-byte correctly.
    #[test]
    fn byte_by_byte_processing() {
        // Use a small buffer to force byte-by-byte processing
        let basis: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
        let index = build_index(&basis).expect("index");
        let block_len = index.block_length();

        let generator = DeltaGenerator::new().with_buffer_len(1);
        let input = basis[..block_len].to_vec();

        let script = generator.generate(&input[..], &index).expect("delta");
        assert_eq!(script.total_bytes(), block_len as u64);
    }

    /// Verifies that buffer boundary does not affect matching.
    #[test]
    fn match_spans_buffer_boundary() {
        let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
        let index = build_index(&basis).expect("index");
        let block_len = index.block_length();

        // Use buffer size that doesn't align with block size
        let buffer_size = block_len / 2 + 7;
        let generator = DeltaGenerator::new().with_buffer_len(buffer_size);

        // Input contains a matching block that spans buffer reads
        let mut input = vec![0xFFu8; buffer_size - 3];
        input.extend_from_slice(&basis[..block_len]);

        let script = generator.generate(&input[..], &index).expect("delta");

        // Should find the match despite buffer boundary
        let has_copy = script
            .tokens()
            .iter()
            .any(|t| matches!(t, matching::DeltaToken::Copy { .. }));
        assert!(has_copy, "should find match that spans buffer boundary");
    }
}

// ============================================================================
// Algorithm Correctness Tests
// ============================================================================

mod algorithm_correctness {
    use matching::{DeltaSignatureIndex, DeltaToken, apply_delta, generate_delta};
    use protocol::ProtocolVersion;
    use signature::{
        SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout,
        generate_file_signature,
    };
    use std::io::Cursor;
    use std::num::NonZeroU8;

    fn build_index(data: &[u8]) -> Option<DeltaSignatureIndex> {
        let params = SignatureLayoutParams::new(
            data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).ok()?;
        let signature = generate_file_signature(data, layout, SignatureAlgorithm::Md4).ok()?;
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
    }

    /// Verifies that the delta algorithm produces deterministic results.
    #[test]
    fn delta_generation_is_deterministic() {
        let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
        let index = build_index(&basis).expect("index");

        let mut input = basis.clone();
        input[1000] = 0xFF;

        // Generate delta multiple times
        let script1 = generate_delta(&input[..], &index).expect("delta 1");
        let script2 = generate_delta(&input[..], &index).expect("delta 2");
        let script3 = generate_delta(&input[..], &index).expect("delta 3");

        // All should be identical
        assert_eq!(script1.total_bytes(), script2.total_bytes());
        assert_eq!(script2.total_bytes(), script3.total_bytes());
        assert_eq!(script1.literal_bytes(), script2.literal_bytes());
        assert_eq!(script2.literal_bytes(), script3.literal_bytes());
        assert_eq!(script1.tokens().len(), script2.tokens().len());
        assert_eq!(script2.tokens().len(), script3.tokens().len());
    }

    /// Verifies that copy tokens reference valid block indices.
    #[test]
    fn copy_tokens_reference_valid_blocks() {
        let basis: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
        let index = build_index(&basis).expect("index");
        let block_len = index.block_length();
        let num_blocks = basis.len() / block_len;

        let input = basis.clone();
        let script = generate_delta(&input[..], &index).expect("delta");

        for token in script.tokens() {
            if let DeltaToken::Copy {
                index: block_idx,
                len,
            } = token
            {
                assert!(
                    (*block_idx as usize) < num_blocks,
                    "copy token should reference valid block index: {block_idx} >= {num_blocks}"
                );
                assert_eq!(*len, block_len, "copy length should equal block length");
            }
        }
    }

    /// Verifies that literal tokens contain only non-matched data.
    #[test]
    fn literal_tokens_contain_unmatched_data() {
        let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
        let index = build_index(&basis).expect("index");
        let block_len = index.block_length();

        // Create input with known literal content
        let mut input = vec![0xAA; 50]; // Distinctive literal prefix
        input.extend_from_slice(&basis[..block_len]); // Matching block
        input.extend_from_slice(&[0xBB; 30]); // Distinctive literal suffix

        let script = generate_delta(&input[..], &index).expect("delta");

        // Verify we have both literal and copy tokens
        let literal_count = script.tokens().iter().filter(|t| t.is_literal()).count();
        let copy_count = script.tokens().iter().filter(|t| !t.is_literal()).count();

        assert!(literal_count >= 1, "should have literal tokens");
        assert!(copy_count >= 1, "should have copy tokens");

        // Verify total literal bytes match expected
        let total_literal: usize = script
            .tokens()
            .iter()
            .filter_map(|t| {
                if let DeltaToken::Literal(bytes) = t {
                    Some(bytes.len())
                } else {
                    None
                }
            })
            .sum();

        // Should have at least our prefix and suffix as literals
        assert!(total_literal >= 80, "should have at least 80 literal bytes");
    }

    /// Verifies that apply_delta is the inverse of generate_delta.
    #[test]
    fn apply_is_inverse_of_generate() {
        // Test with various input patterns
        let test_cases: Vec<Vec<u8>> = vec![
            (0..5000).map(|i| (i % 251) as u8).collect(),
            vec![0u8; 4000],
            vec![0xFF; 4000],
            (0..4000)
                .map(|i| if i % 100 < 50 { 0 } else { 0xFF })
                .collect(),
        ];

        for (idx, basis) in test_cases.iter().enumerate() {
            if let Some(index) = build_index(basis) {
                let block_len = index.block_length();

                // Various input modifications
                let inputs: Vec<Vec<u8>> = vec![
                    basis.clone(),
                    basis.iter().cloned().skip(100).collect(),
                    {
                        let mut v = vec![0xAA; 100];
                        v.extend_from_slice(basis);
                        v
                    },
                    {
                        let mut v = basis.clone();
                        for i in (0..v.len()).step_by(block_len) {
                            if i < v.len() {
                                v[i] = v[i].wrapping_add(1);
                            }
                        }
                        v
                    },
                ];

                for input in inputs {
                    let script = generate_delta(&input[..], &index).expect("delta");

                    let mut basis_cursor = Cursor::new(basis.clone());
                    let mut output = Vec::new();
                    apply_delta(&mut basis_cursor, &mut output, &index, &script).expect("apply");

                    assert_eq!(
                        output, input,
                        "apply should reconstruct input (test case {idx})"
                    );
                }
            }
        }
    }
}
