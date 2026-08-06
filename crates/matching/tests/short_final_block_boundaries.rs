//! Boundary cases for the basis's trailing short block.
//!
//! `short_final_block_match.rs` pins the main behaviours: the tail is matched,
//! it is emitted as a standalone `Copy`, and a mid-file stripe does not probe
//! it. This file covers the edges of that handling, which have a documented
//! history of going wrong.
//!
//! zsync - whose `librcksum` these techniques derive from - shipped two fixes in
//! exactly this area (NEWS, github.com/cph6/zsync):
//!
//! - 0.6 - "fix out-of-bounds memory access when processing last block of
//!   non-compressed download"
//! - 0.6.1 - "fix librcksum handling of zsync streams with sequential_matches
//!   == 1; it was giving false negatives when applying the rsync algorithm"
//!
//! The first is a length/slicing bug on the final short block. Rust turns that
//! class of defect into a panic rather than silent corruption, but a panic on
//! the last block of a transfer is still an availability defect, and it would
//! only reproduce on files whose length is not a block multiple - which is most
//! files. The second is a *silent* defect: a real match quietly demoted to a
//! literal, which looks like "works, slightly less efficient" and so survives
//! review. Neither is caught by a test that only checks byte totals.

use matching::{DeltaGenerator, DeltaScript, DeltaSignatureIndex, DeltaToken, apply_delta};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};
use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

const BLOCK_LEN: u32 = 16;

/// Deterministic, non-repeating filler.
///
/// A short-cycle pattern would make these tests lie: with a period that divides
/// into the scan, an unaligned window near EOF can match an earlier full block
/// by content and swallow the trailing bytes, so "the tail was not matched"
/// would be a statement about the data rather than about the matcher. A 64-bit
/// LCG has a period far beyond any fixture here.
fn pattern(len: usize) -> Vec<u8> {
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u8
        })
        .collect()
}

fn build_index(data: &[u8], block_len: u32) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        Some(NonZeroU32::new(block_len).unwrap()),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature =
        generate_file_signature(data, layout, SignatureAlgorithm::Md4).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
}

fn scan(source: &[u8], index: &DeltaSignatureIndex) -> DeltaScript {
    DeltaGenerator::new()
        .generate(Cursor::new(source.to_vec()), index)
        .expect("scan")
}

fn copy_lens(script: &DeltaScript) -> Vec<usize> {
    script
        .tokens()
        .iter()
        .filter_map(|token| match token {
            DeltaToken::Copy { len, .. } => Some(*len),
            DeltaToken::Literal(_) => None,
        })
        .collect()
}

fn reconstruct(script: &DeltaScript, basis: &[u8], index: &DeltaSignatureIndex) -> Vec<u8> {
    let mut out = Vec::new();
    apply_delta(&mut Cursor::new(basis.to_vec()), &mut out, index, script).expect("apply");
    out
}

/// The tail probe must hold at both extremes of the legal `tail_len` range, not
/// just at the comfortable middle. `tail_len == 1` is the minimum a short block
/// can be; `block_len - 1` is the maximum. Off-by-one handling of either bound
/// is precisely the shape of the zsync 0.6 out-of-bounds defect.
#[test]
fn tail_lengths_at_both_extremes_are_matched_and_reconstruct() {
    for tail_len in [1usize, (BLOCK_LEN - 1) as usize] {
        let basis = pattern(BLOCK_LEN as usize * 3 + tail_len);
        let index = build_index(&basis, BLOCK_LEN);
        assert_eq!(index.block_count(), 4, "3 full blocks plus a short tail");
        assert_eq!(
            index.block(3).len(),
            tail_len,
            "fixture must end with a {tail_len}-byte block"
        );

        let script = scan(&basis, &index);
        assert_eq!(
            copy_lens(&script).last().copied(),
            Some(tail_len),
            "tail_len={tail_len} must be Copied with its own length, not sent as literal"
        );
        assert_eq!(script.literal_bytes(), 0, "tail_len={tail_len}");
        assert_eq!(reconstruct(&script, &basis, &index), basis);
    }
}

