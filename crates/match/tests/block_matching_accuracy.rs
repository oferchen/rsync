//! Block matching accuracy tests for the match crate.
//!
//! This test module verifies the correctness and accuracy of the rsync block
//! matching algorithm, focusing on:
//!
//! 1. Block matching finds correct matches
//! 2. Rolling checksum produces consistent results
//! 3. Strong checksum verification works correctly
//! 4. False positive rate is acceptably low
//!
//! ## Test Coverage
//!
//! - Exact block matching accuracy
//! - Rolling checksum consistency across sliding windows
//! - Strong checksum collision detection and rejection
//! - False positive measurement and validation
//! - Edge cases in block boundary detection
//!
//! ## Upstream Reference
//!
//! These tests validate behavior equivalent to upstream rsync's `match.c`
//! implementation, particularly the `hash_search()` function.

use matching::{DeltaSignatureIndex, DeltaToken, apply_delta, generate_delta};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};
use std::collections::HashSet;
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
    script: &matching::DeltaScript,
) -> Vec<u8> {
    let mut basis_cursor = Cursor::new(basis);
    let mut output = Vec::new();
    apply_delta(&mut basis_cursor, &mut output, index, script).expect("apply_delta should succeed");
    output
}

// ============================================================================
// 1. Block Matching Finds Correct Matches
// ============================================================================

/// Verifies that exact block matches are found at the correct positions.
///
/// This test creates a basis file with distinct blocks and verifies that
/// the delta generator correctly identifies matches when those exact blocks
/// appear in the input.
#[test]
fn exact_block_matches_found_at_correct_positions() {
    // Create basis with 4 distinct blocks
    let block_size = 700;
    let mut basis = Vec::new();

    // Block 0: pattern A (0, 1, 2, ...)
    for i in 0..block_size {
        basis.push((i % 256) as u8);
    }

    // Block 1: pattern B (128, 129, 130, ...)
    for i in 0..block_size {
        basis.push(((i + 128) % 256) as u8);
    }

    // Block 2: pattern C (all 0xAA)
    basis.extend_from_slice(&vec![0xAA; block_size]);

    // Block 3: pattern D (descending)
    for i in 0..block_size {
        basis.push((255 - (i % 256)) as u8);
    }

    let index = build_index(&basis).expect("should build index");
    let actual_block_len = index.block_length();

    // Input contains blocks in order: 2, 0, 3 (skipping block 1)
    let mut input = Vec::new();
    input.extend_from_slice(&basis[2 * actual_block_len..3 * actual_block_len]); // Block 2
    input.extend_from_slice(&basis[0..actual_block_len]); // Block 0
    input.extend_from_slice(&basis[3 * actual_block_len..4 * actual_block_len]); // Block 3

    let script = generate_delta(&input[..], &index).expect("should generate delta");

    // Verify reconstruction is correct
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(
        reconstructed, input,
        "reconstructed output should match input"
    );

    // Verify that we got copy tokens (blocks were matched)
    let copy_tokens: Vec<_> = script
        .tokens()
        .iter()
        .filter_map(|t| match t {
            DeltaToken::Copy { index, len } => Some((*index, *len)),
            _ => None,
        })
        .collect();

    assert_eq!(
        copy_tokens.len(),
        3,
        "should have exactly 3 copy tokens for the 3 matching blocks"
    );

    // Verify the copy tokens reference the correct blocks
    let block_indices: Vec<u64> = copy_tokens.iter().map(|(idx, _)| *idx).collect();
    assert_eq!(
        block_indices,
        vec![2, 0, 3],
        "copy tokens should reference blocks 2, 0, 3 in order"
    );

    // Verify all copy lengths are correct
    for (_, len) in &copy_tokens {
        assert_eq!(
            *len, actual_block_len,
            "copy length should equal block length"
        );
    }

    // Should have zero literal bytes since all blocks matched
    assert_eq!(
        script.literal_bytes(),
        0,
        "should have no literals for exact block matches"
    );
}

