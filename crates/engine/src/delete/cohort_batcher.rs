//! Cohort batching wire-up over the DEL-2.a re-ordering buffer.
//!
//! This module implements the batching strategy specified by
//! `docs/design/del-1c-cohort-batching-strategy.md` on top of the
//! DEL-2.a [`super::ReorderBuffer`] primitive. It is the DEL-2.b
//! deliverable: a synchronous consumer-side adapter that surfaces
//! sealed cohorts in strict wire-ordering rank order, capped at
//! `DRAIN_BATCH_CAP` per batch, with the panic-isolation rules
//! from DEL-1.c section 6 layered in. The parallel consumer flag flip
//! and `Condvar`-driven scope wiring are DEL-2.c.
//!
//! # Cohort key choice
//!
//! Per DEL-1.c section 1, the producer/consumer cohort is the
//! per-destination-parent-directory unit, identified by
//! [`super::DeleteCohortKey`] (a [`std::path::PathBuf`] wrapper). The
//! stable wire-ordering rank is the dense pre-order index assigned by
//! [`super::DirTraversalCursor`]. Per DEL-1.c section 4 the index is a
//! flat [`u64`] so INC_RECURSE segments can flatten
//! `(segment_idx, dir_idx_in_segment)` via `SEGMENT_STRIDE = 1 << 20`
//! without changing the buffer's slot identity.
//!
//! # Why a separate adapter
//!
//! The DEL-2.a primitive exposes the [`super::ReorderBuffer::insert`],
//! [`super::ReorderBuffer::seal`], and
//! [`super::ReorderBuffer::try_drain_ready`] surface as a pure data
//! structure. DEL-2.b wraps that surface with three behaviours the
//! consumer needs but the buffer deliberately leaves out:
//!
//! 1. A single-call `enqueue_cohort` that inserts all of a cohort's
//!    operations under one key/rank and seals it atomically, matching
//!    the "rayon producer owns the cohort end-to-end" decomposition
//!    from DEL-1.c section 3.1.
//! 2. A `drain_batch` that returns a strongly-typed [`CohortBatch`]
//!    grouping the surfaced cohorts under their original keys and
//!    ranks, so the consumer can dispatch them through `DeleteFs`
//!    without re-walking the buffer.
//! 3. A producer-panic latch keyed per DEL-1.c section 6 so the
//!    consumer can bail at the first panicked cohort in a drain batch
//!    rather than only at wake-up start.
//!
//! # Default-byte-identity property
//!
//! The DEL-2.b adapter is dormant by default: the production
//! [`super::emitter::DeleteEmitter`] is unchanged (DEL-2.c flips the
//! flag that routes the receiver through this adapter). With the
//! adapter unused, the sequential emitter's syscall trace and on-wire
//! `MSG_DELETED` / `NDX_DEL_STATS` order are byte-for-byte unchanged.
//!
//! Even when the adapter is exercised with a cohort count of 1 (the
//! collapsed sequential analogue), [`CohortBatcher::drain_batch`] yields
//! a [`CohortBatch`] with exactly one entry whose ops are in producer-
//! insertion order, matching what the existing emitter would have
//! dispatched for the same plan.
//!
//! # Upstream reference
//!
//! - `target/interop/upstream-src/rsync-3.4.1/delete.c:130-225`
//!   (`delete_item`): per-cohort dispatch order the batcher preserves.
//! - `target/interop/upstream-src/rsync-3.4.1/generator.c:272-347`
//!   (`delete_in_dir`): one cohort per destination parent directory.

use std::sync::atomic::{AtomicBool, Ordering};

use super::reorder_buffer::{DeleteCohortKey, DeleteOperation, ReorderBuffer, ReorderBufferError};

/// One sealed cohort surfaced by [`CohortBatcher::drain_batch`].
///
/// Carries the per-parent-dir key, the wire-ordering rank, and the
/// FIFO-ordered [`DeleteOperation`] slice the consumer dispatches.
/// Cloning the key matches the cheap-clone contract documented on
/// [`DeleteCohortKey`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CohortBatchEntry {
    /// Destination-relative parent directory key.
    pub key: DeleteCohortKey,
    /// Stable wire-ordering rank used by the consumer for monotonic
    /// dispatch ordering.
    pub rank: u64,
    /// Pending operations in producer-insertion order; matches the
    /// upstream `delete_in_dir` reverse-directory order.
    pub ops: Vec<DeleteOperation>,
}

impl CohortBatchEntry {
    /// Returns the number of operations the consumer must dispatch
    /// for this cohort.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Returns `true` when the cohort holds no operations (the
    /// panic-recovery empty-cohort shape from DEL-1.c section 6).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// A contiguous-by-rank group of sealed cohorts the consumer drains in
/// one wake-up.
///
/// The vector is bounded by `DRAIN_BATCH_CAP` entries (DEL-1.c section
/// 3.2's `CONSUMER_DRAIN_BATCH_CAP = 8`). Entries are in strictly
/// increasing rank order; an empty batch means no cohort at the head
/// is sealed and the consumer should park for a producer wake-up.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CohortBatch {
    entries: Vec<CohortBatchEntry>,
}

