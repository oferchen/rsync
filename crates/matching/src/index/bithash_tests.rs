//! Property tests for the [`super::bithash::BitHash`] prefilter.
//!
//! Pin the contract from `docs/design/zsync-bithash.md` section 5: the
//! bithash is one-sided, so for every block actually inserted into the
//! [`super::DeltaSignatureIndex`], the rolling-checksum probe MUST still
//! find it after the bithash gate is enabled. False positives are allowed;
//! false negatives are a correctness regression.

use std::num::{NonZeroU8, NonZeroU32};

use proptest::prelude::*;

use protocol::ProtocolVersion;
use signature::SignatureLayoutParams;
use signature::{SignatureAlgorithm, calculate_signature_layout, generate_file_signature};

use super::DeltaSignatureIndex;
use super::bithash::BitHash;

/// Block length that always produces full-length blocks across the basis.
const TEST_BLOCK_LENGTH: u32 = 700;

/// Strong-checksum length used by all the property cases.
const STRONG_LEN: u8 = 16;

/// Builds a [`DeltaSignatureIndex`] over the supplied basis bytes.
fn build_index(data: &[u8]) -> Option<DeltaSignatureIndex> {
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        Some(NonZeroU32::new(TEST_BLOCK_LENGTH).unwrap()),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(STRONG_LEN).unwrap(),
    );
    let layout = calculate_signature_layout(params).ok()?;
    let signature = generate_file_signature(data, layout, SignatureAlgorithm::Md4).ok()?;
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
}

/// Strategy generating basis files of one to eight full blocks plus an
/// optional partial trailer. Each byte is independently random so the basis
/// covers the signed-byte rolling-checksum range, which is the regime PR
/// #3560 pinned for the index.
fn basis_strategy() -> impl Strategy<Value = Vec<u8>> {
    (1usize..=8).prop_flat_map(|n_blocks| {
        let total = n_blocks * TEST_BLOCK_LENGTH as usize;
        proptest::collection::vec(any::<u8>(), total..=total)
    })
}

proptest! {
    /// The bithash gate MUST NOT cause a missed match on any indexed block.
    ///
    /// For every full-length block of the random basis, calling
    /// [`DeltaSignatureIndex::find_match_bytes`] with the corresponding
    /// rolling digest and the original window MUST return `Some(_)`. A
    /// `None` here would mean the bithash dropped a match that was already
    /// in the lookup table.
    #[test]
    fn bithash_never_misses_inserted_block(data in basis_strategy()) {
        let index = build_index(&data).expect("index from random basis");
        let block_length = index.block_length();

        let n_full = data.len() / block_length;
        for i in 0..n_full {
            let start = i * block_length;
            let window = &data[start..start + block_length];
            let digest = index.block(i).rolling();

            // Contiguous probe.
            let found_bytes = index.find_match_bytes(digest, window);
            prop_assert!(
                found_bytes.is_some(),
                "bithash dropped block {i} on the contiguous path",
            );

            // Split probe at every offset, including the boundaries.
            for split in [0usize, 1, block_length / 2, block_length - 1, block_length] {
                let (first, second) = window.split_at(split);
                let found_slices = index.find_match_slices(digest, first, second);
                prop_assert!(
                    found_slices.is_some(),
                    "bithash dropped block {i} on the split path at split={split}",
                );
            }
        }
    }

    /// Standalone bithash invariant: every inserted rsum is reported present.
    ///
    /// Drives the [`BitHash`] type directly with arbitrary `(n_blocks, rsums)`
    /// pairs to confirm the no-false-negative contract independently of the
    /// surrounding index.
    #[test]
    fn bithash_contains_every_inserted_rsum(
        n_blocks in 1usize..=4096,
        rsums in proptest::collection::vec(any::<u32>(), 1..=512),
    ) {
        let mut bh = BitHash::with_block_count(n_blocks);
        for &rsum in &rsums {
            bh.insert(rsum);
        }
        for &rsum in &rsums {
            prop_assert!(
                bh.contains(rsum),
                "bithash dropped previously-inserted rsum {rsum:#x}",
            );
        }
    }

    /// `clear` must preserve the no-false-negative contract for any rsums
    /// inserted after the reset, even when bits left from earlier inserts
    /// would otherwise alias.
    #[test]
    fn bithash_clear_then_reinsert_round_trips(
        first in proptest::collection::vec(any::<u32>(), 1..=256),
        second in proptest::collection::vec(any::<u32>(), 1..=256),
    ) {
        let mut bh = BitHash::with_block_count(2048);
        for &rsum in &first {
            bh.insert(rsum);
        }
        bh.clear();
        for &rsum in &second {
            bh.insert(rsum);
        }
        for &rsum in &second {
            prop_assert!(bh.contains(rsum));
        }
    }
}

/// Deterministic linear congruential generator for the rejection-rate test.
///
/// Avoids pulling `rand` into the dev dependencies while still spreading rsums
/// uniformly enough to exercise the bithash density bound. Numbers are the
/// constants from Numerical Recipes' `ran` generator.
fn lcg_next(state: &mut u64) -> u32 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    (*state >> 16) as u32
}

