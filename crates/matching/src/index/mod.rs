//! Signature index for fast delta block lookup.
//!
//! This module provides [`DeltaSignatureIndex`] for O(1) block lookups during
//! delta generation. It indexes signature blocks by their rolling checksum
//! components `(sum1, sum2)` for efficient matching.
//!
//! Uses a flat open-addressing hash table ([`CompactLookup`]) with Robin Hood
//! probing for cache-friendly O(1) lookups. Each entry is 8 bytes (packed key
//! + block index), fitting 8 entries per 64-byte cache line.

mod bithash;
mod builder;
mod compact_lookup;
mod matched_blocks;
mod trace;

#[cfg(test)]
mod bithash_tests;
#[cfg(test)]
mod matched_blocks_tests;
#[cfg(test)]
mod sparse_match_tests;
#[cfg(test)]
mod tests;

use std::collections::VecDeque;

use checksums::RollingDigest;

use signature::{SignatureAlgorithm, SignatureBlock};

use bithash::BitHash;
use compact_lookup::CompactLookup;
pub use matched_blocks::MatchedBlocks;
pub use trace::{
    HASH_KEY_BITS, HashtableRole, trace_created as trace_hashtable_created,
    trace_destroyed as trace_hashtable_destroyed, trace_growing as trace_hashtable_growing,
};

/// Size of the tag table for quick rolling checksum rejection (2^16 entries).
///
/// Upstream rsync uses a boolean array indexed by the low 16 bits (sum1) of the
/// rolling checksum to reject non-matching positions before probing the hash
/// table. This constant matches upstream's `TABLESIZE` in `match.c`.
const TAG_TABLE_SIZE: usize = 1 << 16;

/// Index over a file signature that accelerates delta matching.
///
/// Uses a flat open-addressing hash table ([`CompactLookup`]) keyed by packed
/// `(sum1, sum2)` for O(1) block lookup with excellent cache locality. A tag
/// table indexed by `sum1` provides fast-path rejection before the hash probe,
/// mirroring upstream rsync's `tag_table` in `match.c`. The block length is
/// stored separately since all indexed blocks have the same canonical length.
#[derive(Clone, Debug)]
pub struct DeltaSignatureIndex {
    block_length: usize,
    strong_length: usize,
    algorithm: SignatureAlgorithm,
    blocks: Vec<SignatureBlock>,
    /// Flat open-addressing lookup keyed by packed (sum1, sum2).
    lookup: CompactLookup,
    /// Tag table for O(1) rejection using sum1 (low 16 bits of rolling checksum).
    /// upstream: match.c - `tag_table[s1]` check before hash probe.
    tag_table: Vec<bool>,
    /// Bithash prefilter mixing both rolling-sum halves.
    ///
    /// Sized to roughly 8x the rsum-bucket count (~1 byte per indexed block),
    /// the bithash rejects ~7/8 of post-tag misses before the hash probe.
    /// Mirrors zsync's `librcksum/rsum.c:362-366` probe expression.
    bithash: BitHash,
    /// Role used for `--debug=HASH` `[<role>]` prefixes on the create,
    /// grow, and destroy lifecycle emissions. Mirrors upstream
    /// `who_am_i()` (`hashtable.c:51,61,101`).
    role: HashtableRole,
    /// Slot count tracked across rebuilds for the matching destroy emission.
    last_traced_size: usize,
}

impl DeltaSignatureIndex {
    /// Returns the role used for `--debug=HASH` `[<role>]` prefixes.
    #[must_use]
    pub const fn role(&self) -> HashtableRole {
        self.role
    }

