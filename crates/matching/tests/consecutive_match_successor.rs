//! The consecutive-match scan trusts a block only when the next source window
//! matches the basis block that FOLLOWS it.
//!
//! With `OC_CONSECUTIVE_MATCH` both oc peers halve the per-block strong sum, so
//! one window matches a basis block by chance with probability
//! `p = 2^-(32+8*h)` instead of upstream's `2^-(32+8*s2)`. zsync's
//! `seq_matches` pays for the halving by requiring a match to be followed by
//! a match on the SUCCESSOR block (`librcksum/rsum.c` `next_match`). A run
//! that may continue on ANY matching block lets two independent chance
//! matches - or one genuine block followed by one chance match anywhere in
//! the basis - pass as trusted, and the halved sums then corrupt the file
//! silently until the whole-file checksum forces a redo.
//!
//! These cells pin the successor rule on genuine blocks and on a constructed
//! false match, and pin that a window refused as a successor is still
//! searched as the start of a new run.

use matching::{DeltaGenerator, DeltaScript, DeltaSignatureIndex, DeltaToken, apply_delta};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayout, SignatureLayoutParams, calculate_signature_layout,
    generate_file_signature,
};
use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

const BLOCK_LEN: usize = 512;
const BLOCKS: usize = 16;

/// Non-repeating filler, so no two basis blocks share content.
fn pattern(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u8
        })
        .collect()
}

fn layout(len: usize, strong_len: Option<u8>) -> SignatureLayout {
    let params = SignatureLayoutParams::new(
        len as u64,
        NonZeroU32::new(BLOCK_LEN as u32),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let base = calculate_signature_layout(params).expect("layout");
    match strong_len {
        None => base,
        Some(s) => SignatureLayout::from_raw_parts(
            base.block_length(),
            base.remainder(),
            base.block_count(),
            NonZeroU8::new(s).unwrap(),
        ),
    }
}

fn index_with(basis: &[u8], strong_len: Option<u8>) -> DeltaSignatureIndex {
    let signature = generate_file_signature(
        basis,
        layout(basis.len(), strong_len),
        SignatureAlgorithm::Md4,
    )
    .expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
}

fn block(basis: &[u8], j: usize) -> &[u8] {
    &basis[j * BLOCK_LEN..(j + 1) * BLOCK_LEN]
}

fn gated(source: &[u8], index: &DeltaSignatureIndex) -> DeltaScript<'static> {
    DeltaGenerator::new()
        .with_consecutive_match_needed(2)
        .generate(source, index)
        .expect("script")
}

fn copies(script: &DeltaScript<'_>) -> Vec<u64> {
    script
        .tokens()
        .iter()
        .filter_map(|t| match t {
            DeltaToken::Copy { index, .. } => Some(*index),
            DeltaToken::Literal(_) => None,
        })
        .collect()
}

fn reconstruct(basis: &[u8], index: &DeltaSignatureIndex, script: &DeltaScript<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    apply_delta(&mut Cursor::new(basis.to_vec()), &mut out, index, script).expect("apply");
    out
}

#[test]
fn a_block_followed_by_its_successor_is_copied() {
    let basis = pattern(BLOCKS * BLOCK_LEN, 0x5ec0_0001);
    let index = index_with(&basis, None);
    let source = [block(&basis, 3), block(&basis, 4)].concat();

    let script = gated(&source, &index);

    assert_eq!(copies(&script), [3, 4]);
    assert_eq!(script.literal_bytes(), 0);
    assert_eq!(reconstruct(&basis, &index, &script), source);
}

/// Blocks 3 and 7 both match genuinely, but 7 is not 3's successor, so
/// neither has the neighbour the halved strong sum needs: both are sent as
/// literals.
#[test]
fn a_block_followed_by_a_non_successor_block_is_not_trusted() {
    let basis = pattern(BLOCKS * BLOCK_LEN, 0x5ec0_0002);
    let index = index_with(&basis, None);
    let source = [block(&basis, 3), block(&basis, 7)].concat();

    let script = gated(&source, &index);

    assert_eq!(copies(&script), [] as [u64; 0]);
    assert_eq!(script.literal_bytes(), source.len() as u64);
    assert_eq!(reconstruct(&basis, &index, &script), source);
}

/// The window refused as block 3's successor still starts the run 7, 8: only
/// block 3, which has no neighbour, is demoted.
#[test]
fn a_window_refused_as_a_successor_starts_a_new_run() {
    let basis = pattern(BLOCKS * BLOCK_LEN, 0x5ec0_0003);
    let index = index_with(&basis, None);
    let source = [block(&basis, 3), block(&basis, 7), block(&basis, 8)].concat();

    let script = gated(&source, &index);

    assert_eq!(copies(&script), [7, 8]);
    assert_eq!(script.literal_bytes(), BLOCK_LEN as u64);
    assert_eq!(reconstruct(&basis, &index, &script), source);
}

/// Builds a window that differs from basis block `target` yet matches it on
/// the rolling sum and the one-byte strong sum the halved receiver sends for
/// a two-byte `s2length`.
///
/// Adding `(+1, -2, +1)` to three adjacent bytes keeps both rolling sums: the
/// byte total is unchanged and so is the position-weighted total
/// (`w - 2(w-1) + (w-2) = 0`). Trying that perturbation at successive
/// positions walks through distinct one-byte MD4 values until one equals the
/// block's; the rolling equality is checked rather than assumed, since the
/// sum reads bytes as signed.
fn forged_match(basis: &[u8], index: &DeltaSignatureIndex, target: usize) -> Vec<u8> {
    let genuine = block(basis, target);
    for pos in 0..BLOCK_LEN - 2 {
        let mut forged = genuine.to_vec();
        forged[pos] = forged[pos].wrapping_add(1);
        forged[pos + 1] = forged[pos + 1].wrapping_sub(2);
        forged[pos + 2] = forged[pos + 2].wrapping_add(1);
        let digest = checksums::RollingDigest::from_bytes(&forged);
        if index.check_block_match_slices(target, digest, &forged, &[]) {
            assert_ne!(forged.as_slice(), genuine);
            return forged;
        }
    }
    panic!(
        "no forged window for block {target} in {} tries",
        BLOCK_LEN - 2
    );
}

/// A genuine block 3 followed by a window that falsely matches block 9 must
/// not become `Copy 3, Copy 9`: that run would replace the window with the
/// wrong bytes. Under the successor rule the window is only compared with
/// block 4, the pair is untrusted, and the output is byte-exact.
#[test]
fn a_false_match_on_a_non_successor_block_is_never_copied() {
    let basis = pattern(BLOCKS * BLOCK_LEN, 0x5ec0_0004);
    let index = index_with(&basis, Some(1));
    assert_eq!(index.strong_length(), 1, "fixture must use the halved sum");
    let forged = forged_match(&basis, &index, 9);
    let source = [block(&basis, 3), forged.as_slice()].concat();

    let script = gated(&source, &index);

    assert!(
        !copies(&script).contains(&9),
        "the forged window was copied from block 9: {:?}",
        copies(&script)
    );
    assert_eq!(reconstruct(&basis, &index, &script), source);
}
