//! Sparse-match adversarial fixture for the delta-matching pipeline.
//!
//! Drives [`DeltaSignatureIndex::from_signature`] +
//! [`DeltaGenerator::generate`] over a synthetic basis/source pair where
//! source and basis have nearly identical length but only a tiny number
//! of basis blocks (`K` in `{0, 1, 2}`) recur in source. The remaining
//! source bytes are deterministically uncorrelated with basis. This
//! exercises the rolling-hash hot path under near-constant rejection,
//! which the design note (`docs/design/zsync-inspired-matching.md`)
//! identifies as the worst case that the planned bithash prefilter
//! (#2059) is intended to accelerate.
//!
//! # Why this matters
//!
//! Today's pre-bithash baseline relies on the 16-bit `tag_table`
//! fast-path (`crates/match/src/index/mod.rs`, mirroring upstream
//! `match.c`'s `tag_table[s1]`). With ~60k indexed blocks the table
//! saturates and almost every rolling-checksum probe survives the gate,
//! which means the full hash-table lookup runs at every offset. A
//! bithash-style probabilistic prefilter (upstream `rsum.c:362-366`)
//! reduces the survival rate. The assertion below pins the *correctness*
//! invariant - matched bytes equal exactly `K * block_size` - so the
//! prefilter work can be benchmarked against this fixture without the
//! risk of a regression in match accuracy.
//!
//! # Construction
//!
//! - Basis bytes are produced by a splitmix64-style finalizer over
//!   `offset` masked to 7 bits, so every byte sits in `[0, 0x7f]` and
//!   the sequence has no short period below 2^32. Adjacent blocks of
//!   any reasonable size are therefore content-distinct, which keeps
//!   the signature index free of accidental duplicates.
//! - Source non-planted bytes use the same finalizer with `offset`
//!   pre-XORed against a constant, masked to 7 bits, then OR'd with
//!   `0x80` to force the high bit. Every non-planted source byte sits
//!   in `[0x80, 0xff]`, disjoint from the basis byte range. No source
//!   window over the non-planted region can therefore match any
//!   indexed basis block, regardless of sliding alignment.
//! - The first `K` blocks of source are copied verbatim from basis, so
//!   the only block-aligned matches available are at offsets
//!   `0, block_size, ..., (K-1) * block_size`. After matching each
//!   planted block the generator jumps forward by `block_size` bytes
//!   (upstream `match.c:265-310`), so non-aligned sliding windows
//!   inside the planted prefix are never probed.
//!
//! # Upstream Reference
//!
//! - `target/interop/upstream-src/rsync-3.4.1/match.c:200-345` -
//!   `hash_search()` two-stage gate (rolling sum then strong sum).
//! - `target/interop/upstream-src/rsync-3.4.1/rsum.c:362-366` -
//!   bithash probe before the hash-table descent (planned in #2059).

use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};
use std::time::{Duration, Instant};

use checksums::strong::Md5Seed;
use matching::{DeltaGenerator, DeltaSignatureIndex, DeltaToken};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

/// Strong-checksum truncation length used across all algorithm matrices.
///
/// Both MD5 (16 bytes) and XXH3-64 (8 bytes) produce digests at least
/// this long, so every algorithm in the matrix supports the layout.
const STRONG_LEN: u8 = 8;

/// Time budget for the smallest fixture. The 64 KB / block_size=1024
/// case must complete well under this on every CI runner; the assertion
/// is a tripwire for accidental quadratic regressions in the matching
/// pipeline.
const SMALL_FIXTURE_BUDGET: Duration = Duration::from_secs(5);

/// Splitmix64 finalizer (Stafford's variant 13). Applied to a seeded
/// offset, this produces deterministic 64-bit pseudo-random output
/// with full 2^64 period and excellent avalanche, eliminating the
/// short-period pitfalls of plain LCG byte generators.
#[inline]
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    x
}

