//! Bounded-memory spill-to-tempfile layer for the reorder buffer.
//!
//! When the in-memory reorder buffer accumulates more data than a configured
//! threshold (default 64 MB), excess items - those furthest from delivery -
//! are serialized to a temporary file on disk. On delivery the buffer
//! transparently reloads spilled items, maintaining the same in-order
//! guarantee as the underlying [`ReorderBuffer`](super::reorder::ReorderBuffer).
//!
//! # Design
//!
//! Items must implement [`SpillCodec`] so they can be encoded to and decoded
//! from bytes. The codec uses a simple length-prefixed binary format -
//! each record is `[u32 len][payload bytes]` - which is compact, fast to
//! seek through, and platform-independent.
//!
//! Spilled items are indexed by `(sequence_number -> file_offset)` in a
//! `BTreeMap` so reload is O(log S) where S is the number of spilled items.
//! By default the temporary file is created via the `tempfile` crate
//! (`SpooledTempFile`) and deleted automatically when the buffer is dropped
//! (RAII cleanup). Callers may supply an explicit spill directory via
//! [`SpillableReorderBuffer::with_spill_dir`], which is more resilient when
//! the directory is shared across long-running transfers.
//!
//! # Spill strategy
//!
//! When `estimated_memory > threshold` after an insert, the buffer spills
//! the *highest-sequence* buffered items first - these are furthest from
//! the delivery cursor (`next_expected`) and thus least likely to be needed
//! soon. Items within a small "hot zone" around `next_expected` are kept
//! in memory to avoid thrashing.
//!
//! # Error handling
//!
//! Every disk operation surfaces its error to the caller via [`SpillError`].
//! Earlier revisions panicked on I/O failure, which translated heavy-transfer
//! ENOSPC and temp-directory-vanish events into process crashes. The current
//! API returns errors so the receiver can map them to rsync exit code 11
//! ([`FileIo`](https://github.com/RsyncProject/rsync/blob/master/errcode.h))
//! and abort cleanly. When an explicit spill directory disappears mid-transfer
//! the buffer attempts a single `create_dir_all` recovery before propagating
//! the failure.
//!
//! # Upstream Reference
//!
//! Upstream rsync processes files sequentially in `recv_files()` and never
//! buffers more than one file's data. This spill mechanism handles the
//! memory pressure that arises from parallel dispatch reordering, which
//! has no upstream equivalent.

use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::path::Path;

use super::reorder::CapacityExceeded;

mod buffer;
pub mod policy;
pub use buffer::SpillableReorderBuffer;
pub use policy::{ReclaimMode, SpillCompression, SpillGranularity, SpillPolicy};

/// Default memory threshold (in bytes) before spilling begins.
///
/// Set to 64 MB, which accommodates roughly 64K items of 1 KB each.
/// Callers can tune this via [`SpillableReorderBuffer::new`].
pub const DEFAULT_SPILL_THRESHOLD: usize = 64 * 1024 * 1024;

/// Errors surfaced by the spill layer.
///
/// Producers should treat any [`SpillError::Io`] as fatal for the affected
/// transfer: ENOSPC, missing spill directories, and partial writes all
/// indicate that the disk backing the reorder buffer can no longer be
/// trusted. The receiver maps these to exit code 11 ([`FileIo`]) so the
/// transfer aborts with the same semantics as upstream rsync's I/O failures.
///
/// [`FileIo`]: https://github.com/RsyncProject/rsync/blob/master/errcode.h
#[derive(Debug)]
pub enum SpillError {
    /// Capacity bound from the underlying ring buffer was exceeded.
    Capacity(CapacityExceeded),
    /// Disk I/O failed while reading or writing spilled items.
    Io(io::Error),
}

impl SpillError {
    /// Returns the underlying I/O error if this is an I/O failure.
    #[must_use]
    pub fn io_error(&self) -> Option<&io::Error> {
        match self {
            SpillError::Io(e) => Some(e),
            SpillError::Capacity(_) => None,
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
        }
    }
}

impl std::error::Error for SpillError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SpillError::Capacity(_) => None,
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

/// Codec for serializing and deserializing items to the spill file.
///
/// Implementations must produce a deterministic byte representation and
/// report an accurate `encoded_size` for memory accounting. The encoded
/// format is opaque to the spill layer - only `encode` and `decode` must
/// agree on the wire format.
pub trait SpillCodec: Sized {
    /// Writes the item to `writer` in a format that [`decode`](Self::decode) can read back.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if writing fails.
    fn encode(&self, writer: &mut dyn Write) -> io::Result<()>;

    /// Reads an item from `reader` that was previously written by [`encode`](Self::encode).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if reading fails or the data is corrupt.
    fn decode(reader: &mut dyn Read) -> io::Result<Self>;

