//! Signature index for fast delta block lookup.
//!
//! This module provides [`DeltaSignatureIndex`] for O(1) block lookups during
//! delta generation. It indexes signature blocks by their rolling checksum
//! components `(sum1, sum2)` for efficient matching.
//!
//! Uses [`FxHashMap`] for 2-5x faster lookups compared to std HashMap,
//! optimized for small integer keys like `(u16, u16)`.

mod builder;

#[cfg(test)]
mod tests;

use std::collections::VecDeque;

use rustc_hash::FxHashMap;

use checksums::RollingDigest;

use signature::{SignatureAlgorithm, SignatureBlock};

/// Size of the tag table for quick rolling checksum rejection (2^16 entries).
///
/// Upstream rsync uses a boolean array indexed by the low 16 bits (sum1) of the
/// rolling checksum to reject non-matching positions before probing the hash
/// table. This constant matches upstream's `TABLESIZE` in `match.c`.
const TAG_TABLE_SIZE: usize = 1 << 16;

/// Index over a file signature that accelerates delta matching.
///
/// Uses [`FxHashMap`] keyed by `(sum1, sum2)` rolling checksum components for O(1)
/// block lookup. A tag table indexed by `sum1` provides fast-path rejection
/// before the hash probe, mirroring upstream rsync's `tag_table` in `match.c`.
/// The block length is stored separately since all indexed blocks have the same
/// canonical length.
#[derive(Clone, Debug)]
pub struct DeltaSignatureIndex {
    block_length: usize,
    strong_length: usize,
    algorithm: SignatureAlgorithm,
    blocks: Vec<SignatureBlock>,
    /// Lookup table keyed by (sum1, sum2) - block length is constant for all entries.
    lookup: FxHashMap<(u16, u16), Vec<usize>>,
    /// Tag table for O(1) rejection using sum1 (low 16 bits of rolling checksum).
    /// upstream: match.c - `tag_table[s1]` check before hash probe.
    tag_table: Vec<bool>,
}

impl DeltaSignatureIndex {
    /// Returns the canonical block length expressed in bytes.
    #[must_use]
    pub const fn block_length(&self) -> usize {
        self.block_length
    }

    /// Returns the total number of signature blocks.
    #[must_use]
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Returns the strong checksum length used by the signature.
    #[must_use]
    pub const fn strong_length(&self) -> usize {
        self.strong_length
    }

    /// Returns the [`SignatureBlock`] for the provided index.
    #[inline]
    #[must_use]
    pub fn block(&self, index: usize) -> &SignatureBlock {
        &self.blocks[index]
    }

    /// Attempts to locate a matching block for a contiguous byte slice.
    #[inline]
    pub fn find_match_bytes(&self, digest: RollingDigest, window: &[u8]) -> Option<usize> {
        if window.len() != self.block_length {
            return None;
        }

        // upstream: match.c - tag_table[s1] fast-path rejects most non-matching
        // positions before the more expensive hash probe.
        if !self.tag_table[digest.sum1() as usize] {
            return None;
        }

        let key = (digest.sum1(), digest.sum2());
        let candidates = self.lookup.get(&key)?;

        // Strong checksum is CPU-intensive; parallelise only when there are
        // enough candidates to amortise rayon's per-call overhead.
        #[cfg(feature = "parallel")]
        if candidates.len() >= Self::PARALLEL_THRESHOLD {
            return self.find_match_parallel(candidates, window);
        }

        self.find_match_sequential(candidates, window)
    }

    /// Minimum number of candidates to trigger parallel verification.
    ///
    /// Below this threshold, the overhead of thread spawning exceeds the benefit.
    /// With 4+ candidates, parallel strong checksum computation provides measurable speedup.
    #[cfg(feature = "parallel")]
    const PARALLEL_THRESHOLD: usize = 4;

    /// Sequential candidate verification (used for few candidates).
    ///
    /// Computes the strong checksum once and compares against all candidates,
    /// mirroring upstream rsync's `done_csum2` flag in `match.c:hash_search()`.
    #[inline]
    fn find_match_sequential(&self, candidates: &[usize], window: &[u8]) -> Option<usize> {
        let strong = self.algorithm.compute_truncated(window, self.strong_length);
        for &index in candidates {
            let block = &self.blocks[index];
            debug_assert_eq!(block.len(), self.block_length);
            if strong.as_slice() == block.strong() {
                return Some(index);
            }
        }
        None
    }

    /// Parallel candidate verification using rayon.
    ///
    /// Computes strong checksums concurrently and returns the first match found.
    /// Uses `find_any` for early termination when a match is discovered.
    #[cfg(feature = "parallel")]
    fn find_match_parallel(&self, candidates: &[usize], window: &[u8]) -> Option<usize> {
        use rayon::prelude::*;

        candidates
            .par_iter()
            .find_any(|&&index| {
                let block = &self.blocks[index];
                let strong = self.algorithm.compute_truncated(window, self.strong_length);
                strong.as_slice() == block.strong()
            })
            .copied()
    }

    /// Attempts to locate a matching block for a possibly non-contiguous window
    /// represented as two slices.
    ///
    /// This avoids O(block_len) ring buffer rotation by feeding both slices
    /// directly into the streaming strong checksum. The combined length of
    /// `first` and `second` must equal `block_length`.
    #[inline]
    pub fn find_match_slices(
        &self,
        digest: RollingDigest,
        first: &[u8],
        second: &[u8],
    ) -> Option<usize> {
        if first.len() + second.len() != self.block_length {
            return None;
        }

        if !self.tag_table[digest.sum1() as usize] {
            return None;
        }

        let key = (digest.sum1(), digest.sum2());
        let candidates = self.lookup.get(&key)?;

        #[cfg(feature = "parallel")]
        if candidates.len() >= Self::PARALLEL_THRESHOLD {
            return self.find_match_slices_parallel(candidates, first, second);
        }

        self.find_match_slices_sequential(candidates, first, second)
    }

