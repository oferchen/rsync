//! Cache-friendly flat hash table replacing `FxHashMap<(u16, u16), Vec<usize>>`.
//!
//! Implements open-addressing with linear probing and Robin Hood displacement.
//! Each entry is 8 bytes (4-byte packed rsum key + 4-byte block index), fitting
//! 8 entries per 64-byte cache line. This replaces the pointer-chasing
//! `FxHashMap` + heap-allocated `Vec<usize>` with a single contiguous allocation.
//!
//! zsync's `librcksum/hash.c` uses a similar flat layout; this module adapts
//! the technique for oc-rsync's `DeltaSignatureIndex`.

/// Sentinel value indicating an empty slot.
const EMPTY_KEY: u32 = u32::MAX;

/// Packs `(sum1, sum2)` into a single `u32` key.
///
/// Uses `(sum2 << 16) | sum1` which matches `RollingDigest::value()` layout.
/// Reserves `u32::MAX` as the empty sentinel - the probability of a real rsum
/// hitting `0xFFFF_FFFF` is negligible (1 in 4 billion), and the tag_table +
/// bithash filters would catch it first.
#[inline]
fn pack_key(sum1: u16, sum2: u16) -> u32 {
    (sum2 as u32) << 16 | sum1 as u32
}

/// Flat open-addressing hash table with Robin Hood linear probing.
///
/// Stores `(key: u32, block_index: u32)` entries in a contiguous `Vec<u64>`.
/// The high 32 bits of each `u64` hold the packed rsum key; the low 32 bits
/// hold the block index. Empty slots use [`EMPTY_KEY`] in the high bits.
///
/// Load factor is kept below 75% to maintain probe-chain locality. All
/// operations are O(1) amortized with excellent cache behavior: the linear
/// probe window typically stays within 1-2 cache lines.
#[derive(Clone, Debug)]
pub(super) struct CompactLookup {
    slots: Vec<u64>,
    mask: u32,
    len: u32,
}

impl CompactLookup {
    /// Builds a table sized for the expected number of entries.
    ///
    /// Capacity is the next power of two >= `4 * n_entries` (25% target load),
    /// with a minimum of 16 slots.
    pub(super) fn with_capacity(n_entries: usize) -> Self {
        let min_cap = (n_entries as u64)
            .saturating_mul(4)
            .next_power_of_two()
            .max(16) as usize;
        let cap = min_cap.min(1 << 30);
        let mask = (cap - 1) as u32;
        Self {
            slots: vec![Self::empty_slot(); cap],
            mask,
            len: 0,
        }
    }

    /// Inserts a `(sum1, sum2) -> block_index` mapping.
    ///
    /// Uses Robin Hood insertion: if a new entry's probe distance exceeds the
    /// incumbent's, swap them and continue inserting the displaced entry. This
    /// bounds the variance of probe-chain lengths, keeping worst-case lookups
    /// short.
    pub(super) fn insert(&mut self, sum1: u16, sum2: u16, block_index: u32) {
        let key = pack_key(sum1, sum2);
        debug_assert_ne!(key, EMPTY_KEY, "rsum 0xFFFFFFFF collides with sentinel");

        let mut pos = (key & self.mask) as usize;
        let mut inserting_key = key;
        let mut inserting_val = block_index;
        let mut inserting_dist: u32 = 0;

        loop {
            let slot = self.slots[pos];
            let slot_key = (slot >> 32) as u32;

            if slot_key == EMPTY_KEY {
                self.slots[pos] = Self::pack_slot(inserting_key, inserting_val);
                self.len += 1;
                return;
            }

            let slot_dist = self.probe_distance(slot_key, pos);
            if inserting_dist > slot_dist {
                let slot_val = slot as u32;
                self.slots[pos] = Self::pack_slot(inserting_key, inserting_val);
                inserting_key = slot_key;
                inserting_val = slot_val;
                inserting_dist = slot_dist;
            }

            pos = ((pos + 1) as u32 & self.mask) as usize;
            inserting_dist += 1;
        }
    }

    /// Returns an iterator over all block indices matching `(sum1, sum2)`.
    ///
    /// The iterator walks the probe chain from the key's home position,
    /// yielding every entry with a matching key. It terminates when it
    /// encounters an empty slot or an entry whose probe distance is less than
    /// the current scan distance (Robin Hood invariant: no matching entry can
    /// exist beyond this point).
    #[inline]
    pub(super) fn find_all(&self, sum1: u16, sum2: u16) -> CompactLookupIter<'_> {
        let key = pack_key(sum1, sum2);
        let start = (key & self.mask) as usize;
        CompactLookupIter {
            table: self,
            key,
            pos: start,
            dist: 0,
        }
    }

    /// Resets all slots to empty, preserving the backing allocation.
    pub(super) fn clear(&mut self) {
        self.slots.fill(Self::empty_slot());
        self.len = 0;
    }

    /// Returns the number of stored entries.
    #[allow(dead_code)]
    pub(super) fn len(&self) -> u32 {
        self.len
    }

    /// Returns the total number of slots (always a power of two).
    pub(super) fn capacity(&self) -> usize {
        (self.mask as usize) + 1
    }

    #[inline]
    fn empty_slot() -> u64 {
        (EMPTY_KEY as u64) << 32
    }

    #[inline]
    fn pack_slot(key: u32, value: u32) -> u64 {
        ((key as u64) << 32) | value as u64
    }

    #[inline]
    fn probe_distance(&self, key: u32, pos: usize) -> u32 {
        let home = (key & self.mask) as usize;
        ((pos as u32).wrapping_sub(home as u32)) & self.mask
    }
}

/// Iterator over block indices matching a specific rsum key.
pub(super) struct CompactLookupIter<'a> {
    table: &'a CompactLookup,
    key: u32,
    pos: usize,
    dist: u32,
}

impl Iterator for CompactLookupIter<'_> {
    type Item = usize;

    #[inline]
    fn next(&mut self) -> Option<usize> {
        loop {
            let slot = self.table.slots[self.pos];
            let slot_key = (slot >> 32) as u32;

            if slot_key == EMPTY_KEY {
                return None;
            }

            let slot_dist = self.table.probe_distance(slot_key, self.pos);
            if slot_dist < self.dist {
                return None;
            }

            self.pos = ((self.pos + 1) as u32 & self.table.mask) as usize;
            self.dist += 1;

            if slot_key == self.key {
                return Some(slot as u32 as usize);
            }
        }
    }
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
    fn probe_distance_wraps_correctly() {
        let table = CompactLookup::with_capacity(4);
        assert_eq!(table.mask, 15);
        assert_eq!(table.probe_distance(0, 0), 0);
        assert_eq!(table.probe_distance(0, 3), 3);
        assert_eq!(table.probe_distance(14, 1), 3);
    }

    #[test]
    fn pack_key_matches_rolling_digest_value() {
        let sum1: u16 = 0x1234;
        let sum2: u16 = 0x5678;
        let packed = pack_key(sum1, sum2);
        assert_eq!(packed, 0x5678_1234);
        assert_eq!(packed & 0xFFFF, sum1 as u32);
        assert_eq!(packed >> 16, sum2 as u32);
    }
}
