//! Class-level tests for the equal-weak-checksum chain bound
//! ([`MAX_CHAIN_LEN`], upstream `match.c`, CVE-2026-70453).
//!
//! The signature that populates a chain is peer-supplied, so a hostile peer
//! can hand the sender thousands of basis blocks sharing one weak checksum.
//! Without a bound, every source offset walks the whole chain and the scan
//! degrades to `O(source_len * chain_len)`.
//!
//! These tests are written over the whole class - chain length and the
//! candidate's position within the chain, swept across the boundary - rather
//! than over one hand-picked case, so a regression cannot slip through by
//! moving the cap or the counter.

use super::*;
use crate::{DeltaGenerator, DeltaToken, apply_delta};
use checksums::RollingDigest;
use protocol::ProtocolVersion;
use signature::{SignatureLayoutParams, calculate_signature_layout, generate_file_signature};
use std::num::{NonZeroU8, NonZeroU32};

/// Block length of the crafted basis. Must exceed [`SITE_STRIDE`] so both
/// halves of a site pair land inside the same block.
const BLOCK_LEN: usize = 2048;

/// Distance between the two bytes of a site pair.
///
/// The rolling sum weights byte `k` by `BLOCK_LEN - k`, so a `+d` at `i` and a
/// `-d` at `i + stride` shifts `s2` by `d * stride`. Choosing `d = 64` and
/// `stride = 1024` gives `65536`, which is zero modulo the 16-bit `s2`.
const SITE_STRIDE: usize = 1024;

/// Builds a `BLOCK_LEN`-byte block whose rolling checksum is exactly `(0, 0)`
/// - identical to an all-zero window - but whose content is not all-zero.
///
/// Each set bit of `variant` toggles one site pair: byte `site` becomes `+64`
/// and byte `site + SITE_STRIDE` becomes `-64` under the signed-byte
/// interpretation upstream uses (`checksum.c`, `schar *buf`). `s1` is
/// unchanged (`+64 - 64`) and `s2` shifts by a multiple of `2^16`, so every
/// variant collides on the weak checksum while differing in content, and
/// therefore in its strong checksum.
fn crafted_block(variant: u32) -> Vec<u8> {
    let mut block = vec![0u8; BLOCK_LEN];
    for bit in 0..12 {
        if variant & (1 << bit) != 0 {
            let site = bit * 64;
            block[site] = 0x40;
            block[site + SITE_STRIDE] = 0xC0;
        }
    }
    block
}

/// Concatenates `n_blocks` distinct crafted blocks into a hostile basis.
///
/// Variant 0 is skipped because it is the all-zero block, which would match an
/// all-zero probe window and mask the chain walk under test.
fn hostile_basis(n_blocks: u32) -> Vec<u8> {
    let mut basis = Vec::with_capacity(n_blocks as usize * BLOCK_LEN);
    for variant in 1..=n_blocks {
        basis.extend_from_slice(&crafted_block(variant));
    }
    basis
}

fn index_over(basis: &[u8], block_len: u32) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        basis.len() as u64,
        Some(NonZeroU32::new(block_len).expect("non-zero block length")),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).expect("non-zero checksum length"),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature =
        generate_file_signature(basis, layout, SignatureAlgorithm::Md4).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
}

fn block_bytes(basis: &[u8], position: usize) -> &[u8] {
    &basis[position * BLOCK_LEN..(position + 1) * BLOCK_LEN]
}

/// Probes the index with the exact content of basis block `position`.
fn probe(index: &DeltaSignatureIndex, basis: &[u8], position: usize) -> Option<usize> {
    let window = block_bytes(basis, position);
    index.find_match_bytes(RollingDigest::from_bytes(window), window)
}

/// The construction the rest of this module rests on: every crafted block
/// shares the all-zero window's rolling checksum, so they all land in one
/// chain, yet no two share a strong checksum.
#[test]
fn crafted_blocks_collide_on_the_weak_checksum_only() {
    let basis = hostile_basis(64);
    let index = index_over(&basis, BLOCK_LEN as u32);

    let zero_window = vec![0u8; BLOCK_LEN];
    let zero_digest = RollingDigest::from_bytes(&zero_window);

    for position in 0..64 {
        let block = index.block(position);
        assert_eq!(
            block.rolling(),
            zero_digest,
            "block {position} must collide with the all-zero window",
        );
    }
    for position in 1..64 {
        assert_ne!(
            index.block(0).strong(),
            index.block(position).strong(),
            "blocks 0 and {position} must differ on the strong checksum",
        );
    }
    // The whole chain really is one chain: every crafted block is reachable
    // from the same lookup key.
    assert_eq!(
        index.lookup_probe(zero_digest.sum1(), zero_digest.sum2()),
        64,
        "all crafted blocks must share one lookup chain",
    );
}