    /// Sequential candidate verification for non-contiguous window data.
    #[inline]
    fn find_match_slices_sequential(
        &self,
        candidates: &[usize],
        first: &[u8],
        second: &[u8],
    ) -> Option<usize> {
        let strong = self
            .algorithm
            .compute_truncated_slices(first, second, self.strong_length);
        for &index in candidates {
            let block = &self.blocks[index];
            debug_assert_eq!(block.len(), self.block_length);
            if strong.as_slice() == block.strong() {
                return Some(index);
            }
        }
        None
    }

    /// Parallel candidate verification for non-contiguous window data.
    #[cfg(feature = "parallel")]
    fn find_match_slices_parallel(
        &self,
        candidates: &[usize],
        first: &[u8],
        second: &[u8],
    ) -> Option<usize> {
        use rayon::prelude::*;

        candidates
            .par_iter()
            .find_any(|&&index| {
                let block = &self.blocks[index];
                let strong =
                    self.algorithm
                        .compute_truncated_slices(first, second, self.strong_length);
                strong.as_slice() == block.strong()
            })
            .copied()
    }

    /// Checks whether a specific block matches the given rolling digest and window data.
    ///
    /// Used by the `want_i` adjacent-match hinting optimization: after a match at
    /// block `i`, the next match is likely at block `i+1`. This method verifies that
    /// hypothesis with a rolling checksum comparison (cheap) followed by a strong
    /// checksum comparison (expensive) only on rolling match, bypassing the hash
    /// table entirely.
    ///
    /// upstream: match.c:144-190 - `want_i` hint check before full hash search.
    #[inline]
    pub fn check_block_match_slices(
        &self,
        block_index: usize,
        digest: RollingDigest,
        first: &[u8],
        second: &[u8],
    ) -> bool {
        if block_index >= self.blocks.len() {
            return false;
        }
        let block = &self.blocks[block_index];
        if block.len() != self.block_length {
            return false;
        }
        let block_digest = block.rolling();
        if digest.sum1() != block_digest.sum1() || digest.sum2() != block_digest.sum2() {
            return false;
        }
        let strong = self
            .algorithm
            .compute_truncated_slices(first, second, self.strong_length);
        strong.as_slice() == block.strong()
    }

    /// Attempts to locate a matching block for a non-contiguous window backed by a [`VecDeque`].
    pub fn find_match_window(
        &self,
        digest: RollingDigest,
        window: &VecDeque<u8>,
        scratch: &mut Vec<u8>,
    ) -> Option<usize> {
        if window.len() != self.block_length {
            return None;
        }

        scratch.clear();
        let (front, back) = window.as_slices();
        scratch.extend_from_slice(front);
        scratch.extend_from_slice(back);
        self.find_match_bytes(digest, scratch.as_slice())
    }

    /// Extends a confirmed block match into a run of consecutive matching blocks.
    ///
    /// After the matcher confirms that block `start_block_index` matches some
    /// target window, this helper probes the immediately following indexed
    /// blocks (`start_block_index + 1`, `+ 2`, ...) against the target bytes
    /// that follow. It returns the count of consecutive blocks, starting at
    /// `start_block_index`, whose rolling and strong checksums match the
    /// corresponding `block_length`-sized chunk of `target`.
    ///
    /// `target` must be a contiguous slice of the source data starting at the
    /// byte offset where block `start_block_index` is expected to match.
    /// `max_blocks` caps the run length so callers can bound buffering.
    ///
    /// The returned count is at least 1 when block `start_block_index` is a
    /// valid full-length basis block and its checksum matches the first
    /// `block_length` bytes of `target`. When the start block is the last
    /// indexed block, the run cannot extend beyond it. The helper does not
    /// change which basis indices are picked, only how many adjacent matches
    /// are confirmed in one call.
    ///
    /// upstream: zsync `librcksum/rsum.c:262` advances `next_match` after
    /// each confirmed match, the same effect this helper realizes in one
    /// call. See `docs/design/zsync-seq-match.md` for the wire-compat
    /// invariant.
    #[must_use]
    pub fn extend_run(&self, start_block_index: usize, target: &[u8], max_blocks: usize) -> usize {
        if max_blocks == 0 || self.block_length == 0 {
            return 0;
        }

        let block_len = self.block_length;
        let mut run = 0usize;
        let mut offset = 0usize;
        while run < max_blocks {
            let block_idx = start_block_index + run;
            if block_idx >= self.blocks.len() {
                break;
            }
            let end = match offset.checked_add(block_len) {
                Some(end) if end <= target.len() => end,
                _ => break,
            };
            let chunk = &target[offset..end];

            let block = &self.blocks[block_idx];
            if block.len() != block_len {
                break;
            }

            let chunk_digest = RollingDigest::from_bytes(chunk);
            let block_digest = block.rolling();
            if chunk_digest.sum1() != block_digest.sum1()
                || chunk_digest.sum2() != block_digest.sum2()
            {
                break;
            }

            let strong = self.algorithm.compute_truncated(chunk, self.strong_length);
            if strong.as_slice() != block.strong() {
                break;
            }

            run += 1;
            offset = end;
        }
        run
    }
}