/// Produces a deterministic basis byte stream confined to the low
/// range `[0, 0x80)`. The splitmix64 mixing eliminates short periods
/// so adjacent blocks are content-distinct at every block size in the
/// matrix.
fn basis_byte(offset: usize) -> u8 {
    (splitmix64(offset as u64) & 0x7f) as u8
}

/// Produces a deterministic source byte stream confined to the high
/// range `[0x80, 0xff]`. The seed XOR ensures source and basis sequences
/// are independent, and the explicit `| 0x80` guarantees byte-level
/// disjointness from any basis byte.
fn source_non_planted_byte(offset: usize) -> u8 {
    let mixed = splitmix64((offset as u64) ^ 0xa3a3_a3a3_a3a3_a3a3);
    ((mixed & 0x7f) as u8) | 0x80
}

/// Builds a `(basis, source)` pair where the first `planted_blocks`
/// basis blocks are copied verbatim into source at the same byte
/// offsets and every other source byte comes from
/// [`source_non_planted_byte`]. Source length matches basis length
/// exactly, satisfying the "within one block" constraint trivially.
fn build_sparse_pair(
    basis_len: usize,
    block_size: usize,
    planted_blocks: usize,
) -> (Vec<u8>, Vec<u8>) {
    assert!(basis_len >= planted_blocks * block_size);
    let mut basis = Vec::with_capacity(basis_len);
    for offset in 0..basis_len {
        basis.push(basis_byte(offset));
    }

    let mut source = Vec::with_capacity(basis_len);
    let planted_bytes = planted_blocks * block_size;
    source.extend_from_slice(&basis[..planted_bytes]);
    for offset in planted_bytes..basis_len {
        source.push(source_non_planted_byte(offset));
    }

    debug_assert_eq!(basis.len(), source.len());
    (basis, source)
}

/// Builds a [`DeltaSignatureIndex`] from `basis` using the requested
/// algorithm and a forced block size. Forcing the block size is crucial
/// because the heuristic in `calculate_signature_layout` would otherwise
/// pick a different size for each fixture scale and obscure the test
/// invariants.
fn build_index(
    basis: &[u8],
    block_size: u32,
    algorithm: SignatureAlgorithm,
) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        basis.len() as u64,
        Some(NonZeroU32::new(block_size).expect("block size must be non-zero")),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(STRONG_LEN).expect("strong length must be non-zero"),
    );
    let layout = calculate_signature_layout(params).expect("signature layout");
    let signature = generate_file_signature(Cursor::new(basis.to_vec()), layout, algorithm)
        .expect("file signature");
    DeltaSignatureIndex::from_signature(&signature, algorithm).expect("delta signature index")
}

/// Drives the production pipeline end-to-end and returns a tuple of
/// (matched_bytes, literal_bytes, total_bytes, copy_token_count, elapsed).
fn run_pipeline(
    basis: &[u8],
    source: &[u8],
    block_size: u32,
    algorithm: SignatureAlgorithm,
) -> (u64, u64, u64, usize, Duration) {
    let index = build_index(basis, block_size, algorithm);
    let started = Instant::now();
    let script = DeltaGenerator::new()
        .generate(Cursor::new(source.to_vec()), &index)
        .expect("delta generation");
    let elapsed = started.elapsed();

    let copy_tokens = script
        .tokens()
        .iter()
        .filter(|t| matches!(t, DeltaToken::Copy { .. }))
        .count();

    let matched = script.copy_bytes();
    let literal = script.literal_bytes();
    let total = script.total_bytes();
    (matched, literal, total, copy_tokens, elapsed)
}

/// MD5 with no seed, mirroring upstream rsync's modern-protocol
/// negotiated path (`checksum.c:get_checksum2()` with
/// `CHECKSUM_SEED_FIX`).
fn md5_algo() -> SignatureAlgorithm {
    SignatureAlgorithm::Md5 {
        seed_config: Md5Seed::none(),
    }
}

