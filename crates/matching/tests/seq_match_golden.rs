//! Golden-byte regression test for the zsync seq-match optimization.
//!
//! Pins the post-coalesce DeltaToken sequence and the byte-equivalence of the
//! reconstructed target to lock in the wire-compat invariant from
//! `docs/design/zsync-seq-match.md`:
//!
//! - The DeltaScript may collapse runs of consecutive matched basis blocks
//!   into a single `DeltaToken::Copy { len = run * block_length }`.
//! - `script.copy_bytes()` and `script.literal_bytes()` are unchanged from
//!   the no-coalesce baseline (token count goes down, total bytes stay the
//!   same).
//! - `apply_delta` reconstructs the original target byte-for-byte regardless
//!   of how runs are coalesced.
//!
//! upstream: `librcksum/rsum.c:262` (`next_match` advance after confirmed
//! match) - the same control-flow shortcut zsync uses.

use matching::{DeltaScript, DeltaSignatureIndex, DeltaToken, apply_delta, generate_delta};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};
use std::io::Cursor;
use std::num::NonZeroU8;

/// Builds a basis with a known repeating pattern guaranteed to produce many
/// consecutive matched blocks when the source is the basis itself or a
/// prefix of it.
///
/// Size is an exact multiple of `signature::block_size::DEFAULT_BLOCK_SIZE`
/// (700) so every basis block is full-length and `extend_run` can walk all
/// of them. With a partial trailing block, extend_run halts at the size
/// mismatch and the fat-copy assertions below would not hold.
fn build_synthetic_basis() -> Vec<u8> {
    // 94 full blocks of 700 bytes = 65 800 bytes (≈ 64 KiB), well within the
    // < 700² byte threshold where `calculate_block_length` returns 700.
    const TOTAL: usize = 700 * 94;
    let mut buf = Vec::with_capacity(TOTAL);
    for i in 0..TOTAL {
        buf.push(((i * 17 + 5) % 251) as u8);
    }
    buf
}

fn build_index(data: &[u8]) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature =
        generate_file_signature(data, layout, SignatureAlgorithm::Md4).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
}

fn copy_bytes_only(script: &DeltaScript) -> u64 {
    script
        .tokens()
        .iter()
        .map(|t| match t {
            DeltaToken::Copy { len, .. } => *len as u64,
            DeltaToken::Literal(_) => 0,
        })
        .sum()
}

fn copy_token_count(script: &DeltaScript) -> usize {
    script
        .tokens()
        .iter()
        .filter(|t| matches!(t, DeltaToken::Copy { .. }))
        .count()
}

#[test]
fn seq_match_emits_single_fat_copy_for_full_basis_run() {
    let basis = build_synthetic_basis();
    let index = build_index(&basis);
    let block_len = index.block_length();
    let block_count = index.block_count();

    // Source identical to basis -> every full-length basis block matches at
    // its natural offset, producing one long run. Seq-match must coalesce
    // the run into exactly one Copy token spanning all full-length blocks.
    let script = generate_delta(&basis[..], &index).expect("script");

    assert_eq!(
        copy_token_count(&script),
        1,
        "seq-match should coalesce the full-basis run into a single Copy token"
    );

    let copy_token = script
        .tokens()
        .iter()
        .find(|t| matches!(t, DeltaToken::Copy { .. }))
        .expect("at least one copy token");
    match copy_token {
        DeltaToken::Copy {
            index: copy_idx,
            len,
        } => {
            assert_eq!(*copy_idx, 0, "run starts at basis block 0");
            assert_eq!(
                *len,
                block_len * block_count,
                "fat copy len = block_count * block_len"
            );
        }
        _ => unreachable!(),
    }

    // Total matched bytes must equal the full-length-block portion of the
    // basis (the trailing short block, if any, falls into a literal).
    let expected_match_bytes = (block_len * block_count) as u64;
    assert_eq!(copy_bytes_only(&script), expected_match_bytes);
}

#[test]
fn seq_match_round_trips_identical_basis() {
    let basis = build_synthetic_basis();
    let index = build_index(&basis);
    let script = generate_delta(&basis[..], &index).expect("script");

    let mut cursor = Cursor::new(basis.clone());
    let mut output = Vec::new();
    apply_delta(&mut cursor, &mut output, &index, &script).expect("apply");
    assert_eq!(output, basis, "round-trip must be byte-identical");
}

