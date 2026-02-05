//! Integration tests for delta reconstruction in the engine crate.
//!
//! These tests verify that the delta reconstruction pipeline correctly:
//! - Applies delta blocks (copy tokens) from a basis file
//! - Inserts literal data
//! - Handles mixed delta/literal sequences
//! - Reconstructs the target file to match the source
//!
//! The tests exercise the engine crate's re-exports of delta functionality
//! from the matching crate, ensuring the integration works correctly for
//! typical rsync transfer scenarios.

use engine::{
    DeltaScript, DeltaSignatureIndex, DeltaToken, SignatureLayoutParams, apply_delta,
    calculate_signature_layout, generate_delta, generate_file_signature,
};
use protocol::ProtocolVersion;
use signature::SignatureAlgorithm;
use std::io::Cursor;
use std::num::{NonZeroU32, NonZeroU8};

// ============================================================================
// Test Utilities
// ============================================================================

/// Creates a signature index from the provided basis data.
fn create_signature_index(basis: &[u8]) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        basis.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("valid layout");
    let signature = generate_file_signature(basis, layout, SignatureAlgorithm::Md4)
        .expect("signature generation");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
        .expect("index creation")
}

/// Creates a signature index with a specific block size hint.
fn create_signature_index_with_block_size(basis: &[u8], block_hint: u32) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        basis.len() as u64,
        NonZeroU32::new(block_hint),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("valid layout");
    let signature = generate_file_signature(basis, layout, SignatureAlgorithm::Md4)
        .expect("signature generation");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
        .expect("index creation")
}

/// Applies a delta script to reconstruct the target file.
fn reconstruct_file(
    basis: &[u8],
    index: &DeltaSignatureIndex,
    script: &DeltaScript,
) -> Vec<u8> {
    let mut basis_cursor = Cursor::new(basis);
    let mut output = Vec::new();
    apply_delta(&mut basis_cursor, &mut output, index, script)
        .expect("delta application should succeed");
    output
}

/// Generates a delta and reconstructs the target, verifying it matches.
fn generate_and_verify(basis: &[u8], target: &[u8]) -> DeltaScript {
    let index = create_signature_index(basis);
    let script = generate_delta(target, &index).expect("delta generation should succeed");
    let reconstructed = reconstruct_file(basis, &index, &script);
    assert_eq!(
        reconstructed, target,
        "reconstructed file should match target"
    );
    script
}

// ============================================================================
// Delta Block Application Tests
// ============================================================================

/// Verifies that a single delta block (copy token) is applied correctly.
#[test]
fn single_delta_block_applied_correctly() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = create_signature_index(&basis);
    let block_len = index.block_length();

    // Script that copies the first block
    let script = DeltaScript::new(
        vec![DeltaToken::Copy {
            index: 0,
            len: block_len,
        }],
        block_len as u64,
        0,
    );

    let reconstructed = reconstruct_file(&basis, &index, &script);
    assert_eq!(
        reconstructed,
        &basis[..block_len],
        "single block should be copied correctly"
    );
}

/// Verifies that multiple sequential delta blocks are applied correctly.
#[test]
fn multiple_sequential_delta_blocks() {
    let basis: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
    let index = create_signature_index(&basis);
    let block_len = index.block_length();

    // Script that copies three sequential blocks
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
            DeltaToken::Copy {
                index: 2,
                len: block_len,
            },
        ],
        (block_len * 3) as u64,
        0,
    );

    let reconstructed = reconstruct_file(&basis, &index, &script);
    assert_eq!(
        reconstructed,
        &basis[..block_len * 3],
        "multiple sequential blocks should be copied correctly"
    );
}

/// Verifies that non-sequential delta blocks (out of order) are applied correctly.
#[test]
fn non_sequential_delta_blocks() {
    let basis: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
    let index = create_signature_index(&basis);
    let block_len = index.block_length();

    // Script that copies blocks in non-sequential order: 2, 0, 3, 1
    let script = DeltaScript::new(
        vec![
            DeltaToken::Copy {
                index: 2,
                len: block_len,
            },
            DeltaToken::Copy {
                index: 0,
                len: block_len,
            },
            DeltaToken::Copy {
                index: 3,
                len: block_len,
            },
            DeltaToken::Copy {
                index: 1,
                len: block_len,
            },
        ],
        (block_len * 4) as u64,
        0,
    );

    let reconstructed = reconstruct_file(&basis, &index, &script);

    // Build expected output with blocks in the specified order
    let mut expected = Vec::new();
    expected.extend_from_slice(&basis[2 * block_len..3 * block_len]);
    expected.extend_from_slice(&basis[0..block_len]);
    expected.extend_from_slice(&basis[3 * block_len..4 * block_len]);
    expected.extend_from_slice(&basis[block_len..2 * block_len]);

    assert_eq!(
        reconstructed, expected,
        "non-sequential blocks should be copied in the correct order"
    );
}

