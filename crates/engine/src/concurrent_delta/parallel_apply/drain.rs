//! Per-file drain primitives for the parallel apply scaffold (SPL-38.e).
//!
//! Extracted from `parallel_apply/mod.rs` as part of the SPL-38 module
//! decomposition. Sibling to [`super::slot_barrier`],
//! [`super::decrement_guard::DecrementGuard`], and [`super::batch`]; reuses
//! the per-slot [`super::slot_barrier::SlotEntry`] map maintained by
//! [`ParallelDeltaApplier`]. DG-3.b (#2569) swapped the DashMap value
//! type from `Arc<SlotBarrier>` to `SlotEntry`; this module now clones
//! `entry.barrier` ([`std::sync::Arc<super::slot_barrier::BarrierState>`])
//! for the FFB-2 wait and unwraps `entry.data`
//! ([`std::sync::Arc<super::slot_barrier::SlotData>`]) for the
//! end-of-file writer reclaim.
//!
//! # Contract
//!
//! The two entry points are tightly paired:
//!
//! * [`ParallelDeltaApplier::flush_workers`] parks the caller on the slot's
//!   [`std::sync::Condvar`] until the per-slot in-flight counter is observed
//!   to be zero, mirroring the FFB-2 barrier wait.
//! * [`ParallelDeltaApplier::finish_file`] bakes that barrier in front of the
//!   [`std::sync::Arc::try_unwrap`] used to reclaim the destination writer,
//!   so callers never have to sequence the wait + reclaim themselves.
//!
//! Both honour the BR-3j.c shard-discipline contract: the DashMap shard
//! guard is dropped before the blocking wait so unrelated `FileNdx` values
//! continue to make progress while a single file drains.

use std::io::Write;
use std::sync::Arc;

use super::super::types::FileNdx;
use super::slot_barrier::SlotEntry;
use super::{ParallelApplyError, ParallelDeltaApplier};

impl ParallelDeltaApplier {
    /// Finalises a file's writer once every submitted chunk has applied.
    ///
    /// Returns the destination writer so the caller can run its own
    /// finalisation step (checksum verify, temp-file rename, metadata
    /// apply). Errors if any chunks remain buffered awaiting a missing
    /// `chunk_sequence`.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if `ndx` is unknown, the slot is still
    /// referenced by another caller, the slot mutex is poisoned, or the
    /// per-file reorder buffer still holds undelivered chunks.
    pub fn finish_file(&self, ndx: impl Into<FileNdx>) -> std::io::Result<Box<dyn Write + Send>> {
        let ndx = ndx.into();
        // FFB-1 Option D: bake the barrier into `finish_file` so callers
        // never have to sequence `flush_workers` + `finish_file`
        // themselves. The barrier waits for every outstanding
        // `SlotHandle` clone (the unit of "in-flight" worker) to drop
        // before we attempt the `Arc::try_unwrap` below. The lookup is
        // a no-op if the slot is already absent, but here we know it
        // exists or we will surface "unknown" on the subsequent remove.
        self.flush_workers(ndx)?;
        // `DashMap::remove` returns the owned `(K, V)` and drops the shard
        // guard immediately; the `Arc::try_unwrap` work below happens
        // outside the shard lock.
        let (_, entry) = self
            .files
            .remove(&ndx)
            .ok_or_else(|| std::io::Error::other(format!("parallel applier file {ndx} unknown")))?;
        // DG-3.b retargets the spin/unwrap from the (now-removed) single
        // `Arc<SlotBarrier>` to the entry's payload Arc. Drop the
        // bookkeeping Arc first so any leftover `DecrementGuard` clones
        // are the only `BarrierState` strong references left; the
        // payload Arc's strong-count trajectory is now what
        // `try_unwrap` reasons about.
        let SlotEntry { data, barrier } = entry;
        drop(barrier);
        // Post-barrier release-race window: `flush_workers` waits for
        // `inflight==0` via the Condvar, which fires from
        // `DecrementGuard::drop` *before* the guard's own adapter Arc
        // has been released (the notify happens inside the drop body;
        // the inner Arcs only drop after the body returns). The window
        // is typically nanoseconds but is reliably observable on Windows
        // under load. Spin-then-yield until the worker's drop completes;
        // the worker is past the notify and its drop fn is just about
        // to return so the wait is bounded. DG-3.c will retire the
        // adapter and let DG-4 delete the spin entirely.
        let mut spin = 0u32;
        while Arc::strong_count(&data) > 1 {
            spin = spin.saturating_add(1);
            if spin >= 1_000 {
                // Past the typical drop window - surface the typed error
                // so a real bug (e.g. caller raced a new `slot_for`
                // against `finish_file`) does not hide forever.
                return Err(ParallelApplyError::ApplierStillReferenced {
                    ndx,
                    strong_count: Arc::strong_count(&data),
                    kind: "finish_file",
                }
                .into());
            }
            if spin < 32 {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
        let slot_data = Arc::try_unwrap(data).map_err(|still_shared| {
            ParallelApplyError::ApplierStillReferenced {
                ndx,
                strong_count: Arc::strong_count(&still_shared),
                kind: "finish_file",
            }
        })?;
        let slot = slot_data.into_slot(ndx, "finish_file")?;
        if !slot.drained() {
            return Err(ParallelApplyError::UndrainedChunks {
                ndx,
                buffered: slot.reorder.buffered_count(),
                kind: "finish_file",
            }
            .into());
        }
        Ok(slot.writer)
    }

    /// Blocks the calling thread until every outstanding [`SlotHandle`]
    /// for `ndx` has been dropped.
    ///
    /// Each call to [`Self::apply_one_chunk`] or
    /// [`Self::apply_batch_parallel`] obtains a [`SlotHandle`] from
    /// [`Self::slot_for`] that bumps the slot's in-flight counter for the
    /// duration of the call (decrement on drop). `flush_workers` parks on
    /// the slot's [`std::sync::Condvar`] until that counter is observed to be zero.
    /// Spurious wakeups are filtered by the wait-while predicate.
    ///
    /// Returns [`Ok`] immediately if `ndx` is not registered (or has
    /// already been finalised through [`Self::finish_file`]); the absence
    /// of a slot is the same observable outcome as a fully-drained slot.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] only if the slot's in-flight mutex was
    /// poisoned by a panicking worker. In that case the typed
    /// [`ParallelApplyError::SlotPoisoned`] variant carries the offending
    /// `ndx` and the `"flush_workers"` call-site tag.
    ///
    /// [`SlotHandle`]: super::SlotHandle
    /// [`Self::slot_for`]: super::ParallelDeltaApplier
    pub fn flush_workers(&self, ndx: impl Into<FileNdx>) -> std::io::Result<()> {
        let ndx = ndx.into();
        // Look up the slot, clone the `Arc<BarrierState>` from the
        // entry, drop the shard guard before waiting. This keeps the
        // DashMap shard available to other NDX values while the caller
        // blocks on the slot's own condvar, preserving the BR-3j.c
        // shard-discipline contract.
        let barrier = match self.files.get(&ndx) {
            Some(guard) => Arc::clone(&guard.value().barrier),
            None => return Ok(()),
        };
        barrier.wait_until_idle(ndx, "flush_workers")
    }
}