/// CLASS TEST over the candidate's position in the chain.
///
/// upstream `match.c` counts candidates that reach the strong-checksum compare
/// and abandons the offset once the count would exceed `MAX_CHAIN_LEN`. With
/// candidates walked in insertion order, basis block `p` is the `p + 1`-th
/// candidate, so it is reachable exactly while `p < MAX_CHAIN_LEN`.
#[test]
fn candidate_is_reachable_exactly_below_the_bound() {
    let n_blocks = MAX_CHAIN_LEN + 200;
    let basis = hostile_basis(n_blocks);
    let index = index_over(&basis, BLOCK_LEN as u32);

    let positions = [
        0,
        1,
        2,
        (MAX_CHAIN_LEN / 2) as usize,
        (MAX_CHAIN_LEN - 2) as usize,
        (MAX_CHAIN_LEN - 1) as usize,
        MAX_CHAIN_LEN as usize,
        (MAX_CHAIN_LEN + 1) as usize,
        (n_blocks - 1) as usize,
    ];

    for position in positions {
        let found = probe(&index, &basis, position);
        if (position as u32) < MAX_CHAIN_LEN {
            assert_eq!(
                found,
                Some(position),
                "candidate at chain position {position} is within the bound \
                 and must still match",
            );
        } else {
            assert_eq!(
                found, None,
                "candidate at chain position {position} is past the bound, so \
                 the walk must have stopped and reported a non-match",
            );
        }
    }
}

/// CLASS TEST over chain length. However long the chain grows, the walk stops
/// at the bound: the first candidate always matches and the last matches only
/// while the chain is short enough to reach it.
#[test]
fn bound_holds_across_chain_lengths() {
    for n_blocks in [
        1u32,
        2,
        16,
        MAX_CHAIN_LEN - 1,
        MAX_CHAIN_LEN,
        MAX_CHAIN_LEN + 1,
    ] {
        let basis = hostile_basis(n_blocks);
        let index = index_over(&basis, BLOCK_LEN as u32);

        assert_eq!(
            probe(&index, &basis, 0),
            Some(0),
            "the first candidate must match at chain length {n_blocks}",
        );

        let last = (n_blocks - 1) as usize;
        let expected = ((last as u32) < MAX_CHAIN_LEN).then_some(last);
        assert_eq!(
            probe(&index, &basis, last),
            expected,
            "last candidate at chain length {n_blocks}",
        );
    }
}

/// Reaching the bound must never corrupt a transfer. An all-zero source over
/// the hostile basis matches nothing, so the delta is pure literal data - and
/// applying it must still reproduce the source byte for byte.
#[test]
fn skipped_data_is_sent_literally_not_corrupted() {
    let n_blocks = MAX_CHAIN_LEN + 200;
    let basis = hostile_basis(n_blocks);
    let index = index_over(&basis, BLOCK_LEN as u32);

    let source = vec![0u8; 8 * BLOCK_LEN];
    let script = DeltaGenerator::new()
        .generate(std::io::Cursor::new(source.clone()), &index)
        .expect("generate");

    assert!(
        script
            .tokens()
            .iter()
            .all(|token| matches!(token, DeltaToken::Literal(_))),
        "no crafted block matches an all-zero window, so the delta is literal",
    );

    let mut rebuilt = Vec::new();
    apply_delta(std::io::Cursor::new(&basis), &mut rebuilt, &index, &script).expect("apply");
    assert_eq!(
        rebuilt, source,
        "the delta must reproduce the source exactly"
    );
}

/// NEUTRALITY. On an honest basis the bound is inert: no chain comes close to
/// it, so the bound cannot change which blocks the matcher finds and cannot
/// perturb the emitted delta.
#[test]
fn bound_is_inert_on_a_realistic_basis() {
    const HONEST_BLOCK_LEN: u32 = 512;
    const HONEST_BLOCKS: usize = 10_000;

    let mut basis = vec![0u8; HONEST_BLOCKS * HONEST_BLOCK_LEN as usize];
    let mut state: u32 = 0x1234_5678;
    for byte in basis.iter_mut() {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *byte = (state >> 24) as u8;
    }
    let index = index_over(&basis, HONEST_BLOCK_LEN);

    let longest = (0..index.block_count())
        .map(|position| {
            let rolling = index.block(position).rolling();
            index.lookup_probe(rolling.sum1(), rolling.sum2())
        })
        .max()
        .expect("at least one block");

    assert!(
        (longest as u32) < MAX_CHAIN_LEN,
        "longest honest chain was {longest}, which reaches the bound of \
         {MAX_CHAIN_LEN}; the bound would stop being observably neutral",
    );

    // End to end: an unmodified source over an honest basis must still resolve
    // to pure Copy tokens.
    let script = DeltaGenerator::new()
        .generate(std::io::Cursor::new(basis.clone()), &index)
        .expect("generate");
    assert!(
        script
            .tokens()
            .iter()
            .all(|token| matches!(token, DeltaToken::Copy { .. })),
        "an identical source must match every block",
    );
}

/// Pins the cap to upstream's literal value. oc must not invent a different
/// limit: a smaller one starts sending literals where upstream sends a Copy,
/// a larger one leaves the DoS window open.
#[test]
fn bound_matches_upstream_max_chain_len() {
    assert_eq!(
        MAX_CHAIN_LEN, 1024,
        "upstream match.c defines MAX_CHAIN_LEN as 1024",
    );
}
