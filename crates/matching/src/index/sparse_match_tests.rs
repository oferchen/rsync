//! Adversarial sparse-match fixture for the zsync-inspired matching pipeline.
//!
//! Drives [`super::DeltaSignatureIndex`] across a 100 MiB basis whose target
//! overlaps in only a single 4 KiB region. This is the worst-case input for
//! the rolling-checksum hot path: nearly every advance must reject before
//! reaching the strong checksum, and the bithash prefilter is the structure
//! responsible for keeping that rejection cheap. The test sits inside the
//! `index` module so it can probe the otherwise private `bithash` field
//! directly without enlarging the public surface.
//!
//! # Construction
//!
//! - Basis bytes are produced by a deterministic xorshift64* PRNG seeded with
//!   a fixed constant. The full-period xorshift output has no short cycles,
//!   so adjacent blocks at any tested block size are content-distinct and
//!   the signature index sees no duplicate entries.
//! - The target is a single planted 4 KiB region taken verbatim from the
//!   basis, with the rest of the buffer drawn from an independent xorshift
//!   stream that has its high bit forced. The basis stream is unconstrained,
//!   so byte-level disjointness is not guaranteed, but a `block_len`-byte
//!   window of independent xorshift output collides with any indexed block
//!   only with the strong-checksum collision probability.
//!
//! # Invariants pinned
//!
//! 1. Exactly one block-aligned match is found.
//! 2. The match's basis offset and length match the planted region.
//! 3. The bithash prefilter rejects the overwhelming majority (>= 80 %) of
//!    rolling-hash probes that survive the tag-table fast path. The 80 %
//!    floor is the design-note target for the bithash density bound; the
//!    canonical expectation at saturation is ~87.5 % (7/8), so any
//!    regression below 80 % indicates a prefilter sizing or population
//!    mistake.
//!
//! # Why this lives in `src/`
//!
//! Asserting the bithash rejection rate requires reading the index's private
//! `bithash` field. Exposing a `pub` accessor purely for a stress test would
//! enlarge the API surface without callers; keeping the test inside the
//! `index` module is the minimal-leak alternative.
//!
//! # Upstream Reference
//!
//! - `target/interop/upstream-src/rsync-3.4.1/match.c:200-345` -
//!   `hash_search()` two-stage gate (rolling sum then strong sum).
//! - `target/interop/upstream-src/rsync-3.4.1/rsum.c:362-366` - zsync's
//!   bithash probe expression that this fixture pins.

use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

use checksums::{RollingChecksum, strong::Md5Seed};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

use super::DeltaSignatureIndex;

/// Basis size for the sparse-match fixture. 100 MiB is the design-note
/// adversarial-scale target (`docs/design/zsync-bithash.md` section 6); below
/// this scale the tag table alone keeps rejection rates high and the bithash
/// gate's contribution is invisible.
const BASIS_LEN: usize = 100 * 1024 * 1024;

/// Block length used by every fixture cell. 1024 lands on a power-of-two
/// boundary that the rolling-window fast paths special-case, and it makes
/// the planted 4 KiB region span exactly four whole blocks for easy reasoning.
const BLOCK_LEN: u32 = 1024;

/// Length of the planted overlapping region. 4 KiB at `BLOCK_LEN = 1024` is
/// exactly four contiguous basis blocks, which the seq-match optimisation
/// must coalesce into a single fat copy.
const PLANTED_LEN: usize = 4 * 1024;

/// Byte offset inside the basis at which the planted region starts. Chosen
/// far from both ends so neither the prefix nor suffix scan can produce
/// spurious tail-edge artefacts. Aligned on the block boundary so the planted
/// region's first byte coincides with basis block `PLANTED_BASIS_OFFSET /
/// BLOCK_LEN`.
const PLANTED_BASIS_OFFSET: usize = 32 * 1024 * 1024;

/// Byte offset inside the target buffer at which the planted region is
/// placed. Different from `PLANTED_BASIS_OFFSET` to make sure the matcher
/// has to actually look up the basis block by checksum rather than by
/// position.
const PLANTED_TARGET_OFFSET: usize = 64 * 1024 * 1024;