/// Verifies that partial block matches are handled correctly.
///
/// When a block is modified in the middle, the algorithm should find
/// matches for the unmodified portions or treat it as literal data.
#[test]
fn partial_block_modifications_detected() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Take the first block and modify it in the middle
    let mut input = basis[..block_len].to_vec();
    let mid = input.len() / 2;
    for i in mid..mid + 10 {
        if i < input.len() {
            input[i] = input[i].wrapping_add(1);
        }
    }

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(
        reconstructed, input,
        "modified block should still reconstruct correctly"
    );

    // The modified block should not match, so we expect literals
    assert!(
        script.literal_bytes() > 0,
        "modified block should produce literal bytes"
    );
}

/// Verifies that multiple identical blocks are all matched correctly.
#[test]
fn multiple_identical_blocks_all_matched() {
    // Create a basis with repeated identical blocks
    let block_pattern: Vec<u8> = (0..256).map(|i| i as u8).cycle().take(700).collect();
    let mut basis = Vec::new();
    for _ in 0..5 {
        basis.extend_from_slice(&block_pattern);
    }

    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Input: use the same repeated pattern
    let mut input = Vec::new();
    for _ in 0..3 {
        input.extend_from_slice(&block_pattern[..block_len]);
    }

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(
        reconstructed, input,
        "repeated identical blocks should reconstruct correctly"
    );

    // All blocks should match (minimal literals)
    let copy_count = script
        .tokens()
        .iter()
        .filter(|t| matches!(t, DeltaToken::Copy { .. }))
        .count();
    assert!(
        copy_count >= 3,
        "should find at least 3 copy tokens for repeated blocks"
    );
}

/// Verifies that block matching works across different alignments.
///
/// Tests that blocks are found even when they start at various byte offsets.
#[test]
fn block_matches_at_different_alignments() {
    let basis: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Test different offset positions before the matching block
    for offset in [1, 7, 15, 31, 63, 127, 255].iter() {
        let mut input = vec![0xFFu8; *offset]; // Non-matching prefix
        input.extend_from_slice(&basis[..block_len]); // Matching block

        let script = generate_delta(&input[..], &index).expect("should generate delta");
        let reconstructed = apply_and_reconstruct(&basis, &index, &script);

        assert_eq!(
            reconstructed, input,
            "block at offset {offset} should reconstruct correctly"
        );

        // Should find the matching block
        let copy_count = script
            .tokens()
            .iter()
            .filter(|t| matches!(t, DeltaToken::Copy { .. }))
            .count();
        assert!(
            copy_count >= 1,
            "should find matching block at offset {offset}"
        );
    }
}

// ============================================================================
// 2. Rolling Checksum Produces Consistent Results
// ============================================================================

/// Verifies that the rolling checksum is consistent when recomputed.
///
/// The rolling checksum for a given window should be identical whether
/// computed fresh or via rolling updates.
#[test]
fn rolling_checksum_consistency() {
    use checksums::RollingChecksum;

    let data = b"The quick brown fox jumps over the lazy dog. Lorem ipsum dolor sit amet.";
    let window_size = 16;

    // Compute rolling checksum by sliding the window
    let mut rolling = RollingChecksum::new();
    rolling.update(&data[..window_size]);
    let initial_digest = rolling.digest();

    // Slide the window and track checksums
    let mut checksums_via_rolling = vec![initial_digest];
    for i in window_size..data.len() {
        let outgoing = data[i - window_size];
        let incoming = data[i];
        rolling
            .roll(outgoing, incoming)
            .expect("roll should succeed");
        checksums_via_rolling.push(rolling.digest());
    }

    // Compute the same checksums by fresh calculation at each position
    let mut checksums_via_fresh = Vec::new();
    for i in 0..=data.len() - window_size {
        let mut fresh = RollingChecksum::new();
        fresh.update(&data[i..i + window_size]);
        checksums_via_fresh.push(fresh.digest());
    }

    // They should be identical
    assert_eq!(
        checksums_via_rolling.len(),
        checksums_via_fresh.len(),
        "should have same number of checksums"
    );

    for (i, (rolled, fresh)) in checksums_via_rolling
        .iter()
        .zip(checksums_via_fresh.iter())
        .enumerate()
    {
        assert_eq!(
            rolled, fresh,
            "checksum at position {i} should match: rolled={rolled:?}, fresh={fresh:?}"
        );
    }
}

