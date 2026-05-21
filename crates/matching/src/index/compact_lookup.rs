//! Compact bucket index keyed on the upper half of the rolling checksum.
//!
//! Translates zsync's `librcksum/hash.c` `rsum_a_mask` trick into oc-rsync's
//! delta matcher. The bucket address is derived from the upper 16 bits of the
//! rolling sum (`rsum >> 16`, equal to [`checksums::RollingDigest::sum2`])
//! while the lower 16 bits ([`checksums::RollingDigest::sum1`]) become the
//! in-bucket discriminator. Shrinking the bucket array from a `(sum1, sum2)`
//! keyspace down to a `sum2`-only space keeps the hottest table cache-line
//! resident even when the basis runs to tens of thousands of blocks.
//!
//! The structure is intentionally chain-based rather than open-addressed:
//!
//! - The bucket array is sized at most `2^16` slots, so its working set is
//!   bounded by 256 KiB regardless of basis size. Adjacent rolling-hash
//!   probes touch the same cache line with high probability.
//! - Chain nodes live in a packed `Vec<ChainEntry>`; entries for a single
//!   bucket are *not* required to be contiguous in memory, but the bucket
//!   array stays tight.
//! - Per-bucket walks check the lower-half discriminator first, mirroring
//!   zsync's `e.r.a != (r.a & rsum_a_mask)` filter in `librcksum/rsum.c:205`.
//!
//! Wire format is unchanged: full `(sum1, sum2)` digests stay in
//! [`signature::SignatureBlock`]. The compact key is an in-memory probe
//! optimisation only and is rebuilt per segment by
//! [`super::DeltaSignatureIndex::rebuild`].

/// Maximum bucket count exponent (`2^16 = 65 536`).
///
/// Bounds the bucket array footprint at `2^16 * 4 = 256 KiB` and matches
/// the natural `sum2` keyspace - never grow the table beyond this because
/// the compact key carries no further entropy.
const MAX_LOG2_BUCKETS: u32 = 16;

/// Minimum bucket count exponent (`2^4 = 16`).
///
/// Keeps the mask stable for tiny basis files. The chain handles collisions
/// regardless, so the floor is purely about preserving the `(pos - home)`
/// arithmetic shape.
const MIN_LOG2_BUCKETS: u32 = 4;

/// Sentinel marking the end of a bucket chain.
///
/// Real entry counts are bounded by `u32::MAX - 1` because the wire-format
/// block index is itself a `u32` and one slot is reserved for the sentinel.
const CHAIN_END: u32 = u32::MAX;

/// Conservative upper bound on chain-entry pre-allocation.
///
/// Caps the initial `Vec::with_capacity` request so callers passing absurd
/// `n_entries` (e.g., `usize::MAX` in fuzz inputs) cannot trigger a
/// capacity-overflow panic. The chain still grows on demand, so the cap
/// only affects the up-front reservation, not the maximum supported
/// basis size.
const MAX_RESERVE_ENTRIES: usize = 1 << 24;

/// Chain entry packing the lower-half discriminator next to the block index
/// and the link to the next entry in the same bucket.
///
/// Layout fits in 12 bytes (10 bytes payload + 2 bytes padding) so a single
/// chain step still loads from at most one cache line.
#[derive(Clone, Copy, Debug)]
struct ChainEntry {
    /// Lower-half discriminator ([`checksums::RollingDigest::sum1`]). Filters
    /// out same-bucket entries before the caller pays for the strong-checksum
    /// verify.
    sum1: u16,
    /// Basis block index this entry refers to.
    block_index: u32,
    /// Link to the next entry in the same bucket chain, or [`CHAIN_END`].
    next: u32,
}

/// Per-bucket head/tail pointers into the chain backing store.
///
/// Tracking the tail explicitly lets [`CompactLookup::insert`] append in
/// O(1) so the iteration order matches insertion order. The `MatchedBlocks`
/// duplicate-block contract relies on first-fit-in-bucket semantics, so
/// the natural insertion order must survive the rewrite.
#[derive(Clone, Copy, Debug)]
struct BucketSlot {
    head: u32,
    tail: u32,
}

impl BucketSlot {
    const EMPTY: Self = Self {
        head: CHAIN_END,
        tail: CHAIN_END,
    };
}

/// Compact bucket index keyed on the upper 16 bits of the rolling sum.
///
/// See the module docs for the ZSO-4 design contract and the duplicate-block
/// correctness rationale shared with [`super::MatchedBlocks`].
#[derive(Clone, Debug)]
pub(super) struct CompactLookup {
    buckets: Vec<BucketSlot>,
    entries: Vec<ChainEntry>,
    mask: u16,
}

impl CompactLookup {
    /// Derives the bucket address from the packed rolling sum.
    ///
    /// `rsum >> 16` is the upper half of the wire-format checksum and matches
    /// [`checksums::RollingDigest::sum2`], mirroring zsync's
    /// `r.a & rsum_a_mask` formulation while staying entirely in-memory.
    #[inline]
    #[must_use]
    pub(super) const fn bucket_for(rsum: u32) -> u16 {
        (rsum >> 16) as u16
    }

