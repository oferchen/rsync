//! Tests for the zsync-inspired sequential-match lookahead (ZSO-2).
//!
//! Pins the contracts in `docs/design/zsync-seq-match.md` (and the parent
//! initiative `project_zsync_optimizations.md`):
//!
//! - [`super::DeltaSignatureIndex::next_match`] links each indexed block to
//!   its source-order successor and is the sole carrier of the lookahead
//!   relationship - never `match_idx + 1` directly.
//! - [`super::DeltaSignatureIndex::try_next_match_bytes`] and the slices
//!   counterpart honour the recorded successor, accept matches that pass
//!   the strong-checksum verify, and reject everything else.
//! - The generator's chain loop drives the lookahead via the new probe API,
//!   so consecutive sequential basis blocks become hits and a diverging
//!   target falls through to the existing full-lookup path.
//! - The link table and the test-only counters reset across
//!   [`super::DeltaSignatureIndex::rebuild`], the per-segment ZSO-7
//!   isolation invariant. The hand-rolled assertion mirrors the bithash
//!   leak test in `bithash_tests.rs` so a regression on one structure
//!   surfaces alongside the other.

use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

use checksums::RollingDigest;
use proptest::prelude::*;

use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

use super::DeltaSignatureIndex;
use crate::generator::DeltaGenerator;
use crate::script::{DeltaToken, apply_delta};

/// Block length used by every fixture so the basis always produces
/// full-length blocks; partial trailing blocks are exercised by the
/// dedicated regression below.
const TEST_BLOCK_LENGTH: u32 = 512;

/// Strong-checksum length used by every fixture. Matches the value in
/// `tests.rs` so the layout knobs stay aligned.
const TEST_STRONG_LEN: u8 = 16;

/// Convenience: build a `DeltaSignatureIndex` over `basis` with the test's
/// fixed block layout. Returns `None` when the basis cannot produce a full
/// block, which the call sites bound out by construction.
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

/// Distinct-content basis: each block is filled with a per-block seed so
/// no two basis blocks share strong-checksum content. Lets the seq-match
/// tests assert exact link-following behaviour without duplicate-bucket
/// noise.
fn distinct_basis(n_blocks: usize) -> Vec<u8> {
    let block_len = TEST_BLOCK_LENGTH as usize;
    let mut basis = Vec::with_capacity(n_blocks * block_len);
    for k in 0..n_blocks {
        let seed = (k as u8).wrapping_mul(31).wrapping_add(7);
        for j in 0..block_len {
            basis.push(seed.wrapping_add((j as u8).wrapping_mul(17)));
        }
    }
    basis
}

/// `next_match[K]` is `Some(K+1)` for every full-length block except the
/// last, which has no recorded successor.
#[test]
fn next_match_chains_sequential_blocks() {
    let basis = distinct_basis(6);
    let index = build_index(&basis).expect("index from sequential basis");
    let last = index.block_count() - 1;
    for k in 0..last {
        assert_eq!(
            index.next_match(k),
            Some(k + 1),
            "next_match[{k}] must point at the immediate successor"
        );
    }
    assert_eq!(
        index.next_match(last),
        None,
        "the tail block must terminate the chain"
    );
}

/// `next_match` is `None` for out-of-range indices, mirroring the
/// `block(...)` panic-free contract documented on the public API.
#[test]
fn next_match_out_of_range_returns_none() {
    let basis = distinct_basis(3);
    let index = build_index(&basis).expect("index");
    assert_eq!(index.next_match(index.block_count()), None);
    assert_eq!(index.next_match(usize::MAX), None);
}

/// `try_next_match_bytes` confirms the linked successor when the strong
/// checksum agrees, bypassing the full rolling-hash lookup. Counter:
/// one probe, one hit, zero misses.
#[test]
fn try_next_match_bytes_hits_linked_successor() {
    let basis = distinct_basis(4);
    let index = build_index(&basis).expect("index");
    let block_len = index.block_length();
    let counters = index.seq_match_counters();
    counters.reset();

    let window = &basis[block_len..2 * block_len];
    let digest = index.block(1).rolling();
    let found = index.try_next_match_bytes(0, digest, window);
    assert_eq!(found, Some(1), "linked successor must be returned");
    assert_eq!(counters.probes(), 1);
    assert_eq!(counters.hits(), 1);
    assert_eq!(counters.misses(), 0);
}