/// Verifies that the same delta block can be referenced multiple times.
#[test]
fn duplicate_delta_block_references() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = create_signature_index(&basis);
    let block_len = index.block_length();

    // Script that copies block 0 three times
    let script = DeltaScript::new(
        vec![
            DeltaToken::Copy {
                index: 0,
                len: block_len,
            },
            DeltaToken::Copy {
                index: 0,
                len: block_len,
            },
            DeltaToken::Copy {
                index: 0,
                len: block_len,
            },
        ],
        (block_len * 3) as u64,
        0,
    );

    let reconstructed = reconstruct_file(&basis, &index, &script);

    // Build expected output with block 0 repeated three times
    let mut expected = Vec::new();
    for _ in 0..3 {
        expected.extend_from_slice(&basis[..block_len]);
    }

    assert_eq!(
        reconstructed, expected,
        "duplicate block references should work correctly"
    );
}

/// Verifies that partial block copies work correctly.
#[test]
fn partial_delta_block_copy() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = create_signature_index(&basis);
    let block_len = index.block_length();

    // Script that copies only part of a block
    let partial_len = block_len / 2;
    let script = DeltaScript::new(
        vec![DeltaToken::Copy {
            index: 0,
            len: partial_len,
        }],
        partial_len as u64,
        0,
    );

    let reconstructed = reconstruct_file(&basis, &index, &script);
    assert_eq!(
        reconstructed,
        &basis[..partial_len],
        "partial block copy should work correctly"
    );
}

// ============================================================================
// Literal Data Insertion Tests
// ============================================================================

/// Verifies that a single literal token is inserted correctly.
#[test]
fn single_literal_inserted_correctly() {
    let basis: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let index = create_signature_index(&basis);

    let literal_data = b"Hello, World!";
    let script = DeltaScript::new(
        vec![DeltaToken::Literal(literal_data.to_vec())],
        literal_data.len() as u64,
        literal_data.len() as u64,
    );

    let reconstructed = reconstruct_file(&basis, &index, &script);
    assert_eq!(
        reconstructed, literal_data,
        "single literal should be inserted correctly"
    );
}

/// Verifies that multiple consecutive literal tokens are inserted correctly.
#[test]
fn multiple_consecutive_literals() {
    let basis: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let index = create_signature_index(&basis);

    let literal1 = b"First ";
    let literal2 = b"Second ";
    let literal3 = b"Third";
    let total_len = literal1.len() + literal2.len() + literal3.len();

    let script = DeltaScript::new(
        vec![
            DeltaToken::Literal(literal1.to_vec()),
            DeltaToken::Literal(literal2.to_vec()),
            DeltaToken::Literal(literal3.to_vec()),
        ],
        total_len as u64,
        total_len as u64,
    );

    let reconstructed = reconstruct_file(&basis, &index, &script);
    assert_eq!(
        reconstructed,
        b"First Second Third",
        "multiple literals should be concatenated correctly"
    );
}

/// Verifies that large literal data is inserted correctly.
#[test]
fn large_literal_insertion() {
    let basis: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let index = create_signature_index(&basis);

    // Create a large literal (larger than typical I/O buffer)
    let literal_data: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();
    let script = DeltaScript::new(
        vec![DeltaToken::Literal(literal_data.clone())],
        literal_data.len() as u64,
        literal_data.len() as u64,
    );

    let reconstructed = reconstruct_file(&basis, &index, &script);
    assert_eq!(
        reconstructed, literal_data,
        "large literal should be inserted correctly"
    );
}

