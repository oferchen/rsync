//! Per-file delta chunk payloads for the parallel apply scaffold.
//!
//! Extracted from `parallel_apply/mod.rs` as part of the module
//! decomposition. Holds the public [`DeltaChunk`] segment type the wire
//! reader hands to [`ParallelDeltaApplier::apply_one_chunk`], plus the
//! internal [`VerifiedChunk`] the rayon verify step produces once a chunk
//! has cleared its strong-checksum comparison.
//!
//! [`ParallelDeltaApplier::apply_one_chunk`]: super::ParallelDeltaApplier::apply_one_chunk

use checksums::strong::strategy::ChecksumDigest;

use super::super::types::FileNdx;

/// A single contiguous segment of a per-file delta apply.
///
/// One chunk corresponds to either a literal-data span (`is_literal = true`)
/// or a basis-file block reference (`is_literal = false`). Either way it
/// carries the bytes already resolved by the wire reader plus the
/// per-file sequence number assigned at submission time.
///
/// Chunks are CPU-light at this stage; the heavy step is the strong-checksum
/// rollup that `ParallelDeltaApplier::verify_chunk` runs on a rayon worker
/// using the negotiated [`ChecksumStrategy`].
///
/// [`ChecksumStrategy`]: checksums::strong::strategy::ChecksumStrategy
#[derive(Debug, Clone)]
pub struct DeltaChunk {
    /// File this chunk belongs to.
    pub ndx: FileNdx,
    /// Monotonic per-file submission sequence number.
    ///
    /// The applier replays chunks for `ndx` in increasing `chunk_sequence`
    /// order, mirroring the per-file byte order the sender emitted.
    pub chunk_sequence: u64,
    /// Resolved bytes for this chunk.
    pub data: Vec<u8>,
    /// `true` for literal payloads, `false` for basis-file matches. The
    /// verify and write paths are identical today; the discriminator is kept
    /// so future stats reporting can split literal vs matched bytes without
    /// touching the public chunk shape.
    pub is_literal: bool,
    /// Optional expected strong-checksum digest for `data`.
    ///
    /// When `Some`, `ParallelDeltaApplier::verify_chunk` computes the
    /// digest of `data` using the negotiated strategy and compares it
    /// byte-for-byte against this value. A mismatch produces a typed
    /// [`ParallelApplyError::ChecksumMismatch`] so the receiver can fall
    /// back to a phase-2 redo or abort the transfer; the corrupt bytes
    /// never reach the destination writer.
    ///
    /// When `None`, the applier skips comparison but still computes the
    /// digest for parity with the verified path (keeping CPU cost stable
    /// across both shapes and exercising the strategy code path). Producers
    /// that have not yet wired the per-chunk expected digest into the
    /// receiver pipeline can leave this as `None` and the applier remains
    /// backward-compatible.
    ///
    /// [`ParallelApplyError::ChecksumMismatch`]: super::ParallelApplyError::ChecksumMismatch
    pub expected_strong: Option<ChecksumDigest>,
}

impl DeltaChunk {
    /// Builds a literal-data chunk with no expected digest attached.
    #[must_use]
    pub fn literal(ndx: impl Into<FileNdx>, chunk_sequence: u64, data: Vec<u8>) -> Self {
        Self {
            ndx: ndx.into(),
            chunk_sequence,
            data,
            is_literal: true,
            expected_strong: None,
        }
    }

    /// Builds a basis-match chunk with no expected digest attached.
    #[must_use]
    pub fn matched(ndx: impl Into<FileNdx>, chunk_sequence: u64, data: Vec<u8>) -> Self {
        Self {
            ndx: ndx.into(),
            chunk_sequence,
            data,
            is_literal: false,
            expected_strong: None,
        }
    }

    /// Builder-style setter that attaches an expected strong-checksum
    /// digest to this chunk.
    ///
    /// The receiver pipeline uses this to opt each chunk into real
    /// per-chunk verification by `ParallelDeltaApplier::verify_chunk`.
    /// Callers that have not negotiated per-chunk checksums (or are
    /// driving the applier from a bench/test that does not need the
    /// extra comparison) can leave the field unset.
    #[must_use]
    pub fn with_expected_strong(mut self, expected: ChecksumDigest) -> Self {
        self.expected_strong = Some(expected);
        self
    }
}

/// CPU-bound verification result handed back from the rayon worker so the
/// owning thread can run the serial per-file write under the per-file mutex.
#[derive(Debug)]
pub(super) struct VerifiedChunk {
    pub(super) chunk: DeltaChunk,
    /// Strong-checksum digest computed for `chunk.data` using the
    /// negotiated strategy. Equal to the chunk's `expected_strong` value
    /// (when one was attached) by construction: a mismatch is reported as
    /// a [`ParallelApplyError::ChecksumMismatch`] before this type is
    /// constructed, so a `VerifiedChunk` is only ever produced for data
    /// that has cleared verification.
    ///
    /// [`ParallelApplyError::ChecksumMismatch`]: super::ParallelApplyError::ChecksumMismatch
    pub(super) digest: ChecksumDigest,
}
