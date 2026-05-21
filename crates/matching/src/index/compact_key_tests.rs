//! Tests for the zsync-inspired compact rolling-key encoding (ZSO-4).
//!
//! Pins the contracts in `project_zsync_optimizations.md` and the inline
//! design notes on [`super::compact_lookup`]:
//!
//! - The bucket array is capped at `2^16` slots regardless of basis size,
//!   matching the `rsum_a_mask` keyspace zsync uses in
//!   `librcksum/hash.c:45`.
//! - Synthetic same-bucket collisions (`rsum >> 16` equal, lower 16 bits
//!   differ) are resolved by the in-bucket discriminator without leaking
//!   false positives into the strong-checksum verify.
//! - End-to-end correctness: an N-block basis with all matching source
//!   yields exactly N `Copy` tokens at the expected offsets, proving the
//!   compact-key reshape preserves rolling-rsum match semantics.
//! - Per-segment ZSO-7 isolation: [`super::DeltaSignatureIndex::rebuild`]
//!   wipes both the bucket heads and the chain backing store so a stale
//!   basis cannot resurface after the next segment populates.
//!
//! Wire-format parity is enforced separately by the protocol golden tests
//! in `crates/protocol/tests/`; the assertions here cover the in-memory
//! contract only.

use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

use super::DeltaSignatureIndex;
use super::compact_lookup::CompactLookup;
use crate::generator::DeltaGenerator;
use crate::script::{DeltaToken, apply_delta};

const TEST_BLOCK_LENGTH: u32 = 512;
const TEST_STRONG_LEN: u8 = 16;

/// Builds a `DeltaSignatureIndex` over `basis` using the test's fixed
/// block layout. Returns `None` when the basis is shorter than one full
/// block, which callers bound out by construction.
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

/// Distinct-content basis so the strong-checksum verify resolves each
/// block uniquely. Mirrors the helper in `seq_match_tests.rs` so the two
/// fixtures stay aligned.
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

/// The bucket array never grows beyond `2^16` slots, even when the basis
/// signature would naively suggest a larger table.
#[test]
fn bucket_size_is_capped_at_2_16() {
    // 70 000 blocks > 2^16 = 65 536, so the naive "next-power-of-two of
    // 2 * n_entries" expansion would jump past the cap. The compact-key
    // table must clamp at `2^16` so the bucket array stays at most 256 KiB.
    let huge = CompactLookup::with_capacity(70_000);
    assert_eq!(huge.capacity(), 1 << 16);

    // The publicly observable `lookup_capacity` accessor on the index
    // honours the same cap end-to-end, so callers that bin by cache level
    // never see an over-budget figure.
    let basis = distinct_basis(8);
    let index = build_index(&basis).expect("index for tiny basis");
    assert!(index.lookup_capacity() <= 1 << 16);

    // `lookup_bytes` traverses `CompactLookup::bucket_bytes` so a regular
    // (non-`--benches`) build sees the call chain and clippy stops marking
    // `bucket_bytes` as dead code. The 256 KiB cap mirrors the bucket cap.
    assert!(index.lookup_bytes() <= 256 * 1024);
}

/// Synthetic `(sum1, sum2)` pairs that share the upper-half bucket
/// address but disagree on the lower-half discriminator must each be
/// findable under their own key, without leaking the sibling entry.
#[test]
fn bucket_collisions_resolved_by_lower_half_check() {
    let mut table = CompactLookup::with_capacity(8);

    // Two rsums with identical `rsum >> 16` but distinct lower halves.
    // Insert them under the *same* bucket and verify the chain walk
    // returns each entry exclusively under its own discriminator.
    let bucket_sum2: u16 = 0xBEEF;
    table.insert(0x0001, bucket_sum2, 11);
    table.insert(0x0002, bucket_sum2, 22);
    table.insert(0x0003, bucket_sum2, 33);

    let a: Vec<usize> = table.find_all(0x0001, bucket_sum2).collect();
    let b: Vec<usize> = table.find_all(0x0002, bucket_sum2).collect();
    let c: Vec<usize> = table.find_all(0x0003, bucket_sum2).collect();
    assert_eq!(a, vec![11]);
    assert_eq!(b, vec![22]);
    assert_eq!(c, vec![33]);

    // A lower-half discriminator that was never inserted into the shared
    // bucket must produce no hits, even though the bucket itself is
    // populated.
    let none: Vec<usize> = table.find_all(0x00FF, bucket_sum2).collect();
    assert!(none.is_empty(), "unrelated discriminator must not leak");

    // The bucket address derives from `rsum >> 16` for every callable
    // synthesis, so the bench-internal helper and the bucket-for helper
    // agree on the placement.
    let synthesized_rsum = (u32::from(bucket_sum2) << 16) | 0x0001;
    assert_eq!(
        DeltaSignatureIndex::bucket_for(synthesized_rsum),
        bucket_sum2
    );
    assert_eq!(CompactLookup::bucket_for(synthesized_rsum), bucket_sum2);
}

