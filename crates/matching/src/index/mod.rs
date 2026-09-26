//! Signature index for fast delta block lookup.
//!
//! This module provides [`DeltaSignatureIndex`] for O(1) block lookups during
//! delta generation. It indexes signature blocks by their rolling checksum
//! components `(sum1, sum2)` for efficient matching.
//!
//! The bucket index ([`CompactLookup`]) mirrors upstream `match.c`'s two
//! addressing modes. Below upstream's `TRADITIONAL_TABLESIZE` (`match.c:45`)
//! it addresses buckets using only the upper half of the rolling sum
//! (`rsum >> 16`, equal to `sum2`), with the lower half (`sum1`) stored as an
//! in-bucket discriminator - the ZSO-4 translation of zsync's
//! `librcksum/hash.c:45` `rsum_a_mask` trick, which keeps the hottest lookup
//! table cache-line resident across rolling-hash advances. Above it the table
//! grows on upstream's `(count/8) * 10 + 11` rule (`match.c:84-88`) and
//! addresses on the full rolling sum (`match.c:74` `BIG_SUM2HASH`), because a
//! `sum2`-only key carries no entropy past `2^16`.

mod bithash;
mod builder;
mod compact_lookup;
mod trace;

#[cfg(test)]
mod bithash_tests;
#[cfg(test)]
mod chain_bound_tests;
#[cfg(test)]
mod compact_key_tests;
#[cfg(test)]
mod seq_match_tests;
#[cfg(test)]
mod sparse_match_tests;
#[cfg(test)]
mod tests;

use std::collections::VecDeque;

use checksums::RollingDigest;

use signature::{SignatureAlgorithm, SignatureBlock};

use bithash::BitHash;
use compact_lookup::CompactLookup;
pub use trace::{
    HASH_KEY_BITS, HashtableRole, trace_created as trace_hashtable_created,
    trace_destroyed as trace_hashtable_destroyed, trace_growing as trace_hashtable_growing,
};

/// Size of the tag table for quick rolling checksum rejection (2^16 entries).
///
/// The tag table is a boolean array indexed by the low 16 bits (`sum1`) of the
/// rolling checksum, rejecting non-matching positions before the bithash and
/// bucket probes. `2^16` is the entire keyspace of a 16-bit index, so this is
/// a complete table rather than a sized one: there is nothing to grow it to,
/// and every entry is addressable.
///
/// It is *not* the analogue of upstream's `hash_table`, whose size does track
/// the block count (`match.c:84-88`); that role belongs to [`CompactLookup`].
/// rsync 3.5.0 has no tag table of its own - upstream dropped the 2.x-era one
/// when `match.c` moved to a chained `hash_table`.
const TAG_TABLE_SIZE: usize = 1 << 16;

/// Maximum number of equal-weak-checksum candidates verified at a single
/// source offset before the matcher gives up and rolls forward a byte.
///
/// upstream: `match.c` `MAX_CHAIN_LEN` (rsync 3.5.0), added for
/// CVE-2026-70453. A weak checksum that collides thousands of times turns the
/// per-offset chain walk into an `O(source_len * chain_len)` scan, and the
/// signature that populates the chain is peer-supplied, so a hostile peer can
/// pin a CPU for hours with a crafted basis. Bounding the per-offset work
/// caps that at `O(source_len * MAX_CHAIN_LEN)`.
///
/// Reaching the bound is a non-match, never an error: the skipped bytes are
/// sent as literal data, so the transfer stays correct and only its size can
/// change. Honest bases never approach the bound - see the
/// `bound_is_inert_on_a_realistic_basis` test.
const MAX_CHAIN_LEN: u32 = 1024;

/// Sentinel marking the absence of a stored successor in [`DeltaSignatureIndex::next_match`].
///
/// The link table stores `u32` block indices; `u32::MAX` is reserved to mean
/// "no successor" without paying a `Vec<Option<u32>>` discriminant. Real basis
/// block counts never reach `u32::MAX` because the wire-format block index is
/// itself a `u32` and one slot is consumed by the sentinel.
const NEXT_MATCH_NONE: u32 = u32::MAX;

