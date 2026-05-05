//! Property and unit tests for [`super::MatchedBlocks`] and the
//! generator's matched-block pruning path.
//!
//! Pins the contracts in `docs/design/zsync-prune.md`:
//!
//! - Pruning never reduces the total matched-byte count vs the no-prune
//!   baseline (monotone non-decreasing under prune).
//! - Pruning preserves the apply round-trip: applying the delta over the
//!   basis reproduces the source byte-for-byte.
//! - Duplicate-content basis blocks are tracked independently; pruning
//!   one sibling leaves the others findable.

use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

use proptest::prelude::*;

use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

use super::DeltaSignatureIndex;
use crate::generator::DeltaGenerator;
use crate::script::{DeltaScript, DeltaToken, apply_delta};

/// Block length that always produces full-length blocks for the
/// proptest-generated basis files.
const TEST_BLOCK_LENGTH: u32 = 512;

/// Strong-checksum length (bytes) used by the property cases. Matches
/// the value used by the existing `index/tests.rs` cases so a fixed
/// signature layout is in play.
const TEST_STRONG_LEN: u8 = 16;

/// Builds a [`DeltaSignatureIndex`] over the supplied basis bytes with
/// the test's fixed block length. Returns `None` when the basis is too
/// short to produce a full block, which the proptest strategies bound
/// out by construction.
fn build_index(basis: &[u8]) -> Option<DeltaSignatureIndex> {
    let params = SignatureLayoutParams::new(
        basis.len() as u64,
        Some(NonZeroU32::new(TEST_BLOCK_LENGTH).unwrap()),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(TEST_STRONG_LEN).unwrap(),
    );
    let layout = calculate_signature_layout(params).ok()?;
    let signature = generate_file_signature(basis, layout, SignatureAlgorithm::Md4).ok()?;
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
}

/// Generates a delta over `source` against `basis`, optionally enabling
/// the matched-block pruning bitmap. The non-prune branch matches the
/// pre-#2069 behaviour exactly and serves as the property baseline.
fn generate_with_prune(
    basis: &[u8],
    source: &[u8],
    prune: bool,
) -> Option<(DeltaScript, DeltaSignatureIndex)> {
    let index = build_index(basis)?;
    let generator = DeltaGenerator::new().with_prune_matched(prune);
    let script = generator.generate(Cursor::new(source), &index).ok()?;
    Some((script, index))
}

/// Returns the total number of bytes accounted for by [`DeltaToken::Copy`]
/// tokens in the script (the "matched bytes" counter referenced by the
/// design contract).
fn copy_bytes(script: &DeltaScript) -> u64 {
    script
        .tokens()
        .iter()
        .filter_map(|tok| match tok {
            DeltaToken::Copy { len, .. } => Some(*len as u64),
            DeltaToken::Literal(_) => None,
        })
        .sum()
}

/// Applies `script` over `basis` via [`apply_delta`] and returns the
/// reconstructed bytes. Used by the round-trip property to assert that
/// pruning never breaks correctness.
fn apply_script(basis: &[u8], index: &DeltaSignatureIndex, script: &DeltaScript) -> Vec<u8> {
    let mut basis_cursor = Cursor::new(basis.to_vec());
    let mut output = Vec::new();
    apply_delta(&mut basis_cursor, &mut output, index, script).expect("apply");
    output
}

/// Strategy for a basis composed of `n_blocks` blocks where each block
/// is either drawn fresh-random or copied from a previously emitted
/// block. The `dup_density` field controls how often duplicates are
/// produced, exercising the duplicate-bucket walk in
/// `find_match_slices_filtered`.
fn duplicate_basis_strategy() -> impl Strategy<Value = Vec<u8>> {
    // distinct_seed is a small set of seeds the basis cycles through to
    // engineer duplicate-content blocks across the file. fill is the
    // all-fill fallback block used when distinct_seed is empty.
    (
        2usize..=6,
        0u8..=255,
        prop::collection::vec(any::<u8>(), 0..=4),
    )
        .prop_map(|(n_blocks, fill, seeds)| {
            let block_len = TEST_BLOCK_LENGTH as usize;
            let mut basis = Vec::with_capacity(n_blocks * block_len);
            for i in 0..n_blocks {
                let mut block = vec![0u8; block_len];
                if seeds.is_empty() {
                    for byte in &mut block {
                        *byte = fill;
                    }
                } else {
                    let seed = seeds[i % seeds.len()];
                    for (j, byte) in block.iter_mut().enumerate() {
                        *byte = seed.wrapping_add((j as u8).wrapping_mul(17));
                    }
                }
                basis.extend_from_slice(&block);
            }
            basis
        })
}