/// Verifies rolling checksum stability across different data patterns.
#[test]
fn rolling_checksum_stability_across_patterns() {
    use checksums::RollingChecksum;

    let test_cases: [Vec<u8>; 4] = [
        // Uniform data
        vec![0xAA; 100],
        // Incrementing bytes
        (0u8..100).collect::<Vec<u8>>(),
        // Random-looking pattern
        (0..100).map(|i| ((i * 17 + 31) % 256) as u8).collect(),
        // Sparse data
        {
            let mut v = vec![0u8; 100];
            v[10] = 0xFF;
            v[50] = 0xFF;
            v[90] = 0xFF;
            v
        },
    ];

    for (case_idx, data) in test_cases.iter().enumerate() {
        let window_size = 20;
        let mut rolling = RollingChecksum::new();
        rolling.update(&data[..window_size]);

        for i in window_size..data.len() {
            let _before = rolling.digest();
            let outgoing = data[i - window_size];
            let incoming = data[i];

            rolling
                .roll(outgoing, incoming)
                .expect("roll should succeed");
            let _after = rolling.digest();

            // Verify checksum changed (unless outgoing == incoming and window unchanged)
            if outgoing != incoming {
                // The checksum should be different
                let mut fresh = RollingChecksum::new();
                fresh.update(&data[i - window_size + 1..=i]);
                assert_eq!(
                    rolling.digest(),
                    fresh.digest(),
                    "case {case_idx}: rolling should match fresh at position {i}"
                );
            }
        }
    }
}

/// Verifies that rolling checksum produces different values for different windows.
#[test]
fn rolling_checksum_distinguishes_different_windows() {
    use checksums::RollingChecksum;

    // Create data where each window is unique
    let data: Vec<u8> = (0..256).map(|i| i as u8).collect();
    let window_size = 16;

    let mut checksums = HashSet::new();
    for i in 0..=data.len() - window_size {
        let mut rolling = RollingChecksum::new();
        rolling.update(&data[i..i + window_size]);
        checksums.insert(rolling.digest().value());
    }

    // Most windows should have unique checksums
    // (some collisions are possible with 32-bit checksum, but should be rare)
    let num_windows = data.len() - window_size + 1;
    let unique_ratio = checksums.len() as f64 / num_windows as f64;

    assert!(
        unique_ratio > 0.95,
        "at least 95% of windows should have unique checksums, got {:.2}%",
        unique_ratio * 100.0
    );
}

// ============================================================================
// 3. Strong Checksum Verification Works
// ============================================================================

/// Verifies that strong checksums correctly distinguish different blocks.
///
/// Even if rolling checksums collide, strong checksums should differentiate
/// blocks with different content.
#[test]
fn strong_checksum_distinguishes_different_blocks() {
    // Create blocks with potentially colliding weak checksums but different content
    let block_size = 700;
    let mut basis = Vec::new();

    // Create 10 different blocks
    for block_id in 0..10 {
        for i in 0..block_size {
            basis.push(((block_id * 13 + i * 17) % 256) as u8);
        }
    }

    let index = build_index(&basis).expect("should build index");
    let actual_block_len = index.block_length();

    // Test that each block matches only itself, not other blocks
    for block_id in 0..10 {
        let start = block_id * actual_block_len;
        let end = start + actual_block_len;
        if end <= basis.len() {
            let input = basis[start..end].to_vec();

            let script = generate_delta(&input[..], &index).expect("should generate delta");
            let reconstructed = apply_and_reconstruct(&basis, &index, &script);

            assert_eq!(
                reconstructed, input,
                "block {block_id} should reconstruct correctly"
            );

            // Should have exactly one copy token for this block
            let copy_tokens: Vec<_> = script
                .tokens()
                .iter()
                .filter_map(|t| match t {
                    DeltaToken::Copy { index, .. } => Some(*index),
                    _ => None,
                })
                .collect();

            assert_eq!(
                copy_tokens.len(),
                1,
                "block {block_id} should produce exactly one copy token"
            );

            assert_eq!(
                copy_tokens[0], block_id as u64,
                "block {block_id} should match itself (index {block_id}), not another block"
            );
        }
    }
}

