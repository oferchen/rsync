//! Per-generator-session bitmap of basis blocks that have already been
//! matched and emitted as `DeltaToken::Copy`.
//!
//! The bitmap shrinks the candidate set returned by the rolling-checksum
//! probe in [`super::DeltaSignatureIndex::find_match_slices_filtered`] so
//! that an already-matched basis block is not strong-checksum-verified at
//! later source offsets.
//!
//! # Duplicate-block correctness
//!
//! Bits are keyed by basis `block_idx`, not by `(rsum, strong)` tuple.
//! Two blocks with identical content occupy distinct `block_idx` slots,
//! so marking one leaves the other findable. The bucket walk visits every
//! candidate index in insertion order and picks the first whose bit is
//! still clear, mirroring upstream rsync's first-fit-in-bucket semantics.
//!
//! # Wire compatibility
//!
//! This is a probe-side, in-memory optimization. It changes which basis
//! `block_idx` is named in a `Copy` token when duplicate-content siblings
//! exist, never which bytes the receiver applies. Wire framing, golden
//! payloads, and interop behaviour are unchanged.
//!
//! See `docs/design/zsync-prune.md` for the full contract and the zsync
//! `librcksum` precedent.

const BITS_PER_WORD: usize = u64::BITS as usize;

/// Bitmap with one bit per basis block that records which blocks have
/// been emitted as `DeltaToken::Copy` during the current generator
/// session.
///
/// Constructed via [`MatchedBlocks::with_block_count`], mutated through
/// [`MatchedBlocks::mark_matched`], and queried with
/// [`MatchedBlocks::is_matched`]. [`MatchedBlocks::clear`] resets every
/// bit so the same allocation can be reused across generator sessions
/// (e.g., INC_RECURSE per-segment rebuilds).
#[derive(Clone, Debug, Default)]
pub struct MatchedBlocks {
    bits: Vec<u64>,
    block_count: usize,
}

impl MatchedBlocks {
    /// Allocates a bitmap sized for `block_count` basis blocks with every
    /// bit cleared.
    ///
    /// A `block_count` of zero produces an empty bitmap that rejects every
    /// [`MatchedBlocks::is_matched`] query; matching callers should skip
    /// pruning entirely in that case.
    #[must_use]
    pub fn with_block_count(block_count: usize) -> Self {
        let words = block_count.div_ceil(BITS_PER_WORD);
        Self {
            bits: vec![0u64; words],
            block_count,
        }
    }

    /// Returns the number of basis blocks the bitmap was sized for.
    #[must_use]
    pub const fn block_count(&self) -> usize {
        self.block_count
    }

    /// Marks the basis block at `idx` as matched.
    ///
    /// Out-of-range indices are ignored so callers do not need to guard
    /// the call site against malformed candidate vectors.
    #[inline]
    pub fn mark_matched(&mut self, idx: usize) {
        if idx >= self.block_count {
            return;
        }
        let word = idx / BITS_PER_WORD;
        let bit = idx % BITS_PER_WORD;
        self.bits[word] |= 1u64 << bit;
    }

    /// Returns `true` when the basis block at `idx` has been marked as
    /// matched.
    ///
    /// Out-of-range indices return `false`. The hot path uses this in
    /// the candidate-filtering step before the strong-checksum verify
    /// in [`super::DeltaSignatureIndex::find_match_slices_filtered`].
    #[inline]
    #[must_use]
    pub fn is_matched(&self, idx: usize) -> bool {
        if idx >= self.block_count {
            return false;
        }
        let word = idx / BITS_PER_WORD;
        let bit = idx % BITS_PER_WORD;
        (self.bits[word] & (1u64 << bit)) != 0
    }

    /// Resets every bit so the allocation can be reused for a new
    /// generator session without re-allocating.
    pub fn clear(&mut self) {
        for word in &mut self.bits {
            *word = 0;
        }
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn with_block_count_zero_has_no_capacity() {
        let m = MatchedBlocks::with_block_count(0);
        assert_eq!(m.block_count(), 0);
        assert!(!m.is_matched(0));
    }

    #[test]
    fn with_block_count_rounds_up_to_word_size() {
        let m = MatchedBlocks::with_block_count(1);
        assert_eq!(m.block_count(), 1);
        assert_eq!(m.bits.len(), 1);

        let m = MatchedBlocks::with_block_count(BITS_PER_WORD);
        assert_eq!(m.bits.len(), 1);

        let m = MatchedBlocks::with_block_count(BITS_PER_WORD + 1);
        assert_eq!(m.bits.len(), 2);
    }

    #[test]
    fn mark_then_query_round_trip() {
        let mut m = MatchedBlocks::with_block_count(200);
        for idx in [0, 1, 63, 64, 65, 127, 128, 199] {
            assert!(!m.is_matched(idx));
            m.mark_matched(idx);
            assert!(m.is_matched(idx), "bit {idx} should be set");
        }
        for idx in [2, 3, 62, 100, 198] {
            assert!(!m.is_matched(idx), "bit {idx} should still be clear");
        }
    }

    #[test]
    fn out_of_range_indices_are_ignored() {
        let mut m = MatchedBlocks::with_block_count(10);
        m.mark_matched(10);
        m.mark_matched(usize::MAX);
        assert!(!m.is_matched(10));
        assert!(!m.is_matched(usize::MAX));
    }

    #[test]
    fn clear_resets_all_bits() {
        let mut m = MatchedBlocks::with_block_count(130);
        for idx in 0..130 {
            m.mark_matched(idx);
        }
        m.clear();
        for idx in 0..130 {
            assert!(!m.is_matched(idx), "bit {idx} should be clear");
        }
    }

    #[test]
    fn mark_is_idempotent() {
        let mut m = MatchedBlocks::with_block_count(8);
        m.mark_matched(3);
        m.mark_matched(3);
        m.mark_matched(3);
        assert!(m.is_matched(3));
    }

    #[test]
    fn default_is_empty() {
        let m = MatchedBlocks::default();
        assert_eq!(m.block_count(), 0);
        assert!(!m.is_matched(0));
    }
}