impl CohortBatch {
    /// Constructs an empty batch.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Borrows the surfaced cohorts in strict rank order.
    #[must_use]
    pub fn entries(&self) -> &[CohortBatchEntry] {
        &self.entries
    }

    /// Returns the number of cohorts in the batch.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` when the batch surfaced no cohorts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Consumes the batch and yields the entries by value so the
    /// consumer can move each cohort's ops into its dispatch loop.
    #[must_use]
    pub fn into_entries(self) -> Vec<CohortBatchEntry> {
        self.entries
    }
}

/// Synchronous adapter that wires DEL-1.c's cohort batching strategy
/// onto a DEL-2.a [`ReorderBuffer`].
///
/// The adapter is single-threaded for DEL-2.b: it owns the buffer and
/// surfaces every producer/consumer operation as a direct method call.
/// DEL-2.c layers a [`std::sync::Condvar`] scope on top so multiple
/// rayon producers and a single consumer thread can share one adapter
/// instance.
///
/// # Invariants
///
/// - Every [`Self::enqueue_cohort`] call records exactly one cohort:
///   one insert per op (or one [`ReorderBuffer::register_empty`] for the
///   zero-op case) followed by one seal.
/// - [`Self::drain_batch`] surfaces at most `DRAIN_BATCH_CAP` cohorts
///   per call in strictly increasing rank order, mirroring DEL-1.c
///   section 3.2.
/// - A producer-side panic recorded via [`Self::record_panic`] is
///   surfaced to the consumer through [`Self::is_panicked`]; the
///   recommended consumer pattern is to call [`Self::is_panicked`]
///   between dispatches inside one drained batch, per DEL-1.c section 6.
#[derive(Debug, Default)]
pub struct CohortBatcher {
    buffer: ReorderBuffer,
    panicked: AtomicBool,
}

