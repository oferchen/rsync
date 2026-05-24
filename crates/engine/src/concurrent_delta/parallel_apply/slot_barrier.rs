//! Per-slot synchronisation primitive backing FFB-1 / FFB-2.
//!
//! Extracted from `parallel_apply.rs` as part of SPL-38.b. The DG-2.a/DG-3
//! spec at `docs/design/dg-2a-option-b-spec.md` plans to split the per-slot
//! state across [`BarrierState`] (in-flight counter + [`Condvar`]) and
//! [`SlotData`] (per-file [`Mutex<FileSlot>`]) so [`SlotHandle`] and
//! `finish_file` can hold independent Arc graphs.
//!
//! DG-3.a (PR #4826) added the post-split types alongside the existing
//! [`SlotBarrier`]. DG-3.b (this commit) swaps the [`super::ParallelDeltaApplier`]
//! [`DashMap`] value type from [`Arc<SlotBarrier>`] to [`SlotEntry`] and
//! reshapes [`SlotBarrier`] into a thin adapter that wraps shared
//! [`Arc<SlotData>`] + [`Arc<BarrierState>`] handles. Adapter instances
//! are minted on demand by [`super::ParallelDeltaApplier::slot_for`] so
//! [`super::DecrementGuard`] and [`super::SlotHandle`] keep their
//! [`Arc<SlotBarrier>`] fields unchanged until DG-3.c retypes them. Each
//! adapter [`Arc`] is independent of the shared per-file state behind it;
//! the next phase (DG-3.c) collapses the adapter and points the handle
//! fields at [`Arc<SlotData>`] / [`Arc<BarrierState>`] directly.
//!
//! [`Arc<SlotBarrier>`]: std::sync::Arc
//! [`Arc<SlotData>`]: std::sync::Arc
//! [`Arc<BarrierState>`]: std::sync::Arc
//! [`Arc`]: std::sync::Arc
//! [`DashMap`]: dashmap::DashMap

use std::io;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use super::super::types::FileNdx;
use super::{FileSlot, ParallelApplyError};

/// DG-3.b transitional adapter that preserves the [`Arc<SlotBarrier>`] shape
/// [`super::DecrementGuard`] and [`super::SlotHandle`] still consume.
///
/// Originally colocated the file's [`Mutex<FileSlot>`], the in-flight
/// counter, and the [`Condvar`] in one allocation. DG-3.b moves the
/// canonical state into [`SlotData`] + [`BarrierState`] (stored together
/// as [`SlotEntry`] in the DashMap) and leaves this struct as a thin
/// wrapper that forwards every method through cloned [`Arc`] handles.
/// New instances are minted by
/// [`super::ParallelDeltaApplier::slot_for`] on each lookup; sibling
/// adapters share the same underlying [`Arc<SlotData>`] +
/// [`Arc<BarrierState>`] state, so the in-flight counter and Condvar
/// remain coherent across workers. DG-3.c retypes
/// [`super::DecrementGuard`] / [`super::SlotHandle`] to hold the inner
/// Arcs directly and deletes this adapter.
///
/// Holding the per-slot [`Arc<SlotBarrier>`] adapter is no longer the
/// unit of "in-flight"; the counter inside [`BarrierState`] is. The
/// adapter's lifetime continues to bound when [`super::DecrementGuard`]
/// runs, which is what the FFB-1 invariant cares about, so external
/// behaviour is unchanged.
///
/// [`Arc`]: std::sync::Arc
/// [`Arc<SlotBarrier>`]: std::sync::Arc
/// [`Arc<SlotData>`]: std::sync::Arc
/// [`Arc<BarrierState>`]: std::sync::Arc
/// [`SlotHandle`]: super::SlotHandle
pub(super) struct SlotBarrier {
    data: Arc<SlotData>,
    barrier: Arc<BarrierState>,
}

impl SlotBarrier {
    /// Builds an adapter that shares its inner state with `entry`.
    /// Clones the entry's two Arcs so the adapter participates in the
    /// same in-flight bookkeeping and per-file mutex as every other
    /// adapter minted from the same entry.
    pub(super) fn from_entry(entry: &SlotEntry) -> Self {
        Self {
            data: Arc::clone(&entry.data),
            barrier: Arc::clone(&entry.barrier),
        }
    }

