//! Reusable accumulation buffer and bounce-copy telemetry for [`super::IocpDiskBatch`].
//!
//! Splits the buffer concern out of the main writer so the page-aligned arena
//! integration from WPG-4 lives next to the alignment-aware accessors that
//! `WriteFile` chunking depends on. The bounce-copy counter is co-located
//! because the only place it increments is the aligned-submission path that
//! consumes this buffer.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::page_aligned::PageAlignedBuffer;

/// Reusable accumulation buffer for batched writes.
///
/// `IocpDiskBatch` stages caller data here before splitting it into chunks
/// for overlapped submission. The buffer comes from one of two arenas:
///
/// - [`BatchBuffer::Vec`]: standard heap allocation, no alignment guarantees.
///   Used when buffered I/O is in effect.
/// - [`BatchBuffer::PageAligned`]: page-aligned allocation backed by
///   [`PageAlignedBuffer`]. Used when `IocpConfig::unbuffered` is set so
///   chunks handed to `WriteFile` on the no-buffering handle do not force
///   the kernel into an aligned-scratch bounce copy.
pub(super) enum BatchBuffer {
    /// Standard heap-allocated buffer (no alignment guarantee).
    Vec(Vec<u8>),
    /// Page-aligned buffer for `FILE_FLAG_NO_BUFFERING` submissions.
    PageAligned(PageAlignedBuffer),
}

impl BatchBuffer {
    pub(super) fn len(&self) -> usize {
        match self {
            Self::Vec(v) => v.len(),
            Self::PageAligned(b) => b.capacity(),
        }
    }

    pub(super) fn as_slice(&self) -> &[u8] {
        match self {
            Self::Vec(v) => v.as_slice(),
            Self::PageAligned(b) => b.as_slice(),
        }
    }

    pub(super) fn as_mut_slice(&mut self) -> &mut [u8] {
        match self {
            Self::Vec(v) => v.as_mut_slice(),
            Self::PageAligned(b) => b.as_mut_slice(),
        }
    }

    pub(super) fn is_page_aligned(&self) -> bool {
        matches!(self, Self::PageAligned(_))
    }
}

/// Process-wide telemetry: total bounce-buffer copies the IOCP write path
/// has avoided by submitting page-aligned buffers to no-buffering handles.
///
/// Exposed via [`bounce_copies_avoided`] for benchmark and status output.
/// Pure counter; safe to read from any thread.
pub(super) static BOUNCE_COPIES_AVOIDED: AtomicU64 = AtomicU64::new(0);

/// Returns the cumulative count of bounce-buffer copies avoided by the
/// page-aligned IOCP write path since process start.
///
/// Each increment corresponds to a single `WriteFile` submission that used
/// a page-aligned buffer on a `FILE_FLAG_NO_BUFFERING` handle, sparing the
/// kernel from allocating an aligned scratch buffer and memcpying the
/// caller's data into it before issuing the I/O.
#[must_use]
pub fn bounce_copies_avoided() -> u64 {
    BOUNCE_COPIES_AVOIDED.load(Ordering::Relaxed)
}

/// Resets the process-wide bounce-copy counter to zero.
///
/// Used by tests to isolate observation windows. Production code should
/// treat the counter as monotonic.
#[doc(hidden)]
pub fn reset_bounce_copies_avoided_for_test() {
    BOUNCE_COPIES_AVOIDED.store(0, Ordering::SeqCst);
}