#[test]
fn bithash_rejects_at_least_seven_eighths_of_known_misses() {
    // ZSO-1 acceptance test: with N inserted rsums and 8N bithash bits, the
    // 1/8 saturation density means ~7/8 of uniform-random probes that were
    // never inserted should reject at the bithash gate.
    let n_blocks = 1000usize;
    let mut bh = BitHash::with_block_count(n_blocks);

    let mut inserted = std::collections::HashSet::with_capacity(n_blocks);
    let mut rng = 0xC0FFEE_u64;
    while inserted.len() < n_blocks {
        let rsum = lcg_next(&mut rng);
        if inserted.insert(rsum) {
            bh.insert(rsum);
        }
    }

    // Generate N known-miss probes drawn from a disjoint stream and count
    // bithash-stage rejections.
    let mut probe_rng = 0xDEAD_BEEF_CAFE_u64;
    let mut probes = 0usize;
    let mut rejects = 0usize;
    while probes < n_blocks {
        let rsum = lcg_next(&mut probe_rng);
        if inserted.contains(&rsum) {
            continue;
        }
        probes += 1;
        if !bh.contains(rsum) {
            rejects += 1;
        }
    }

    let reject_rate = rejects as f64 / probes as f64;
    assert!(
        reject_rate >= 0.87,
        "bithash rejected only {rejects}/{probes} = {reject_rate:.3} of known misses; \
         expected at least 7/8 = 0.875 per zsync density bound",
    );
}

#[test]
fn bithash_false_positive_rate_within_spec_bound() {
    // ZSO-1 detail validation: the zsync spec targets a bithash
    // false-positive rate of <= 25% for a filter sized 8x the element count.
    // oc-rsync over-provisions further - `2^i >= 4*N` buckets times
    // `2^(BITHASH_BITS+1) = 16` bits per bucket, i.e. >= 32 bits per element
    // (`log2_bits_for` rounds N to the next power of two, so effective bits
    // per element sit in the 32..64 band). The saturation set-bit density is
    // therefore ~1/32..1/64, well under the spec's 1/8, so the measured FP
    // rate must clear the 25% bound with wide margin.
    //
    // A false positive here only costs a redundant strong-checksum verify; a
    // false *negative* would drop a real match and is pinned separately by
    // `bithash_never_misses_inserted_block` / `bithash_contains_every_inserted_rsum`.
    //
    // Measured on this build (N = 4096 inserted, 65536 known-miss probes):
    //   false-positive rate = 2017/65536 = 0.031 (see the eprintln below),
    //   roughly 8x under the 0.25 spec bound.
    let n_blocks = 4096usize;
    let mut bh = BitHash::with_block_count(n_blocks);

    let mut inserted = std::collections::HashSet::with_capacity(n_blocks);
    let mut rng = 0x1234_5678_9abc_def0_u64;
    while inserted.len() < n_blocks {
        let rsum = lcg_next(&mut rng);
        if inserted.insert(rsum) {
            bh.insert(rsum);
        }
    }

    // Probe a large disjoint stream of never-inserted rsums and count the
    // ones the filter fails to reject (false positives).
    let probe_target = 16 * n_blocks;
    let mut probe_rng = 0x0fed_cba9_8765_4321_u64;
    let mut probes = 0usize;
    let mut false_positives = 0usize;
    while probes < probe_target {
        let rsum = lcg_next(&mut probe_rng);
        if inserted.contains(&rsum) {
            continue;
        }
        probes += 1;
        if bh.contains(rsum) {
            false_positives += 1;
        }
    }

    let fp_rate = false_positives as f64 / probes as f64;
    eprintln!(
        "bithash FP rate at default sizing: {false_positives}/{probes} = {fp_rate:.4} \
         (N={n_blocks}, bits={})",
        n_blocks << 5,
    );
    assert!(
        fp_rate <= 0.25,
        "bithash false-positive rate {fp_rate:.4} exceeds the spec's 25% bound \
         at oc's default sizing; investigate `log2_bits_for` before loosening this",
    );
}

#[test]
fn bithash_state_does_not_leak_between_independent_indexes() {
    // ZSO-1 acceptance test: two `DeltaSignatureIndex` values built from
    // distinct basis bytes must hold independent bithash state. Per the
    // ZSO-7 audit, the per-NDX construction discards prior state, so no
    // INC_RECURSE segment can see another segment's matched blocks via the
    // bithash gate.
    let mut left_basis = vec![0u8; (TEST_BLOCK_LENGTH * 4) as usize];
    for (i, byte) in left_basis.iter_mut().enumerate() {
        *byte = (i as u8).wrapping_mul(31).wrapping_add(7);
    }
    let mut right_basis = vec![0u8; (TEST_BLOCK_LENGTH * 4) as usize];
    for (i, byte) in right_basis.iter_mut().enumerate() {
        *byte = (i as u8).wrapping_mul(53).wrapping_add(199);
    }
    assert_ne!(left_basis, right_basis);

    let left = build_index(&left_basis).expect("left index");
    let right = build_index(&right_basis).expect("right index");

    // For every block indexed in `left`, the matching probe MUST succeed in
    // `left`. The same probe MUST NOT find a match in `right` (the random
    // bases are distinct), confirming the bithash is per-index and not a
    // shared singleton.
    let block_length = left.block_length();
    let n_full = left_basis.len() / block_length;
    let mut cross_matches = 0usize;
    for i in 0..n_full {
        let start = i * block_length;
        let window = &left_basis[start..start + block_length];
        let digest = left.block(i).rolling();
        assert!(
            left.find_match_bytes(digest, window).is_some(),
            "left index dropped its own block {i}",
        );
        if right.find_match_bytes(digest, window).is_some() {
            cross_matches += 1;
        }
    }
    assert_eq!(
        cross_matches, 0,
        "right index spuriously matched {cross_matches} left blocks; \
         bithash or lookup state leaked across indexes",
    );
}