    /// Returns the approximate in-memory size of this item in bytes.
    ///
    /// Used for memory accounting to decide when to spill. Does not need
    /// to be exact - a conservative overestimate is fine.
    fn estimated_size(&self) -> usize;
}

/// Backing storage for spilled bytes.
///
/// Two flavours are supported:
///
/// - `Spooled` - the default. Wraps `tempfile::SpooledTempFile`, which keeps
///   small spills in memory and rolls over to disk past a threshold. The OS
///   deletes the file when the buffer is dropped.
/// - `Directory` - opens a single anonymous tempfile inside a caller-provided
///   directory. If the directory vanishes mid-transfer (operator cleanup,
///   container restart) the buffer performs one `create_dir_all` retry
///   before surfacing the error.
pub(super) enum SpillBackend {
    Spooled(tempfile::SpooledTempFile),
    Directory(File),
}

impl SpillBackend {
    pub(super) fn file(&mut self) -> &mut dyn ReadWriteSeek {
        match self {
            SpillBackend::Spooled(f) => f,
            SpillBackend::Directory(f) => f,
        }
    }
}

/// Trait object alias to keep the [`SpillBackend::file`] accessor honest.
pub(super) trait ReadWriteSeek: Read + Write + Seek {}
impl<T: Read + Write + Seek + ?Sized> ReadWriteSeek for T {}

/// Diagnostic counters for spill activity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpillStats {
    /// Number of items currently spilled to disk.
    pub spilled_items: usize,
    /// Total spill-to-disk events since creation.
    pub spill_events: u64,
    /// Total reload-from-disk events since creation.
    pub reload_events: u64,
    /// Current estimated in-memory bytes.
    pub memory_used: usize,
    /// Configured spill threshold in bytes.
    pub threshold: usize,
    /// Number of times the spill directory was re-created after vanishing.
    pub dir_recreate_events: u64,
}

/// Opens the appropriate backend for a spill file.
pub(super) fn open_backend(dir: Option<&Path>) -> io::Result<SpillBackend> {
    match dir {
        Some(dir) => Ok(SpillBackend::Directory(tempfile::tempfile_in(dir)?)),
        None => {
            // SpooledTempFile keeps small spills in memory (up to 1 MB) and
            // rolls over to disk for larger volumes, avoiding disk I/O for
            // transient pressure spikes.
            Ok(SpillBackend::Spooled(tempfile::SpooledTempFile::new(
                1024 * 1024,
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_result_spill_codec_roundtrip() {
        use crate::concurrent_delta::types::DeltaResult;

        let original = DeltaResult::success(42u32, 1000, 300, 700).with_sequence(5);
        let mut encoded = Vec::new();
        original.encode(&mut encoded).unwrap();

        let decoded = DeltaResult::decode(&mut encoded.as_slice()).unwrap();
        assert_eq!(decoded.ndx().get(), 42);
        assert_eq!(decoded.sequence(), 5);
        assert_eq!(decoded.bytes_written(), 1000);
        assert_eq!(decoded.literal_bytes(), 300);
        assert_eq!(decoded.matched_bytes(), 700);
        assert!(decoded.is_success());
    }

    #[test]
    fn delta_result_needs_redo_codec_roundtrip() {
        use crate::concurrent_delta::types::DeltaResult;

        let original =
            DeltaResult::needs_redo(10u32, "checksum mismatch".to_string()).with_sequence(3);
        let mut encoded = Vec::new();
        original.encode(&mut encoded).unwrap();

        let decoded = DeltaResult::decode(&mut encoded.as_slice()).unwrap();
        assert_eq!(decoded.ndx().get(), 10);
        assert_eq!(decoded.sequence(), 3);
        assert!(decoded.needs_retry());
    }

    #[test]
    fn delta_result_failed_codec_roundtrip() {
        use crate::concurrent_delta::types::DeltaResult;

        let original = DeltaResult::failed(99u32, "I/O error on disk".to_string()).with_sequence(7);
        let mut encoded = Vec::new();
        original.encode(&mut encoded).unwrap();

        let decoded = DeltaResult::decode(&mut encoded.as_slice()).unwrap();
        assert_eq!(decoded.ndx().get(), 99);
        assert_eq!(decoded.sequence(), 7);
        assert!(!decoded.is_success());
        assert!(!decoded.needs_retry());
    }

    #[test]
    fn spill_error_display_and_source() {
        let cap_err = SpillError::from(CapacityExceeded);
        assert_eq!(format!("{cap_err}"), "reorder buffer capacity exceeded");
        assert!(std::error::Error::source(&cap_err).is_none());

        let io_err = SpillError::from(io::Error::new(io::ErrorKind::StorageFull, "disk full"));
        assert!(format!("{io_err}").contains("disk full"));
        assert!(std::error::Error::source(&io_err).is_some());
    }
}
