//! Bithash prefilter for the [`DeltaSignatureIndex`].
//!
//! Translates zsync's `librcksum` bithash mechanism (see
//! `librcksum/hash.c:101-102` and `librcksum/rsum.c:362-366` in zsync 0.6.2)
//! into oc-rsync's existing index. The bithash is an oversized one-sided bit
//! filter that rejects ~7/8 of non-matching rolling-hash probes in O(1)
//! before the strong-checksum hash-table walk.
//!
//! The structure is built once per signature, populated alongside the
//! [`super::DeltaSignatureIndex::tag_table`], and probed alongside it on every
//! rolling-hash advance. It never leaves the receiver's memory; no wire,
//! capability, or golden-byte surface is affected.
//!
//! Sizing follows the design note `docs/design/zsync-bithash.md`:
//!
//! - Bucket count `2^i` is chosen so that `2^i >= 4 * N` for `N` blocks,
//!   clamped at a `2^7` minimum to keep the mask stable for tiny basis files.
//! - Bit count is `2^(i + BITHASH_BITS)`, i.e. 8 times the bucket count
//!   (zsync's `BITHASHBITS = 3`), giving the canonical 1/8 set-bit density
//!   and the matching 7/8 reject rate on uniform misses.
//! - The bit count is capped at `2^28` (32 MiB of bits, 4 MiB of bytes) to
//!   bound memory at adversarial input sizes per the parent design.

/// Logarithm of the per-bucket bit budget.
///
/// Mirrors zsync's `BITHASHBITS` macro from `librcksum/internal.h:83`.
/// Each `2^i` bucket of the rsum hash table is paired with `2^BITHASH_BITS
/// = 8` bithash bits, giving a 1/8 set-bit density at saturation.
pub(super) const BITHASH_BITS: u32 = 3;

/// Minimum bithash size in bits, expressed as a power-of-two exponent.
///
/// `2^7 = 128` bits keeps the mask stable for very small basis files and
/// matches the 1 KB lower bound discussed in `docs/design/zsync-bithash.md`.
const MIN_LOG2_BITS: u32 = 7;

/// Maximum bithash size in bits, expressed as a power-of-two exponent.
///
/// `2^28 = 256 Mibits = 32 MiB`, capping memory at the parent design's
/// adversarial-input ceiling. Beyond this, the bithash starts costing more
/// in cache misses than it saves in hash-table probes.
const MAX_LOG2_BITS: u32 = 28;

/// One-sided rolling-checksum prefilter mirroring zsync's bithash.
///
/// Sized to roughly `8 * 4 * N` bits for `N` indexed blocks (the `8x` factor
/// is `2^BITHASH_BITS`, the `4x` factor is the standard rsum-bucket
/// over-allocation). At saturation the array carries one set bit per indexed
/// block (1/8 density), so a uniform-random rsum probe rejects with
/// probability ~7/8 in a single masked load.
///
/// # Invariants
///
/// - [`Self::insert`] only ever sets bits; it never clears them.
/// - [`Self::contains`] returns `true` for every rsum that was previously
///   passed to [`Self::insert`]: the filter is one-sided, so missed matches
///   are impossible by construction.
/// - [`Self::clear`] resets the bit array to all zeros without releasing the
///   backing allocation, mirroring [`super::DeltaSignatureIndex::rebuild`]'s
///   reuse of the tag table.
#[derive(Clone, Debug)]
pub(super) struct BitHash {
    bits: Vec<u64>,
    mask: u32,
}

impl BitHash {
    /// Builds a bithash sized for the requested block count.
    ///
    /// The bit count is the smallest power of two `>= 32 * n_blocks`,
    /// clamped to `[2^MIN_LOG2_BITS, 2^MAX_LOG2_BITS]`. The result is always
    /// a multiple of 64 bits, so the [`u64`] backing store has no tail
    /// padding to special-case.
    pub(super) fn with_block_count(n_blocks: usize) -> Self {
        let log2_bits = log2_bits_for(n_blocks);
        let total_bits = 1usize << log2_bits;
        let words = total_bits / u64::BITS as usize;
        let mask = ((total_bits - 1) as u64 & u32::MAX as u64) as u32;
        Self {
            bits: vec![0u64; words],
            mask,
        }
    }

    /// Records the presence of a block with the given packed rolling sum.
    ///
    /// `rsum` is the 32-bit value returned by
    /// [`checksums::RollingDigest::value`]. Mirrors zsync's
    /// `bithash[(h & bithashmask) >> 3] |= 1 << (h & 7)`.
    #[inline]
    pub(super) fn insert(&mut self, rsum: u32) {
        let (word_index, bit_index) = self.locate(rsum);
        self.bits[word_index] |= 1u64 << bit_index;
    }

    /// Reports whether the given rolling sum may correspond to an indexed block.
    ///
    /// Returns `true` for every previously inserted rsum (no false
    /// negatives). May return `true` for rsums that were never inserted
    /// (false positives), but at the design density bound these account for
    /// roughly 1/8 of uniform-random probes.
    #[inline]
    pub(super) fn contains(&self, rsum: u32) -> bool {
        let (word_index, bit_index) = self.locate(rsum);
        (self.bits[word_index] >> bit_index) & 1 == 1
    }