    /// Builds a bucket table sized for the expected number of entries.
    ///
    /// Bucket count is the smallest power of two `>= 2 * n_entries`, clamped
    /// to `[2^MIN_LOG2_BUCKETS, 2^MAX_LOG2_BUCKETS]`. The chain backing store
    /// is reserved at a conservative upper bound (`MAX_RESERVE_ENTRIES`) so
    /// adversarial inputs cannot trigger an oversized allocation up-front;
    /// real basis sizes never approach the cap before paging concerns kick
    /// in elsewhere.
    pub(super) fn with_capacity(n_entries: usize) -> Self {
        let log2_buckets = log2_buckets_for(n_entries);
        let n_buckets = 1usize << log2_buckets;
        let mask = (n_buckets - 1) as u16;
        let reserve = n_entries.min(MAX_RESERVE_ENTRIES);
        Self {
            buckets: vec![BucketSlot::EMPTY; n_buckets],
            entries: Vec::with_capacity(reserve),
            mask,
        }
    }

    /// Inserts a `(sum1, sum2) -> block_index` mapping.
    ///
    /// Entries are appended to the tail of the bucket chain so the iteration
    /// order matches insertion order. Preserving insertion order keeps the
    /// `MatchedBlocks` first-fit-in-bucket semantics intact: the matcher
    /// picks the earliest unmarked basis index when several blocks share a
    /// bucket and discriminator.
    pub(super) fn insert(&mut self, sum1: u16, sum2: u16, block_index: u32) {
        debug_assert_ne!(
            block_index, CHAIN_END,
            "block_index u32::MAX collides with chain sentinel",
        );
        let bucket = self.bucket_index(sum2);
        let entry_idx = self.entries.len() as u32;
        self.entries.push(ChainEntry {
            sum1,
            block_index,
            next: CHAIN_END,
        });

        let slot = self.buckets[bucket];
        if slot.head == CHAIN_END {
            self.buckets[bucket] = BucketSlot {
                head: entry_idx,
                tail: entry_idx,
            };
        } else {
            self.entries[slot.tail as usize].next = entry_idx;
            self.buckets[bucket].tail = entry_idx;
        }
    }

    /// Returns an iterator over all block indices matching `(sum1, sum2)`.
    ///
    /// Walks the `sum2`-derived bucket chain in insertion order and yields
    /// entries whose lower-half discriminator equals `sum1`. The
    /// strong-checksum verify still gates the final caller-visible match -
    /// this iterator only filters out chain entries that cannot possibly
    /// match.
    #[inline]
    pub(super) fn find_all(&self, sum1: u16, sum2: u16) -> CompactLookupIter<'_> {
        let bucket = self.bucket_index(sum2);
        CompactLookupIter {
            table: self,
            sum1,
            next: self.buckets[bucket].head,
        }
    }

    /// Masks `sum2` into the bucket-array address space.
    ///
    /// Equivalent to `sum2 & self.mask`, but kept as a single helper so the
    /// insert and lookup paths cannot drift apart.
    #[inline]
    fn bucket_index(&self, sum2: u16) -> usize {
        (sum2 & self.mask) as usize
    }

    /// Resets all bucket heads and chain entries, preserving the backing
    /// allocations for the next per-segment rebuild.
    pub(super) fn clear(&mut self) {
        self.buckets.fill(BucketSlot::EMPTY);
        self.entries.clear();
    }

    /// Returns the number of stored entries.
    #[allow(dead_code)]
    pub(super) fn len(&self) -> u32 {
        self.entries.len() as u32
    }

    /// Returns the number of bucket slots (always a power of two, `<= 2^16`).
    ///
    /// Reported as the bench harnesses' "lookup capacity" - the metric they
    /// pair against the local CPU cache hierarchy.
    pub(super) fn capacity(&self) -> usize {
        (self.mask as usize) + 1
    }

    /// Returns the byte footprint of the bucket array allocation.
    ///
    /// The chain backing store is excluded so the figure tracks the
    /// cache-resident hot table only. Exposed at crate-public visibility
    /// so [`DeltaSignatureIndex::lookup_bytes`] forwards a measurement
    /// helper that is reachable through the public API; `pub(super)`
    /// gave dead-code false positives in the binary-only build path
    /// because rustc cannot trace pub-to-restricted-pub call chains.
    pub fn bucket_bytes(&self) -> usize {
        self.buckets.len() * core::mem::size_of::<BucketSlot>()
    }
}

/// Iterator yielding chain entries that match a given discriminator.
pub(super) struct CompactLookupIter<'a> {
    table: &'a CompactLookup,
    sum1: u16,
    next: u32,
}