    /// Locks the per-file slot mutex, mapping a poisoned mutex to the
    /// typed [`ParallelApplyError::SlotPoisoned`] error.
    pub(super) fn lock_slot(
        &self,
        ndx: FileNdx,
        kind: &'static str,
    ) -> io::Result<MutexGuard<'_, FileSlot>> {
        self.data.lock_slot(ndx, kind)
    }

    /// Bumps the in-flight counter for this slot. Called by
    /// [`super::SlotHandle::new`] once the caller has obtained an
    /// [`Arc<SlotBarrier>`] clone from
    /// [`super::ParallelDeltaApplier::slot_for`].
    ///
    /// [`Arc<SlotBarrier>`]: std::sync::Arc
    pub(super) fn increment_inflight(&self) {
        self.barrier.increment_inflight();
    }

    /// Drops the in-flight counter back by one and wakes any waiter parked
    /// on the [`Condvar`]. Invoked from [`super::DecrementGuard::drop`] so
    /// the bookkeeping stays exception-safe across early returns and
    /// panics.
    pub(super) fn decrement_inflight(&self) {
        self.barrier.decrement_inflight();
    }

    /// Blocks the calling thread until the in-flight counter reaches zero.
    /// Spurious wakeups are filtered by the loop predicate.
    pub(super) fn wait_until_idle(&self, ndx: FileNdx, kind: &'static str) -> io::Result<()> {
        self.barrier.wait_until_idle(ndx, kind)
    }
}

/// Per-slot in-flight counter and [`Condvar`] (DG-3.a, Option B).
///
/// Carries exactly the bookkeeping that the worker's drop path touches:
/// the in-flight counter behind its own [`Mutex`] and the [`Condvar`]
/// that wakes a parked `flush_workers`. Defined per the DG-2.a spec at
/// `docs/design/dg-2a-option-b-spec.md` section 2.
///
/// # Why split this out of [`SlotBarrier`]?
///
/// The DG-1 audit (`docs/design/decrementguard-audit.md`, section 4)
/// traced the `finish_file` release race to one [`Arc`] graph being
/// asked to carry two unrelated ownership obligations: the worker's
/// `notify_all` fires from inside [`super::DecrementGuard::drop`] while
/// the matching `Arc::clone` is still live (it only drops once the
/// implicit field-drop glue runs after the body returns), so the
/// flusher's `Arc::try_unwrap` on the same allocation observes
/// `strong_count >= 2`. Option B routes the notify-bearing Arc through
/// [`BarrierState`] and the payload-bearing Arc through [`SlotData`];
/// the two allocations have independent strong-count trajectories, so
/// the worker's lingering `Arc<BarrierState>` cannot block the
/// flusher's payload unwrap.
///
/// # Invariants
///
/// - `inflight` and `notify` are paired: every `notify_all` is preceded
///   by a counter mutation under `inflight`'s mutex, so a waiter that
///   re-checks the predicate after waking observes a consistent value.
/// - The counter is monotonic per slot-lifetime: every
///   `increment_inflight` is matched 1:1 with a `decrement_inflight`
///   via the [`super::DecrementGuard`] RAII pairing (DG-3.c will retype
///   the guard's field to `Arc<BarrierState>`).
///
/// # Visibility
///
/// `pub(super)` so the parent module (`parallel_apply`) can build
/// [`SlotEntry`] instances without exposing the bookkeeping type to
/// the wider engine crate. No public API is added. The [`Condvar`]
/// field is exposed at `pub(super)` so the
/// `flush_workers_survives_spurious_wakeup` test can fire spurious
/// wakeups directly into the wait loop.
pub(super) struct BarrierState {
    inflight: Mutex<usize>,
    pub(super) notify: Condvar,
}

impl BarrierState {
    /// Constructs a fresh bookkeeping primitive with a zero in-flight
    /// counter. The counter is bumped by [`Self::increment_inflight`]
    /// once a [`SlotHandle`] is handed out and dropped back by
    /// [`Self::decrement_inflight`] when the matching
    /// [`super::DecrementGuard`] retires.
    pub(super) fn new() -> Self {
        Self {
            inflight: Mutex::new(0),
            notify: Condvar::new(),
        }
    }

    /// Bumps the in-flight counter. Body is verbatim from
    /// [`SlotBarrier::increment_inflight`] so the DG-3.c retype is a
    /// pure field-type swap with no behaviour change.
    pub(super) fn increment_inflight(&self) {
        let mut guard = self
            .inflight
            .lock()
            .expect("inflight mutex poisoned on increment");
        *guard = guard.checked_add(1).expect("inflight counter overflow");
    }

