//! Per-slot synchronisation primitives backing FFB-1 / FFB-2.
//!
//! Extracted from `parallel_apply.rs` as part of SPL-38.b. The per-slot
//! state is split across [`BarrierState`] (in-flight counter +
//! [`Condvar`]) and [`SlotData`] (per-file [`Mutex<FileSlot>`]), stored
//! together as a [`SlotEntry`] in the [`super::ParallelDeltaApplier`]
//! [`DashMap`]. The split lets [`super::SlotHandle`] (the payload
//! [`Arc<SlotData>`]) and `finish_file`'s writer reclaim
//! ([`Arc::try_unwrap`] on the same payload Arc) hold independent Arc
//! graphs from the notify-bearing [`Arc<BarrierState>`] the
//! [`super::DecrementGuard`] carries, per the Option-B spec at
//! `docs/design/dg-2a-option-b-spec.md`.
//!
//! [`Arc<SlotData>`]: std::sync::Arc
//! [`Arc<BarrierState>`]: std::sync::Arc
//! [`DashMap`]: dashmap::DashMap

use std::io;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use super::super::types::FileNdx;
use super::{FileSlot, ParallelApplyError};

/// Per-slot in-flight counter and [`Condvar`] (DG-3.a, Option B).
///
/// Carries exactly the bookkeeping that the worker's drop path touches:
/// the in-flight counter behind its own [`Mutex`] and the [`Condvar`]
/// that wakes a parked `flush_workers`. Defined per the DG-2.a spec at
/// `docs/design/dg-2a-option-b-spec.md` section 2.
///
/// # Why a dedicated bookkeeping type?
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
///   via the [`super::DecrementGuard`] RAII pairing. The guard holds an
///   [`Arc<BarrierState>`] so the pairing travels on this allocation
///   directly.
///
/// [`Arc<BarrierState>`]: std::sync::Arc
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
    /// once a [`super::handle::SlotHandle`] is handed out and dropped
    /// back by [`Self::decrement_inflight`] when the matching
    /// [`super::DecrementGuard`] retires.
    pub(super) fn new() -> Self {
        Self {
            inflight: Mutex::new(0),
            notify: Condvar::new(),
        }
    }

    /// Bumps the in-flight counter under its mutex. Called from
    /// [`super::handle::SlotHandle::new`] when a slot handle is handed
    /// out.
    pub(super) fn increment_inflight(&self) {
        let mut guard = self
            .inflight
            .lock()
            .expect("inflight mutex poisoned on increment");
        *guard = guard.checked_add(1).expect("inflight counter overflow");
    }

    /// Drops the in-flight counter by one and wakes every waiter parked
    /// on the [`Condvar`]. The saturating subtract guards against a
    /// poisoned-then-rebuilt path that observes zero: this is internal
    /// bookkeeping, not a security invariant, so a panic here would
    /// only mask a real worker bug. `notify_all` is fired
    /// unconditionally so any waiter parked on the condvar gets a
    /// chance to re-evaluate the predicate; spurious wakeups are
    /// filtered by [`Self::wait_until_idle`]'s `wait_while` loop.
    ///
    /// Invoked exclusively from [`super::DecrementGuard::drop`].
    pub(super) fn decrement_inflight(&self) {
        let mut guard = self
            .inflight
            .lock()
            .expect("inflight mutex poisoned on decrement");
        *guard = guard.saturating_sub(1);
        self.notify.notify_all();
    }

    /// Blocks until the in-flight counter reaches zero. Spurious
    /// wakeups are filtered by the loop predicate.
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
/// # Why a dedicated payload type?
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
///   [`ParallelApplyError::SlotPoisoned`] so the io-error surface is
///   consistent across the lock and reclaim paths.
///
/// # Visibility
///
/// `pub(super)` so the parent module can construct [`SlotEntry`]
/// values and read the payload back out. No public API is added.
pub(super) struct SlotData {
    slot: Mutex<FileSlot>,
}

impl SlotData {
    /// Wraps a [`FileSlot`] in its own mutex.
    pub(super) fn new(slot: FileSlot) -> Self {
        Self {
            slot: Mutex::new(slot),
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

/// DashMap value carrying the two per-slot Arcs under Option B (DG-3.a).
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
///   barrier Arc flows to `DecrementGuard.barrier` and `flush_workers`'s
///   local wait clone. See DG-2.a spec section 3 for the steady-state
///   strong-count table.
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
    /// in-flight counter starts at zero; the first
    /// [`super::handle::SlotHandle`] constructed from a clone of this
    /// entry will bump it via [`BarrierState::increment_inflight`].
    pub(super) fn new(slot: FileSlot) -> Self {
        Self {
            data: Arc::new(SlotData::new(slot)),
            barrier: Arc::new(BarrierState::new()),
        }
    }
}

#[cfg(test)]
mod dg_3a_tests {
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use super::super::super::types::FileNdx;
    use super::super::FileSlot;
    use super::super::handle::SlotHandle;
    use super::{BarrierState, SlotData, SlotEntry};

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

    /// Smoke test: building a [`SlotHandle`] from a [`SlotEntry`] bumps
    /// the in-flight counter, and dropping the handle returns it to zero
    /// via the companion `DecrementGuard`. Verifies the collapsed handle
    /// wires both the increment (construction) and decrement (drop)
    /// sides of the bookkeeping so a parked `wait_until_idle` unblocks.
    #[test]
    fn slot_handle_increments_then_decrements_on_drop() {
        let entry = SlotEntry::new(dummy_file_slot());
        let barrier = Arc::clone(&entry.barrier);
        let handle = SlotHandle::new(entry);
        // Counter is now 1. Probe the payload path by locking the slot
        // through the handle.
        let guard = handle
            .lock_slot(FileNdx::new(3), "slot_handle_smoke")
            .expect("lock_slot through the handle should succeed");
        assert_eq!(guard.bytes_written(), 0);
        drop(guard);
        // Dropping the handle fires the DecrementGuard, returning the
        // counter to zero so `wait_until_idle` resolves promptly without
        // any manual decrement.
        drop(handle);
        barrier
            .wait_until_idle(FileNdx::new(3), "slot_handle_smoke")
            .expect("counter should be idle after handle drop");
    }
}