/// A basis that divides evenly has no short block, and the probe must be a clean
/// no-op there.
///
/// upstream: `sender.c:109-110` guards the substitution with
/// `&& s->remainder != 0`, so every block - including the last - keeps
/// `blength`. Nothing may be emitted shorter than a full block.
#[test]
fn an_exact_multiple_basis_emits_no_short_copy() {
    let basis = pattern(BLOCK_LEN as usize * 4);
    let index = build_index(&basis, BLOCK_LEN);
    assert_eq!(index.block_count(), 4);
    for position in 0..4 {
        assert_eq!(index.block(position).len(), BLOCK_LEN as usize);
    }

    let script = scan(&basis, &index);
    for len in copy_lens(&script) {
        assert_eq!(
            len % BLOCK_LEN as usize,
            0,
            "no Copy may be shorter than a full block when there is no remainder"
        );
    }
    assert_eq!(script.literal_bytes(), 0);
    assert_eq!(reconstruct(&script, &basis, &index), basis);
}

/// `find_tail_match` takes the window as TWO slices, because the caller's ring
/// buffer may have wrapped. Splitting at the wrong place is exactly how the
/// zsync 0.6 out-of-bounds access happened, so every split is exercised here -
/// including both degenerate ends, `first` empty and `second` empty.
#[test]
fn find_tail_match_accepts_every_slice_split_of_the_window() {
    let tail_len = (BLOCK_LEN - 5) as usize;
    let basis = pattern(BLOCK_LEN as usize * 2 + tail_len);
    let index = build_index(&basis, BLOCK_LEN);
    let tail_block = index.block_count() - 1;
    assert_eq!(index.block(tail_block).len(), tail_len);

    let tail = &basis[basis.len() - tail_len..];
    let digest = checksums::RollingDigest::from_bytes(tail);

    for split in 0..=tail_len {
        let (first, second) = tail.split_at(split);
        assert_eq!(
            index.find_tail_match(digest, first, second, None),
            Some(tail_block),
            "split at {split} of {tail_len} must find the same block"
        );
    }
}

/// The same two-slice path, driven through the real scan rather than called
/// directly.
///
/// The generator's ring buffer only wraps once it has been filled and drained
/// across the end of its backing storage, which needs a source long enough for
/// the scan to have consumed several blocks before EOF. Without a case like
/// this the wrapped `(first, second)` arm is reachable in production but never
/// executed by a test.
#[test]
fn a_wrapped_window_still_matches_the_tail_through_the_scan() {
    let tail_len = 9usize;
    let basis = pattern(BLOCK_LEN as usize * 40 + tail_len);
    let index = build_index(&basis, BLOCK_LEN);
    assert_eq!(index.block(index.block_count() - 1).len(), tail_len);

    // Break an early block so the scan slides byte-by-byte and the ring buffer
    // wraps, instead of clearing on every aligned match.
    let mut source = basis.clone();
    source[BLOCK_LEN as usize + 1] ^= 0xFF;

    let script = scan(&source, &index);
    assert_eq!(
        copy_lens(&script).last().copied(),
        Some(tail_len),
        "the tail must still be found when the window is wrapped"
    );
    assert_eq!(reconstruct(&script, &source, &index), source);
}

/// zsync 0.6.1 fixed `sequential_matches == 1` giving FALSE NEGATIVES - real
/// matches silently demoted to literals.
///
/// The failure signature is not a crash and not a wrong result, just a match
/// quietly becoming a literal, so a totals-only test would not distinguish it
/// from correct behaviour on a partially-matching fixture. Here the source is
/// the basis, so at `needed == 1` every single block including the tail must be
/// Copied and the literal count must be exactly zero - which no false negative
/// can satisfy.
#[test]
fn seq_matches_one_demotes_nothing() {
    let tail_len = 11usize;
    let basis = pattern(BLOCK_LEN as usize * 5 + tail_len);
    let index = build_index(&basis, BLOCK_LEN);

    let script = DeltaGenerator::new()
        .with_consecutive_match_needed(1)
        .generate(Cursor::new(basis.clone()), &index)
        .expect("seq=1 scan");

    assert_eq!(
        script.literal_bytes(),
        0,
        "no block may be demoted at seq_matches == 1: {:?}",
        script.tokens()
    );
    assert_eq!(copy_lens(&script).iter().sum::<usize>(), basis.len());
    assert_eq!(
        copy_lens(&script).last().copied(),
        Some(tail_len),
        "the tail is a block like any other at seq_matches == 1"
    );
    assert_eq!(reconstruct(&script, &basis, &index), basis);
}
