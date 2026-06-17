//! Construction logic for [`DeltaSignatureIndex`].
//!
//! Provides [`from_signature`](DeltaSignatureIndex::from_signature) and
//! [`rebuild`](DeltaSignatureIndex::rebuild) methods that populate the tag
//! table, bithash prefilter, and [`CompactLookup`] from a [`FileSignature`].

use signature::{FileSignature, SignatureAlgorithm, SignatureBlock};

use super::compact_lookup::CompactLookup;
use super::trace::{HashtableRole, trace_created, trace_growing};
use super::{
    BitHash, CONSUMED_BITS_PER_WORD, DeltaSignatureIndex, NEXT_MATCH_NONE, TAG_TABLE_SIZE,
    build_consumed_words,
};

/// Shared helper that indexes full-length blocks into the tag table, bithash,
/// compact lookup table, and sequential-match successor links.
///
/// The successor link table is written in a single pass: while walking the
/// block list in order we remember the index of the previous full-length
/// block and patch its slot once a successor appears. Partial-length blocks
/// break the chain (they are not indexed and never become successors), which
/// preserves wire compatibility under trailing-partial-block layouts.
///
/// Returns `true` if at least one full-length block was indexed.
fn populate_index(
    blocks: &[SignatureBlock],
    block_length: usize,
    tag_table: &mut [bool],
    bithash: &mut BitHash,
    lookup: &mut CompactLookup,
    next_match: &mut [u32],
) -> bool {
    let mut has_full_blocks = false;
    let mut prev_full: Option<usize> = None;
    for (index, block) in blocks.iter().enumerate() {
        if block.len() != block_length {
            continue;
        }
        has_full_blocks = true;
        let digest = block.rolling();
        tag_table[digest.sum1() as usize] = true;
        bithash.insert(digest.value());
        lookup.insert(digest.sum1(), digest.sum2(), index as u32);
        if let Some(prev) = prev_full {
            // upstream: zsync `librcksum/rsum.c:262` records the
            // immediately-following block as the next-match candidate.
            next_match[prev] = index as u32;
        }
        prev_full = Some(index);
    }
    has_full_blocks
}

impl DeltaSignatureIndex {
    /// Builds a signature index from the provided [`FileSignature`].
    ///
    /// The helper only indexes blocks that match the canonical block length
    /// reported by the layout. Files that produce fewer than one full block
    /// therefore return `None`, mirroring upstream rsync's behaviour of
    /// disabling the rolling checksum pipeline for very small payloads.
    ///
    /// The returned index emits its `--debug=HASH` create line with the
    /// default `HashtableRole::Sender` prefix - upstream's `who_am_i()`
    /// returns `"sender"` for the side that calls `build_hash_table`.
    /// Callers operating from a different role (receiver-side local
    /// delta apply, generator-side fixtures) should pass an explicit
    /// role via [`Self::from_signature_with_role`].
    pub fn from_signature(
        signature: &FileSignature,
        algorithm: SignatureAlgorithm,
    ) -> Option<Self> {
        Self::from_signature_with_role(signature, algorithm, HashtableRole::Sender)
    }

    /// Builds a signature index, attributing `--debug=HASH` emissions to
    /// the provided role.
    ///
    /// Mirrors upstream rsync's `who_am_i()` prefix on the
    /// `created hashtable` line emitted from `hashtable.c:45-53`.
    pub fn from_signature_with_role(
        signature: &FileSignature,
        algorithm: SignatureAlgorithm,
        role: HashtableRole,
    ) -> Option<Self> {
        let block_length = signature.layout().block_length().get() as usize;
        let strong_length = usize::from(signature.layout().strong_sum_length().get());
        let blocks: Vec<SignatureBlock> = signature.blocks().to_vec();

        let requested = blocks.len();
        let mut lookup = CompactLookup::with_capacity(requested);
        let mut tag_table = vec![false; TAG_TABLE_SIZE];
        let mut bithash = BitHash::with_block_count(requested);
        // The seq-match link table holds one slot per signature block, sized
        // to the raw signature length (including any trailing partial block)
        // so `next_match[K]` is addressable for every K the lookup can yield.
        let mut next_match = vec![NEXT_MATCH_NONE; requested];

        if !populate_index(
            &blocks,
            block_length,
            &mut tag_table,
            &mut bithash,
            &mut lookup,
            &mut next_match,
        ) {
            return None;
        }

        let size = lookup.capacity();
        let consumed = build_consumed_words(blocks.len());
        let index = Self {
            block_length,
            strong_length,
            algorithm,
            blocks,
            lookup,
            tag_table,
            bithash,
            next_match,
            consumed,
            role,
            last_traced_size: size,
            #[cfg(any(test, feature = "bench-internal"))]
            seq_match_counters: std::sync::Arc::new(super::SeqMatchCounters::default()),
        };
        // upstream: hashtable.c:45-53 - one HASH,1 emission per hashtable
        // creation, with optional `req:` prefix when the rounded-up size
        // differs from the caller's requested capacity.
        trace_created(role, index.identifier(), requested, size);
        Some(index)
    }

    /// Rebuilds the index in-place from a new signature, reusing the
    /// existing `CompactLookup` allocation.
    ///
    /// Mirrors upstream rsync's hash table reuse pattern (match.c):
    /// the table is cleared and repopulated rather than freed and
    /// re-allocated, avoiding per-file malloc/free overhead.
    ///
    /// Returns `false` if the signature has no full blocks (caller
    /// should discard the index).
    pub fn rebuild(&mut self, signature: &FileSignature, algorithm: SignatureAlgorithm) -> bool {
        let block_length = signature.layout().block_length().get() as usize;
        let strong_length = usize::from(signature.layout().strong_sum_length().get());

        self.block_length = block_length;
        self.strong_length = strong_length;
        self.algorithm = algorithm;
        self.blocks.clear();
        self.blocks.extend_from_slice(signature.blocks());
        self.lookup.clear();
        self.tag_table.iter_mut().for_each(|v| *v = false);
        self.bithash.clear();
        // Per ZSO-7 isolation: every link from the prior segment must be
        // cleared before re-population so a stale successor never leaks
        // across the per-NDX `rebuild` boundary.
        self.next_match.clear();
        self.next_match.resize(self.blocks.len(), NEXT_MATCH_NONE);
        // ZSO-7 per-segment lifecycle: reset prune state so a new
        // segment starts with no stale consumed bits. Resize when the
        // new signature's block count changes the required word count.
        let words_needed = self.blocks.len().div_ceil(CONSUMED_BITS_PER_WORD);
        if self.consumed.len() == words_needed {
            self.reset_consumed();
        } else {
            self.consumed = build_consumed_words(self.blocks.len());
        }
        #[cfg(any(test, feature = "bench-internal"))]
        self.seq_match_counters.reset();

        let ok = populate_index(
            &self.blocks,
            block_length,
            &mut self.tag_table,
            &mut self.bithash,
            &mut self.lookup,
            &mut self.next_match,
        );

        if ok {
            let size = self.lookup.capacity();
            // upstream: hashtable.c:100-103 - emit when the bucket count
            // changes from the previously traced value. Our `rebuild`
            // reuses the same allocation when possible, so the size only
            // ever differs when the caller's signature width changes.
            if size != self.last_traced_size {
                trace_growing(self.role, self.identifier(), size);
                self.last_traced_size = size;
            }
        }

        ok
    }
}