/// An all-matching N-block source must reproduce byte-for-byte under the
/// compact-key reshape. Emitted Copy tokens cover every basis block (the
/// seq-match coalescer may collapse them into a single span), no literal
/// bytes leak, and apply_delta reproduces the original source.
#[test]
fn compact_key_does_not_break_match_correctness() {
    const N_BLOCKS: usize = 16;
    let basis = distinct_basis(N_BLOCKS);
    let index = build_index(&basis).expect("index for N-block basis");
    assert_eq!(index.block_count(), N_BLOCKS);

    let generator = DeltaGenerator::new();
    let script = generator
        .generate(Cursor::new(basis.as_slice()), &index)
        .expect("delta generation");

    let copy_tokens: Vec<&DeltaToken> = script
        .tokens()
        .iter()
        .filter(|t| matches!(t, DeltaToken::Copy { .. }))
        .collect();
    assert!(
        !copy_tokens.is_empty(),
        "matching source must emit at least one Copy token",
    );

    // Copy tokens (possibly coalesced) must address every basis block in
    // order without gaps. Track the cumulative byte offset the basis side
    // would read from; it must match `N_BLOCKS * block_length` at the end.
    let block_len = index.block_length();
    let mut basis_offset = 0usize;
    let mut next_block = 0u64;
    for tok in &copy_tokens {
        let DeltaToken::Copy {
            index: block_index,
            len,
        } = tok
        else {
            unreachable!("filtered above");
        };
        assert_eq!(
            *block_index, next_block,
            "Copy run must continue at the next basis block",
        );
        assert_eq!(*len % block_len, 0, "Copy len must be a block multiple");
        let n_blocks = len / block_len;
        next_block += n_blocks as u64;
        basis_offset += len;
    }
    assert_eq!(
        next_block as usize, N_BLOCKS,
        "Copy tokens must cover every basis block",
    );
    assert_eq!(
        basis_offset,
        N_BLOCKS * block_len,
        "Copy tokens must cover every basis byte",
    );
    assert_eq!(
        script.literal_bytes(),
        0,
        "an all-matching source must not emit any literal bytes",
    );

    let mut reconstructed = Vec::with_capacity(basis.len());
    apply_delta(
        Cursor::new(basis.as_slice()),
        &mut reconstructed,
        &index,
        &script,
    )
    .expect("apply_delta");
    assert_eq!(
        reconstructed, basis,
        "apply_delta(basis, tokens) must reproduce the source bytes",
    );
}

/// Per-segment ZSO-7 isolation: `rebuild` wipes the bucket heads and the
/// chain backing store so a stale basis cannot resurface as a phantom
/// match after the new segment populates.
#[test]
fn compact_key_state_resets_in_rebuild() {
    let basis_one = distinct_basis(6);
    let mut index = build_index(&basis_one).expect("first basis");

    // The pre-rebuild basis matches itself, anchoring the comparison after
    // the rebuild swaps in a disjoint basis.
    let pre_window: Vec<u8> = basis_one[..index.block_length()].to_vec();
    let pre_digest = index.block(0).rolling();
    assert!(index.find_match_bytes(pre_digest, &pre_window).is_some());

    // Build a brand-new signature that shares no content with `basis_one`
    // and feed it through `rebuild`. The compact bucket array, the chain
    // backing store, the tag table, the bithash, and the `next_match`
    // link table must all have lost every trace of `basis_one`.
    let mut basis_two = Vec::with_capacity(8 * TEST_BLOCK_LENGTH as usize);
    for k in 0..8 {
        let seed = ((k as u8).wrapping_mul(53)).wrapping_add(0x80);
        for j in 0..TEST_BLOCK_LENGTH as usize {
            basis_two.push(seed.wrapping_add((j as u8).wrapping_mul(19)));
        }
    }

    let params = SignatureLayoutParams::new(
        basis_two.len() as u64,
        Some(NonZeroU32::new(TEST_BLOCK_LENGTH).unwrap()),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(TEST_STRONG_LEN).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let sig_two = generate_file_signature(basis_two.as_slice(), layout, SignatureAlgorithm::Md4)
        .expect("signature");
    let ok = index.rebuild(&sig_two, SignatureAlgorithm::Md4);
    assert!(ok, "rebuild with full-length blocks must succeed");

    // The old basis must no longer match: every byte sequence from
    // `basis_one` was wiped from the compact bucket chains.
    assert!(
        index.find_match_bytes(pre_digest, &pre_window).is_none(),
        "stale pre-rebuild basis must not resurface as a match"
    );

    // The new basis matches itself end-to-end, proving the bucket table
    // was re-populated rather than left empty.
    for k in 0..index.block_count() {
        let digest = index.block(k).rolling();
        let offset = k * index.block_length();
        let window: Vec<u8> = basis_two[offset..offset + index.block_length()].to_vec();
        assert!(
            index.find_match_bytes(digest, &window).is_some(),
            "block {k} of the post-rebuild basis must match",
        );
    }
}
