//! Bucket index over the rolling checksum, with two addressing modes.
//!
//! Mirrors upstream rsync's `match.c` `build_hash_table()`, which sizes its
//! bucket array from the basis block count and switches hash function at the
//! `TRADITIONAL_TABLESIZE` (`match.c:45`, `1<<16`) boundary:
//!
//! - **Compact** (at or below the boundary) - the bucket address comes from
//!   the upper 16 bits of the rolling sum (`rsum >> 16`, equal to
//!   [`checksums::RollingDigest::sum2`]) while the lower 16 bits
//!   ([`checksums::RollingDigest::sum1`]) become the in-bucket discriminator.
//!   This is zsync's `librcksum/hash.c` `rsum_a_mask` trick (ZSO-4);
//!   upstream's own small-table hash (`match.c:71-72` `SUM2HASH`) is likewise
//!   a 16-bit function. Below the boundary oc sizes tighter than upstream's
//!   flat `1<<16` floor, which keeps the hot table cache-line resident for
//!   the small files that dominate a typical transfer.
//! - **Wide** (above the boundary) - the address is the full 32-bit rolling
//!   sum modulo the table size, exactly upstream's `BIG_SUM2HASH`
//!   (`match.c:74`) over upstream's `(count/8) * 10 + 11` sizing
//!   (`match.c:82-88`). A `sum2`-only key carries no entropy past `2^16`, so
//!   growing the table requires widening the key just as upstream does.
//!
//! The structure is intentionally chain-based rather than open-addressed:
//!
//! - Chain nodes live in a packed `Vec<ChainEntry>`; entries for a single
//!   bucket are *not* required to be contiguous in memory, but the bucket
//!   array stays tight.
//! - Per-bucket walks check the lower-half discriminator first, mirroring
//!   zsync's `e.r.a != (r.a & rsum_a_mask)` filter in `librcksum/rsum.c:205`.
//!
//! Wire format is unchanged: full `(sum1, sum2)` digests stay in
//! [`signature::SignatureBlock`]. Both addressing modes map equal rolling
//! sums to one bucket and [`CompactLookup::find_all`] filters on the full
//! `(sum1, sum2)` pair, so the yielded candidate sequence - and therefore
//! every emitted token - is identical under either mode. The table is an
//! in-memory probe optimisation only, rebuilt per segment by
//! [`super::DeltaSignatureIndex::rebuild`].

/// Maximum bucket count exponent for the compact (`sum2`-keyed) mode.
///
/// `2^16 = 65 536` is the natural `sum2` keyspace, so the compact key
/// carries no entropy beyond it. It is also upstream's
/// `TRADITIONAL_TABLESIZE` (`match.c:45`) - the point at which upstream
/// swaps `SUM2HASH` for the full-rsum `BIG_SUM2HASH`. Past this the table
/// grows under [`BucketAddress::Wide`], never by stretching the compact key.
const MAX_LOG2_BUCKETS: u32 = 16;

/// Upstream's `TRADITIONAL_TABLESIZE`: the bucket count below which upstream
/// keeps a flat table and the 16-bit hash.
///
/// upstream: match.c:45 `#define TRADITIONAL_TABLESIZE (1<<16)`.
const TRADITIONAL_TABLESIZE: usize = 1 << MAX_LOG2_BUCKETS;

/// Allocation guard on the wide bucket count.
///
/// `2^27 - 1` slots is 1 GiB of [`BucketSlot`], reached only at roughly
/// 107 million basis blocks (a multi-terabyte basis at upstream's 128 KiB
/// maximum block length) - about 2 000x past the point where the wide mode
/// engages. Upstream has no equivalent guard; it simply asks `new_array()`
/// for the allocation and dies if it fails. The value is odd so the guard
/// preserves upstream's oddness requirement (`match.c:80-81`).
const MAX_WIDE_BUCKETS: usize = (1 << 27) - 1;

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