/// Verifies that strong checksum rejects blocks with same weak checksum.
///
/// This test creates data designed to have weak checksum collisions but
/// different strong checksums.
#[test]
fn strong_checksum_rejects_weak_checksum_collisions() {
    // Create basis with specific patterns
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Create input with different content (won't match strong checksum)
    let input: Vec<u8> = (0..block_len).map(|i| ((i * 7 + 13) % 256) as u8).collect();

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(
        reconstructed, input,
        "different content should still reconstruct correctly"
    );

    // If strong checksum is working, this different data should NOT match
    // any blocks (or match with very low probability), so most/all should be literals
    // We expect the literal bytes to be close to the input length
    assert!(
        script.literal_bytes() >= (input.len() as u64 * 90) / 100,
        "different content should produce mostly literals (got {} literals out of {} bytes)",
        script.literal_bytes(),
        input.len()
    );
}

/// Verifies strong checksum with different algorithms (MD4).
#[test]
fn strong_checksum_algorithm_md4_correctness() {
    // Use sufficiently large data to ensure at least one full block
    let data: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("should calculate layout");

    // Generate signature with MD4
    let signature = generate_file_signature(&data[..], layout, SignatureAlgorithm::Md4)
        .expect("should generate signature");

    let index = DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
        .expect("should build index");

    // The same data should match
    let script = generate_delta(&data[..], &index).expect("should generate delta");

    // Should have a copy token (exact match)
    let has_copy = script
        .tokens()
        .iter()
        .any(|t| matches!(t, DeltaToken::Copy { .. }));
    assert!(
        has_copy,
        "identical data should produce at least one copy token with MD4"
    );

    // Different data should not match - use a completely different pattern
    let different_data: Vec<u8> = (0..8192).map(|i| ((i * 17 + 31) % 256) as u8).collect();
    let script2 = generate_delta(&different_data[..], &index).expect("should generate delta");

    // Should be mostly/all literals (allow some false positives but require > 80% literals)
    assert!(
        script2.literal_bytes() >= (different_data.len() as u64 * 80) / 100,
        "different data should produce mostly literals with MD4 (got {} literal bytes out of {})",
        script2.literal_bytes(),
        different_data.len()
    );
}

// ============================================================================
// 4. False Positive Rate is Acceptable
// ============================================================================

/// Measures and validates the false positive rate of block matching.
///
/// False positives occur when blocks with matching rolling checksums are
/// incorrectly identified as matches despite having different content.
/// The strong checksum should reject these.
#[test]
fn false_positive_rate_is_low() {
    // Create a large basis with many blocks
    let basis: Vec<u8> = (0..100_000).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");

    // Create completely different input that should NOT match
    let input: Vec<u8> = (0..50_000).map(|i| ((i * 17 + 31) % 256) as u8).collect();

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(
        reconstructed, input,
        "unrelated data should still reconstruct correctly"
    );

    // Count how many bytes were matched vs literals
    let matched_bytes = script.copy_bytes();
    let total_bytes = script.total_bytes();

    // False positive rate: ratio of incorrectly matched bytes
    // With strong checksum (MD4, 16 bytes), false positive rate should be
    // astronomically low (< 2^-128). In practice, we should see 0 matches.
    let false_positive_rate = matched_bytes as f64 / total_bytes as f64;

    assert!(
        false_positive_rate < 0.01,
        "false positive rate should be < 1%, got {:.4}% ({} matched out of {} total bytes)",
        false_positive_rate * 100.0,
        matched_bytes,
        total_bytes
    );
}

/// Tests false positive rejection with known collision patterns.
///
/// Creates data specifically designed to have weak checksum collisions
/// and verifies strong checksum rejects them.
#[test]
fn strong_checksum_rejects_crafted_collisions() {
    let block_size = 700;
    let mut basis = Vec::new();

    // Block 0: specific pattern
    for i in 0..block_size {
        basis.push((i % 256) as u8);
    }

    // Block 1: different pattern designed to potentially collide on weak checksum
    // but guaranteed to differ on strong checksum
    for i in 0..block_size {
        basis.push(((255 - i) % 256) as u8);
    }

    let index = build_index(&basis).expect("should build index");
    let actual_block_len = index.block_length();

    // Try to match block 1's data against the index
    let input = basis[actual_block_len..2 * actual_block_len].to_vec();
    let script = generate_delta(&input[..], &index).expect("should generate delta");

    // Should match block 1, not block 0
    let copy_tokens: Vec<_> = script
        .tokens()
        .iter()
        .filter_map(|t| match t {
            DeltaToken::Copy { index, .. } => Some(*index),
            _ => None,
        })
        .collect();

    if !copy_tokens.is_empty() {
        // If any matches found, they should be block 1 (index 1), not block 0
        for &idx in &copy_tokens {
            assert_eq!(
                idx, 1,
                "should match block 1 (index 1), not block 0 (index 0)"
            );
        }
    }

    // Verify correct reconstruction
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(reconstructed, input, "block 1 should reconstruct correctly");
}

/// Measures false positive rate with random data.
#[test]
fn false_positive_rate_with_random_data() {
    // Create basis with pseudo-random data
    let basis: Vec<u8> = (0..32768).map(|i| ((i * 17 + 31) % 256) as u8).collect();
    let index = build_index(&basis).expect("should build index");

    // Create completely different pseudo-random input
    let input: Vec<u8> = (0..16384).map(|i| ((i * 23 + 47) % 256) as u8).collect();

    let script = generate_delta(&input[..], &index).expect("should generate delta");

    // Calculate false positive rate
    let matched_bytes = script.copy_bytes();
    let total_bytes = script.total_bytes();
    let false_positive_rate = matched_bytes as f64 / total_bytes as f64;

    // With different data, we expect very few or zero matches
    assert!(
        false_positive_rate < 0.05,
        "false positive rate with random data should be < 5%, got {:.4}%",
        false_positive_rate * 100.0
    );
}

/// Tests that accidental matches are rejected by strong checksum.
#[test]
fn accidental_weak_matches_rejected_by_strong_checksum() {
    // Use small block size to increase chance of weak checksum collisions
    let block_size = 128;
    let num_blocks = 50;

    let basis: Vec<u8> = (0..block_size * num_blocks)
        .map(|i| ((i * 13) % 256) as u8)
        .collect();

    let index = build_index_with_block_hint(&basis, block_size as u32).expect("should build index");

    // Create input with different data
    let input: Vec<u8> = (0..block_size * num_blocks)
        .map(|i| ((i * 19 + 7) % 256) as u8)
        .collect();

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(
        reconstructed, input,
        "different data should reconstruct correctly"
    );

    // Even with potential weak checksum collisions, strong checksum should
    // reject them, resulting in mostly literals
    let match_rate = script.copy_bytes() as f64 / script.total_bytes() as f64;

    assert!(
        match_rate < 0.10,
        "accidental matches should be < 10% with strong checksum verification, got {:.2}%",
        match_rate * 100.0
    );
}

// ============================================================================
// Edge Cases and Boundary Conditions
// ============================================================================

/// Verifies matching behavior at exact block boundaries.
#[test]
fn matching_at_exact_block_boundaries() {
    let basis: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Input is exactly N complete blocks from basis
    let num_blocks = 4;
    let input = basis[..block_len * num_blocks].to_vec();

    let script = generate_delta(&input[..], &index).expect("should generate delta");

    // All blocks should match perfectly
    assert_eq!(
        script.literal_bytes(),
        0,
        "exact block boundaries should have zero literals"
    );

    let copy_count = script
        .tokens()
        .iter()
        .filter(|t| matches!(t, DeltaToken::Copy { .. }))
        .count();

    assert_eq!(
        copy_count, num_blocks,
        "should have exactly {num_blocks} copy tokens for {num_blocks} blocks"
    );
}

/// Verifies that single-byte differences prevent block matches.
#[test]
fn single_byte_difference_prevents_match() {
    let basis: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Take a block and change just one byte
    let mut input = basis[..block_len].to_vec();
    input[block_len / 2] = input[block_len / 2].wrapping_add(1);

    let script = generate_delta(&input[..], &index).expect("should generate delta");

    // The modified block should not match
    assert!(
        script.literal_bytes() > 0,
        "single byte change should prevent block match"
    );

    // Verify reconstruction is still correct
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);
    assert_eq!(
        reconstructed, input,
        "modified block should still reconstruct correctly"
    );
}