    /// Resets every bit, preserving the backing allocation.
    ///
    /// Used by [`super::DeltaSignatureIndex::rebuild`] to recycle the bithash
    /// across per-segment INC_RECURSE rebuilds without re-allocating.
    #[inline]
    pub(super) fn clear(&mut self) {
        for word in &mut self.bits {
            *word = 0;
        }
    }

    /// Returns the fraction of bits currently set, in `[0.0, 1.0]`.
    ///
    /// Useful for the `bench-internal` rejection-rate harness described in
    /// `docs/design/zsync-bithash.md` section 7. Cheap on small bithashes,
    /// linear in the bit count for large ones; not on any hot path. The
    /// `#[allow(dead_code)]` keeps default release builds quiet: the
    /// accessor is referenced only by the `bench-internal`-gated
    /// `DeltaSignatureIndex::bithash_utilization` in `index/mod.rs` and
    /// by the unit tests in `bithash_tests.rs`.
    #[allow(dead_code)]
    pub(super) fn utilization(&self) -> f64 {
        let set: u32 = self.bits.iter().map(|w| w.count_ones()).sum();
        let total = (self.bits.len() as u64) * u64::BITS as u64;
        if total == 0 {
            0.0
        } else {
            f64::from(set) / total as f64
        }
    }

    /// Decomposes an rsum into `(word_index, bit_within_word)`.
    ///
    /// The low 6 bits of the masked rsum select the bit inside the 64-bit
    /// word; the next bits up to `log2(total_bits)` select the word. This is
    /// the natural extension of zsync's byte/bit decomposition to a `u64`
    /// backing store, and it preserves the property that the low 3 bits of
    /// `rsum` drive the in-byte bit selection.
    #[inline]
    fn locate(&self, rsum: u32) -> (usize, u32) {
        let masked = (rsum & self.mask) as usize;
        let bit_index = (masked & (u64::BITS as usize - 1)) as u32;
        let word_index = masked >> 6;
        (word_index, bit_index)
    }
}

/// Returns the `log2` of the chosen bit-array size for a block count.
///
/// Picks the smallest exponent `k` with `2^k >= 32 * n_blocks` (the `4x`
/// bucket factor times zsync's `8x` bithash factor), then clamps into
/// `[MIN_LOG2_BITS, MAX_LOG2_BITS]`.
fn log2_bits_for(n_blocks: usize) -> u32 {
    let target_bits = (n_blocks as u64).saturating_mul(1u64 << (BITHASH_BITS + 2));
    let log2 = if target_bits <= 1 {
        MIN_LOG2_BITS
    } else {
        // Smallest k with 2^k >= target_bits is `64 - leading_zeros(target_bits - 1)`.
        let raw = u64::BITS - (target_bits - 1).leading_zeros();
        raw.max(MIN_LOG2_BITS)
    };
    log2.min(MAX_LOG2_BITS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_size_is_clamped() {
        let bh = BitHash::with_block_count(0);
        assert_eq!(bh.bits.len(), 1usize << (MIN_LOG2_BITS - 6));
        assert_eq!(bh.mask as u64, (1u64 << MIN_LOG2_BITS) - 1);
    }

    #[test]
    fn max_size_is_capped() {
        let bh = BitHash::with_block_count(usize::MAX);
        assert_eq!(bh.bits.len(), 1usize << (MAX_LOG2_BITS - 6));
        assert_eq!(bh.mask as u64, (1u64 << MAX_LOG2_BITS) - 1);
    }

    #[test]
    fn insert_then_contains_round_trip() {
        let mut bh = BitHash::with_block_count(1024);
        for rsum in [0u32, 1, 0xDEAD_BEEF, 0xFFFF_FFFF, 0xBC00_F800] {
            bh.insert(rsum);
            assert!(bh.contains(rsum), "rsum {rsum:#x} should be present");
        }
    }

    #[test]
    fn clear_resets_all_bits() {
        let mut bh = BitHash::with_block_count(1024);
        bh.insert(42);
        bh.insert(0xCAFEBABE);
        assert!(bh.contains(42));
        bh.clear();
        assert!(!bh.contains(42));
        assert!(!bh.contains(0xCAFEBABE));
        assert_eq!(bh.utilization(), 0.0);
    }

    #[test]
    fn utilization_is_bounded_by_one_eighth_at_saturation() {
        let n_blocks = 4096usize;
        let mut bh = BitHash::with_block_count(n_blocks);
        for i in 0..n_blocks as u64 {
            // Spread the rsums across the address space using a cheap mixer
            // so we approximate the uniform-random density bound.
            let rsum = (i.wrapping_mul(0x9E37_79B9_7F4A_7C15) & u32::MAX as u64) as u32;
            bh.insert(rsum);
        }
        let util = bh.utilization();
        // The design target is 1/8 = 0.125 with N inserts into 8N bits.
        // Allow a 5% absolute slack for finite-size variance.
        assert!(util <= 0.175, "utilization {util} exceeds 1/8 + slack");
    }
}
