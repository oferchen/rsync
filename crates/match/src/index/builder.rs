//! Construction logic for [`DeltaSignatureIndex`].
//!
//! Provides [`from_signature`](DeltaSignatureIndex::from_signature) and
//! [`rebuild`](DeltaSignatureIndex::rebuild) methods that populate the tag
//! table and lookup map from a [`FileSignature`].

use rustc_hash::FxHashMap;

use signature::{FileSignature, SignatureAlgorithm, SignatureBlock};

use super::{DeltaSignatureIndex, TAG_TABLE_SIZE};

/// Shared helper that indexes full-length blocks into the tag table and lookup map.
///
/// Returns `true` if at least one full-length block was indexed.
fn populate_index(
    blocks: &[SignatureBlock],
    block_length: usize,
    tag_table: &mut [bool],
    lookup: &mut FxHashMap<(u16, u16), Vec<usize>>,
) -> bool {
    let mut has_full_blocks = false;
    for (index, block) in blocks.iter().enumerate() {
        if block.len() != block_length {
            continue;
        }
        has_full_blocks = true;
        let digest = block.rolling();
        tag_table[digest.sum1() as usize] = true;
        lookup
            .entry((digest.sum1(), digest.sum2()))
            .or_default()
            .push(index);
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
    pub fn from_signature(
        signature: &FileSignature,
        algorithm: SignatureAlgorithm,
    ) -> Option<Self> {
        let block_length = signature.layout().block_length().get() as usize;
        let strong_length = usize::from(signature.layout().strong_sum_length().get());
        let blocks: Vec<SignatureBlock> = signature.blocks().to_vec();

        let mut lookup: FxHashMap<(u16, u16), Vec<usize>> =
            FxHashMap::with_capacity_and_hasher(blocks.len(), Default::default());
        let mut tag_table = vec![false; TAG_TABLE_SIZE];

        if !populate_index(&blocks, block_length, &mut tag_table, &mut lookup) {
            return None;
        }

        Some(Self {
            block_length,
            strong_length,
            algorithm,
            blocks,
            lookup,
            tag_table,
        })
    }

    /// Rebuilds the index in-place from a new signature, reusing the
    /// existing `FxHashMap` allocation.
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

        populate_index(
            &self.blocks,
            block_length,
            &mut self.tag_table,
            &mut self.lookup,
        )
    }
}