/// Verifies that empty literal tokens are handled correctly.
#[test]
fn empty_literal_tokens() {
    let basis: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let index = create_signature_index(&basis);

    let script = DeltaScript::new(
        vec![
            DeltaToken::Literal(b"Before".to_vec()),
            DeltaToken::Literal(vec![]),
            DeltaToken::Literal(b"After".to_vec()),
        ],
        11,
        11,
    );

    let reconstructed = reconstruct_file(&basis, &index, &script);
    assert_eq!(
        reconstructed,
        b"BeforeAfter",
        "empty literals should not break reconstruction"
    );
}

// ============================================================================
// Mixed Delta/Literal Tests
// ============================================================================

/// Verifies that alternating delta and literal tokens work correctly.
#[test]
fn alternating_delta_and_literal() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = create_signature_index(&basis);
    let block_len = index.block_length();

    let literal1 = b"HEADER";
    let literal2 = b"FOOTER";
    let total_len = literal1.len() + block_len + literal2.len();

    let script = DeltaScript::new(
        vec![
            DeltaToken::Literal(literal1.to_vec()),
            DeltaToken::Copy {
                index: 0,
                len: block_len,
            },
            DeltaToken::Literal(literal2.to_vec()),
        ],
        total_len as u64,
        (literal1.len() + literal2.len()) as u64,
    );

    let reconstructed = reconstruct_file(&basis, &index, &script);

    let mut expected = Vec::new();
    expected.extend_from_slice(literal1);
    expected.extend_from_slice(&basis[..block_len]);
    expected.extend_from_slice(literal2);

    assert_eq!(
        reconstructed, expected,
        "alternating delta and literal should work correctly"
    );
}

/// Verifies complex interleaving of delta blocks and literals.
#[test]
fn complex_delta_literal_interleaving() {
    let basis: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
    let index = create_signature_index(&basis);
    let block_len = index.block_length();

    let literal1 = b"START";
    let literal2 = b"MIDDLE";
    let literal3 = b"END";
    let total_len = literal1.len() + (block_len * 2) + literal2.len() + block_len + literal3.len();

    let script = DeltaScript::new(
        vec![
            DeltaToken::Literal(literal1.to_vec()),
            DeltaToken::Copy {
                index: 0,
                len: block_len,
            },
            DeltaToken::Copy {
                index: 1,
                len: block_len,
            },
            DeltaToken::Literal(literal2.to_vec()),
            DeltaToken::Copy {
                index: 2,
                len: block_len,
            },
            DeltaToken::Literal(literal3.to_vec()),
        ],
        total_len as u64,
        (literal1.len() + literal2.len() + literal3.len()) as u64,
    );

    let reconstructed = reconstruct_file(&basis, &index, &script);

    let mut expected = Vec::new();
    expected.extend_from_slice(literal1);
    expected.extend_from_slice(&basis[..block_len]);
    expected.extend_from_slice(&basis[block_len..block_len * 2]);
    expected.extend_from_slice(literal2);
    expected.extend_from_slice(&basis[block_len * 2..block_len * 3]);
    expected.extend_from_slice(literal3);

    assert_eq!(
        reconstructed, expected,
        "complex interleaving should work correctly"
    );
}

/// Verifies that a literal in the middle of sequential blocks works.
#[test]
fn literal_between_sequential_blocks() {
    let basis: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
    let index = create_signature_index(&basis);
    let block_len = index.block_length();

    let literal = b"INSERTED";
    let total_len = (block_len * 3) + literal.len();

    let script = DeltaScript::new(
        vec![
            DeltaToken::Copy {
                index: 0,
                len: block_len,
            },
            DeltaToken::Literal(literal.to_vec()),
            DeltaToken::Copy {
                index: 1,
                len: block_len,
            },
            DeltaToken::Copy {
                index: 2,
                len: block_len,
            },
        ],
        total_len as u64,
        literal.len() as u64,
    );

    let reconstructed = reconstruct_file(&basis, &index, &script);

    let mut expected = Vec::new();
    expected.extend_from_slice(&basis[..block_len]);
    expected.extend_from_slice(literal);
    expected.extend_from_slice(&basis[block_len..block_len * 3]);

    assert_eq!(
        reconstructed, expected,
        "literal between sequential blocks should work correctly"
    );
}

// ============================================================================
// File Matching Tests (Source Reconstruction)
// ============================================================================