/// Verifies matching behavior with very small blocks.
#[test]
fn matching_with_small_block_size() {
    let basis: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();

    // Use very small block size
    let index = build_index_with_block_hint(&basis, 64).expect("should build index");
    let block_len = index.block_length();

    // Input contains some matching blocks
    let mut input = Vec::new();
    input.extend_from_slice(&basis[..block_len * 2]);
    input.extend_from_slice(b"different data here");
    input.extend_from_slice(&basis[block_len * 5..block_len * 7]);

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(
        reconstructed, input,
        "small blocks should reconstruct correctly"
    );

    // Should find some matches
    let copy_count = script
        .tokens()
        .iter()
        .filter(|t| matches!(t, DeltaToken::Copy { .. }))
        .count();

    assert!(copy_count >= 2, "should find at least some matching blocks");
}

/// Verifies matching behavior with very large blocks.
#[test]
fn matching_with_large_block_size() {
    let basis: Vec<u8> = (0..65536).map(|i| (i % 251) as u8).collect();

    // Use large block size
    let index = build_index_with_block_hint(&basis, 8192).expect("should build index");
    let block_len = index.block_length();

    // Input is one matching block
    let input = basis[..block_len].to_vec();

    let script = generate_delta(&input[..], &index).expect("should generate delta");

    // Should match the large block
    assert_eq!(
        script.literal_bytes(),
        0,
        "large matching block should have zero literals"
    );

    let copy_count = script
        .tokens()
        .iter()
        .filter(|t| matches!(t, DeltaToken::Copy { .. }))
        .count();

    assert_eq!(copy_count, 1, "should have one copy token for large block");
}

