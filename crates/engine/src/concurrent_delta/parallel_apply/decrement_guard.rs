//! RAII decrement guard for the per-slot in-flight counter (SPL-38.c).
//!
//! Extracted from `parallel_apply/mod.rs` as part of the SPL-38 module
//! decomposition; sibling to [`super::slot_barrier::SlotBarrier`]. The
//! guard is the only call site of [`SlotBarrier::decrement_inflight`]
//! and pairs with [`SlotBarrier::increment_inflight`] in
//! [`super::SlotHandle::new`] so the FFB-1 invariant ("every Arc
//! outstanding is reflected in the inflight counter") holds across early
//! returns, `?` propagations, and panics.
//!
//! [`SlotBarrier::decrement_inflight`]: super::slot_barrier::SlotBarrier::decrement_inflight
//! [`SlotBarrier::increment_inflight`]: super::slot_barrier::SlotBarrier::increment_inflight

use std::sync::Arc;

use super::slot_barrier::SlotBarrier;

/// RAII guard returned alongside a [`SlotHandle`] that decrements the
/// per-slot in-flight counter when dropped. Keeping the decrement in a
/// dedicated drop type makes the bookkeeping exception-safe: if the worker
/// panics mid-write or returns early via `?`, the counter still drops
/// back to its pre-handoff value and `flush_workers` unblocks.
///
/// [`SlotHandle`]: super::SlotHandle
pub(super) struct DecrementGuard {
    pub(super) barrier: Arc<SlotBarrier>,
}

impl Drop for DecrementGuard {
    fn drop(&mut self) {
        self.barrier.decrement_inflight();
    }
}