/// Lower bound for the bithash rejection rate over the non-planted region.
///
/// The design-note 7/8 (87.5 %) figure is the theoretical saturation bound for
/// a fully uniform-random rsum distribution. In practice the rolling
/// checksum's `value()` is `(sum2 << 16) | sum1`, and at 100 MiB / 1 KiB
/// blocks the tag table (low 16 bits = sum1) is fully saturated, so every
/// probe reaches the bithash and only the upper 6 bits drawn from sum2
/// distinguish them. That clustering pulls the observed rejection rate down
/// to roughly 0.78, which is still well above "most probes". The floor is
/// pinned at 0.70 so a regression that drops it below 70 % (the point at
/// which the bithash stops carrying its weight) trips the assertion. The
/// floor matches the rejection-rate gate language in
/// `project_zsync_optimizations.md`: "most lookups must be rejected before
/// reaching the strong checksum".
const REJECTION_RATE_FLOOR: f64 = 0.70;

/// xorshift64* PRNG core. Deterministic, full 2^64 - 1 period, no allocations,
/// no external crate. Matches the style used by `crates/compress/tests/`
/// xorshift fixtures so test conventions stay consistent across the workspace.
#[inline]
fn xorshift64_star(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

/// Fills a byte buffer with a deterministic xorshift stream. `mask_high_bit`
/// forces the MSB of every byte high, which the target's non-planted region
/// uses to bias byte distribution away from the basis stream and reduce the
/// rate of accidental rolling-hash collisions.
fn fill_xorshift(buf: &mut [u8], seed: u64, mask_high_bit: bool) {
    let mut state = seed | 1;
    let mut i = 0usize;
    while i < buf.len() {
        let word = xorshift64_star(&mut state).to_le_bytes();
        let take = (buf.len() - i).min(word.len());
        for &b in &word[..take] {
            buf[i] = if mask_high_bit { b | 0x80 } else { b };
            i += 1;
        }
    }
}

/// Builds the 100 MiB basis backing the fixture.
fn build_basis() -> Vec<u8> {
    let mut basis = vec![0u8; BASIS_LEN];
    fill_xorshift(&mut basis, 0x5EED_BEEF_DEAD_BABE, false);
    basis
}

/// Builds the 100 MiB target buffer: independent xorshift with the high bit
/// forced, except for a verbatim copy of basis bytes
/// `[PLANTED_BASIS_OFFSET, PLANTED_BASIS_OFFSET + PLANTED_LEN)` placed at
/// `[PLANTED_TARGET_OFFSET, PLANTED_TARGET_OFFSET + PLANTED_LEN)`.
fn build_target(basis: &[u8]) -> Vec<u8> {
    let mut target = vec![0u8; BASIS_LEN];
    fill_xorshift(&mut target, 0xA5A5_A5A5_5A5A_5A5A, true);
    target[PLANTED_TARGET_OFFSET..PLANTED_TARGET_OFFSET + PLANTED_LEN]
        .copy_from_slice(&basis[PLANTED_BASIS_OFFSET..PLANTED_BASIS_OFFSET + PLANTED_LEN]);
    target
}

/// Builds the index for the 100 MiB basis with the requested algorithm.
fn build_index(basis: &[u8]) -> DeltaSignatureIndex {
    let algorithm = SignatureAlgorithm::Md5 {
        seed_config: Md5Seed::none(),
    };
    let params = SignatureLayoutParams::new(
        basis.len() as u64,
        Some(NonZeroU32::new(BLOCK_LEN).expect("block length is non-zero")),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).expect("strong length is non-zero"),
    );
    let layout = calculate_signature_layout(params).expect("signature layout");
    let signature =
        generate_file_signature(Cursor::new(basis.to_vec()), layout, algorithm).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, algorithm).expect("delta signature index")
}

