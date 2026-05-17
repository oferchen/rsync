//! Error type for the spill-to-tempfile layer.
//!
//! Producers should treat any [`SpillError::Io`] as fatal for the affected
//! transfer: ENOSPC, missing spill directories, and partial writes all
//! indicate that the disk backing the reorder buffer can no longer be
//! trusted. The receiver maps these to exit code 11 ([`FileIo`]) so the
//! transfer aborts with the same semantics as upstream rsync's I/O failures.
//!
//! [`UnsupportedCompression`](SpillError::UnsupportedCompression) is surfaced
//! when a spill file written by a `spill-compression` build is read by a
//! default build that has no codec available - the on-disk tag byte
//! advertises the algorithm so we fail loudly rather than decode garbage.
//!
//! [`FileIo`]: https://github.com/RsyncProject/rsync/blob/master/errcode.h

use std::io;

use super::super::reorder::CapacityExceeded;

/// Errors surfaced by the spill layer.
#[derive(Debug)]
pub enum SpillError {
    /// Capacity bound from the underlying ring buffer was exceeded.
    Capacity(CapacityExceeded),
    /// Disk I/O failed while reading or writing spilled items.
    Io(io::Error),
    /// On-disk record advertises a compression algorithm this build cannot
    /// decode. Holds the unknown tag byte for diagnostics.
    UnsupportedCompression(u8),
}

impl SpillError {
    /// Returns the underlying I/O error if this is an I/O failure.
    #[must_use]
    pub fn io_error(&self) -> Option<&io::Error> {
        match self {
            SpillError::Io(e) => Some(e),
            SpillError::Capacity(_) | SpillError::UnsupportedCompression(_) => None,
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
        }
    }
}

impl std::error::Error for SpillError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SpillError::Capacity(_) | SpillError::UnsupportedCompression(_) => None,
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
