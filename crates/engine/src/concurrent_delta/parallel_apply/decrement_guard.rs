//! RAII decrement guard for the per-slot in-flight counter (SPL-38.c).
//!
//! Extracted from `parallel_apply/mod.rs` as part of the SPL-38 module
//! decomposition; sibling to [`super::slot_barrier::BarrierState`]. The
//! guard is the only call site of [`BarrierState::decrement_inflight`]
//! and pairs with [`BarrierState::increment_inflight`] (invoked from
//! [`super::SlotHandle::new`] via the [`super::slot_barrier::SlotBarrier`]
//! adapter) so the FFB-1 invariant ("every Arc outstanding is reflected
//! in the inflight counter") holds across early returns, `?`
//! propagations, and panics.
//!
//! DG-3.c retyped the guard's `barrier` field from `Arc<SlotBarrier>`
//! to `Arc<BarrierState>` per the DG-2.a Option-B spec
//! (`docs/design/dg-2a-option-b-spec.md` section 2). The payload Arc
//! ([`super::slot_barrier::SlotData`]) and the notify-bearing Arc
//! ([`BarrierState`]) now have independent strong-count trajectories,
//! so the worker's lingering decrement-guard Arc no longer extends the
//! payload Arc's strong count past the flusher's `Arc::try_unwrap`. The
//! `SlotHandle` retype is deferred to a later DG-3.x task; the
//! [`super::slot_barrier::SlotBarrier`] adapter stays as a bridge until
//! then.
//!
//! [`BarrierState`]: super::slot_barrier::BarrierState
//! [`BarrierState::decrement_inflight`]: super::slot_barrier::BarrierState::decrement_inflight
//! [`BarrierState::increment_inflight`]: super::slot_barrier::BarrierState::increment_inflight

use std::sync::Arc;

use super::slot_barrier::BarrierState;

/// RAII guard returned alongside a [`SlotHandle`] that decrements the
/// per-slot in-flight counter when dropped. Keeping the decrement in a
/// dedicated drop type makes the bookkeeping exception-safe: if the worker
/// panics mid-write or returns early via `?`, the counter still drops
/// back to its pre-handoff value and `flush_workers` unblocks.
///
/// The field holds [`Arc<BarrierState>`] (DG-3.c) rather than the
/// previous `Arc<SlotBarrier>` adapter so the worker's lingering clone
/// on drop lives on a strong-count graph disjoint from the payload Arc
/// the flusher unwraps. See `docs/design/dg-2a-option-b-spec.md`
/// section 2 for the wider rationale.
///
/// [`SlotHandle`]: super::SlotHandle
/// [`Arc<BarrierState>`]: std::sync::Arc
pub(super) struct DecrementGuard {
    pub(super) barrier: Arc<BarrierState>,
}

impl Drop for DecrementGuard {
    fn drop(&mut self) {
        self.barrier.decrement_inflight();
    }
}