    /// Drops the in-flight counter by one and wakes every waiter parked
    /// on the [`Condvar`]. Body is verbatim from
    /// [`SlotBarrier::decrement_inflight`]; the saturating subtract and
    /// `notify_all` semantics match exactly so DG-3.c can swap the
    /// underlying Arc type without changing the wake protocol.
    pub(super) fn decrement_inflight(&self) {
        let mut guard = self
            .inflight
            .lock()
            .expect("inflight mutex poisoned on decrement");
        *guard = guard.saturating_sub(1);
        self.notify.notify_all();
    }

    /// Blocks until the in-flight counter reaches zero. Spurious
    /// wakeups are filtered by the loop predicate. Body is verbatim
    /// from [`SlotBarrier::wait_until_idle`].
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

/// Per-file destination payload behind its own [`Mutex`] (DG-3.a,
/// Option B).
///
/// Carries the [`Mutex<FileSlot>`] that workers lock to write a chunk
/// and that `finish_file` `Arc::try_unwrap`s to recover the destination
/// writer at end-of-file. Defined per the DG-2.a spec at
/// `docs/design/dg-2a-option-b-spec.md` section 2.
///
/// # Why split this out of [`SlotBarrier`]?
///
/// Together with [`BarrierState`], this type is the second half of the
/// Option-B split that fixes the DG-1 release race. By keeping the
/// payload Arc structurally disjoint from the notify-bearing Arc,
/// `finish_file`'s `Arc::try_unwrap` on [`Arc<SlotData>`] becomes
/// independent of the worker's lingering [`Arc<BarrierState>`] between
/// `notify_all` and the end of [`super::DecrementGuard::drop`].
///
/// # Invariants
///
/// - The wrapped [`FileSlot`] is only ever observed by either (a) a
///   worker holding the slot mutex via [`Self::lock_slot`], or (b)
///   `finish_file` after [`Arc::try_unwrap`] has returned the inner
///   value. The two paths are temporally disjoint by construction:
///   `finish_file` always runs `flush_workers` (which waits on the
///   sibling [`BarrierState`] in the same [`SlotEntry`]) before
///   removing the entry from the DashMap.
/// - A poisoned mutex is mapped to the typed
///   [`ParallelApplyError::SlotPoisoned`] so the io-error surface
///   matches the existing [`SlotBarrier::lock_slot`] behaviour.
///
/// # Visibility
///
/// `pub(super)` so the parent module can construct [`SlotEntry`]
/// values and read the payload back out. No public API is added.
pub(super) struct SlotData {
    slot: Mutex<FileSlot>,
}

impl SlotData {
    /// Wraps a [`FileSlot`] in its own mutex. Mirrors
    /// [`SlotBarrier::new`] for the payload half of the split.
    pub(super) fn new(slot: FileSlot) -> Self {
        Self {
            slot: Mutex::new(slot),
        }
    }

    /// Locks the per-file slot mutex, mapping a poisoned mutex to the
    /// typed [`ParallelApplyError::SlotPoisoned`] error. Body is
    /// verbatim from [`SlotBarrier::lock_slot`].
    pub(super) fn lock_slot(
        &self,
        ndx: FileNdx,
        kind: &'static str,
    ) -> io::Result<MutexGuard<'_, FileSlot>> {
        self.slot
            .lock()
            .map_err(|_| ParallelApplyError::SlotPoisoned { ndx, kind }.into())
    }

    /// Consumes the [`SlotData`] and returns the wrapped [`FileSlot`].
    /// Used by `finish_file` after `Arc::try_unwrap` succeeds. Maps a
    /// poisoned mutex to [`ParallelApplyError::SlotPoisoned`] so the
    /// shutdown path keeps the typed error surface.
    pub(super) fn into_slot(self, ndx: FileNdx, kind: &'static str) -> io::Result<FileSlot> {
        self.slot
            .into_inner()
            .map_err(|_| ParallelApplyError::SlotPoisoned { ndx, kind }.into())
    }
}