/// XXH3-64 with seed 0, the strong checksum negotiated when both peers
/// advertise the modern checksum capability string.
fn xxh3_algo() -> SignatureAlgorithm {
    SignatureAlgorithm::Xxh3 { seed: 0 }
}

/// Asserts the canonical invariants for a sparse-match pipeline run.
/// Centralised so every scale + algorithm combination exercises the
/// same checks.
///
/// upstream: rsum.c:362-366 - bithash will reduce probes; today's
/// pre-bithash baseline assertion is that matched_bytes equals exactly
/// `K * block_size` and literal_bytes equals `source.len() - K *
/// block_size`. The number of strong-checksum verify calls is not
/// directly observable through the public API, so it is intentionally
/// not asserted here.
fn assert_sparse_match_invariants(
    source_len: u64,
    block_size: u32,
    planted: usize,
    matched: u64,
    literal: u64,
    total: u64,
    copy_tokens: usize,
) {
    let expected_matched = planted as u64 * u64::from(block_size);
    assert_eq!(
        matched, expected_matched,
        "matched bytes must equal exactly K * block_size"
    );
    assert_eq!(
        literal,
        source_len - expected_matched,
        "literal bytes must equal source.len() - K * block_size"
    );
    assert_eq!(
        total, source_len,
        "total bytes emitted must equal source.len()"
    );
    assert_eq!(
        copy_tokens, planted,
        "exactly `planted` copy tokens must be emitted"
    );
}

/// Runs every `(planted_k, algorithm)` combination at the given basis
/// length and block size. Used by every scale-specific test below to
/// keep the matrix declaration concise.
fn exercise_matrix(basis_len: usize, block_size: u32, enforce_budget: bool) {
    let block_size_usize = block_size as usize;
    for &planted in &[0usize, 1, 2] {
        for &algo in &[md5_algo(), xxh3_algo()] {
            let (basis, source) = build_sparse_pair(basis_len, block_size_usize, planted);
            let (matched, literal, total, copy_tokens, elapsed) =
                run_pipeline(&basis, &source, block_size, algo);
            assert_sparse_match_invariants(
                source.len() as u64,
                block_size,
                planted,
                matched,
                literal,
                total,
                copy_tokens,
            );
            if enforce_budget {
                assert!(
                    elapsed < SMALL_FIXTURE_BUDGET,
                    "smallest fixture must finish under {:?}, took {:?} for K={} algo={:?}",
                    SMALL_FIXTURE_BUDGET,
                    elapsed,
                    planted,
                    algo,
                );
            }
        }
    }
}

/// Smallest fixture: 64 KB basis + source, block_size = 1024, all K and
/// both algorithms. Enforces the worst-case timing sanity check.
#[test]
fn sparse_match_64kb_block1024() {
    exercise_matrix(64 * 1024, 1024, true);
}

/// 64 KB basis + source, block_size = 4096. Block-size sweep at the
/// smallest scale.
#[test]
fn sparse_match_64kb_block4096() {
    exercise_matrix(64 * 1024, 4096, true);
}

/// 1 MB basis + source, block_size = 1024. Mid scale, every K and both
/// algorithms. The timing budget is not enforced beyond the smallest
/// fixture because the ratio between scales depends on the runner.
#[test]
fn sparse_match_1mb_block1024() {
    exercise_matrix(1024 * 1024, 1024, false);
}

/// 1 MB basis + source, block_size = 4096.
#[test]
fn sparse_match_1mb_block4096() {
    exercise_matrix(1024 * 1024, 4096, false);
}

/// 16 MB stress case, block_size = 1024. Far below the design note's
/// 100 MB target but still big enough to make the rolling-hash hot
/// path dominate. Marked `#[ignore]` so the default `cargo nextest`
/// matrix does not pick it up; CI configurations that opt into stress
/// testing run it explicitly via `cargo nextest run --run-ignored
/// only`.
#[test]
#[ignore]
fn sparse_match_16mb_block1024() {
    exercise_matrix(16 * 1024 * 1024, 1024, false);
}