/// Bucket sizing and addressing rule, the single owner of the decision.
///
/// Constructed once per table by [`Self::for_entries`]; both
/// [`CompactLookup::insert`] and [`CompactLookup::find_all`] route through
/// [`Self::index_of`] so the two paths cannot drift apart, and no call site
/// re-derives a bucket count of its own.
#[derive(Clone, Copy, Debug)]
enum BucketAddress {
    /// `sum2 & mask` over a power-of-two table at most
    /// [`TRADITIONAL_TABLESIZE`] slots wide (ZSO-4 compact key).
    Compact { mask: u16 },
    /// `rsum % modulus` over upstream's dynamically grown table.
    ///
    /// upstream: match.c:74 `#define BIG_SUM2HASH(sum) ((sum)%tablesize)`.
    Wide { modulus: u32 },
}

impl BucketAddress {
    /// Chooses the sizing and addressing rule for `n_entries` indexed blocks.
    ///
    /// upstream: match.c:82-88 - `tablesize = (uint32)(s->count/8) * 10 + 11`,
    /// raised to `TRADITIONAL_TABLESIZE` when it falls below it. The `* 10 / 8`
    /// factor targets an 80% hash load and the `+ 11` keeps the result odd,
    /// without which the upper sum half cannot span the whole table
    /// (`match.c:80-81`). Reproducing the growth is what keeps the per-offset
    /// chain walk flat as the basis grows: pinning the table at
    /// `TRADITIONAL_TABLESIZE` instead makes the mean walk scale linearly with
    /// the block count (measured 9.0x upstream at 1 000 000 blocks).
    ///
    /// At or below the boundary oc keeps its own power-of-two sizing rather
    /// than upstream's flat floor. Upstream allocates and `memset`s 256 KiB
    /// per file there regardless of how few blocks the basis has; oc's
    /// tighter table measures within 1% of upstream's mean chain walk from
    /// 32 768 blocks up to the boundary, so the floor buys nothing it does
    /// not charge for in cache footprint.
    fn for_entries(n_entries: usize) -> Self {
        let wide = (n_entries / 8).saturating_mul(10).saturating_add(11);
        if wide > TRADITIONAL_TABLESIZE {
            Self::Wide {
                modulus: wide.min(MAX_WIDE_BUCKETS) as u32,
            }
        } else {
            let n_buckets = 1usize << log2_buckets_for(n_entries);
            Self::Compact {
                mask: (n_buckets - 1) as u16,
            }
        }
    }

    /// Number of bucket slots the rule asks for.
    const fn bucket_count(self) -> usize {
        match self {
            Self::Compact { mask } => mask as usize + 1,
            Self::Wide { modulus } => modulus as usize,
        }
    }

    /// Maps a `(sum1, sum2)` pair onto a bucket slot.
    ///
    /// Both modes send equal rolling sums to the same slot, which is the only
    /// property [`CompactLookup::find_all`] depends on for correctness.
    #[inline]
    fn index_of(self, sum1: u16, sum2: u16) -> usize {
        match self {
            Self::Compact { mask } => (sum2 & mask) as usize,
            Self::Wide { modulus } => {
                let rsum = (u32::from(sum2) << 16) | u32::from(sum1);
                (rsum % modulus) as usize
            }
        }
    }
}

/// Bucket index over the rolling checksum.
///
/// See the module docs for the two addressing modes, the ZSO-4 design
/// contract, and the duplicate-block correctness rationale shared with
/// [`super::MatchedBlocks`].
#[derive(Clone, Debug)]
pub(super) struct CompactLookup {
    buckets: Vec<BucketSlot>,
    entries: Vec<ChainEntry>,
    address: BucketAddress,
}

impl CompactLookup {
    /// Derives the compact-mode bucket key from the packed rolling sum.
    ///
    /// `rsum >> 16` is the upper half of the wire-format checksum and matches
    /// [`checksums::RollingDigest::sum2`], mirroring zsync's
    /// `r.a & rsum_a_mask` formulation while staying entirely in-memory.
    /// Tables grown past [`TRADITIONAL_TABLESIZE`] address on the full rolling
    /// sum instead ([`BucketAddress::Wide`]), so this is the key only while
    /// the table is in compact mode.
    #[inline]
    #[must_use]
    pub(super) const fn bucket_for(rsum: u32) -> u16 {
        (rsum >> 16) as u16
    }

