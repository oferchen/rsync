//! The sender scan must match the basis's short final block.
//!
//! Upstream reaches that block through three cooperating pieces of
//! `hash_search()`: the scan bound comes from the LAST block's length
//! (`end = len + 1 - s->sums[s->count-1].len`, `match.c:174`), the rolling
//! window shrinks once there is nothing left to feed it (`--k` when `!more`,
//! `match.c:321-331`), and a candidate is accepted only when
//! `l = MIN(blength, len-offset)` equals `s->sums[i].len`
//! (`match.c:222-224`). Together those admit exactly one short match, at
//! offset `len - tail_len`.
//!
//! Why this matters beyond byte counts: the short block must be emitted as its
//! OWN `Copy` carrying its TRUE length. The seq-match coalescer models a run as
//! `run_len * block_len` (`generator.rs::flush_seq_match_run`), so a run that
//! absorbed a 400-byte block would claim 700 bytes for it, over-run the source
//! cursor the wire layer uses to feed `see_deflate_token`, and desync the
//! receiver's compressed-token dictionary. A test that only checks totals would
//! pass through that bug, so the assertions below pin token identity, not sums.

use matching::{DeltaGenerator, DeltaScript, DeltaSignatureIndex, DeltaToken, apply_delta};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};
use std::io::Cursor;
use std::num::NonZeroU8;

/// Full basis block length that `calculate_block_length` picks for the sizes
/// used here.
const BLOCK_LEN: usize = 700;
/// Length of the deliberately short final block.
const TAIL_LEN: usize = 400;
/// 292 full blocks plus a 400-byte tail: the layout that first exposed the gap.
const FULL_BLOCKS: usize = 292;
const BASIS_LEN: usize = FULL_BLOCKS * BLOCK_LEN + TAIL_LEN;

/// Deterministic, non-repeating filler.
///
/// A short-cycle pattern would be actively misleading here: with a period that
/// divides evenly into the scan, an unaligned window near EOF can match some
/// earlier full block by content and swallow the trailing bytes, so the tail
/// probe never sees them. This 64-bit LCG has a period far beyond any fixture
/// size, which keeps "the tail was not matched" a statement about the matcher
/// rather than about the data.
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

fn scan(source: &[u8], index: &DeltaSignatureIndex) -> DeltaScript {
    DeltaGenerator::new()
        .generate(Cursor::new(source.to_vec()), index)
        .expect("scan")
}

fn matched_bytes(script: &DeltaScript) -> u64 {
    script
        .tokens()
        .iter()
        .filter(|t| !t.is_literal())
        .map(|t| t.byte_len() as u64)
        .sum()
}

/// Walks the token stream and returns `(source_offset, basis_index, len)` for
/// every `Copy` whose length is shorter than a full block.
fn short_copies(script: &DeltaScript, block_len: usize) -> Vec<(u64, u64, usize)> {
    let mut out = Vec::new();
    let mut offset = 0u64;
    for token in script.tokens() {
        if let DeltaToken::Copy { index, len } = token {
            if *len < block_len {
                out.push((offset, *index, *len));
            }
        }
        offset += token.byte_len() as u64;
    }
    out
}

fn reconstruct(script: &DeltaScript, basis: &[u8], index: &DeltaSignatureIndex) -> Vec<u8> {
    let mut out = Vec::new();
    apply_delta(&mut Cursor::new(basis.to_vec()), &mut out, index, script).expect("apply");
    out
}

/// The layout premise every other test here rests on: the signature really does
/// end in a 400-byte block, so a mismatch would otherwise silently make the
/// tail assertions vacuous.
#[test]
fn basis_signature_ends_in_a_short_block() {
    let index = build_index(&pattern(BASIS_LEN));
    assert_eq!(index.block_length(), BLOCK_LEN);
    assert_eq!(index.block_count(), FULL_BLOCKS + 1);
    assert_eq!(index.block(FULL_BLOCKS).len(), TAIL_LEN);
    assert_eq!(index.block(FULL_BLOCKS - 1).len(), BLOCK_LEN);
}

/// The regression itself: a source that differs in the interior but keeps the
/// trailing 400 bytes must reuse the basis's final short block rather than
/// re-sending it as literal data.
#[test]
fn short_final_block_is_matched_not_resent_as_literal() {
    let basis = pattern(BASIS_LEN);
    let index = build_index(&basis);

    // Flip one byte in every odd full block; leave the 400-byte tail intact.
    let mut source = basis.clone();
    for block in (1..FULL_BLOCKS).step_by(2) {
        source[block * BLOCK_LEN] ^= 0xFF;
    }

    let script = scan(&source, &index);
    let tail = short_copies(&script, BLOCK_LEN);
    assert_eq!(
        tail,
        vec![((BASIS_LEN - TAIL_LEN) as u64, FULL_BLOCKS as u64, TAIL_LEN)],
        "expected exactly one short Copy: the final basis block, at the file's tail"
    );

    // The 400 tail bytes moved from the literal side of the ledger to the
    // matched side - the measured symptom against upstream rsync 3.4.4.
    let full_matches = matched_bytes(&script) - TAIL_LEN as u64;
    assert_eq!(full_matches % BLOCK_LEN as u64, 0);
    assert_eq!(
        script.literal_bytes(),
        BASIS_LEN as u64 - matched_bytes(&script)
    );
    assert_eq!(reconstruct(&script, &basis, &index), source);
}