/// 16 MB stress case, block_size = 4096. Same gating rationale as
/// [`sparse_match_16mb_block1024`].
#[test]
#[ignore]
fn sparse_match_16mb_block4096() {
    exercise_matrix(16 * 1024 * 1024, 4096, false);
}

/// Sanity-check: the byte-domain construction guarantees that planted
/// regions and non-planted regions are byte-disjoint. If this property
/// ever breaks the higher-level assertions can produce false negatives,
/// so it is asserted directly on the smallest fixture.
#[test]
fn byte_ranges_are_disjoint_between_basis_and_source_non_planted() {
    let (basis, source) = build_sparse_pair(64 * 1024, 1024, 0);
    for &b in &basis {
        assert!(b < 0x80, "basis bytes must stay below 0x80 (got {b:#04x})");
    }
    for &s in &source {
        assert!(
            s >= 0x80,
            "source non-planted bytes must stay at or above 0x80 (got {s:#04x})"
        );
    }
}

/// Guard rail for the construction itself: with `K = 0` the source
/// must contain no byte that can match any basis byte at any offset,
/// because the byte ranges are disjoint. This is the property that
/// enforces "matched bytes == 0" rigorously, independent of any
/// rolling-hash collision argument.
#[test]
fn k_zero_construction_has_no_byte_in_common_with_basis() {
    let (basis, source) = build_sparse_pair(64 * 1024, 1024, 0);
    let basis_set: std::collections::BTreeSet<u8> = basis.iter().copied().collect();
    for (offset, &s) in source.iter().enumerate() {
        assert!(
            !basis_set.contains(&s),
            "source byte at offset {offset} (value {s:#04x}) collides with basis byte set"
        );
    }
}

/// Negative control: when the planted block count equals zero, the
/// pipeline must emit a single literal stream covering every source
/// byte and zero copy tokens. Asserted at the smallest scale only;
/// larger scales are exercised through [`exercise_matrix`].
#[test]
fn k_zero_smallest_fixture_emits_only_literals() {
    let (basis, source) = build_sparse_pair(64 * 1024, 1024, 0);
    let (matched, literal, total, copy_tokens, _) = run_pipeline(&basis, &source, 1024, md5_algo());
    assert_eq!(matched, 0, "K=0 must produce zero matched bytes");
    assert_eq!(
        literal,
        source.len() as u64,
        "K=0 must emit every source byte as a literal"
    );
    assert_eq!(total, source.len() as u64);
    assert_eq!(copy_tokens, 0, "K=0 must emit zero copy tokens");
}

/// Positive control: when the planted block count equals two the
/// pipeline must emit exactly two copy tokens at indices 0 and 1, and
/// the literal payload must equal the unmatched suffix.
#[test]
fn k_two_smallest_fixture_emits_two_copy_tokens_then_literal() {
    let block_size: usize = 1024;
    let (basis, source) = build_sparse_pair(64 * 1024, block_size, 2);
    let index = build_index(&basis, block_size as u32, md5_algo());
    let script = DeltaGenerator::new()
        .generate(Cursor::new(source.clone()), &index)
        .expect("delta generation");

    let copy_indices: Vec<u64> = script
        .tokens()
        .iter()
        .filter_map(|t| match t {
            DeltaToken::Copy { index, .. } => Some(*index),
            DeltaToken::Literal(_) => None,
        })
        .collect();
    assert_eq!(
        copy_indices,
        vec![0, 1],
        "copy tokens must reference basis blocks 0 and 1 in order"
    );

    let literal_bytes_emitted: usize = script
        .tokens()
        .iter()
        .filter_map(|t| match t {
            DeltaToken::Literal(payload) => Some(payload.len()),
            DeltaToken::Copy { .. } => None,
        })
        .sum();
    assert_eq!(
        literal_bytes_emitted,
        source.len() - 2 * block_size,
        "literal payload must cover every byte outside the two planted blocks"
    );
}
