//! Per-file slot handle returned from [`ParallelDeltaApplier::slot_for`].
//!
//! Extracted from `parallel_apply/mod.rs` as part of the module
//! decomposition. [`SlotHandle`] holds the per-file payload
//! [`Arc<SlotData>`] so callers can lock the per-file slot mutex, while a
//! companion [`super::decrement_guard::DecrementGuard`] keeps the slot's
//! in-flight counter accurate for the whole lifetime of the handle
//! (including early returns and panics).
//!
//! [`ParallelDeltaApplier::slot_for`]: super::ParallelDeltaApplier::slot_for
//! [`Arc<SlotData>`]: std::sync::Arc

use std::io;
use std::sync::{Arc, MutexGuard};

use super::super::types::FileNdx;
use super::decrement_guard::DecrementGuard;
use super::file_slot::FileSlot;
use super::slot_barrier::{SlotData, SlotEntry};

/// Handle returned from [`ParallelDeltaApplier::slot_for`].
///
/// Holds the per-file payload [`Arc<SlotData>`] so callers can lock the
/// per-file slot mutex via [`SlotHandle::lock_slot`]. The companion
/// [`DecrementGuard`] keeps the slot's in-flight counter accurate for the
/// entire lifetime of the handle, including early returns and panics: the
/// counter increments when [`SlotHandle::new`] runs and decrements when
/// the handle drops.
///
/// Field declaration order is load-bearing: [`Self::data`] drops first,
/// releasing the worker's payload [`Arc<SlotData>`] clone, and only then
/// does `_decrement` fire `notify_all`. That ordering keeps the payload
/// Arc's strong-count trajectory disjoint from the notify-bearing
/// [`Arc<super::slot_barrier::BarrierState>`], so `finish_file`'s
/// `Arc::try_unwrap` on the payload never observes the worker's lingering
/// bookkeeping clone (the DG-1 release race).
///
/// The handle deliberately does not expose the bare [`Arc`] - callers go
/// through [`SlotHandle::lock_slot`] so the FFB-1 invariant ("every clone
/// outstanding is reflected in the inflight counter") cannot be bypassed.
///
/// [`ParallelDeltaApplier::slot_for`]: super::ParallelDeltaApplier::slot_for
/// [`Arc<SlotData>`]: std::sync::Arc
/// [`Arc<super::slot_barrier::BarrierState>`]: std::sync::Arc
pub(super) struct SlotHandle {
    /// Per-file payload Arc. Dropped first when the handle goes out of
    /// scope so the worker's clone is gone before `_decrement` fires the
    /// Condvar notify.
    data: Arc<SlotData>,
    _decrement: DecrementGuard,
}

impl SlotHandle {
    /// Bumps the slot's in-flight counter and returns the handle. The
    /// counter is decremented when the returned handle is dropped.
    ///
    /// Consumes a [`SlotEntry`] clone (two `Arc`s). The bookkeeping
    /// [`Arc<super::slot_barrier::BarrierState>`] is handed to the
    /// [`DecrementGuard`] so the worker's lingering decrement-guard Arc
    /// rides a strong-count graph disjoint from the payload
    /// [`Arc<SlotData>`] the flusher unwraps.
    ///
    /// [`Arc<SlotData>`]: std::sync::Arc
    /// [`Arc<super::slot_barrier::BarrierState>`]: std::sync::Arc
    pub(super) fn new(entry: SlotEntry) -> Self {
        let SlotEntry { data, barrier } = entry;
        barrier.increment_inflight();
        let decrement = DecrementGuard { barrier };
        Self {
            data,
            _decrement: decrement,
        }
    }

    /// Locks the per-file [`FileSlot`] for the duration of the returned
    /// guard. The in-flight counter remains held by `self`; the lock
    /// covers only the per-file write critical section.
    pub(super) fn lock_slot(
        &self,
        ndx: FileNdx,
        kind: &'static str,
    ) -> io::Result<MutexGuard<'_, FileSlot>> {
        self.data.lock_slot(ndx, kind)
    }
}
