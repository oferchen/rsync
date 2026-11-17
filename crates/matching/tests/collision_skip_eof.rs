//! A weak-checksum collision skip that lands on the short final block's
//! offset must leave that block unprobed, as upstream's scan does.
//!
//! upstream: match.c:366-381 `hash_search()` - after the 4th consecutive offset
//! whose weak sum hits but whose strong sum fails, the scan jumps
//! `MIN(k-1, end-1-offset)` bytes and resets the counter to 3. The window at the
//! landing offset is never tested: the code falls through to `null_hash`, rolls
//! one byte and `++offset` (match.c:412). The scan bound is
//! `end = len + 1 - s->sums[s->count-1].len` (match.c:197), so the short final
//! block can only match at `end - 1`. When a skip lands exactly there, upstream
//! sends the tail as literal data.
//!
//! The fixture makes every full-length source window collide: the source is a
//! run of one byte value, and the basis's only full block is that run with a
//! `+1, -2, +1` perturbation, which keeps both halves of the rolling sum and
//! changes the content. After the first four collisions the scan therefore
//! skips `blength - 1` bytes per step, so which source lengths put a landing on
//! the tail offset follows from arithmetic alone.

use matching::{DeltaGenerator, DeltaSignatureIndex, apply_delta};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};
use std::io::Cursor;
use std::num::NonZeroU8;

const BLOCK_LEN: usize = 700;
const TAIL_LEN: usize = 100;
const FILL: u8 = 0x40;

/// One colliding full block followed by a `TAIL_LEN`-byte tail of `FILL`.
fn basis() -> Vec<u8> {
    let mut block = vec![FILL; BLOCK_LEN];
    block[300] += 1;
    block[301] -= 2;
    block[302] += 1;
    block.extend(std::iter::repeat_n(FILL, TAIL_LEN));
    block
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

/// Bytes upstream matches for a source of `len` bytes of `FILL`.
///
/// Full windows are tested at offsets 0-3 (the 4th collision triggers the
/// first skip) and then every `BLOCK_LEN` bytes. The last of them, `o`, skips
/// to `MIN(o + BLOCK_LEN - 1, tail)`; the tail is probed only when it lies
/// beyond that landing.
fn upstream_matched(len: usize) -> u64 {
    let last_full = len - BLOCK_LEN;
    let o = 3 + (last_full - 3) / BLOCK_LEN * BLOCK_LEN;
    let tail = len - TAIL_LEN;
    if tail > o + BLOCK_LEN - 1 {
        TAIL_LEN as u64
    } else {
        0
    }
}

/// The premise: the perturbed block collides on the weak sum but not the
/// strong one, so every full source window is a false alarm.
#[test]
fn the_fixture_block_collides_with_the_fill_run() {
    let basis = basis();
    let index = build_index(&basis);
    assert_eq!(index.block_length(), BLOCK_LEN);
    let run = [FILL; BLOCK_LEN];
    let mut rolling = checksums::RollingChecksum::new();
    rolling.update(&run);
    let mut colliding = checksums::RollingChecksum::new();
    colliding.update(&basis[..BLOCK_LEN]);
    assert_eq!(rolling.digest(), colliding.digest());
    assert_ne!(&basis[..BLOCK_LEN], &run[..]);
}

/// Sweeps the source length over several skip periods so every landing
/// position relative to the tail, including the exact hit, is covered.
#[test]
fn a_skip_landing_on_the_tail_offset_leaves_the_tail_literal() {
    let basis = basis();
    let index = build_index(&basis);
    let mut landed_on_tail = 0;
    for len in 4 * BLOCK_LEN..6 * BLOCK_LEN {
        let source = vec![FILL; len];
        let script = DeltaGenerator::new()
            .generate(Cursor::new(source.clone()), &index)
            .expect("scan");
        let matched: u64 = script
            .tokens()
            .iter()
            .filter(|token| !token.is_literal())
            .map(|token| token.byte_len() as u64)
            .sum();
        assert_eq!(
            matched,
            upstream_matched(len),
            "source of {len} bytes: matched bytes differ from upstream's scan"
        );
        let mut out = Vec::new();
        apply_delta(&mut Cursor::new(basis.clone()), &mut out, &index, &script).expect("apply");
        assert_eq!(out, source, "source of {len} bytes must reconstruct");
        if (len - TAIL_LEN - 3) % BLOCK_LEN == BLOCK_LEN - 1 {
            landed_on_tail += 1;
        }
    }
    assert!(landed_on_tail >= 2, "the sweep must include exact landings");
}