impl Iterator for CompactLookupIter<'_> {
    type Item = usize;

    #[inline]
    fn next(&mut self) -> Option<usize> {
        loop {
            if self.next == CHAIN_END {
                return None;
            }
            let entry = self.table.entries[self.next as usize];
            self.next = entry.next;
            if entry.sum1 == self.sum1 {
                return Some(entry.block_index as usize);
            }
        }
    }
}

/// Returns the bucket-count exponent for the requested entry count.
///
/// Picks the smallest `k` with `2^k >= 2 * n_entries`, then clamps into
/// `[MIN_LOG2_BUCKETS, MAX_LOG2_BUCKETS]`. The `2x` factor keeps the
/// per-bucket chain length bounded at roughly half a slot per entry on
/// average for uniformly distributed `sum2` values.
fn log2_buckets_for(n_entries: usize) -> u32 {
    let target = (n_entries as u64).saturating_mul(2);
    let raw = if target <= 1 {
        MIN_LOG2_BUCKETS
    } else {
        u64::BITS - (target - 1).leading_zeros()
    };
    raw.clamp(MIN_LOG2_BUCKETS, MAX_LOG2_BUCKETS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_find_single() {
        let mut table = CompactLookup::with_capacity(16);
        table.insert(100, 200, 42);
        let results: Vec<usize> = table.find_all(100, 200).collect();
        assert_eq!(results, vec![42]);
    }

    #[test]
    fn find_missing_returns_empty() {
        let mut table = CompactLookup::with_capacity(16);
        table.insert(100, 200, 42);
        let results: Vec<usize> = table.find_all(999, 999).collect();
        assert!(results.is_empty());
    }

    #[test]
    fn multiple_entries_same_key() {
        let mut table = CompactLookup::with_capacity(16);
        table.insert(10, 20, 0);
        table.insert(10, 20, 1);
        table.insert(10, 20, 2);
        let mut results: Vec<usize> = table.find_all(10, 20).collect();
        results.sort_unstable();
        assert_eq!(results, vec![0, 1, 2]);
    }

    #[test]
    fn distinct_keys_do_not_interfere() {
        let mut table = CompactLookup::with_capacity(64);
        for i in 0u16..20 {
            table.insert(i, i.wrapping_mul(7), i as u32);
        }
        for i in 0u16..20 {
            let results: Vec<usize> = table.find_all(i, i.wrapping_mul(7)).collect();
            assert_eq!(results, vec![i as usize]);
        }
    }

    #[test]
    fn clear_resets_table() {
        let mut table = CompactLookup::with_capacity(16);
        table.insert(1, 2, 3);
        assert_eq!(table.len(), 1);
        table.clear();
        assert_eq!(table.len(), 0);
        assert!(table.find_all(1, 2).next().is_none());
    }

    #[test]
    fn stress_many_entries() {
        let n = 10_000usize;
        let mut table = CompactLookup::with_capacity(n);
        for i in 0..n {
            let sum1 = (i & 0xFFFF) as u16;
            let sum2 = ((i >> 3) & 0xFFFF) as u16;
            table.insert(sum1, sum2, i as u32);
        }
        assert_eq!(table.len() as usize, n);

        for i in 0..n {
            let sum1 = (i & 0xFFFF) as u16;
            let sum2 = ((i >> 3) & 0xFFFF) as u16;
            let results: Vec<usize> = table.find_all(sum1, sum2).collect();
            assert!(results.contains(&i), "missing entry {i}");
        }
    }

    #[test]
    fn bucket_address_uses_upper_half() {
        let rsum: u32 = 0x1234_5678;
        assert_eq!(CompactLookup::bucket_for(rsum), 0x1234);
        assert_eq!(rsum >> 16, u32::from(CompactLookup::bucket_for(rsum)));
    }

    #[test]
    fn bucket_count_is_capped_at_two_to_sixteen() {
        let table = CompactLookup::with_capacity(usize::MAX);
        assert_eq!(table.capacity(), 1 << MAX_LOG2_BUCKETS);
        assert_eq!(table.mask, u16::MAX);
    }

    #[test]
    fn bucket_count_is_floored_for_tiny_inputs() {
        let table = CompactLookup::with_capacity(0);
        assert_eq!(table.capacity(), 1 << MIN_LOG2_BUCKETS);
    }

    #[test]
    fn lower_half_discriminator_filters_same_bucket() {
        // Two synthetic rsums sharing the upper-half bucket address but
        // disagreeing on the lower-half discriminator. The chain walk must
        // expose each entry under its own `(sum1, sum2)` key without leaking
        // the sibling.
        let mut table = CompactLookup::with_capacity(16);
        table.insert(0xAAAA, 0x1234, 7);
        table.insert(0xBBBB, 0x1234, 9);
        let results_a: Vec<usize> = table.find_all(0xAAAA, 0x1234).collect();
        let results_b: Vec<usize> = table.find_all(0xBBBB, 0x1234).collect();
        assert_eq!(results_a, vec![7]);
        assert_eq!(results_b, vec![9]);
    }
}
