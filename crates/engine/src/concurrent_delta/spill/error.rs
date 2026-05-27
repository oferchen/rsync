//! Error type for the spill-to-tempfile layer.
//!
//! Producers should treat any [`SpillError::Io`] as fatal for the affected
//! transfer: ENOSPC, missing spill directories, and partial writes all
//! indicate that the disk backing the reorder buffer can no longer be
//! trusted. The receiver maps these to exit code 11 ([`FileIo`]) so the
//! transfer aborts with the same semantics as upstream rsync's I/O failures.
//!
//! [`FileIo`]: https://github.com/RsyncProject/rsync/blob/master/errcode.h

use std::io;
use std::path::PathBuf;

use super::super::reorder::CapacityExceeded;

/// Errors surfaced by the spill layer.
///
/// Producers should treat any [`SpillError::Io`] as fatal for the affected
/// transfer: ENOSPC, missing spill directories, and partial writes all
/// indicate that the disk backing the reorder buffer can no longer be
/// trusted. The receiver maps these to exit code 11 ([`FileIo`]) so the
/// transfer aborts with the same semantics as upstream rsync's I/O failures.
///
/// [`UnsupportedCompression`](Self::UnsupportedCompression) is surfaced when a
/// spill file written by a `spill-compression` build is read by a default
/// build that has no codec available - the on-disk tag byte advertises the
/// algorithm so we fail loudly rather than decode garbage.
///
/// [`PriorSpillsLost`](Self::PriorSpillsLost) is surfaced when the spill
/// directory vanishes mid-transfer after one or more records were already
/// written to disk. Recovery is refused because those records cannot be
/// reconstructed; the typed variant lets the receiver emit an actionable
/// diagnostic for the operator instead of a generic `NotFound`.
///
/// [`SpillDisabled`](Self::SpillDisabled) is surfaced when in-memory-only mode
/// is active and the reorder buffer exceeds its capacity threshold. The caller
/// should increase the threshold, reduce concurrency, or permit disk spill.
///
/// [`FileIo`]: https://github.com/RsyncProject/rsync/blob/master/errcode.h
#[derive(Debug)]
pub enum SpillError {
    /// Capacity bound from the underlying ring buffer was exceeded.
    Capacity(CapacityExceeded),
    /// Disk I/O failed while reading or writing spilled items.
    Io(io::Error),
    /// On-disk record advertises a compression algorithm this build cannot
    /// decode. Holds the unknown tag byte for diagnostics.
    UnsupportedCompression(u8),
    /// Caller-supplied spill directory vanished mid-transfer after prior
    /// records were already on disk. Carries the directory that vanished
    /// and the count of records known to be unrecoverable.
    PriorSpillsLost {
        /// Directory that disappeared after prior spill writes had committed.
        dir: PathBuf,
        /// Number of records (sequences) that were spilled to disk and can
        /// no longer be reloaded.
        count: usize,
    },
    /// Spill-to-disk was requested but the policy forbids disk I/O.
    ///
    /// Returned when [`SpillPolicy::in_memory_only`](super::policy::SpillPolicy::in_memory_only)
    /// is `true` and the reorder buffer exceeds its capacity threshold.
    /// Callers should either increase the threshold, reduce concurrency,
    /// or switch to a policy that permits disk spill.
    SpillDisabled,
}

impl SpillError {
    /// Returns the underlying I/O error if this is an I/O failure.
    #[must_use]
    pub fn io_error(&self) -> Option<&io::Error> {
        match self {
            SpillError::Io(e) => Some(e),
            SpillError::Capacity(_)
            | SpillError::UnsupportedCompression(_)
            | SpillError::PriorSpillsLost { .. }
            | SpillError::SpillDisabled => None,
        }
    }

    /// Returns `true` if this error indicates the disk is out of space.
    #[must_use]
    pub fn is_out_of_space(&self) -> bool {
        self.io_error()
            .is_some_and(|e| e.kind() == io::ErrorKind::StorageFull)
    }
}

impl std::fmt::Display for SpillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpillError::Capacity(_) => write!(f, "reorder buffer capacity exceeded"),
            SpillError::Io(e) => write!(f, "reorder spill I/O failed: {e}"),
            SpillError::UnsupportedCompression(tag) => write!(
                f,
                "reorder spill record uses compression tag 0x{tag:02x} not supported by this build"
            ),
            SpillError::PriorSpillsLost { dir, count } => write!(
                f,
                "prior spill directory {} vanished; {count} chunk(s) cannot be recovered",
                dir.display()
            ),
            SpillError::SpillDisabled => write!(
                f,
                "reorder buffer exceeded capacity but spill-to-disk is disabled (in-memory-only policy)"
            ),
        }
    }
}

impl std::error::Error for SpillError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SpillError::Capacity(_)
            | SpillError::UnsupportedCompression(_)
            | SpillError::PriorSpillsLost { .. }
            | SpillError::SpillDisabled => None,
            SpillError::Io(e) => Some(e),
        }
    }
}

impl From<CapacityExceeded> for SpillError {
    fn from(e: CapacityExceeded) -> Self {
        SpillError::Capacity(e)
    }
}

impl From<io::Error> for SpillError {
    fn from(e: io::Error) -> Self {
        SpillError::Io(e)
    }
}
