//! Per-file slot handle returned from [`ParallelDeltaApplier::slot_for`].
//!
//! Extracted from `parallel_apply/mod.rs` as part of the module
//! decomposition. [`SlotHandle`] wraps an [`Arc<SlotBarrier>`] so callers
//! can lock the per-file slot mutex, while a companion
//! [`super::decrement_guard::DecrementGuard`] keeps the slot's in-flight
//! counter accurate for the whole lifetime of the handle (including early
//! returns and panics).
//!
//! [`ParallelDeltaApplier::slot_for`]: super::ParallelDeltaApplier::slot_for

use std::io;
use std::sync::{Arc, MutexGuard};

use super::super::types::FileNdx;
use super::decrement_guard::DecrementGuard;
use super::file_slot::FileSlot;
use super::slot_barrier::SlotBarrier;

/// Handle returned from [`ParallelDeltaApplier::slot_for`].
///
/// Wraps an [`Arc<SlotBarrier>`] so callers can lock the per-file slot
/// mutex via [`SlotHandle::lock_slot`]. The companion [`DecrementGuard`]
/// keeps the slot's in-flight counter accurate for the entire lifetime of
/// the handle, including early returns and panics: the counter increments
/// when [`SlotHandle::new`] runs and decrements when the handle drops.
///
/// The handle deliberately does not expose the bare [`Arc`] - callers go
/// through [`SlotHandle::lock_slot`] so the FFB-1 invariant ("every clone
/// outstanding is reflected in the inflight counter") cannot be bypassed.
///
/// [`ParallelDeltaApplier::slot_for`]: super::ParallelDeltaApplier::slot_for
pub(super) struct SlotHandle {
    barrier: Arc<SlotBarrier>,
    _decrement: DecrementGuard,
}

impl SlotHandle {
    /// Bumps the slot's in-flight counter and returns the handle. The
    /// counter is decremented when the returned handle is dropped.
    ///
    /// DG-3.c routes the [`DecrementGuard`]'s clone through the
    /// adapter's inner `Arc<BarrierState>` (see [`SlotBarrier::barrier`])
    /// so the worker's lingering decrement-guard Arc no longer extends
    /// the payload Arc's strong count past the flusher's
    /// `Arc::try_unwrap`. The handle's own `barrier` field still carries
    /// the [`Arc<SlotBarrier>`] adapter until a future DG-3.x task
    /// retypes [`SlotHandle`].
    ///
    /// [`SlotBarrier::barrier`]: super::slot_barrier::SlotBarrier::barrier
    pub(super) fn new(barrier: Arc<SlotBarrier>) -> Self {
        barrier.increment_inflight();
        let decrement = DecrementGuard {
            barrier: Arc::clone(barrier.barrier()),
        };
        Self {
            barrier,
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
        self.barrier.lock_slot(ndx, kind)
    }
}