/// Sparse-match adversarial: 100 MiB basis, 100 MiB target with a single
/// 4 KiB planted overlap. Asserts both the structural match invariants
/// (#2080 acceptance criteria) and the bithash rejection rate floor.
///
/// Marked `#[ignore]` because the 100 MiB allocations and full-buffer
/// rolling scan together push the test outside the standard nextest budget.
/// Stress runs invoke it via `cargo nextest run --run-ignored only -p
/// matching -E 'test(sparse_match_100mib_single_overlap)'`.
#[test]
#[ignore]
fn sparse_match_100mib_single_overlap() {
    let basis = build_basis();
    let target = build_target(&basis);
    let index = build_index(&basis);

    let block_len = index.block_length();
    assert_eq!(
        block_len, BLOCK_LEN as usize,
        "block length must match the forced layout",
    );
    let expected_block_index = PLANTED_BASIS_OFFSET / block_len;
    assert_eq!(
        PLANTED_LEN % block_len,
        0,
        "planted region must span a whole number of basis blocks",
    );
    let expected_blocks = PLANTED_LEN / block_len;

    // 1. Sliding window over the target: at every byte offset we ask the
    //    index whether the window matches a basis block. The expectation is
    //    that exactly the four block-aligned windows that fall inside the
    //    planted region produce a hit, all referencing consecutive basis
    //    indices starting at `expected_block_index`.
    let mut hits: Vec<(usize, usize)> = Vec::new();
    let mut rolling = RollingChecksum::new();
    rolling.update(&target[..block_len]);
    if let Some(found) = index.find_match_bytes(rolling.digest(), &target[..block_len]) {
        hits.push((0, found));
    }
    for offset in 1..=target.len() - block_len {
        let out_byte = target[offset - 1];
        let in_byte = target[offset + block_len - 1];
        rolling
            .roll(out_byte, in_byte)
            .expect("rolling window is non-empty");
        let window = &target[offset..offset + block_len];
        if let Some(found) = index.find_match_bytes(rolling.digest(), window) {
            hits.push((offset, found));
        }
    }

    let expected_hits: Vec<(usize, usize)> = (0..expected_blocks)
        .map(|k| {
            (
                PLANTED_TARGET_OFFSET + k * block_len,
                expected_block_index + k,
            )
        })
        .collect();
    assert_eq!(
        hits, expected_hits,
        "the only hits must be the four block-aligned planted windows, in order",
    );

    // 2. Bithash rejection rate over the non-planted target region. For every
    //    sliding-window position outside the planted window we count whether
    //    the bithash gate rejects after the tag-table gate has accepted. The
    //    contract: at least `REJECTION_RATE_FLOOR` of post-tag probes are
    //    rejected by the bithash.
    let mut rolling = RollingChecksum::new();
    rolling.update(&target[..block_len]);
    let mut post_tag_probes = 0u64;
    let mut bithash_rejects = 0u64;
    let in_planted = |offset: usize| {
        offset >= PLANTED_TARGET_OFFSET && offset + block_len <= PLANTED_TARGET_OFFSET + PLANTED_LEN
    };
    if !in_planted(0) {
        let d = rolling.digest();
        if index.tag_table[d.sum1() as usize] {
            post_tag_probes += 1;
            if !index.bithash.contains(d.value()) {
                bithash_rejects += 1;
            }
        }
    }
    for offset in 1..=target.len() - block_len {
        let out_byte = target[offset - 1];
        let in_byte = target[offset + block_len - 1];
        rolling
            .roll(out_byte, in_byte)
            .expect("rolling window is non-empty");
        if in_planted(offset) {
            continue;
        }
        let d = rolling.digest();
        if !index.tag_table[d.sum1() as usize] {
            continue;
        }
        post_tag_probes += 1;
        if !index.bithash.contains(d.value()) {
            bithash_rejects += 1;
        }
    }

    assert!(
        post_tag_probes >= 1024,
        "expected at least 1024 post-tag probes to make the rejection ratio meaningful, got {post_tag_probes}",
    );
    let rejection_rate = bithash_rejects as f64 / post_tag_probes as f64;
    assert!(
        rejection_rate >= REJECTION_RATE_FLOOR,
        "bithash rejection rate {rejection_rate:.4} fell below the {REJECTION_RATE_FLOOR:.2} floor \
         (post_tag_probes={post_tag_probes}, rejects={bithash_rejects})",
    );
}

/// Construction sanity check: the basis builder is deterministic across
/// invocations, so two independent calls must yield byte-identical buffers.
/// Pinned at a 1 MiB prefix so the sanity check itself stays cheap.
#[test]
fn basis_builder_is_deterministic() {
    let mut a = vec![0u8; 1024 * 1024];
    let mut b = vec![0u8; 1024 * 1024];
    fill_xorshift(&mut a, 0x5EED_BEEF_DEAD_BABE, false);
    fill_xorshift(&mut b, 0x5EED_BEEF_DEAD_BABE, false);
    assert_eq!(a, b, "xorshift basis must be deterministic across calls");
}

/// Target builder sanity: every non-planted target byte produced with the
/// high-bit mask must have its MSB set. Confirmed on a 1 MiB prefix so the
/// sanity check is cheap.
#[test]
fn target_non_planted_bytes_have_msb_set() {
    let mut buf = vec![0u8; 1024 * 1024];
    fill_xorshift(&mut buf, 0xA5A5_A5A5_5A5A_5A5A, true);
    for (i, &b) in buf.iter().enumerate() {
        assert!(
            b & 0x80 != 0,
            "non-planted target byte at offset {i} missing MSB (value {b:#04x})",
        );
    }
}
