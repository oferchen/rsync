//! Per-slot synchronisation primitive backing FFB-1 / FFB-2.
//!
//! Extracted from `parallel_apply.rs` as part of SPL-38.b. Owns the
//! [`Mutex<FileSlot>`] payload plus the in-flight worker counter and
//! [`Condvar`] that back `flush_workers(ndx)`. The DG-2.a/DG-3 spec at
//! `docs/design/dg-2a-option-b-spec.md` plans to rename this type to
//! `BarrierState` and split the slot payload out; until that lands the
//! current shape is preserved verbatim.

use std::io;
use std::sync::{Condvar, Mutex, MutexGuard};

use super::super::types::FileNdx;
use super::{FileSlot, ParallelApplyError};

/// Per-slot barrier primitive backing FFB-1 / FFB-2.
///
/// Colocates the file's [`Mutex<FileSlot>`] with an in-flight worker
/// counter and a [`Condvar`] so `flush_workers(ndx)` can block the caller
/// until every outstanding [`SlotHandle`] for `ndx` has been dropped. The
/// counter sits behind its own [`Mutex`] so the barrier wait never
/// contends with the per-file write critical section: workers take the
/// slot mutex to write, and the counter mutex only to bump or decrement.
///
/// Holding the per-slot [`Arc`] is the unit of "in-flight"; the counter
/// tracks how many of those `Arc` clones are currently outstanding so the
/// `Condvar` can fire deterministically the moment the last clone drops.
///
/// [`Arc`]: std::sync::Arc
/// [`SlotHandle`]: super::SlotHandle
pub(super) struct SlotBarrier {
    pub(super) slot: Mutex<FileSlot>,
    pub(super) inflight: Mutex<usize>,
    pub(super) notify: Condvar,
}

impl SlotBarrier {
    pub(super) fn new(slot: FileSlot) -> Self {
        Self {
            slot: Mutex::new(slot),
            inflight: Mutex::new(0),
            notify: Condvar::new(),
        }
    }

    /// Locks the per-file slot mutex, mapping a poisoned mutex to the
    /// typed [`ParallelApplyError::SlotPoisoned`] error.
    pub(super) fn lock_slot(
        &self,
        ndx: FileNdx,
        kind: &'static str,
    ) -> io::Result<MutexGuard<'_, FileSlot>> {
        self.slot
            .lock()
            .map_err(|_| ParallelApplyError::SlotPoisoned { ndx, kind }.into())
    }

    /// Bumps the in-flight counter for this slot. Called by [`SlotHandle::new`]
    /// once the caller has obtained an [`Arc<SlotBarrier>`] clone from
    /// [`ParallelDeltaApplier::slot_for`].
    ///
    /// [`Arc<SlotBarrier>`]: std::sync::Arc
    /// [`SlotHandle::new`]: super::SlotHandle::new
    /// [`ParallelDeltaApplier::slot_for`]: super::ParallelDeltaApplier::slot_for
    pub(super) fn increment_inflight(&self) {
        let mut guard = self
            .inflight
            .lock()
            .expect("inflight mutex poisoned on increment");
        *guard = guard.checked_add(1).expect("inflight counter overflow");
    }

    /// Drops the in-flight counter back by one and wakes any waiter parked
    /// on the [`Condvar`]. Invoked from [`DecrementGuard::drop`] so the
    /// bookkeeping stays exception-safe across early returns and panics.
    ///
    /// [`DecrementGuard::drop`]: super::DecrementGuard
    pub(super) fn decrement_inflight(&self) {
        let mut guard = self
            .inflight
            .lock()
            .expect("inflight mutex poisoned on decrement");
        // Saturating subtract: a poisoned-then-rebuilt path that observes
        // zero must not panic the worker on its way out. The counter is
        // an internal bookkeeping primitive, not a security invariant.
        *guard = guard.saturating_sub(1);
        // Wake every waiter; `flush_workers` re-checks the predicate under
        // the mutex so spurious wakeups are harmless.
        self.notify.notify_all();
    }

    /// Blocks the calling thread until the in-flight counter reaches zero.
    /// Spurious wakeups are filtered by the loop predicate.
    pub(super) fn wait_until_idle(&self, ndx: FileNdx, kind: &'static str) -> io::Result<()> {
        let guard = self
            .inflight
            .lock()
            .map_err(|_| ParallelApplyError::SlotPoisoned { ndx, kind })?;
        let _final = self
            .notify
            .wait_while(guard, |inflight| *inflight > 0)
            .map_err(|_| ParallelApplyError::SlotPoisoned { ndx, kind })?;
        Ok(())
    }
}