/// A successor probe with the wrong window misses on the strong checksum
/// and counts as a miss. The full lookup remains the safety net.
#[test]
fn try_next_match_bytes_misses_on_wrong_window() {
    let basis = distinct_basis(4);
    let index = build_index(&basis).expect("index");
    let block_len = index.block_length();
    let counters = index.seq_match_counters();
    counters.reset();

    let mut window = basis[block_len..2 * block_len].to_vec();
    // Force a strong-checksum mismatch while leaving the rolling sum alone
    // when the rolling sum happens to round-trip; we explicitly construct
    // the digest from the corrupted window to skip the rolling fast-path
    // rejection and prove the strong-checksum gate fires.
    window[0] ^= 0xAA;
    let digest = RollingDigest::from_bytes(&window);
    let found = index.try_next_match_bytes(0, digest, &window);
    assert_eq!(found, None, "corrupted window must not pass strong verify");
    assert_eq!(counters.probes(), 1);
    assert_eq!(counters.hits(), 0);
    assert_eq!(counters.misses(), 1);
}

/// Probing with `last_match` at the tail produces no probe at all: there is
/// nothing to look up, so the counters stay at zero.
#[test]
fn try_next_match_bytes_tail_block_skips_probe() {
    let basis = distinct_basis(4);
    let index = build_index(&basis).expect("index");
    let block_len = index.block_length();
    let last = index.block_count() - 1;
    let counters = index.seq_match_counters();
    counters.reset();

    let window = &basis[..block_len];
    let digest = index.block(0).rolling();
    let found = index.try_next_match_bytes(last, digest, window);
    assert_eq!(found, None);
    assert_eq!(counters.probes(), 0);
    assert_eq!(counters.hits(), 0);
    assert_eq!(counters.misses(), 0);
}

/// Slices probe mirrors the contiguous probe and accepts split windows
/// straight from a ring buffer.
#[test]
fn try_next_match_slices_handles_split_window() {
    let basis = distinct_basis(4);
    let index = build_index(&basis).expect("index");
    let block_len = index.block_length();
    let counters = index.seq_match_counters();
    counters.reset();

    let window = &basis[block_len..2 * block_len];
    let split = block_len / 3;
    let (first, second) = window.split_at(split);
    let digest = index.block(1).rolling();
    let found = index.try_next_match_slices(0, digest, first, second);
    assert_eq!(found, Some(1));
    assert_eq!(counters.hits(), 1);
}

/// Slices probe rejects windows whose combined length is wrong without
/// touching the strong checksum, counted as a miss.
#[test]
fn try_next_match_slices_wrong_length_misses() {
    let basis = distinct_basis(4);
    let index = build_index(&basis).expect("index");
    let counters = index.seq_match_counters();
    counters.reset();

    let digest = index.block(1).rolling();
    let found = index.try_next_match_slices(0, digest, &[], &[]);
    assert_eq!(found, None);
    assert_eq!(counters.probes(), 1);
    assert_eq!(counters.hits(), 0);
    assert_eq!(counters.misses(), 1);
}

/// End-to-end: when the source is the basis verbatim, the generator's
/// chain loop drives the lookahead probe at every adjacent boundary. The
/// counter must show one hit per inner-chain transition (block_count - 1
/// transitions across one contiguous chain run).
#[test]
fn seq_match_hits_consecutive_blocks() {
    let basis = distinct_basis(6);
    let index = build_index(&basis).expect("index");
    let counters = index.seq_match_counters();
    counters.reset();
    let generator = DeltaGenerator::new();
    let script = generator
        .generate(Cursor::new(basis.clone()), &index)
        .expect("script");

    // Round-trip: the script must reproduce the basis byte-for-byte.
    let mut basis_cursor = Cursor::new(basis.clone());
    let mut output = Vec::new();
    apply_delta(&mut basis_cursor, &mut output, &index, &script).expect("apply");
    assert_eq!(output, basis);

    // Every match after the first lands through the seq-match probe.
    let expected_probes = (index.block_count() as u64) - 1;
    assert_eq!(
        counters.probes(),
        expected_probes,
        "expected one seq-match probe per inner-chain transition"
    );
    assert_eq!(
        counters.hits(),
        expected_probes,
        "every sequential transition should hit the linked successor"
    );
    assert_eq!(counters.misses(), 0);
}

/// End-to-end: when the source diverges after a confirmed run, the
/// lookahead probe misses at the divergence and the full lookup picks
/// up the trailing literal/match work. Counters must show at least one
/// miss alongside the run-internal hits.
#[test]
fn seq_match_falls_back_on_mismatch() {
    let block_len = TEST_BLOCK_LENGTH as usize;
    let basis = distinct_basis(5);
    let mut source = basis[..3 * block_len].to_vec();
    // Append literal bytes that do not appear in the basis to force the
    // lookahead probe to miss at the boundary after block 2.
    source.extend(std::iter::repeat_n(0xFFu8, block_len + 7));

    let index = build_index(&basis).expect("index");
    let counters = index.seq_match_counters();
    counters.reset();
    let generator = DeltaGenerator::new();
    let script = generator
        .generate(Cursor::new(source.clone()), &index)
        .expect("script");

    // Apply round-trip must hold even with the fallback path engaged.
    let mut basis_cursor = Cursor::new(basis.clone());
    let mut output = Vec::new();
    apply_delta(&mut basis_cursor, &mut output, &index, &script).expect("apply");
    assert_eq!(output, source);

    // Two confirmed transitions inside the basis-aligned prefix, both
    // taken by the seq-match probe.
    assert!(
        counters.hits() >= 2,
        "expected at least the two in-run transitions to hit the seq-match probe, got {}",
        counters.hits()
    );
    // The divergence after block 2 forces at least one probe to miss and
    // fall through to the full lookup.
    assert!(
        counters.misses() >= 1,
        "expected at least one seq-match miss at the divergence point, got {}",
        counters.misses()
    );
}