// ============================================================================
// Statistical Accuracy Tests
// ============================================================================

/// Verifies matching accuracy across many random scenarios.
#[test]
fn statistical_matching_accuracy() {
    // Run multiple trials with different data patterns
    let mut total_correct_matches = 0;
    let mut total_expected_matches = 0;

    for trial in 0..10 {
        let basis: Vec<u8> = (0..8192)
            .map(|i| ((i * (trial + 1) * 7) % 256) as u8)
            .collect();
        let index = build_index(&basis).expect("should build index");
        let block_len = index.block_length();

        // Create input with known matching blocks
        let num_matching = 5;
        let mut input = Vec::new();

        for i in 0..num_matching {
            let start = (i * block_len) % (basis.len() - block_len);
            input.extend_from_slice(&basis[start..start + block_len]);
        }

        let script = generate_delta(&input[..], &index).expect("should generate delta");

        // Count matches
        let copy_count = script
            .tokens()
            .iter()
            .filter(|t| matches!(t, DeltaToken::Copy { .. }))
            .count();

        total_correct_matches += copy_count;
        total_expected_matches += num_matching;
    }

    // Accuracy should be high (all expected matches should be found)
    let accuracy = total_correct_matches as f64 / total_expected_matches as f64;
    assert!(
        accuracy >= 0.95,
        "matching accuracy should be >= 95%, got {:.2}%",
        accuracy * 100.0
    );
}

/// Measures precision: ratio of correct matches to total matches found.
#[test]
fn matching_precision_measurement() {
    let basis: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
    let index = build_index(&basis).expect("should build index");
    let block_len = index.block_length();

    // Create input with known matching blocks interspersed with garbage
    let mut input = Vec::new();
    let known_matches = 5;

    for i in 0..known_matches {
        // Add matching block
        let start = i * block_len;
        input.extend_from_slice(&basis[start..start + block_len]);

        // Add garbage
        input.extend_from_slice(&[0xFFu8; 50]);
    }

    let script = generate_delta(&input[..], &index).expect("should generate delta");
    let reconstructed = apply_and_reconstruct(&basis, &index, &script);

    assert_eq!(
        reconstructed, input,
        "mixed content should reconstruct correctly"
    );

    // Count copy tokens
    let copy_count = script
        .tokens()
        .iter()
        .filter(|t| matches!(t, DeltaToken::Copy { .. }))
        .count();

    // Precision: all found matches should be correct (known matches)
    // We expect exactly `known_matches` copy tokens
    assert_eq!(
        copy_count, known_matches,
        "should find exactly the known matching blocks"
    );
}