/// Verifies that an identical file reconstructs perfectly with all delta blocks.
#[test]
fn identical_file_matches_source() {
    let basis: Vec<u8> = (0..10000).map(|i| (i % 251) as u8).collect();
    let target = basis.clone();

    let script = generate_and_verify(&basis, &target);

    // Should be mostly copies
    let copy_bytes = script.copy_bytes();
    let literal_bytes = script.literal_bytes();
    assert!(
        copy_bytes > literal_bytes * 10,
        "identical file should be mostly delta blocks: copy={}, literal={}",
        copy_bytes,
        literal_bytes
    );
}

/// Verifies that a file with a small modification reconstructs correctly.
#[test]
fn modified_file_matches_source() {
    let basis: Vec<u8> = (0..10000).map(|i| (i % 251) as u8).collect();
    let mut target = basis.clone();

    // Modify a small region
    for i in 5000..5100 {
        target[i] = 0xFF;
    }

    let script = generate_and_verify(&basis, &target);

    // Should have both copies and literals
    assert!(
        script.copy_bytes() > 0,
        "modified file should have delta blocks"
    );
    assert!(
        script.literal_bytes() > 0,
        "modified file should have literals"
    );
}

/// Verifies that a file with insertions reconstructs correctly.
#[test]
fn file_with_insertions_matches_source() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = create_signature_index(&basis);
    let _block_len = index.block_length();

    let mut target = basis[..5000].to_vec();
    target.extend_from_slice(b"INSERTED DATA HERE");
    target.extend_from_slice(&basis[5000..]);

    generate_and_verify(&basis, &target);
}

/// Verifies that a file with deletions reconstructs correctly.
#[test]
fn file_with_deletions_matches_source() {
    let basis: Vec<u8> = (0..10000).map(|i| (i % 251) as u8).collect();
    let index = create_signature_index(&basis);
    let _block_len = index.block_length();

    // Delete middle portion
    let mut target = basis[..3000].to_vec();
    target.extend_from_slice(&basis[7000..]);

    generate_and_verify(&basis, &target);
}

/// Verifies that a completely different file reconstructs correctly with all literals.
#[test]
fn completely_different_file_matches_source() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let target: Vec<u8> = (0..5000).map(|i| ((i * 7 + 13) % 256) as u8).collect();

    let script = generate_and_verify(&basis, &target);

    // Should be all literals
    assert_eq!(
        script.literal_bytes(),
        target.len() as u64,
        "completely different file should be all literals"
    );
}

/// Verifies that a file with reordered blocks reconstructs correctly.
#[test]
fn file_with_reordered_blocks_matches_source() {
    let basis: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
    let index = create_signature_index(&basis);
    let block_len = index.block_length();

    // Reorder blocks: 2, 0, 3, 1
    let mut target = Vec::new();
    target.extend_from_slice(&basis[2 * block_len..3 * block_len]);
    target.extend_from_slice(&basis[0..block_len]);
    target.extend_from_slice(&basis[3 * block_len..4 * block_len]);
    target.extend_from_slice(&basis[block_len..2 * block_len]);

    let script = generate_and_verify(&basis, &target);

    // Should find all blocks despite reordering
    let copy_count = script
        .tokens()
        .iter()
        .filter(|t| matches!(t, DeltaToken::Copy { .. }))
        .count();
    assert!(
        copy_count >= 4,
        "should find all reordered blocks: found {}",
        copy_count
    );
}

// ============================================================================
// Edge Cases
// ============================================================================

/// Verifies reconstruction with an empty script produces empty output.
#[test]
fn empty_script_produces_empty_output() {
    let basis: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let index = create_signature_index(&basis);

    let script = DeltaScript::new(vec![], 0, 0);

    let reconstructed = reconstruct_file(&basis, &index, &script);
    assert!(
        reconstructed.is_empty(),
        "empty script should produce empty output"
    );
}

/// Verifies reconstruction with only literals (no delta blocks).
#[test]
fn only_literals_no_delta_blocks() {
    let basis: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let index = create_signature_index(&basis);

    let literal_data = b"This is only literal data, no blocks referenced.";
    let script = DeltaScript::new(
        vec![DeltaToken::Literal(literal_data.to_vec())],
        literal_data.len() as u64,
        literal_data.len() as u64,
    );

    let reconstructed = reconstruct_file(&basis, &index, &script);
    assert_eq!(reconstructed, literal_data);
}

/// Verifies reconstruction with only delta blocks (no literals).
#[test]
fn only_delta_blocks_no_literals() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = create_signature_index(&basis);
    let block_len = index.block_length();

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

    let reconstructed = reconstruct_file(&basis, &index, &script);
    assert_eq!(reconstructed, &basis[..block_len * 2]);
}