/// Composite strategy: produces a `(basis, source)` pair where the
/// source is built from a permutation of unique basis-block positions
/// plus an optional literal tail. Each basis block is referenced at
/// most once, which keeps the property
/// [`prune_does_not_reduce_matched_bytes`] in its valid domain. The
/// surplus-source-occurrence regime (where prune-on emits literals
/// while prune-off emits duplicate Copy tokens) is covered by the
/// hand-rolled [`excess_source_occurrences_fall_through_to_literal`]
/// regression instead.
fn basis_and_source_strategy() -> impl Strategy<Value = (Vec<u8>, Vec<u8>)> {
    let block_len = TEST_BLOCK_LENGTH as usize;
    duplicate_basis_strategy().prop_flat_map(move |basis| {
        let n_blocks = (basis.len() / block_len).max(1);
        // Generate a permutation of [0, n_blocks) by sorting a vector of
        // sortkey-tagged indices. proptest does not ship a permutation
        // strategy; this trick is the documented workaround.
        (
            Just(basis),
            prop::collection::vec(any::<u32>(), n_blocks..=n_blocks),
            prop::collection::vec(any::<u8>(), 0..=12),
            1usize..=n_blocks,
        )
            .prop_map(move |(b, sortkeys, tail, take)| {
                let mut order: Vec<(u32, usize)> = sortkeys
                    .into_iter()
                    .enumerate()
                    .map(|(i, k)| (k, i))
                    .collect();
                order.sort_unstable_by_key(|&(k, _)| k);
                let mut source = Vec::new();
                for &(_, idx) in order.iter().take(take) {
                    let start = idx * block_len;
                    let end = (start + block_len).min(b.len());
                    source.extend_from_slice(&b[start..end]);
                }
                source.extend_from_slice(&tail);
                (b, source)
            })
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        ..ProptestConfig::default()
    })]

    /// Pruning never reduces the total bytes attributed to Copy tokens.
    /// The contract from `docs/design/zsync-prune.md` allows the COPY
    /// index field to differ between the two runs (different sibling),
    /// but the matched-byte count is monotone non-decreasing.
    #[test]
    fn prune_does_not_reduce_matched_bytes(
        (basis, source) in basis_and_source_strategy(),
    ) {
        let off = generate_with_prune(&basis, &source, false);
        let on = generate_with_prune(&basis, &source, true);
        if let (Some((s_off, _)), Some((s_on, _))) = (off, on) {
            prop_assert!(
                copy_bytes(&s_on) >= copy_bytes(&s_off),
                "pruning reduced matched bytes: on={} off={}",
                copy_bytes(&s_on),
                copy_bytes(&s_off)
            );
        }
    }

    /// Pruning preserves the apply round-trip: the reconstructed source
    /// must exactly equal the input under both prune-on and prune-off
    /// configurations. This is the duplicate-bucket correctness
    /// invariant: dropping the wrong sibling would corrupt the output.
    #[test]
    fn prune_preserves_apply_round_trip(
        (basis, source) in basis_and_source_strategy(),
    ) {
        if let Some((s_on, idx_on)) = generate_with_prune(&basis, &source, true) {
            let reconstructed = apply_script(&basis, &idx_on, &s_on);
            prop_assert_eq!(reconstructed, source.clone(), "prune-on apply mismatch");
        }
        if let Some((s_off, idx_off)) = generate_with_prune(&basis, &source, false) {
            let reconstructed = apply_script(&basis, &idx_off, &s_off);
            prop_assert_eq!(reconstructed, source, "prune-off apply mismatch");
        }
    }
}

/// Hand-rolled regression: a basis with three identical blocks plus a
/// source containing all three in order. Without pruning the matcher
/// might collapse all three Copy tokens onto basis index 0; with
/// pruning it must walk the duplicate bucket and emit each sibling
/// once. Either way the apply round-trip and matched-byte count must
/// hold.
#[test]
fn duplicate_basis_blocks_yield_round_trip() {
    let block_len = TEST_BLOCK_LENGTH as usize;
    let mut basis = Vec::with_capacity(block_len * 3);
    let pattern: Vec<u8> = (0..block_len).map(|i| (i % 251) as u8).collect();
    for _ in 0..3 {
        basis.extend_from_slice(&pattern);
    }
    let source = basis.clone();

    let (script_on, idx_on) = generate_with_prune(&basis, &source, true).expect("prune-on script");
    let (script_off, idx_off) =
        generate_with_prune(&basis, &source, false).expect("prune-off script");

    let on = apply_script(&basis, &idx_on, &script_on);
    let off = apply_script(&basis, &idx_off, &script_off);
    assert_eq!(on, source);
    assert_eq!(off, source);
    assert!(copy_bytes(&script_on) >= copy_bytes(&script_off));
}

/// Hand-rolled regression: source has more occurrences of the duplicate
/// block than the basis does. Once every basis sibling has been
/// pruned, later occurrences must fall through to literal emission
/// rather than re-matching block 0. The apply round-trip is the gate.
#[test]
fn excess_source_occurrences_fall_through_to_literal() {
    let block_len = TEST_BLOCK_LENGTH as usize;
    let pattern: Vec<u8> = (0..block_len).map(|i| ((i * 7) % 251) as u8).collect();

    // basis: two duplicate blocks; source: four duplicate blocks.
    let mut basis = Vec::with_capacity(block_len * 2);
    basis.extend_from_slice(&pattern);
    basis.extend_from_slice(&pattern);

    let mut source = Vec::with_capacity(block_len * 4);
    for _ in 0..4 {
        source.extend_from_slice(&pattern);
    }

    let (script_on, idx_on) = generate_with_prune(&basis, &source, true).expect("prune-on script");
    let reconstructed = apply_script(&basis, &idx_on, &script_on);
    assert_eq!(reconstructed, source);
}
