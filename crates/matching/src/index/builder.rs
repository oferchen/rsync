//! Construction logic for [`DeltaSignatureIndex`].
//!
//! Provides [`from_signature`](DeltaSignatureIndex::from_signature) and
//! [`rebuild`](DeltaSignatureIndex::rebuild) methods that populate the tag
//! table, bithash prefilter, and [`CompactLookup`] from a [`FileSignature`].

use signature::{FileSignature, SignatureAlgorithm, SignatureBlock};

use super::compact_lookup::CompactLookup;
use super::trace::{HashtableRole, trace_created, trace_growing};
use super::{BitHash, DeltaSignatureIndex, TAG_TABLE_SIZE};

/// Shared helper that indexes full-length blocks into the tag table, bithash,
/// and compact lookup table.
///
/// Returns `true` if at least one full-length block was indexed.
fn populate_index(
    blocks: &[SignatureBlock],
    block_length: usize,
    tag_table: &mut [bool],
    bithash: &mut BitHash,
    lookup: &mut CompactLookup,
) -> bool {
    let mut has_full_blocks = false;
    for (index, block) in blocks.iter().enumerate() {
        if block.len() != block_length {
            continue;
        }
        has_full_blocks = true;
        let digest = block.rolling();
        tag_table[digest.sum1() as usize] = true;
        bithash.insert(digest.value());
        lookup.insert(digest.sum1(), digest.sum2(), index as u32);
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

        if !populate_index(
            &blocks,
            block_length,
            &mut tag_table,
            &mut bithash,
            &mut lookup,
        ) {
            return None;
        }

        let size = lookup.capacity();
        let index = Self {
            block_length,
            strong_length,
            algorithm,
            blocks,
            lookup,
            tag_table,
            bithash,
            role,
            last_traced_size: size,
        };
        // upstream: hashtable.c:45-53 - one HASH,1 emission per hashtable
        // creation, with optional `req:` prefix when the rounded-up size
        // differs from the caller's requested capacity.
        trace_created(role, index.identifier(), requested, size);
        Some(index)
    }

    /// Rebuilds the index in-place from a new signature, reusing the
    /// existing [`CompactLookup`] allocation.
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

        let ok = populate_index(
            &self.blocks,
            block_length,
            &mut self.tag_table,
            &mut self.bithash,
            &mut self.lookup,
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
