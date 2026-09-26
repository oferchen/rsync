//! A basis block matches every source window that carries its content, as
//! often as that content repeats.
//!
//! upstream: match.c:229-345 `hash_search()` never retires a chain entry
//! outside `--inplace`, so one basis copy of a block answers any number of
//! source copies with a `Copy` token each. Sending the repeats as literals
//! instead changes the token stream, the `--stats` Literal/Matched split and
//! the bytes on the wire, so these tests pin the exact per-block index
//! sequence rather than only reconstruction.

use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

use matching::{DeltaGenerator, DeltaScript, DeltaSignatureIndex, DeltaToken, apply_delta};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

const BLOCK_LEN: usize = 1024;

fn lcg_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state: u64 = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u8
        })
        .collect()
}

fn build_index(basis: &[u8]) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        basis.len() as u64,
        Some(NonZeroU32::new(BLOCK_LEN as u32).expect("non-zero block length")),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).expect("non-zero strong length"),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature =
        generate_file_signature(basis, layout, SignatureAlgorithm::Md4).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
}

/// One entry per wire token: `Some(block)` for a block match, `None` for a
/// literal run. A coalesced seq-match `Copy` expands to one match per block,
/// exactly as the wire encoder sends it.
fn wire_tokens(script: &DeltaScript) -> Vec<Option<u64>> {
    let mut out = Vec::new();
    for token in script.tokens() {
        match token {
            DeltaToken::Literal(_) => out.push(None),
            DeltaToken::Copy { index, len } => {
                let blocks = len.div_ceil(BLOCK_LEN) as u64;
                out.extend((0..blocks).map(|k| Some(index + k)));
            }
        }
    }
    out
}

fn assert_reconstructs(
    basis: &[u8],
    index: &DeltaSignatureIndex,
    script: &DeltaScript,
    source: &[u8],
) {
    let mut out = Vec::new();
    apply_delta(Cursor::new(basis), &mut out, index, script).expect("apply");
    assert_eq!(out, source, "the delta must reconstruct the source");
}

/// Basis holds block C once; the source holds it five times, in two runs
/// separated by literal bytes. Every copy of C must go out as a `Copy` of
/// basis block 2, and the matched-data total must be `5 * BLOCK_LEN`.
#[test]
fn one_basis_copy_answers_every_source_repeat() {
    let blocks: Vec<Vec<u8>> = (0..4u64)
        .map(|k| lcg_bytes(0xC0FF_EE00 + k, BLOCK_LEN))
        .collect();
    let basis = blocks.concat();
    let c = &blocks[2];

    let mut source = lcg_bytes(0x0BAD_5EED, 300);
    source.extend_from_slice(c);
    source.extend_from_slice(c);
    source.extend_from_slice(&lcg_bytes(0x0BAD_5EEE, 17));
    source.extend_from_slice(c);
    source.extend_from_slice(c);
    source.extend_from_slice(c);

    let index = build_index(&basis);
    assert!(
        !index.has_duplicate_blocks(),
        "the basis holds C exactly once"
    );
    let (script, counters) = DeltaGenerator::new()
        .generate_counted(Cursor::new(&source), &index)
        .expect("generate");

    assert_eq!(
        wire_tokens(&script),
        vec![None, Some(2), Some(2), None, Some(2), Some(2), Some(2)],
        "each repeat of C must be a Copy of basis block 2"
    );
    assert_eq!(script.copy_bytes(), 5 * BLOCK_LEN as u64);
    assert_eq!(script.literal_bytes(), 317);
    assert_eq!(counters.matches, 5);
    assert_reconstructs(&basis, &index, &script, &source);
}

/// A sparse, VM-image-like basis: zero blocks at indices 1, 2 and 4 among
/// random ones. The source carries more zero blocks than the basis.
///
/// The expected sequence follows upstream's rules: the `want_i` hint (the
/// block after the previous match) wins when it matches (match.c:321-334),
/// otherwise the chain walk returns the HIGHEST matching index first, because
/// `build_hash_table()` head-inserts (match.c:98-110). So zero windows off the
/// hint all resolve to block 4, and none falls back to a literal.
#[test]
fn zero_blocks_match_repeatedly_in_upstream_chain_order() {
    let zero = vec![0u8; BLOCK_LEN];
    let r: Vec<Vec<u8>> = (0..3u64)
        .map(|k| lcg_bytes(0x5A55_0000 + k, BLOCK_LEN))
        .collect();
    let basis = [&r[0][..], &zero, &zero, &r[1], &zero, &r[2]].concat();

    let mut source = r[0].clone();
    for _ in 0..6 {
        source.extend_from_slice(&zero);
    }
    source.extend_from_slice(&r[1]);
    source.extend_from_slice(&zero);
    source.extend_from_slice(&zero);

    let index = build_index(&basis);
    let (script, counters) = DeltaGenerator::new()
        .generate_counted(Cursor::new(&source), &index)
        .expect("generate");

    assert_eq!(
        wire_tokens(&script),
        [0, 1, 2, 4, 4, 4, 4, 3, 4, 4].map(Some).to_vec(),
        "zero blocks must match repeatedly, highest chain entry first"
    );
    assert_eq!(script.literal_bytes(), 0);
    assert_eq!(script.copy_bytes(), source.len() as u64);
    assert_eq!(counters.matches, 10);
    assert_reconstructs(&basis, &index, &script, &source);
}

/// The parallel scan must emit the sequential scan's exact tokens on a source
/// that repeats a basis block, at every stripe count.
#[test]
fn parallel_scan_matches_sequential_on_repeated_blocks() {
    let blocks: Vec<Vec<u8>> = (0..64u64)
        .map(|k| lcg_bytes(0x9A2A_1100 + k, BLOCK_LEN))
        .collect();
    let basis = blocks.concat();
    let mut source = Vec::new();
    for round in 0..96usize {
        source.extend_from_slice(&blocks[(round * 7) % 64]);
        source.extend_from_slice(&blocks[5]);
        if round % 3 == 0 {
            source.extend_from_slice(&lcg_bytes(round as u64, 211));
        }
    }

    let index = build_index(&basis);
    assert!(!index.has_duplicate_blocks());
    let generator = DeltaGenerator::new();
    let sequential = generator
        .generate(Cursor::new(&source), &index)
        .expect("sequential");
    let block5_copies = wire_tokens(&sequential)
        .iter()
        .filter(|t| **t == Some(5))
        .count();
    assert!(
        block5_copies >= 96,
        "block 5 repeats at least 96 times and must match each time (got {block5_copies})"
    );
    assert_reconstructs(&basis, &index, &sequential, &source);

    for stripes in [1usize, 2, 3, 4, 8] {
        assert_eq!(
            generator.forced_stripe_count(source.len(), &index, stripes),
            stripes,
            "the fixture must genuinely split into {stripes} stripes"
        );
        let (striped, _) = generator
            .generate_chunked_forced(&source, &index, stripes)
            .expect("striped");
        assert_eq!(
            wire_tokens(&striped),
            wire_tokens(&sequential),
            "{stripes} stripes must reproduce the sequential tokens"
        );
    }
}