/// Per-run tally of the delta matcher's probe diagnostics, mirroring the two
/// counters upstream reports on the `-vv` `total:` line.
///
/// Semantics are oc-native. Upstream (`match.c:202`) increments `hash_hits`
/// once per source offset whose `SUM2HASH` bucket is non-empty, and
/// (`match.c:239`) `false_alarms` once per candidate whose weak sum matches but
/// whose strong sum does not. oc has no such hashed-bucket table: instead
/// `hash_hits` counts positives of the zsync bithash prefilter (the coarse gate
/// analogous to a bucket hit) and `false_alarms` counts bithash positives that
/// then fail the exact rolling-sum + strong-sum verify. Because upstream's
/// counts are inflated by `SUM2HASH` collisions that oc's structure does not
/// have, the two numbers are NOT expected to equal the upstream oracle; they
/// carry genuine signal about oc's own bithash false-positive rate. The
/// `matches` count and the `data` byte total on the same line DO match upstream
/// exactly.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProbeCounters {
    /// Source offsets that passed the bithash prefilter (the coarse gate; some
    /// will still fail the exact verify and count as false alarms).
    pub hash_hits: u64,
    /// Bithash positives that produced no confirmed match (exact rolling-sum or
    /// strong-sum verify rejected them).
    pub false_alarms: u64,
}

/// Index over a file signature that accelerates delta matching.
///
/// Uses a chained bucket table (`CompactLookup`) sized on upstream's
/// `match.c` rule for O(1) block lookup. Below upstream's
/// `TRADITIONAL_TABLESIZE` it addresses on the upper half of the rolling sum
/// (`sum2`) for cache locality, keeping the lower half (`sum1`) inside each
/// chain entry as an in-bucket discriminator per zsync's `librcksum`
/// `rsum_a_mask` trick (ZSO-4); above it the table grows and addresses on the
/// full rolling sum as upstream does. A tag table indexed by `sum1` provides
/// fast-path rejection before the bucket walk, and the bithash prefilter
/// (ZSO-1) rejects the bulk of post-tag misses before the chain probe.
#[derive(Clone, Debug)]
pub struct DeltaSignatureIndex {
    block_length: usize,
    strong_length: usize,
    algorithm: SignatureAlgorithm,
    blocks: Vec<SignatureBlock>,
    /// Compact bucket lookup keyed on the upper half of the rolling sum
    /// (`rsum >> 16`); the lower 16 bits live inside each chain entry as
    /// the in-bucket discriminator. See the ZSO-4 module-level docs.
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
    /// Sequential-match lookahead: `next_match[k]` is the block index that
    /// followed block `k` in source-file order during indexing, or
    /// [`NEXT_MATCH_NONE`] when block `k` has no full-length successor.
    ///
    /// Lets the matcher skip the rolling-rsum table lookup when a confirmed
    /// match at block `K` was already followed by a confirmed match at the
    /// linked successor in the basis. Mirrors zsync's `next_match` slot from
    /// `librcksum/rsum.c:262` while preserving wire-format bytes - the field
    /// is in-memory only and is cleared by [`Self::rebuild`] so per-segment
    /// INC_RECURSE indexes never inherit stale links.
    next_match: Vec<u32>,
    /// `true` when two or more indexed full-length blocks share identical
    /// content (equal rolling checksum and equal strong checksum).
    ///
    /// Computed once at construction ([`Self::from_signature_with_role`] and
    /// [`Self::rebuild`]) so callers can query it in O(1). The parallel
    /// delta scan ([`crate::DeltaGenerator::generate_chunked`]) starts each
    /// stripe with a fresh `want_i` hint; on a basis with duplicate-content
    /// blocks that can change which sibling a source window resolves to (and
    /// therefore the emitted `Copy{index}`) relative to the sequential
    /// [`crate::DeltaGenerator::generate`]. A duplicate-free basis has exactly
    /// one candidate per window, so the wiring layer only engages the parallel
    /// scan when this returns `false`.
    has_duplicate_blocks: bool,
    /// Role used for `--debug=HASH` `[<role>]` prefixes on the create,
    /// grow, and destroy lifecycle emissions. Mirrors upstream
    /// `who_am_i()` (`hashtable.c:51,61,101`).
    role: HashtableRole,
    /// Slot count tracked across rebuilds for the matching destroy emission.
    last_traced_size: usize,
    /// Test-only seq-match probe counters. Wrapped in `Arc<...>` so the
    /// [`Clone`] derive shares the counter across handles, which is what
    /// the per-segment isolation tests need to assert. Production builds
    /// skip the field entirely so the hot path carries zero overhead.
    #[cfg(any(test, feature = "bench-internal"))]
    seq_match_counters: std::sync::Arc<SeqMatchCounters>,
}