/// The coalescing hazard, pinned directly.
///
/// With the source identical to the basis, every full block matches in one
/// unbroken run, so the seq-match coalescer produces a single fat Copy. The
/// short final block must stay OUTSIDE that run and carry `TAIL_LEN`, not
/// `BLOCK_LEN`. Checking only `matched_bytes` would not catch a coalesced tail,
/// because a fat Copy of 293 blocks reports the same total as 292 blocks plus a
/// 700-byte tail - and both are wrong by the same 300 bytes on the wire.
#[test]
fn short_final_block_copy_is_standalone_and_carries_its_true_length() {
    let basis = pattern(BASIS_LEN);
    let index = build_index(&basis);

    let script = scan(&basis, &index);
    assert_eq!(
        script.tokens(),
        &[
            DeltaToken::Copy {
                index: 0,
                len: FULL_BLOCKS * BLOCK_LEN,
            },
            DeltaToken::Copy {
                index: FULL_BLOCKS as u64,
                len: TAIL_LEN,
            },
        ],
        "the short final block must be its own Copy, never folded into the run"
    );
    assert_eq!(script.literal_bytes(), 0);
    assert_eq!(script.total_bytes(), BASIS_LEN as u64);
    assert_eq!(reconstruct(&script, &basis, &index), basis);
}

/// A file smaller than one block is a single short block, and it must match.
///
/// This also settles a claim the removed in-code justification made: index
/// construction does not fail for a single-partial-block basis.
#[test]
fn whole_file_shorter_than_one_block_matches_as_a_single_copy() {
    let basis = pattern(TAIL_LEN);
    let index = build_index(&basis);
    assert_eq!(index.block_count(), 1);
    assert_eq!(index.block(0).len(), TAIL_LEN);

    let script = scan(&basis, &index);
    assert_eq!(
        script.tokens(),
        &[DeltaToken::Copy {
            index: 0,
            len: TAIL_LEN,
        }]
    );
    assert_eq!(script.literal_bytes(), 0);
    assert_eq!(reconstruct(&script, &basis, &index), basis);
}

/// Upstream's window can only shrink to `tail_len` while it still has that many
/// bytes left, so a source that ends fewer than `tail_len` bytes past its last
/// match has no reachable tail position. Truncating the source mid-tail must
/// therefore leave those bytes literal, not match a shorter prefix of the final
/// block.
#[test]
fn a_tail_shorter_than_the_final_block_stays_literal() {
    let basis = pattern(BASIS_LEN);
    let index = build_index(&basis);

    let source = &basis[..BASIS_LEN - 100];
    let script = scan(source, &index);
    assert!(
        short_copies(&script, BLOCK_LEN).is_empty(),
        "a 300-byte residue must not match the 400-byte final block"
    );
    assert_eq!(reconstruct(&script, &basis, &index), source);
}

/// The parallel striped scan must not place the short final block mid-file.
///
/// A stripe's EOF is not the file's EOF, and upstream's `l = MIN(blength,
/// len-offset)` test never admits a short block anywhere but the true tail. The
/// content of the final block is planted at a stripe-interior offset here, so an
/// unguarded per-stripe probe would match it in the wrong place.
#[test]
fn striped_scan_places_the_short_block_only_at_the_true_tail() {
    let basis = pattern(BASIS_LEN);
    let index = build_index(&basis);

    // 6 MiB of source: comfortably above the 1 MiB-per-stripe floor, so
    // `generate_chunked` really splits.
    let source_len = 6 * 1024 * 1024;
    let mut source: Vec<u8> = (0..source_len)
        .map(|i| ((i * 31 + 7) % 253) as u8)
        .collect();
    let tail_block = &basis[BASIS_LEN - TAIL_LEN..];
    // Plant the final block's bytes deep inside the source, and again at the end.
    let planted = 2 * 1024 * 1024;
    source[planted..planted + TAIL_LEN].copy_from_slice(tail_block);
    source[source_len - TAIL_LEN..].copy_from_slice(tail_block);

    let script = DeltaGenerator::new()
        .generate_chunked(&source, &index, 8)
        .expect("chunked scan");

    let shorts = short_copies(&script, BLOCK_LEN);
    for &(offset, _, _) in &shorts {
        assert_eq!(
            offset,
            (source_len - TAIL_LEN) as u64,
            "a short Copy may only appear at the source's tail, never at a stripe boundary"
        );
    }
    assert_eq!(reconstruct(&script, &basis, &index), source);
}

/// The consecutive-match extension deliberately does NOT take the tail.
///
/// That path halves the per-block strong sum and only trusts a match a
/// neighbouring block corroborates; the final short block has no successor to
/// corroborate it, so admitting it would mean a second, weaker acceptance rule
/// on the one path whose checksums are half-length. The tail stays literal
/// there - byte-exact, only larger - and this pins that choice so it cannot be
/// changed silently. The path is opt-in on both peers
/// (`OC_CONSECUTIVE_MATCH`), so the default wire is unaffected.
#[test]
fn consecutive_match_gate_leaves_the_short_final_block_literal() {
    let basis = pattern(BASIS_LEN);
    let index = build_index(&basis);

    let script = DeltaGenerator::new()
        .with_consecutive_match_needed(2)
        .generate(Cursor::new(basis.clone()), &index)
        .expect("gated scan");

    assert!(
        short_copies(&script, BLOCK_LEN).is_empty(),
        "the gated path must not emit a short Copy"
    );
    assert_eq!(script.literal_bytes(), TAIL_LEN as u64);
    assert_eq!(reconstruct(&script, &basis, &index), basis);
}
