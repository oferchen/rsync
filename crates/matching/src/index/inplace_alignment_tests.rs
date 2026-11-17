//! Bound on the `--inplace` aligned block index.
//!
//! upstream: match.c:295 and :301 (rsync 3.5.2) - `aligned_i` advances one
//! block per boundary the scan passes and is only used while
//! `aligned_i < s->count`; 3.5.2 added that bound after an out-of-bounds read
//! of `s->sums[aligned_i]`. Here the index is
//! `ceil(offset / block_length)`, which reaches `block_count` at or past the
//! last boundary. Without the bound in
//! [`DeltaSignatureIndex::check_block_match_slices`] the lookup would index
//! past the signature and panic (Rust bounds-checks the slice, so the failure
//! is a crash of the scan, not a stray read).

use super::*;
use checksums::RollingChecksum;
use protocol::ProtocolVersion;
use signature::{SignatureLayoutParams, calculate_signature_layout, generate_file_signature};
use std::num::{NonZeroU8, NonZeroU32};

const BLOCK_LEN: usize = 700;

fn index_over(basis: &[u8]) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        basis.len() as u64,
        Some(NonZeroU32::new(BLOCK_LEN as u32).unwrap()),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature =
        generate_file_signature(basis, layout, SignatureAlgorithm::Md4).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
}

/// An all-zero window: its weak sum is zero, so off a boundary the realign
/// arm (match.c:300) looks at the next boundary too.
fn zero_window() -> (Vec<u8>, RollingDigest) {
    let window = vec![0u8; BLOCK_LEN];
    let mut rolling = RollingChecksum::new();
    rolling.update(&window);
    (window, rolling.digest())
}

/// Sweeps every offset from one block before the last boundary to two blocks
/// past it, on and off boundaries.
fn assert_no_alignment_past_the_end(index: &DeltaSignatureIndex, full_blocks: usize) {
    let (window, digest) = zero_window();
    let last_boundary = (full_blocks * BLOCK_LEN) as u64;
    for offset in last_boundary - BLOCK_LEN as u64 + 1..=last_boundary + 2 * BLOCK_LEN as u64 {
        assert_eq!(
            index.inplace_alignment(offset, digest, &window, &[]),
            InplaceAlignment::Keep,
            "offset {offset}: no block starts at or after the last boundary"
        );
    }
}

/// The premise: a zero window at a boundary of a zero basis does align, so
/// the sweeps below are not passing because the arm never engages.
#[test]
fn a_zero_window_at_a_boundary_aligns_to_its_block() {
    let index = index_over(&vec![0u8; 3 * BLOCK_LEN]);
    let (window, digest) = zero_window();
    assert_eq!(
        index.inplace_alignment(BLOCK_LEN as u64, digest, &window, &[]),
        InplaceAlignment::Aligned(1)
    );
    assert_eq!(
        index.inplace_alignment(BLOCK_LEN as u64 - 1, digest, &window, &[]),
        InplaceAlignment::Realign { block: 1, skip: 1 }
    );
}

/// A basis whose length is an exact multiple of the block length: the last
/// boundary's aligned index equals `block_count`.
#[test]
fn inplace_alignment_stops_at_the_end_of_an_exact_multiple_basis() {
    let index = index_over(&vec![0u8; 3 * BLOCK_LEN]);
    assert_eq!(index.block_count(), 3);
    assert_no_alignment_past_the_end(&index, 3);
}

/// A basis ending in a short block: the last boundary's aligned index is the
/// short block (rejected on length, match.c:304) and one past it is
/// `block_count`.
#[test]
fn inplace_alignment_stops_at_the_end_of_a_short_tailed_basis() {
    let index = index_over(&vec![0u8; 3 * BLOCK_LEN + 300]);
    assert_eq!(index.block_count(), 4);
    assert_no_alignment_past_the_end(&index, 3);
}