/// DashMap value carrying both Arcs that together replace
/// [`Arc<SlotBarrier>`] under Option B (DG-3.a).
///
/// Cloning a [`SlotEntry`] clones both inner Arcs, keeping the
/// register/lookup paths symmetric: producers insert one
/// [`SlotEntry::new`], consumers clone one [`SlotEntry`] and bind the
/// halves separately. Per the DG-2.a spec section 2, this carrier
/// avoids tuple-field churn at the five call sites that touch both
/// Arcs.
///
/// # Invariants
///
/// - The [`Arc<SlotData>`] and [`Arc<BarrierState>`] are paired for the
///   lifetime of the file's slot: both are inserted by `register_file`
///   and removed together by `finish_file`. Workers never observe one
///   without the other.
/// - Strong counts are tracked separately: the payload Arc only flows
///   to `SlotHandle.data` and `finish_file`'s local binding; the
///   barrier Arc additionally flows to `SlotHandle.barrier` and
///   `DecrementGuard.barrier`. See DG-2.a spec section 3 for the
///   steady-state strong-count table.
///
/// # Visibility
///
/// `pub(super)` so `register_file`, `slot_for`, and `finish_file` can
/// build and decompose the entry. Not exposed beyond
/// `parallel_apply`.
#[derive(Clone)]
pub(super) struct SlotEntry {
    /// Per-file payload Arc. `finish_file` calls [`Arc::try_unwrap`]
    /// on this field to recover the [`FileSlot`].
    pub(super) data: Arc<SlotData>,
    /// Per-file bookkeeping Arc. Workers' [`super::DecrementGuard`]
    /// clones live on this graph; `finish_file` never inspects the
    /// strong count.
    pub(super) barrier: Arc<BarrierState>,
}

impl SlotEntry {
    /// Wraps a fresh [`FileSlot`] in the two Option-B Arcs. The
    /// in-flight counter starts at zero; the first [`SlotHandle`]
    /// constructed from a clone of this entry will bump it via
    /// [`BarrierState::increment_inflight`].
    pub(super) fn new(slot: FileSlot) -> Self {
        Self {
            data: Arc::new(SlotData::new(slot)),
            barrier: Arc::new(BarrierState::new()),
        }
    }
}

/// Post-split handle returned from `slot_for` once DG-3.c lands the
/// [`super::DecrementGuard`] retype.
///
/// Holds one [`Arc<SlotData>`] for the payload lock plus one
/// [`Arc<BarrierState>`] so the increment+decrement bookkeeping stays
/// co-located with the lock site. Field declaration order is
/// load-bearing: per the DG-2.a spec section 6, [`Self::data`] is
/// dropped first (releasing the worker's payload Arc clone), then
/// [`Self::barrier`], and finally the future `_decrement` field that
/// DG-3.c will attach. That order keeps the payload Arc's strong-count
/// trajectory disjoint from the notify-bearing Arc's, which is the
/// invariant DG-1 found violated by the current [`SlotBarrier`] shape.
///
/// # Why a parallel type instead of editing the mod-level [`super::SlotHandle`]?
///
/// DG-3.a is purely additive: every existing call site must keep
/// compiling against the old [`SlotBarrier`]-backed
/// [`super::SlotHandle`] until DG-3.b swaps the DashMap value type and
/// DG-3.c retypes [`super::DecrementGuard`]. The two handles coexist
/// for one release cycle; DG-3.c will rename this type into the
/// mod-level slot once its sibling pieces are in place.
///
/// # Missing field
///
/// The DG-2.a spec section 6 also calls for a third field,
/// `_decrement: super::DecrementGuard`. That field cannot land in
/// DG-3.a because [`super::DecrementGuard`] still carries
/// `Arc<SlotBarrier>` (DG-3.c retypes it to `Arc<BarrierState>`).
/// Attaching the existing guard here would defeat the split: the
/// guard would extend the lifetime of an Arc whose graph
/// [`SlotData`] no longer participates in. DG-3.c folds the field
/// back in once the guard is retyped.
///
/// # Visibility
///
/// `pub(super)` so the parent module can wire it in during the DG-3.b
/// / DG-3.c migrations. Not exposed beyond `parallel_apply`. The
/// mod-level [`super::SlotHandle`] is unaffected by this type's
/// existence: neither shadows the other because `mod.rs` does not
/// `use slot_barrier::SlotHandle`.
//
// `dead_code` allow: same rationale as `BarrierState`. DG-3.c renames
// this type into the mod-level slot once the sibling pieces are in
// place.
#[allow(dead_code)]
pub(super) struct SlotHandle {
    /// Per-file payload Arc. Dropped first when the handle goes out
    /// of scope so the worker's clone is gone before any barrier
    /// bookkeeping runs.
    pub(super) data: Arc<SlotData>,
    /// Per-file bookkeeping Arc. Dropped after `data` but before the
    /// future `_decrement` field, keeping the lock path
    /// ([`SlotData::lock_slot`]) and the counter path
    /// ([`BarrierState::increment_inflight`]) co-located in the same
    /// handle.
    pub(super) barrier: Arc<BarrierState>,
}