/// Per-segment isolation (ZSO-7): when an index is rebuilt for a new
/// signature, the `next_match` links and the seq-match counters must
/// reset cleanly. Mirrors the bithash leak test.
#[test]
fn seq_match_state_does_not_leak_between_indexes() {
    let basis_a = distinct_basis(4);
    let mut index = build_index(&basis_a).expect("index_a");
    let counters = index.seq_match_counters();

    // Warm up the counters via the contiguous probe.
    let block_len = index.block_length();
    let window = &basis_a[block_len..2 * block_len];
    let digest = index.block(1).rolling();
    let _ = index.try_next_match_bytes(0, digest, window);
    assert_eq!(counters.hits(), 1, "warm-up probe should hit");

    // Rebuild over a fresh, smaller basis. The successor links from
    // `basis_a` must be wiped and the counters must be back to zero.
    let basis_b = distinct_basis(2);
    let params_b = SignatureLayoutParams::new(
        basis_b.len() as u64,
        Some(NonZeroU32::new(TEST_BLOCK_LENGTH).unwrap()),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(TEST_STRONG_LEN).unwrap(),
    );
    let layout_b = calculate_signature_layout(params_b).expect("layout_b");
    let sig_b = generate_file_signature(basis_b.as_slice(), layout_b, SignatureAlgorithm::Md4)
        .expect("signature_b");
    let ok = index.rebuild(&sig_b, SignatureAlgorithm::Md4);
    assert!(ok, "rebuild must report success on a full-block signature");

    // Counter handle is shared with the index via Arc, so a successful
    // reset propagates without re-fetching the handle.
    assert_eq!(counters.probes(), 0);
    assert_eq!(counters.hits(), 0);
    assert_eq!(counters.misses(), 0);

    // Successor chain matches the new basis (two blocks, one link).
    assert_eq!(index.next_match(0), Some(1));
    assert_eq!(index.next_match(1), None);

    // Indices past the new tail must not surface a successor inherited
    // from the larger pre-rebuild basis.
    for k in 2..6 {
        assert_eq!(index.next_match(k), None, "stale link at {k}");
    }
}

proptest! {
    // Property: for every full-length block of a random distinct basis, the
    // successor recorded in `next_match` matches the next full-length block
    // in source order and the contiguous probe verifies cleanly.
    #[test]
    fn next_match_is_consistent_with_basis_order(n_blocks in 2usize..=8) {
        let basis = distinct_basis(n_blocks);
        let index = build_index(&basis).expect("index");
        let block_len = index.block_length();
        let last = index.block_count() - 1;
        let counters = index.seq_match_counters();
        counters.reset();

        for k in 0..last {
            let successor = index.next_match(k).expect("interior block has successor");
            prop_assert_eq!(successor, k + 1);

            let window = &basis[(k + 1) * block_len..(k + 2) * block_len];
            let digest = index.block(successor).rolling();
            let found = index.try_next_match_bytes(k, digest, window);
            prop_assert_eq!(found, Some(successor));
        }
        prop_assert_eq!(index.next_match(last), None);
        prop_assert_eq!(counters.hits() as usize, last);
        prop_assert_eq!(counters.misses(), 0);
    }
}

/// Hand-rolled regression mirroring the generator wiring: a single Copy
/// token over the whole basis must emit, demonstrating the seq-match path
/// coalesces consecutive matches.
#[test]
fn generator_emits_single_copy_token_over_full_basis() {
    let basis = distinct_basis(4);
    let index = build_index(&basis).expect("index");
    let generator = DeltaGenerator::new();
    let script = generator
        .generate(Cursor::new(basis.clone()), &index)
        .expect("script");
    let copy_tokens: Vec<&DeltaToken> = script
        .tokens()
        .iter()
        .filter(|t| matches!(t, DeltaToken::Copy { .. }))
        .collect();
    assert_eq!(
        copy_tokens.len(),
        1,
        "seq-match must coalesce the chain into one Copy token"
    );
}
