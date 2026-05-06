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