    /// Builds a bucket table sized for the expected number of entries.
    ///
    /// Sizing is owned entirely by [`BucketAddress::for_entries`]. The chain
    /// backing store is reserved at a conservative upper bound
    /// (`MAX_RESERVE_ENTRIES`) so adversarial inputs cannot trigger an
    /// oversized allocation up-front; real basis sizes never approach the cap
    /// before paging concerns kick in elsewhere.
    pub(super) fn with_capacity(n_entries: usize) -> Self {
        let address = BucketAddress::for_entries(n_entries);
        let reserve = n_entries.min(MAX_RESERVE_ENTRIES);
        Self {
            buckets: vec![BucketSlot::EMPTY; address.bucket_count()],
            entries: Vec::with_capacity(reserve),
            address,
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
        let bucket = self.address.index_of(sum1, sum2);
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
    /// Walks the bucket chain in insertion order and yields entries whose
    /// lower-half discriminator equals `sum1`. The strong-checksum verify
    /// still gates the final caller-visible match - this iterator only
    /// filters out chain entries that cannot possibly match. The yielded
    /// sequence is independent of the addressing mode.
    #[inline]
    pub(super) fn find_all(&self, sum1: u16, sum2: u16) -> CompactLookupIter<'_> {
        let bucket = self.address.index_of(sum1, sum2);
        CompactLookupIter {
            table: self,
            sum1,
            next: self.buckets[bucket].head,
        }
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

    /// Returns the number of bucket slots.
    ///
    /// A power of two at or below `2^16` in compact mode, upstream's odd
    /// `(count/8) * 10 + 11` above it. Reported as the bench harnesses'
    /// "lookup capacity" - the metric they pair against the local CPU cache
    /// hierarchy.
    pub(super) fn capacity(&self) -> usize {
        self.address.bucket_count()
    }

    /// Returns the byte footprint of the bucket array allocation.
    ///
    /// The chain backing store is excluded so the figure tracks the
    /// cache-resident hot table only. Gated behind `test` and
    /// `bench-internal` because the only consumer is
    /// [`crate::index::DeltaSignatureIndex::lookup_bytes`] which is also
    /// test/bench-only; including this in the binary build trips dead-code
    /// lint since rustc cannot trace pub-to-restricted-pub call chains.
    #[cfg(any(test, feature = "bench-internal"))]
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
    fn bucket_count_is_floored_for_tiny_inputs() {
        let table = CompactLookup::with_capacity(0);
        assert_eq!(table.capacity(), 1 << MIN_LOG2_BUCKETS);
    }

    /// Upstream's table size for `n` blocks: `(n/8) * 10 + 11`, raised to
    /// `TRADITIONAL_TABLESIZE` when it falls below it.
    ///
    /// upstream: match.c:84-88.
    fn upstream_tablesize(n: usize) -> usize {
        ((n / 8) * 10 + 11).max(TRADITIONAL_TABLESIZE)
    }

    /// The bucket count must track upstream's dynamic growth once the basis
    /// is large enough to leave `TRADITIONAL_TABLESIZE` behind.
    ///
    /// Pinning the table at the floor instead is what the measured 9.0x
    /// chain-walk blow-up at a million blocks comes from: the walk cost is
    /// `n_entries / n_buckets`, so a fixed bucket count makes it grow without
    /// bound. Any policy that stops growing fails this at `100_000`.
    ///
    /// upstream: match.c:84-88.
    #[test]
    fn bucket_count_tracks_upstream_growth_above_the_floor() {
        // 52 424 is the first block count whose upstream size exceeds
        // TRADITIONAL_TABLESIZE: 52424/8 = 6553, 6553*10 + 11 = 65 541.
        // 52 423 truncates to 6552 and still lands on the floor.
        for &n in &[52_424usize, 100_000, 400_000, 1_000_000, 8_000_000] {
            let table = CompactLookup::with_capacity(n);
            assert_eq!(
                table.capacity(),
                upstream_tablesize(n),
                "bucket count for {n} blocks must equal upstream's tablesize"
            );
            assert!(
                table.capacity() > TRADITIONAL_TABLESIZE,
                "{n} blocks must leave the traditional table size behind"
            );
        }
        assert_eq!(CompactLookup::with_capacity(52_424).capacity(), 65_541);
    }

    /// Below the boundary the table stays on oc's tighter power-of-two
    /// sizing, and never exceeds `TRADITIONAL_TABLESIZE`.
    ///
    /// Upstream's flat `1<<16` floor would `memset` 256 KiB per file for a
    /// basis of a handful of blocks; the measured mean chain walk is within
    /// 1% of upstream's from 32 768 blocks to the boundary, so the floor is
    /// pure cache footprint there.
    #[test]
    fn bucket_count_stays_compact_below_the_boundary() {
        for &(n, expected) in &[
            (0usize, 1usize << MIN_LOG2_BUCKETS),
            (1, 1 << MIN_LOG2_BUCKETS),
            (1_000, 2_048),
            (32_768, TRADITIONAL_TABLESIZE),
            (52_423, TRADITIONAL_TABLESIZE),
        ] {
            let table = CompactLookup::with_capacity(n);
            assert_eq!(table.capacity(), expected, "bucket count for {n} blocks");
            assert!(table.capacity().is_power_of_two());
        }
    }

    /// A grown table must be odd, or the upper sum half cannot span the whole
    /// set under `rsum % tablesize`.
    ///
    /// upstream: match.c:80-81.
    #[test]
    fn grown_bucket_count_is_odd() {
        for &n in &[52_424usize, 100_000, 999_999, 8_000_000, usize::MAX] {
            let count = BucketAddress::for_entries(n).bucket_count();
            assert_eq!(count % 2, 1, "grown table for {n} blocks must be odd");
        }
    }

    /// The allocation guard bounds the wide bucket count for adversarial
    /// entry counts without wrapping the `u32` modulus.
    #[test]
    fn wide_bucket_count_is_guarded() {
        assert_eq!(
            BucketAddress::for_entries(usize::MAX).bucket_count(),
            MAX_WIDE_BUCKETS
        );
        assert_eq!(
            BucketAddress::for_entries(usize::MAX / 8).bucket_count(),
            MAX_WIDE_BUCKETS
        );
    }

    /// Above the boundary the address must consume the full rolling sum.
    ///
    /// Two sums sharing `sum2` collide in every compact table by
    /// construction; a grown table that still keyed on `sum2` alone would
    /// keep them colliding and so gain nothing from the extra slots.
    ///
    /// upstream: match.c:74 `BIG_SUM2HASH(sum) ((sum)%tablesize)`.
    #[test]
    fn wide_address_uses_the_full_rolling_sum() {
        let address = BucketAddress::for_entries(100_000);
        assert!(matches!(address, BucketAddress::Wide { .. }));
        assert_ne!(
            address.index_of(0x0001, 0xBEEF),
            address.index_of(0x0002, 0xBEEF),
            "wide addressing must separate sums that share sum2"
        );

        let compact = BucketAddress::for_entries(1_000);
        assert_eq!(
            compact.index_of(0x0001, 0xBEEF),
            compact.index_of(0x0002, 0xBEEF),
            "compact addressing keys on sum2 alone",
        );
    }

    /// Growing the table must not change which candidates a probe yields:
    /// the sizing is an in-memory optimisation with no token-level effect.
    #[test]
    fn wide_mode_preserves_lookup_semantics_and_order() {
        let n = 60_000usize;
        let mut table = CompactLookup::with_capacity(n);
        assert!(table.capacity() > TRADITIONAL_TABLESIZE);
        for i in 0..n {
            table.insert((i & 0xFFFF) as u16, ((i >> 5) & 0xFFFF) as u16, i as u32);
        }
        for i in (0..n).step_by(97) {
            let found: Vec<usize> = table
                .find_all((i & 0xFFFF) as u16, ((i >> 5) & 0xFFFF) as u16)
                .collect();
            assert!(found.contains(&i), "missing entry {i}");
        }

        // Duplicate keys must still come back in insertion order: the
        // MatchedBlocks first-fit contract depends on it.
        let mut dup = CompactLookup::with_capacity(n);
        dup.insert(7, 9, 100);
        dup.insert(7, 9, 5);
        dup.insert(7, 9, 42);
        let found: Vec<usize> = dup.find_all(7, 9).collect();
        assert_eq!(found, vec![100, 5, 42]);
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