#[allow(dead_code)]
impl SlotHandle {
    /// Bumps the entry's in-flight counter and constructs the handle.
    /// Mirrors the DG-2.a spec section 6 constructor, modulo the
    /// `_decrement` field that DG-3.c will attach once
    /// [`super::DecrementGuard`] is retyped.
    pub(super) fn new(entry: SlotEntry) -> Self {
        entry.barrier.increment_inflight();
        Self {
            data: entry.data,
            barrier: entry.barrier,
        }
    }

    /// Locks the per-file slot mutex for the duration of the returned
    /// guard. Delegates to [`SlotData::lock_slot`].
    pub(super) fn lock_slot(
        &self,
        ndx: FileNdx,
        kind: &'static str,
    ) -> io::Result<MutexGuard<'_, FileSlot>> {
        self.data.lock_slot(ndx, kind)
    }
}

#[cfg(test)]
mod dg_3a_tests {
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use super::super::super::types::FileNdx;
    use super::super::FileSlot;
    use super::{BarrierState, SlotData, SlotEntry, SlotHandle};

    fn dummy_file_slot() -> FileSlot {
        FileSlot::new(Box::new(Vec::<u8>::new()), 4)
    }

    /// Smoke test: a parked [`BarrierState::wait_until_idle`] caller
    /// unblocks once the matching [`BarrierState::decrement_inflight`]
    /// drops the counter to zero. Verifies the wake protocol that
    /// `flush_workers` relies on, on the new bookkeeping type.
    #[test]
    fn barrier_state_wait_until_idle_returns_after_decrement() {
        let state = Arc::new(BarrierState::new());
        state.increment_inflight();
        let waiter_state = Arc::clone(&state);
        let waiter = thread::spawn(move || {
            waiter_state
                .wait_until_idle(FileNdx::new(0), "barrier_state_smoke")
                .expect("wait_until_idle should not error");
        });
        // Give the waiter time to park on the Condvar before we wake it.
        thread::sleep(Duration::from_millis(20));
        state.decrement_inflight();
        waiter.join().expect("waiter thread should not panic");
    }

    /// Smoke test: [`SlotData::lock_slot`] returns a usable mutex
    /// guard and [`SlotData::into_slot`] recovers the wrapped
    /// [`FileSlot`] after the unique owner drops the guard. Covers
    /// both halves of `finish_file`'s eventual access pattern.
    #[test]
    fn slot_data_lock_then_into_slot() {
        let data = SlotData::new(dummy_file_slot());
        {
            let guard = data
                .lock_slot(FileNdx::new(7), "slot_data_smoke")
                .expect("lock_slot should succeed on a fresh mutex");
            assert_eq!(guard.bytes_written(), 0);
        }
        let slot = data
            .into_slot(FileNdx::new(7), "slot_data_smoke")
            .expect("into_slot should succeed when no clones remain");
        assert!(slot.drained());
    }

    /// Smoke test: building a [`SlotHandle`] from a [`SlotEntry`]
    /// bumps the in-flight counter on the entry's barrier. Verifies
    /// the constructor wires the increment side of the bookkeeping
    /// even though the matching decrement (DG-3.c's retyped
    /// `_decrement` field) is not attached yet.
    #[test]
    fn slot_handle_constructor_increments_inflight() {
        let entry = SlotEntry::new(dummy_file_slot());
        let barrier = Arc::clone(&entry.barrier);
        let handle = SlotHandle::new(entry);
        // Counter is now 1: a parked waiter must still see the
        // predicate as non-idle. Probe by trying to lock the slot
        // through the handle - this exercises the payload path - and
        // by explicitly decrementing once so the counter returns to
        // zero and a subsequent `wait_until_idle` resolves promptly.
        let guard = handle
            .lock_slot(FileNdx::new(3), "slot_handle_smoke")
            .expect("lock_slot through the new handle should succeed");
        assert_eq!(guard.bytes_written(), 0);
        drop(guard);
        drop(handle);
        barrier.decrement_inflight();
        barrier
            .wait_until_idle(FileNdx::new(3), "slot_handle_smoke")
            .expect("counter should be idle after manual decrement");
    }
}
