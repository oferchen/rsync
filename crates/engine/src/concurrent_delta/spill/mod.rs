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
//! from bytes. The on-disk format is `[u32 len][payload bytes]` per record;
//! payload contents and per-record fan-out are controlled by
//! [`SpillGranularity`]. The default ([`SpillGranularity::WholeBatch`])
//! packs every candidate selected by a single spill event into one record
//! so the 4-byte length header is amortised across many items.
//! [`SpillGranularity::PerItem`] writes one record per item, matching the
//! historical layout and keeping a single reload's decode cost bounded to
//! a single payload. Both formats are compact, fast to seek through, and
//! platform-independent.
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
//! in memory to avoid thrashing. Under
//! [`SpillGranularity::WholeBatch`] every non-hot-zone candidate is
//! evicted in one batched write; under [`SpillGranularity::PerItem`] the
//! eviction stops as soon as the in-memory budget drops back below the
//! threshold.
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

use std::io::{self, Read, Write};

mod error;

pub use error::SpillError;

pub mod env;
pub mod policy;
pub mod rss;
pub mod stats;
pub use env::{
    ENV_SPILL_COMPRESSION, ENV_SPILL_DIR, ENV_SPILL_THRESHOLD_BYTES, apply_env_overrides,
};
pub use policy::{ReclaimMode, SpillCompression, SpillGranularity, SpillPolicy, SpillReclaim};
pub use stats::SpillStats;

mod tempfile;

mod buffer;

pub use buffer::SpillableReorderBuffer;

#[cfg(test)]
mod tests_per_knob;

/// Default memory threshold (in bytes) before spilling begins.
///
/// Set to 64 MB, which accommodates roughly 64K items of 1 KB each.
/// Callers can tune this via [`SpillableReorderBuffer::new`].
pub const DEFAULT_SPILL_THRESHOLD: usize = 64 * 1024 * 1024;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::concurrent_delta::reorder::CapacityExceeded;

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

        let unsupported = SpillError::UnsupportedCompression(0x01);
        let rendered = format!("{unsupported}");
        assert!(
            rendered.contains("0x01"),
            "display should mention the unknown tag: {rendered}"
        );
        assert!(std::error::Error::source(&unsupported).is_none());
        assert!(unsupported.io_error().is_none());
        assert!(!unsupported.is_out_of_space());

        let lost = SpillError::PriorSpillsLost {
            dir: std::path::PathBuf::from("/tmp/spill-vanished"),
            count: 3,
        };
        let rendered = format!("{lost}");
        assert!(
            rendered.contains("/tmp/spill-vanished") && rendered.contains("3 chunk"),
            "display should mention dir and count: {rendered}"
        );
        assert!(std::error::Error::source(&lost).is_none());
        assert!(lost.io_error().is_none());
        assert!(!lost.is_out_of_space());

        let disabled = SpillError::SpillDisabled;
        let rendered = format!("{disabled}");
        assert!(
            rendered.contains("disabled"),
            "display should mention disabled: {rendered}"
        );
        assert!(std::error::Error::source(&disabled).is_none());
        assert!(disabled.io_error().is_none());
        assert!(!disabled.is_out_of_space());
    }
}
