//! Typed error variants for the receive-side parallel delta applier.
//!
//! Extracted from `parallel_apply/mod.rs` as part of the module
//! decomposition. Holds [`ParallelApplyError`] (the user-visible shutdown
//! and verification failure modes) and its [`io::Error`] bridge so existing
//! `io::Result`-shaped callers keep their API.

use std::io;

use checksums::strong::strategy::ChecksumAlgorithmKind;
use thiserror::Error;

use super::super::types::FileNdx;

/// Typed error variants for [`ParallelDeltaApplier::finish_file`] shutdown
/// paths.
///
/// The audit at `docs/audits/arc-try-unwrap-classification.md` (ATU-3,
/// tracked in #2380) flagged the previous opaque `io::Error::other(...)`
/// message as user-visible but undiagnosable: it omitted the residual
/// [`Arc::strong_count`], the offending `FileNdx`, and the failure mode
/// (still-in-flight vs poisoned). Each variant below carries enough
/// context for an operator to locate the leaking worker or the
/// panicking holder.
///
/// [`ParallelDeltaApplier::finish_file`]: super::ParallelDeltaApplier::finish_file
/// [`Arc::strong_count`]: std::sync::Arc::strong_count
#[derive(Debug, Error)]
pub enum ParallelApplyError {
    /// The per-file slot's [`Arc`](std::sync::Arc) still has outstanding clones; a
    /// worker has not yet released its reference. The applier cannot
    /// extract the writer until every clone has been dropped.
    #[error(
        "ParallelDeltaApplier::{kind}: file slot still referenced for ndx={ndx} (strong_count={strong_count})"
    )]
    ApplierStillReferenced {
        /// File index whose slot is still shared.
        ndx: FileNdx,
        /// Observed [`Arc::strong_count`] at the failure site.
        ///
        /// Always `>= 2` when this variant is constructed.
        ///
        /// [`Arc::strong_count`]: std::sync::Arc::strong_count
        strong_count: usize,
        /// Static tag identifying the call site (e.g. `"finish_file"`).
        kind: &'static str,
    },
    /// The per-file slot's mutex was poisoned by a panicking holder.
    /// The applier cannot reuse the writer; the caller must abort the
    /// transfer for `ndx`.
    #[error("ParallelDeltaApplier::{kind}: file slot mutex poisoned for ndx={ndx}")]
    SlotPoisoned {
        /// File index whose slot mutex was poisoned.
        ndx: FileNdx,
        /// Static tag identifying the call site (e.g. `"finish_file"`).
        kind: &'static str,
    },
    /// The per-file reorder buffer still holds chunks awaiting a
    /// missing sequence number when finish was requested. Indicates the
    /// producer dropped a chunk or stopped submitting before the
    /// stream completed.
    #[error(
        "ParallelDeltaApplier::{kind}: file {ndx} finished with chunks still buffered ({buffered})"
    )]
    UndrainedChunks {
        /// File index whose reorder buffer was non-empty at finish.
        ndx: FileNdx,
        /// Number of chunks still buffered.
        buffered: usize,
        /// Static tag identifying the call site (e.g. `"finish_file"`).
        kind: &'static str,
    },
    /// The strong checksum computed from `chunk.data` did not match the
    /// expected digest the producer attached to the chunk. The receiver
    /// must abort the chunk's file (or fall back to phase-2 redo) rather
    /// than commit corrupted bytes.
    #[error(
        "ParallelDeltaApplier::verify_chunk: checksum mismatch for ndx={ndx} sequence={chunk_sequence} algorithm={algorithm} expected={expected} actual={actual}"
    )]
    ChecksumMismatch {
        /// File index whose chunk failed verification.
        ndx: FileNdx,
        /// Per-file sequence number of the failing chunk.
        chunk_sequence: u64,
        /// Algorithm used for the digest comparison.
        algorithm: ChecksumAlgorithmKind,
        /// Expected digest as a hex string (from the chunk metadata).
        expected: String,
        /// Actual digest computed from `chunk.data`, as a hex string.
        actual: String,
    },
}

impl From<ParallelApplyError> for io::Error {
    /// Maps a [`ParallelApplyError`] to an [`io::Error`] so existing
    /// callers keep their `io::Result`-shaped API. The full typed
    /// message - including `ndx`, `strong_count`, and the call-site tag -
    /// is preserved as the `Display` payload.
    fn from(value: ParallelApplyError) -> Self {
        io::Error::other(value.to_string())
    }
}