impl CohortBatcher {
    /// Constructs an empty batcher backed by a fresh
    /// [`ReorderBuffer`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of cohorts currently buffered (sealed or not).
    ///
    /// Mirrors [`ReorderBuffer::len`] so DEL-2.c can drive its
    /// `Condvar` predicates without piercing the abstraction.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Returns `true` when the batcher holds no cohorts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Returns `true` when the batcher cannot accept a new cohort key
    /// without triggering [`ReorderBufferError::BufferFull`].
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.buffer.is_full()
    }

    /// Returns the rank of the head cohort, mirroring
    /// [`ReorderBuffer::head_rank`].
    #[must_use]
    pub fn head_rank(&self) -> Option<u64> {
        self.buffer.head_rank()
    }

    /// Returns `true` when the next [`Self::drain_batch`] call would
    /// surface at least one cohort, mirroring
    /// [`ReorderBuffer::head_is_ready`].
    #[must_use]
    pub fn head_is_ready(&self) -> bool {
        self.buffer.head_is_ready()
    }

    /// Records a complete cohort end-to-end and seals it for drain.
    ///
    /// This is the producer-side single-call API: one cohort key, one
    /// rank, and the cohort's operations in producer-insertion order
    /// (the upstream `delete_in_dir` reverse-directory order from
    /// DEL-1.b section 3.1). The call materialises the cohort with one
    /// pass over `ops` and seals it before returning, so the consumer
    /// can drain it on the next [`Self::drain_batch`] wake-up.
    ///
    /// An empty `ops` slice records the cohort via
    /// [`ReorderBuffer::register_empty`], matching the panic-recovery
    /// empty-cohort path from DEL-1.c section 6 so the wire stream sees
    /// the cohort boundary even when the producer had no extras to emit.
    ///
    /// # Errors
    ///
    /// - [`ReorderBufferError::BufferFull`] when adding a new cohort
    ///   would push the buffer past [`super::MAX_BUFFERED_COHORTS`].
    /// - [`ReorderBufferError::RankConflict`] when `key` is already
    ///   buffered under a different rank (a producer/consumer ordering
    ///   bug).
    pub fn enqueue_cohort(
        &mut self,
        key: DeleteCohortKey,
        rank: u64,
        ops: Vec<DeleteOperation>,
    ) -> Result<(), ReorderBufferError> {
        if ops.is_empty() {
            self.buffer.register_empty(key.clone(), rank)?;
        } else {
            let mut iter = ops.into_iter();
            let first = iter
                .next()
                .expect("non-empty ops vec has at least one entry");
            self.buffer.insert(key.clone(), rank, first)?;
            for op in iter {
                // Re-inserting under an already-buffered key cannot
                // trigger BufferFull (the slot is allocated) and
                // cannot trigger RankConflict (rank matches the first
                // insert's). Surface unexpected errors verbatim.
                self.buffer.insert(key.clone(), rank, op)?;
            }
        }
        self.buffer.seal(&key)
    }

    /// Drains up to `DRAIN_BATCH_CAP` contiguous-by-rank sealed
    /// cohorts into a [`CohortBatch`].
    ///
    /// Returns an empty batch when the head cohort is unsealed; the
    /// consumer should park on a producer wake-up and retry. Surfaced
    /// cohorts are removed from the underlying buffer, freeing slot
    /// capacity for in-flight producers.
    ///
    /// The strict cohort-order rule from DEL-1.b section 2.3 carries
    /// through unchanged: the drain stops at the first unsealed cohort
    /// even if later sealed cohorts are pending.
    #[must_use]
    pub fn drain_batch(&mut self) -> CohortBatch {
        let drained = self.buffer.try_drain_ready_with_meta();
        let entries = drained
            .into_iter()
            .map(|(key, rank, ops)| CohortBatchEntry { key, rank, ops })
            .collect();
        CohortBatch { entries }
    }

    /// Latches the producer-panicked flag so the consumer observes it
    /// per DEL-1.c section 6.
    ///
    /// The flag is sticky: subsequent calls to [`Self::is_panicked`]
    /// return `true` for the lifetime of the batcher. DEL-2.c's
    /// consumer thread reads it between dispatches inside one drained
    /// batch and bails at the first panicked cohort rather than at
    /// wake-up start.
    pub fn record_panic(&self) {
        self.panicked.store(true, Ordering::Release);
    }

    /// Returns `true` when [`Self::record_panic`] has been called.
    ///
    /// The acquire ordering pairs with [`Self::record_panic`]'s
    /// release so producer-side panic state is visible to a consumer
    /// running on a different thread once DEL-2.c wires this through
    /// the parallel scope.
    #[must_use]
    pub fn is_panicked(&self) -> bool {
        self.panicked.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::super::plan::DeleteEntryKind;
    use super::super::reorder_buffer::DRAIN_BATCH_CAP;
    use super::*;

    fn key(path: &str) -> DeleteCohortKey {
        DeleteCohortKey::new(PathBuf::from(path))
    }

    fn op(name: &str) -> DeleteOperation {
        DeleteOperation::new(
            PathBuf::from(format!("/dst/{name}")),
            OsString::from(name),
            DeleteEntryKind::File,
        )
    }

    /// DEL-2.b: with a single sealed cohort the batcher collapses to a
    /// one-entry batch carrying the producer's ops in insertion order.
    /// This is the byte-identity property: a cohort-size-1 drain looks
    /// exactly like one plan dispatch from the sequential emitter.
    #[test]
    fn single_cohort_collapses_to_one_entry_batch() {
        let mut batcher = CohortBatcher::new();
        let ops = vec![op("a"), op("b"), op("c")];
        batcher.enqueue_cohort(key("dir0"), 0, ops).unwrap();
        let batch = batcher.drain_batch();
        assert_eq!(batch.len(), 1);
        let entry = &batch.entries()[0];
        assert_eq!(entry.key, key("dir0"));
        assert_eq!(entry.rank, 0);
        assert_eq!(entry.len(), 3);
        let names: Vec<&OsString> = entry.ops.iter().map(|o| &o.leaf).collect();
        assert_eq!(
            names,
            vec![
                &OsString::from("a"),
                &OsString::from("b"),
                &OsString::from("c"),
            ]
        );
        assert!(batcher.is_empty());
    }

    /// DEL-1.b section 2.3 / DEL-1.c section 3.2: cohorts surfaced in
    /// one drained batch must be in strictly increasing rank order and
    /// each cohort's ops must remain in FIFO insertion order. This is
    /// the wire-ordering invariant the batcher inherits from the
    /// buffer and must not violate when grouping cohorts into a batch.
    #[test]
    fn batch_preserves_strict_rank_order_and_fifo_within_cohort() {
        let mut batcher = CohortBatcher::new();
        // Enqueue cohorts in shuffled rank order. Each cohort holds two
        // ops in insertion order. The producer side is allowed to seal
        // out of order; the consumer drain must still surface cohorts
        // by ascending rank with per-cohort FIFO preserved.
        let cohort_order = [3, 0, 5, 1, 4, 2];
        for &rank in &cohort_order {
            let cohort_key = key(&format!("dir{rank}"));
            let ops = vec![
                op(&format!("first-of-{rank}")),
                op(&format!("second-of-{rank}")),
            ];
            batcher
                .enqueue_cohort(cohort_key, rank as u64, ops)
                .unwrap();
        }
        let batch = batcher.drain_batch();
        assert_eq!(batch.len(), 6);
        for (idx, entry) in batch.entries().iter().enumerate() {
            assert_eq!(entry.rank, idx as u64, "rank must be ascending");
            assert_eq!(entry.key, key(&format!("dir{idx}")));
            assert_eq!(entry.len(), 2);
            assert_eq!(entry.ops[0].leaf, OsString::from(format!("first-of-{idx}")));
            assert_eq!(
                entry.ops[1].leaf,
                OsString::from(format!("second-of-{idx}"))
            );
        }
    }

    /// DEL-1.c section 3.2: drain cap caps the batch at
    /// `DRAIN_BATCH_CAP` and the next drain picks up the remaining
    /// cohorts in strict rank order. A still-unsealed cohort at the
    /// head blocks the drain entirely - the contiguous-only rule from
    /// DEL-1.b section 2.3 carries through batching unchanged.
    #[test]
    fn drain_cap_respected_and_unsealed_head_blocks_batch() {
        let mut batcher = CohortBatcher::new();
        // Enqueue 20 cohorts (all sealed via the single-call API).
        for rank in 0..20 {
            batcher
                .enqueue_cohort(key(&format!("d{rank}")), rank as u64, vec![op("x")])
                .unwrap();
        }
        let first = batcher.drain_batch();
        assert_eq!(first.len(), DRAIN_BATCH_CAP);
        for (idx, entry) in first.entries().iter().enumerate() {
            assert_eq!(entry.rank, idx as u64);
        }
        let second = batcher.drain_batch();
        assert_eq!(second.len(), DRAIN_BATCH_CAP);
        for (idx, entry) in second.entries().iter().enumerate() {
            assert_eq!(entry.rank, (DRAIN_BATCH_CAP + idx) as u64);
        }
        let third = batcher.drain_batch();
        assert_eq!(third.len(), 20 - 2 * DRAIN_BATCH_CAP);

        // Unsealed-head rule: insert ops via the raw buffer accessor
        // so we can prove an unsealed cohort blocks the drain even
        // with later sealed cohorts present. enqueue_cohort always
        // seals, so we drive the lower-level buffer for this case.
        let mut blocking = CohortBatcher::new();
        blocking
            .buffer
            .insert(key("blocker"), 100, op("blocking"))
            .unwrap();
        // Note: not sealed.
        blocking
            .enqueue_cohort(key("later"), 200, vec![op("y")])
            .unwrap();
        let blocked = blocking.drain_batch();
        assert!(
            blocked.is_empty(),
            "unsealed head must block the whole batch"
        );
        // Sealing the head unblocks both cohorts in strict order.
        blocking.buffer.seal(&key("blocker")).unwrap();
        let unblocked = blocking.drain_batch();
        assert_eq!(unblocked.len(), 2);
        assert_eq!(unblocked.entries()[0].rank, 100);
        assert_eq!(unblocked.entries()[1].rank, 200);
    }

    /// DEL-1.c section 6: the producer-panic latch is sticky and
    /// observable to the consumer between dispatches inside a drained
    /// batch. The latch does not by itself drop cohorts from the
    /// batch - that is the consumer's job (bail at first panicked
    /// cohort encountered during the drain walk).
    #[test]
    fn panic_latch_is_sticky_and_observable() {
        let batcher = CohortBatcher::new();
        assert!(!batcher.is_panicked());
        batcher.record_panic();
        assert!(batcher.is_panicked());
        // Sticky: a second observation still reports true.
        assert!(batcher.is_panicked());
    }

    /// DEL-1.c section 6 empty-cohort recovery: enqueuing a cohort
    /// with no ops still occupies a slot, drains as an empty entry,
    /// and preserves the surrounding cohorts' rank order. The wire
    /// stream sees the cohort boundary even though no `MSG_DELETED`
    /// frame is emitted, which mirrors upstream's "empty cohorts are
    /// no-ops" semantics from DEL-1.b section 4.4.
    #[test]
    fn empty_cohort_drains_without_disturbing_order() {
        let mut batcher = CohortBatcher::new();
        batcher
            .enqueue_cohort(key("d0"), 0, vec![op("first")])
            .unwrap();
        batcher.enqueue_cohort(key("d1"), 1, Vec::new()).unwrap();
        batcher
            .enqueue_cohort(key("d2"), 2, vec![op("third")])
            .unwrap();
        let batch = batcher.drain_batch();
        assert_eq!(batch.len(), 3);
        assert_eq!(batch.entries()[0].len(), 1);
        assert_eq!(batch.entries()[1].len(), 0, "empty cohort drains empty");
        assert_eq!(batch.entries()[2].len(), 1);
        let ranks: Vec<u64> = batch.entries().iter().map(|e| e.rank).collect();
        assert_eq!(ranks, vec![0, 1, 2]);
    }
}