    /// Overrides the role used in subsequent HASH emissions.
    ///
    /// Useful for call sites that build the index in a generic helper
    /// and only learn the actual `who_am_i()` value later.
    pub fn set_role(&mut self, role: HashtableRole) {
        self.role = role;
    }

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
        self.find_match_bytes_filtered(digest, window, None)
    }

    /// Attempts to locate a matching block for a contiguous byte slice,
    /// skipping basis blocks already marked in `matched`.
    ///
    /// When `matched` is `None` this behaves identically to
    /// [`Self::find_match_bytes`]. When `Some`, candidate basis blocks
    /// whose bit is set in the bitmap are filtered out before the
    /// strong-checksum verify, mirroring zsync's `librcksum`
    /// matched-block pruning. Pruning never reduces the set of source
    /// bytes that can be matched: see [`MatchedBlocks`] for the
    /// duplicate-block correctness contract.
    #[inline]
    pub fn find_match_bytes_filtered(
        &self,
        digest: RollingDigest,
        window: &[u8],
        matched: Option<&MatchedBlocks>,
    ) -> Option<usize> {
        if window.len() != self.block_length {
            return None;
        }

        // upstream: match.c - tag_table[s1] fast-path rejects most non-matching
        // positions before the more expensive hash probe.
        if !self.tag_table[digest.sum1() as usize] {
            return None;
        }

        // zsync-style bithash prefilter: rejects ~7/8 of post-tag misses
        // using both sum halves before the hash-table probe. One-sided, so
        // never produces a false negative.
        if !self.bithash.contains(digest.value()) {
            return None;
        }

        let strong = self.algorithm.compute_truncated(window, self.strong_length);
        for index in self.lookup.find_all(digest.sum1(), digest.sum2()) {
            if matches!(matched, Some(m) if m.is_matched(index)) {
                continue;
            }
            let block = &self.blocks[index];
            debug_assert_eq!(block.len(), self.block_length);
            if strong.as_slice() == block.strong() {
                return Some(index);
            }
        }
        None
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
        self.find_match_slices_filtered(digest, first, second, None)
    }

    /// Attempts to locate a matching block for a non-contiguous window,
    /// skipping basis blocks already marked in `matched`.
    ///
    /// Mirrors [`Self::find_match_bytes_filtered`] for the two-slice
    /// window form used by the generator's ring buffer. Passing `None`
    /// is equivalent to [`Self::find_match_slices`].
    #[inline]
    pub fn find_match_slices_filtered(
        &self,
        digest: RollingDigest,
        first: &[u8],
        second: &[u8],
        matched: Option<&MatchedBlocks>,
    ) -> Option<usize> {
        if first.len() + second.len() != self.block_length {
            return None;
        }

        if !self.tag_table[digest.sum1() as usize] {
            return None;
        }

        if !self.bithash.contains(digest.value()) {
            return None;
        }

        let strong = self
            .algorithm
            .compute_truncated_slices(first, second, self.strong_length);
        for index in self.lookup.find_all(digest.sum1(), digest.sum2()) {
            if matches!(matched, Some(m) if m.is_matched(index)) {
                continue;
            }
            let block = &self.blocks[index];
            debug_assert_eq!(block.len(), self.block_length);
            if strong.as_slice() == block.strong() {
                return Some(index);
            }
        }
        None
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

impl Drop for DeltaSignatureIndex {
    /// Emits `--debug=HASH` level 1 destroy diagnostic, mirroring upstream
    /// rsync's `hashtable_destroy` (`hashtable.c:60-63`).
    ///
    /// Clones produced by `#[derive(Clone)]` carry the same `role` and
    /// `last_traced_size`, so each clone's drop fires an independent
    /// upstream-format line - the count matches the create count when
    /// callers honour the `Drop` contract.
    fn drop(&mut self) {
        let id = self.identifier();
        trace::trace_destroyed(self.role, id, self.last_traced_size);
    }
}

impl DeltaSignatureIndex {
    /// Returns a stable identifier for `--debug=HASH` emissions.
    ///
    /// Upstream prints `(long)tbl` (the heap address of the hashtable
    /// struct). We approximate that with the address of the index's
    /// own struct so the create/destroy emissions for a single index
    /// share the same identifier.
    #[inline]
    pub(super) fn identifier(&self) -> usize {
        self as *const Self as usize
    }
}

/// Bench-only accessors used by the harnesses in `crates/matching/benches/`.
///
/// Behind the internal `bench-internal` feature flag so the surface never
/// reaches release builds. See `docs/design/zsync-bithash.md` section 7
/// for the rejection-rate methodology these accessors support.
#[cfg(feature = "bench-internal")]
impl DeltaSignatureIndex {
    /// Returns the fraction of bithash bits currently set, in `[0.0, 1.0]`.
    ///
    /// At saturation the bithash carries one set bit per indexed block
    /// (1/8 density target), so a uniform-random rsum probe rejects with
    /// probability roughly `1.0 - utilization`.
    #[must_use]
    pub fn bithash_utilization(&self) -> f64 {
        self.bithash.utilization()
    }

    /// Returns `true` when the bithash prefilter would let `rsum` through
    /// to the strong-checksum verify step.
    ///
    /// Mirrors the inner `bithash.contains(...)` probe in
    /// [`Self::find_match_bytes_filtered`], without the tag-table or
    /// strong-checksum work. Lets the bench harness count rejection rate
    /// without instrumenting the production hot path.
    #[must_use]
    pub fn bithash_admits(&self, rsum: u32) -> bool {
        self.bithash.contains(rsum)
    }

    /// Returns `true` when the `tag_table` would let `sum1` through to
    /// the bithash probe.
    ///
    /// Mirrors the first-line tag-table gate so the bench can isolate
    /// post-tag bithash rejection from the upstream-style fast path.
    #[must_use]
    pub fn tag_admits(&self, sum1: u16) -> bool {
        self.tag_table[sum1 as usize]
    }

    /// Slot count of the underlying compact lookup table.
    ///
    /// The lookup table is a flat `Vec<u64>` open-addressing hash whose
    /// footprint dominates the cache behaviour of a hot lookup loop.
    /// Bench harnesses use this to bin index sizes against the local CPU
    /// cache hierarchy (L1 / L2 / LLC / main memory).
    #[must_use]
    pub fn lookup_capacity(&self) -> usize {
        self.lookup.capacity()
    }

    /// Byte size of the underlying compact lookup table allocation.
    ///
    /// Equal to `lookup_capacity() * 8` for the current 8-byte slot layout.
    /// Reported separately so harnesses keep working if the slot width
    /// changes (#2072 packed-key candidate).
    #[must_use]
    pub fn lookup_bytes(&self) -> usize {
        self.lookup.capacity() * core::mem::size_of::<u64>()
    }

    /// Drives the compact lookup probe directly, skipping the tag-table
    /// and bithash prefilters and the strong-checksum verify.
    ///
    /// Returns the number of basis block indices yielded by the
    /// `find_all` iterator at the requested `(sum1, sum2)` key. Bench
    /// harnesses use this to isolate the cache cost of the open-addressed
    /// probe chain itself from the surrounding match pipeline.
    #[must_use]
    pub fn lookup_probe(&self, sum1: u16, sum2: u16) -> usize {
        self.lookup.find_all(sum1, sum2).count()
    }
}