/// Test-only seq-match probe counters.
///
/// Three atomic counters cover the lookahead state machine:
///
/// - `probes`: every call into a `try_next_match_*` entry point.
/// - `hits`: probes whose strong-checksum verify confirmed the linked successor.
/// - `misses`: probes that fell through to the full rolling-rsum lookup.
///
/// The split lets the per-segment isolation test assert that the counters
/// reset cleanly across [`DeltaSignatureIndex::rebuild`].
#[cfg(any(test, feature = "bench-internal"))]
#[derive(Debug, Default)]
pub struct SeqMatchCounters {
    probes: std::sync::atomic::AtomicU64,
    hits: std::sync::atomic::AtomicU64,
    misses: std::sync::atomic::AtomicU64,
}

#[cfg(any(test, feature = "bench-internal"))]
impl SeqMatchCounters {
    /// Returns the total number of [`DeltaSignatureIndex::try_next_match_slices`]
    /// (and contiguous-bytes equivalent) calls observed since the last reset.
    #[must_use]
    pub fn probes(&self) -> u64 {
        self.probes.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Returns the number of probes that confirmed the linked successor.
    #[must_use]
    pub fn hits(&self) -> u64 {
        self.hits.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Returns the number of probes that fell through to the full lookup.
    #[must_use]
    pub fn misses(&self) -> u64 {
        self.misses.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Clears every counter back to zero.
    pub fn reset(&self) {
        self.probes.store(0, std::sync::atomic::Ordering::Relaxed);
        self.hits.store(0, std::sync::atomic::Ordering::Relaxed);
        self.misses.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

impl DeltaSignatureIndex {
    /// Returns the compact-mode bucket key for `rsum`.
    ///
    /// The compact key is `rsum >> 16` (equal to
    /// [`checksums::RollingDigest::sum2`]); the lower 16 bits become the
    /// in-chain discriminator. Exposed so callers and tests can reason
    /// about bucket collisions without reaching into the private bucket
    /// table. Tables grown past upstream's `TRADITIONAL_TABLESIZE` address
    /// on the full rolling sum instead, so this is the key only while the
    /// table is in compact mode.
    #[inline]
    #[must_use]
    pub const fn bucket_for(rsum: u32) -> u16 {
        CompactLookup::bucket_for(rsum)
    }

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

    /// Returns the number of full-length basis blocks, excluding a trailing
    /// partial (short) block.
    ///
    /// The consecutive-match gated scan only matches full-length blocks, so it
    /// can never form a run of `>= 2` unless the basis holds at least two full
    /// blocks. A file that is one full block plus a short tail has
    /// `block_count() == 2` but only one matchable block; gating it would demote
    /// the lone match to a literal and re-send the whole file. Callers use this
    /// (not `block_count`) to decide whether gating is viable.
    #[must_use]
    pub fn full_block_count(&self) -> usize {
        self.blocks
            .iter()
            .filter(|b| b.len() == self.block_length)
            .count()
    }

    /// Returns the strong checksum length used by the signature.
    #[must_use]
    pub const fn strong_length(&self) -> usize {
        self.strong_length
    }

    /// Returns `true` when two or more indexed full-length blocks share
    /// identical content (equal rolling and strong checksums).
    ///
    /// A duplicate-free basis (`false`) is the precondition for engaging the
    /// parallel scan in [`crate::DeltaGenerator::generate_chunked`] without
    /// diverging from the sequential [`crate::DeltaGenerator::generate`]: with
    /// every content unique there is exactly one matching basis block per
    /// source window, so the per-stripe `want_i` hint cannot change which
    /// block a window resolves to. See the field docs on
    /// [`DeltaSignatureIndex`].
    #[must_use]
    pub const fn has_duplicate_blocks(&self) -> bool {
        self.has_duplicate_blocks
    }

    /// Returns the [`SignatureBlock`] for the provided index.
    #[inline]
    #[must_use]
    pub fn block(&self, index: usize) -> &SignatureBlock {
        &self.blocks[index]
    }

    /// Returns the sequential-match successor recorded for `block_index`, if any.
    ///
    /// The successor is the basis block that immediately followed
    /// `block_index` in source-file order during indexing. Source-sequential
    /// targets typically match the successor at the very next window offset,
    /// so callers can probe the successor directly via
    /// [`Self::try_next_match_slices`] and skip the rolling-rsum table
    /// lookup on the common path.
    ///
    /// Returns `None` for the trailing block, for any block index past the
    /// end of the basis, and for non-full-length blocks that were not
    /// linked during construction.
    ///
    /// upstream: zsync `librcksum/rsum.c:262` populates an equivalent
    /// `next_match` slot after every confirmed match.
    #[inline]
    #[must_use]
    pub fn next_match(&self, block_index: usize) -> Option<usize> {
        let raw = *self.next_match.get(block_index)?;
        if raw == NEXT_MATCH_NONE {
            None
        } else {
            Some(raw as usize)
        }
    }

    /// Probes the sequential-match successor of `last_match` against a
    /// contiguous target window without consulting the tag table, bithash,
    /// or compact lookup.
    ///
    /// Returns `Some(successor)` when the linked successor exists and its
    /// strong checksum matches `window`. Returns `None` when there is no
    /// recorded successor, the digest's `(sum1, sum2)` disagrees with the
    /// stored block, or the strong checksum verify fails. Callers MUST fall
    /// back to [`Self::find_match_bytes`] on a miss.
    ///
    /// The rolling-checksum prefilter is a cheap two-word compare against
    /// the linked block's stored digest, so a miss here costs the same as
    /// the existing `want_i` hint check in `match.c:144-190`.
    #[inline]
    pub fn try_next_match_bytes(
        &self,
        last_match: usize,
        digest: RollingDigest,
        window: &[u8],
    ) -> Option<usize> {
        let next = self.next_match(last_match)?;

        #[cfg(any(test, feature = "bench-internal"))]
        self.seq_match_counters
            .probes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        if window.len() != self.block_length {
            #[cfg(any(test, feature = "bench-internal"))]
            self.seq_match_counters
                .misses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return None;
        }
        let block = &self.blocks[next];
        let block_digest = block.rolling();
        if digest.sum1() != block_digest.sum1() || digest.sum2() != block_digest.sum2() {
            #[cfg(any(test, feature = "bench-internal"))]
            self.seq_match_counters
                .misses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return None;
        }
        let strong = self.algorithm.compute_truncated(window, self.strong_length);
        if strong.as_slice() == block.strong() {
            #[cfg(any(test, feature = "bench-internal"))]
            self.seq_match_counters
                .hits
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(next)
        } else {
            #[cfg(any(test, feature = "bench-internal"))]
            self.seq_match_counters
                .misses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            None
        }
    }

    /// Probes the sequential-match successor of `last_match` against a
    /// non-contiguous target window represented as two slices.
    ///
    /// Mirrors [`Self::try_next_match_bytes`] for the ring-buffer split
    /// form used by the generator.
    #[inline]
    pub fn try_next_match_slices(
        &self,
        last_match: usize,
        digest: RollingDigest,
        first: &[u8],
        second: &[u8],
    ) -> Option<usize> {
        let next = self.next_match(last_match)?;

        #[cfg(any(test, feature = "bench-internal"))]
        self.seq_match_counters
            .probes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        if first.len() + second.len() != self.block_length {
            #[cfg(any(test, feature = "bench-internal"))]
            self.seq_match_counters
                .misses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return None;
        }
        let block = &self.blocks[next];
        let block_digest = block.rolling();
        if digest.sum1() != block_digest.sum1() || digest.sum2() != block_digest.sum2() {
            #[cfg(any(test, feature = "bench-internal"))]
            self.seq_match_counters
                .misses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return None;
        }
        let strong = self
            .algorithm
            .compute_truncated_slices(first, second, self.strong_length);
        if strong.as_slice() == block.strong() {
            #[cfg(any(test, feature = "bench-internal"))]
            self.seq_match_counters
                .hits
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(next)
        } else {
            #[cfg(any(test, feature = "bench-internal"))]
            self.seq_match_counters
                .misses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            None
        }
    }

    /// Returns a shared handle to the test-only seq-match probe counters.
    ///
    /// Behind the same `cfg(any(test, feature = "bench-internal"))` gate
    /// that gates the counters themselves, so release builds never expose
    /// the accessor.
    #[cfg(any(test, feature = "bench-internal"))]
    #[must_use]
    pub fn seq_match_counters(&self) -> std::sync::Arc<SeqMatchCounters> {
        std::sync::Arc::clone(&self.seq_match_counters)
    }

    /// Attempts to locate a matching block for a contiguous byte slice.
    #[inline]
    pub fn find_match_bytes(&self, digest: RollingDigest, window: &[u8]) -> Option<usize> {
        self.probe_bytes(digest, window, None)
    }

    /// Shared probe core behind [`Self::find_match_bytes`] and the
    /// counted [`Self::find_match_window_counted`].
    ///
    /// When `counters` is `Some`, tallies upstream-style
    /// `hash_hits`/`false_alarms` (see [`ProbeCounters`]) as a side effect; the
    /// returned match index is identical either way, so the counted probe can
    /// never change which bytes the matcher reuses.
    #[inline]
    fn probe_bytes(
        &self,
        digest: RollingDigest,
        window: &[u8],
        mut counters: Option<&mut ProbeCounters>,
    ) -> Option<usize> {
        if window.len() != self.block_length {
            return None;
        }

        // upstream: match.c - tag_table[s1] fast-path rejects most non-matching
        // positions before the more expensive hash probe.
        if !self.tag_table[digest.sum1() as usize] {
            return None;
        }

        // zsync-style bithash prefilter: rejects ~7/8 of post-tag misses using
        // both sum halves before the hash-table probe. One-sided (no false
        // negatives) but it does admit false positives. A positive here is oc's
        // analogue of upstream's coarse hash-bucket hit (match.c:202
        // `hash_hits++`): it still has to pass the exact rolling-sum +
        // strong-sum verify below, and a positive that fails that verify is a
        // false alarm (match.c:239). oc counts bithash positives rather than
        // upstream's SUM2HASH bucket hits, so these two diagnostics are
        // oc-native - see [`ProbeCounters`].
        if !self.bithash.contains(digest.value()) {
            return None;
        }
        if let Some(counters) = counters.as_mut() {
            counters.hash_hits = counters.hash_hits.saturating_add(1);
        }

        let strong = self.algorithm.compute_truncated(window, self.strong_length);
        if let Some(index) = self.walk_chain(digest, strong.as_slice()) {
            return Some(index);
        }
        // A bithash positive that produced no confirmed match: the exact
        // rolling-sum / strong-sum verify rejected it. upstream: match.c:239
        // `false_alarms++`.
        if let Some(counters) = counters.as_mut() {
            counters.false_alarms = counters.false_alarms.saturating_add(1);
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
        self.walk_chain(digest, strong.as_slice())
    }

    /// Walks the equal-weak-checksum chain for `digest` and returns the first
    /// candidate whose strong checksum equals `strong`.
    ///
    /// Shared by the contiguous and two-slice probes so the chain-walk rule -
    /// upstream's chain order and the [`MAX_CHAIN_LEN`] bound - lives in one
    /// place. Every basis block stays on its chain for the whole scan, so a
    /// block matches as many source windows as carry its content.
    ///
    /// upstream: `match.c:229-256` `hash_search()` inner chain loop. The chain
    /// yields the highest block index first (see [`CompactLookup::insert`]),
    /// so among duplicate-content blocks the same sibling wins as upstream.
    #[inline]
    fn walk_chain(&self, digest: RollingDigest, strong: &[u8]) -> Option<usize> {
        let mut chain_len = 0u32;
        for index in self.lookup.find_all(digest.sum1(), digest.sum2()) {
            // upstream: `match.c:250` - bound the work spent on one pathological
            // bucket, charging every same-weak-sum candidate before deciding
            // whether it is a match. Past the cap the offset is a non-match and
            // the caller rolls forward a byte, so the skipped data goes out as
            // literals.
            chain_len += 1;
            if chain_len > MAX_CHAIN_LEN {
                return None;
            }
            let block = &self.blocks[index];
            debug_assert_eq!(block.len(), self.block_length);
            if strong == block.strong() {
                return Some(index);
            }
        }
        None
    }

    /// Attempts to match a short trailing window against a basis block of the
    /// same (partial) length.
    ///
    /// The full-length matchers reject any window whose length differs from
    /// `block_length`, and `populate_index` deliberately excludes partial
    /// blocks from the tag/bithash/lookup tables. That leaves the basis file's
    /// final short block unmatchable through the fast paths, so the sender
    /// would always emit the source file's trailing partial block as literal
    /// data. Upstream rsync matches it: `hash_search()` shrinks its final
    /// window to `l = MIN(blength, len-offset)` and requires `l == s->sums[i].len`
    /// (`match.c:222-224`), so a same-length short block still matches.
    ///
    /// This method mirrors that tail case. The combined length of `first` and
    /// `second` must be shorter than `block_length` (a full-length window
    /// belongs on the fast path). It scans for a basis block
    /// whose recorded length equals that combined length and whose rolling and
    /// strong checksums match, returning its index. Partial blocks only ever
    /// occur as the final block, so this scan touches at most one candidate in
    /// practice.
    ///
    /// # Why this bypasses the BitHash, tag table and compact lookup
    ///
    /// Do not "optimize" this onto the prefilter path - it would silently stop
    /// matching short blocks. All three structures are keyed on rolling sums
    /// computed over `block_length` bytes, while a short final block's rolling
    /// sum spans only `tail_len` bytes, so probing them with it is a category
    /// error rather than a slower path. `populate_index` deliberately never
    /// inserts a partial block, so the lookup cannot return one and the BitHash
    /// would reject it as a negative. The linear scan is the only way in.
    ///
    /// # Cost
    ///
    /// O(`block_count`), with a single length comparison for every block that
    /// is not the short one. Callers must invoke this **at most once per
    /// file**: the sender's EOF drain probes exactly one source offset
    /// (`len - tail_len`), because upstream's shrinking window admits no other
    /// position where a short block can match. Calling it per window would be
    /// quadratic.
    ///
    /// The scan is not narrowed to the last block even though the wire and
    /// local signature builders both put the only short block there. This is a
    /// public API over a [`signature::FileSignature`], and `SignatureBlock::from_raw_parts`
    /// lets a caller hand over blocks of any length in any order, so a
    /// last-block-only check would be an unsound assumption rather than an
    /// optimization.
    #[inline]
    pub fn find_tail_match(
        &self,
        digest: RollingDigest,
        first: &[u8],
        second: &[u8],
    ) -> Option<usize> {
        let tail_len = first.len() + second.len();
        if tail_len == 0 || tail_len >= self.block_length {
            return None;
        }

        let mut strong: Option<signature::DigestBuf> = None;
        for (index, block) in self.blocks.iter().enumerate() {
            if block.len() != tail_len {
                continue;
            }
            let block_digest = block.rolling();
            if digest.sum1() != block_digest.sum1() || digest.sum2() != block_digest.sum2() {
                continue;
            }
            let strong = strong.get_or_insert_with(|| {
                self.algorithm
                    .compute_truncated_slices(first, second, self.strong_length)
            });
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

    /// [`Self::find_match_window`] that also tallies upstream-style
    /// `hash_hits`/`false_alarms` into `counters` for the `-vv` `total:` line.
    ///
    /// upstream: `match.c:180-244 hash_search()` maintains the same two counters
    /// while scanning a file; the local-copy delta loop calls this in place of
    /// the uncounted probe so it can reproduce `match_report()` (`match.c:439`).
    pub fn find_match_window_counted(
        &self,
        digest: RollingDigest,
        window: &VecDeque<u8>,
        scratch: &mut Vec<u8>,
        counters: &mut ProbeCounters,
    ) -> Option<usize> {
        if window.len() != self.block_length {
            return None;
        }

        scratch.clear();
        let (front, back) = window.as_slices();
        scratch.extend_from_slice(front);
        scratch.extend_from_slice(back);
        self.probe_bytes(digest, scratch.as_slice(), Some(counters))
    }

    /// Attempts to match a short trailing [`VecDeque`] window against a basis
    /// block of the same partial length.
    ///
    /// The [`VecDeque`]-backed companion to [`Self::find_tail_match`], used by
    /// the local-copy delta matcher. `window` must be shorter than
    /// `block_length`; a full-length window belongs on
    /// [`Self::find_match_window`]. See [`Self::find_tail_match`] for the
    /// upstream reference (`match.c:222-224`).
    pub fn find_tail_match_window(
        &self,
        digest: RollingDigest,
        window: &VecDeque<u8>,
        scratch: &mut Vec<u8>,
    ) -> Option<usize> {
        let tail_len = window.len();
        if tail_len == 0 || tail_len >= self.block_length {
            return None;
        }

        scratch.clear();
        let (front, back) = window.as_slices();
        scratch.extend_from_slice(front);
        scratch.extend_from_slice(back);
        self.find_tail_match(digest, scratch.as_slice(), &[])
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
#[cfg(any(test, feature = "bench-internal"))]
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
    /// [`Self::find_match_bytes`], without the tag-table or
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

    /// Bucket-slot count of the underlying lookup table.
    ///
    /// A power of two at or below `2^16` while the ZSO-4 compact key
    /// addresses the table, and upstream's `(count/8) * 10 + 11`
    /// (`match.c:84`) once the basis grows past that. Bench harnesses use
    /// this to bin index sizes against the local CPU cache hierarchy
    /// (L1 / L2 / LLC / main memory).
    #[must_use]
    pub fn lookup_capacity(&self) -> usize {
        self.lookup.capacity()
    }

    /// Byte size of the bucket-array allocation backing the compact lookup.
    ///
    /// Reports the hot table only; chain-entry storage is excluded so the
    /// figure tracks the cache-resident slot array a probe touches first.
    /// Test- and bench-only: the enclosing `impl` block is gated behind
    /// `cfg(any(test, feature = "bench-internal"))`.
    #[must_use]
    pub fn lookup_bytes(&self) -> usize {
        self.lookup.bucket_bytes()
    }

    /// Drives the compact lookup probe directly, skipping the tag-table
    /// and bithash prefilters and the strong-checksum verify.
    ///
    /// Returns the number of basis block indices yielded by the
    /// `find_all` iterator at the requested `(sum1, sum2)` key. Bench
    /// harnesses use this to isolate the cache cost of the chain-based
    /// probe walk itself from the surrounding match pipeline.
    #[must_use]
    pub fn lookup_probe(&self, sum1: u16, sum2: u16) -> usize {
        self.lookup.find_all(sum1, sum2).count()
    }
}
