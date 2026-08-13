//! Characterization test for the basis-index adjacency divergence.
//!
//! Canonical zsync's `seq_matches` requires a run's blocks to be adjacent in
//! BASIS BLOCK INDEX, enforced at three layers: the hash key folds in the next
//! block's rolling sum (`librcksum/internal.h:100-108`), only run-starts are
//! indexed (`hash.c:62-84` bounded by `blocks + 1 - seq_matches`), and the
//! candidate's `e[1].r` is re-checked explicitly (`rsum.c:211`).
//!
//! oc's `generate_gated` has none of that: it repeats an unconstrained lookup
//! at each successive SOURCE offset and pushes whatever basis index comes back,
//! so it requires only source adjacency. This is a deliberate, currently
//! UNRESOLVED divergence - see "Divergence: no basis-index adjacency check" in
//! `docs/design/zsync-seq-match.md`, which records it as an open user decision.
//!
//! These tests pin the CURRENT behaviour rather than zsync's. That is the
//! point: the divergence must not be changed silently in either direction.
//! Adding the `basis_idx == prev + 1` check will fail
//! `non_adjacent_basis_blocks_are_still_trusted_as_a_run`, which is the signal
//! to go settle the decision, not to edit the assertion.

use matching::{DeltaGenerator, DeltaSignatureIndex, DeltaToken, apply_delta};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};
use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

const BLOCK: usize = 700;

/// Distinct, non-periodic block contents.
///
/// A periodic fixture is a trap here: an unaligned window can match an earlier
/// block by content, so a "non-adjacent" run would form for reasons unrelated
/// to the gate. A 64-bit LCG seeded per block keeps every block unique and
/// keeps every unaligned window a non-match.
fn block_bytes(seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..BLOCK)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect()
}

fn build_index(data: &[u8]) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        Some(NonZeroU32::new(BLOCK as u32).expect("nonzero block")),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature =
        generate_file_signature(data, layout, SignatureAlgorithm::Md4).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
}

fn copy_indices(tokens: &[DeltaToken]) -> Vec<u64> {
    tokens
        .iter()
        .filter_map(|t| match t {
            DeltaToken::Copy { index, .. } => Some(*index),
            DeltaToken::Literal(_) => None,
        })
        .collect()
}

/// Basis of four distinct blocks; source is basis block 2 followed by block 0.
///
/// The two source blocks are source-adjacent but map to basis indices 2 and 0,
/// which are NOT index-adjacent (0 != 2 + 1). zsync would reject this pair and
/// send literals. oc trusts it and emits two Copy tokens.
#[test]
fn non_adjacent_basis_blocks_are_still_trusted_as_a_run() {
    let mut basis = Vec::new();
    for seed in 0..4u64 {
        basis.extend_from_slice(&block_bytes(seed));
    }
    let index = build_index(&basis);
    assert!(
        index.full_block_count() >= 2,
        "fixture must clear the too-few-blocks fallback in DeltaGenerator::generate"
    );

    let mut source = block_bytes(2);
    source.extend_from_slice(&block_bytes(0));

    let script = DeltaGenerator::default()
        .with_consecutive_match_needed(2)
        .generate(Cursor::new(&source), &index)
        .expect("generate");

    assert_eq!(
        copy_indices(script.tokens()),
        vec![2, 0],
        "oc trusts a source-adjacent pair whose basis indices are not adjacent; \
         zsync's seq_matches would demote both to literals (rsum.c:211)"
    );
    assert_eq!(
        script.literal_bytes(),
        0,
        "the whole source came from the basis, so nothing should be sent literally"
    );

    let mut out = Vec::new();
    apply_delta(Cursor::new(&basis), &mut out, &index, &script).expect("apply");
    assert_eq!(out, source, "reconstruction must be byte-exact");
}

/// The divergence is confined to WHICH pairs are trusted, not to whether a
/// trusted pair reconstructs. A genuinely adjacent run must behave identically
/// under either rule, so this case pins the shared baseline.
#[test]
fn adjacent_basis_blocks_form_a_run_under_either_rule() {
    let mut basis = Vec::new();
    for seed in 0..4u64 {
        basis.extend_from_slice(&block_bytes(seed));
    }
    let index = build_index(&basis);

    let mut source = block_bytes(1);
    source.extend_from_slice(&block_bytes(2));

    let script = DeltaGenerator::default()
        .with_consecutive_match_needed(2)
        .generate(Cursor::new(&source), &index)
        .expect("generate");

    assert_eq!(copy_indices(script.tokens()), vec![1, 2]);
    assert_eq!(script.literal_bytes(), 0);

    let mut out = Vec::new();
    apply_delta(Cursor::new(&basis), &mut out, &index, &script).expect("apply");
    assert_eq!(out, source);
}

/// A lone match is demoted whether or not adjacency is checked. This is the
/// integrity backstop the halved `s2length` leans on, so it is pinned
/// separately from the adjacency question.
#[test]
fn a_lone_match_is_demoted_to_a_literal() {
    let mut basis = Vec::new();
    for seed in 0..4u64 {
        basis.extend_from_slice(&block_bytes(seed));
    }
    let index = build_index(&basis);

    // One basis block, then bytes that match nothing.
    let mut source = block_bytes(1);
    source.extend_from_slice(&block_bytes(99));

    let script = DeltaGenerator::default()
        .with_consecutive_match_needed(2)
        .generate(Cursor::new(&source), &index)
        .expect("generate");

    assert!(
        copy_indices(script.tokens()).is_empty(),
        "a run of one must never be emitted as a Copy under the gated scan"
    );
    assert_eq!(script.literal_bytes(), source.len() as u64);

    let mut out = Vec::new();
    apply_delta(Cursor::new(&basis), &mut out, &index, &script).expect("apply");
    assert_eq!(out, source);
}