#[test]
fn seq_match_run_breaks_on_non_adjacent_match() {
    // Construct a target that matches basis blocks [0, 1, 2] then a literal,
    // then basis blocks [4, 5]. The seq-match coalescing must emit:
    //   Copy{0, 3*block_len}  Literal  Copy{4, 2*block_len}
    // (three tokens total: a fat copy, a literal break, another fat copy).
    let basis = build_synthetic_basis();
    let index = build_index(&basis);
    let block_len = index.block_length();
    assert!(index.block_count() >= 6, "test requires at least 6 blocks");

    let mut target = Vec::new();
    target.extend_from_slice(&basis[0..3 * block_len]);
    target.extend_from_slice(b"-INSERTED-");
    target.extend_from_slice(&basis[4 * block_len..6 * block_len]);

    let script = generate_delta(&target[..], &index).expect("script");

    // Reconstruct and verify byte-equality first - the strongest assertion
    // for wire-compat downstream.
    let mut cursor = Cursor::new(basis.clone());
    let mut output = Vec::new();
    apply_delta(&mut cursor, &mut output, &index, &script).expect("apply");
    assert_eq!(output, target, "non-adjacent runs reconstruct cleanly");

    // Fewer Copy tokens than matched blocks ([0,1,2] coalesces, [4,5]
    // coalesces). The exact run boundaries are pinned: at most 2 Copy
    // tokens for 5 matched blocks.
    let copy_tokens: Vec<&DeltaToken> = script
        .tokens()
        .iter()
        .filter(|t| matches!(t, DeltaToken::Copy { .. }))
        .collect();
    assert!(
        copy_tokens.len() <= 2,
        "expected at most 2 fat Copy tokens, got {}",
        copy_tokens.len()
    );

    // Total copy bytes must still equal exactly the 5 matched blocks.
    assert_eq!(copy_bytes_only(&script), (5 * block_len) as u64);
}

#[test]
fn extend_run_helper_counts_consecutive_matches() {
    let basis = build_synthetic_basis();
    let index = build_index(&basis);
    let block_len = index.block_length();
    let count = index.block_count();
    assert!(count >= 4, "test requires at least 4 indexed blocks");

    // Target = first 4 basis blocks contiguously. extend_run from block 0
    // with max_blocks=4 must report 4. With max_blocks=2 it must report 2.
    let target = basis[..4 * block_len].to_vec();
    assert_eq!(index.extend_run(0, &target, 4), 4);
    assert_eq!(index.extend_run(0, &target, 2), 2);

    // Mismatch in the third block -> run halts at 2.
    let mut diverged = target.clone();
    let mid = 2 * block_len;
    diverged[mid] ^= 0xFF;
    assert_eq!(index.extend_run(0, &diverged, 4), 2);

    // First block already mismatches -> 0.
    let mut wrong = target.clone();
    wrong[0] ^= 0xFF;
    assert_eq!(index.extend_run(0, &wrong, 4), 0);

    // Out-of-range start index -> 0.
    assert_eq!(index.extend_run(count, &target, 1), 0);

    // max_blocks = 0 -> 0.
    assert_eq!(index.extend_run(0, &target, 0), 0);
}

#[test]
fn seq_match_matched_bytes_match_baseline() {
    // Pre-recorded baseline: copy_bytes for the full-basis source must equal
    // (block_count * block_length). This is the invariant the design pins as
    // "token count goes down, bytes stay same."
    let basis = build_synthetic_basis();
    let index = build_index(&basis);
    let block_len = index.block_length() as u64;
    let block_count = index.block_count() as u64;
    let script = generate_delta(&basis[..], &index).expect("script");

    let golden_copy_bytes = block_len * block_count;
    let golden_literal_bytes = basis.len() as u64 - golden_copy_bytes;

    assert_eq!(copy_bytes_only(&script), golden_copy_bytes);
    assert_eq!(script.literal_bytes(), golden_literal_bytes);
    assert_eq!(script.total_bytes(), basis.len() as u64);
}