/// Verifies reconstruction with a single byte.
#[test]
fn single_byte_reconstruction() {
    let basis: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let target = vec![42u8];

    generate_and_verify(&basis, &target);
}

/// Verifies reconstruction with different block sizes.
#[test]
fn different_block_sizes() {
    let basis: Vec<u8> = (0..32768).map(|i| (i % 251) as u8).collect();

    // Test with small block size
    let index_small = create_signature_index_with_block_size(&basis, 128);
    let target = basis[..5000].to_vec();
    let script_small = generate_delta(target.as_slice(), &index_small).expect("delta generation");
    let reconstructed_small = reconstruct_file(&basis, &index_small, &script_small);
    assert_eq!(reconstructed_small, target, "small block size failed");

    // Test with large block size
    let index_large = create_signature_index_with_block_size(&basis, 4096);
    let script_large = generate_delta(target.as_slice(), &index_large).expect("delta generation");
    let reconstructed_large = reconstruct_file(&basis, &index_large, &script_large);
    assert_eq!(reconstructed_large, target, "large block size failed");
}

// ============================================================================
// Real-World Scenario Tests
// ============================================================================

/// Simulates a text file with a line inserted in the middle.
/// Uses a larger file to ensure blocks are generated (small files have no full blocks).
#[test]
fn text_file_line_insertion() {
    // Create a larger file to ensure full blocks are generated
    // (rsync doesn't use delta transfer for files smaller than block size)
    let mut original_lines = Vec::new();
    for i in 1..=200 {
        original_lines.push(format!("Line {:03}: This is some text content for the file.\n", i));
    }
    let original_text = original_lines.join("");

    let mut modified_lines = original_lines.clone();
    modified_lines.insert(100, "INSERTED LINE: This is new content added in the middle.\n".to_string());
    let modified_text = modified_lines.join("");

    let basis = original_text.as_bytes();
    let target = modified_text.as_bytes();

    generate_and_verify(basis, target);
}

/// Simulates a binary file with a header modification.
#[test]
fn binary_file_header_modification() {
    // Simulate a binary file with a header
    let mut basis = vec![0xFF, 0xFE, 0x01, 0x00]; // Original header
    basis.extend((0..10000).map(|i| (i % 256) as u8));

    let mut target = vec![0xFF, 0xFE, 0x02, 0x00]; // Modified header (version bump)
    target.extend((0..10000).map(|i| (i % 256) as u8));

    generate_and_verify(&basis, &target);
}

/// Simulates a file with appended data (log file scenario).
#[test]
fn file_with_appended_data() {
    let basis: Vec<u8> = (0..5000).map(|i| (i % 251) as u8).collect();
    let mut target = basis.clone();
    target.extend_from_slice(b"New log entries appended here...\n");
    target.extend((5000..6000).map(|i| (i % 256) as u8));

    let script = generate_and_verify(&basis, &target);

    // Should have literals for the appended data
    assert!(
        script.literal_bytes() > 0,
        "appended data should be literals"
    );
}

/// Simulates a sparse modification pattern (patching).
#[test]
fn sparse_modification_pattern() {
    let basis: Vec<u8> = (0..20000).map(|i| (i % 251) as u8).collect();
    let mut target = basis.clone();

    // Make small modifications at regular intervals
    for i in (0..target.len()).step_by(1000) {
        if i + 10 <= target.len() {
            for j in 0..10 {
                target[i + j] = 0xFF;
            }
        }
    }

    let script = generate_and_verify(&basis, &target);

    // Should have both copies and literals
    assert!(script.copy_bytes() > 0, "should have delta blocks");
    assert!(script.literal_bytes() > 0, "should have literals");
}

/// Simulates a large file transfer scenario.
#[test]
fn large_file_reconstruction() {
    // 1 MB file
    let basis: Vec<u8> = (0..1_048_576).map(|i| (i % 251) as u8).collect();
    let mut target = basis.clone();

    // Modify a small section in the middle
    let mid = target.len() / 2;
    for i in 0..1000 {
        target[mid + i] = 0xAA;
    }

    let script = generate_and_verify(&basis, &target);

    // Most should be copies
    assert!(
        script.copy_bytes() > script.literal_bytes() * 100,
        "large file should be mostly delta blocks"
    );
}
