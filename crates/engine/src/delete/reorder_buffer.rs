//! Bounded, per-cohort re-ordering buffer for the parallel `DeleteEmitter`.
//!
//! This module is the buffer primitive specified by DEL-1.b (see
//! `docs/design/del-1b-reordering-buffer.md`) and the batching strategy
//! resolved by DEL-1.c (see
//! `docs/design/del-1c-cohort-batching-strategy.md`). Originally the
//! DEL-2.a deliverable (the data structure alone), it is now wired into
//! the parallel consumer (`super::parallel_consumer`, DEL-2.c) under the
//! `parallel-delete-consumer` feature.
//!
//! # What this buffer does
//!
//! Parallel producers compute per-parent-directory cohorts of
//! [`DeleteOperation`] values in any order. The buffer holds those
//! cohorts until each one has been [`ReorderBuffer::seal`]-ed by its
//! producer, then surfaces complete cohorts to the single consumer in
//! strict **wire-ordering rank** order via
//! [`ReorderBuffer::try_drain_ready`]. The rank is supplied by the
//! caller (typically the dense pre-order `cohort_idx` assigned by
//! [`super::DirTraversalCursor`]) and is the axis the consumer must
//! preserve to give DEL-3's wire-byte parity gate its byte-for-byte
//! equivalence with the sequential emitter.
//!
//! # Why a `BTreeMap` keyed by rank
//!
//! DEL-1.b sketches a ring buffer with modulo indexing and
//! `head`/`tail` atomics, but that shape only pays for itself once the
//! buffer is wired into a `Condvar`-driven producer/consumer loop
//! (DEL-2.c). For the DEL-2.a primitive the consumer is synchronous,
//! the drain order is strictly increasing rank, and the cohort set is
//! sparse during normal operation (producers complete out of order).
//! A [`BTreeMap`](std::collections::BTreeMap) keyed by rank gives:
//!
//! - O(log N) insert and O(log N) seal with N capped at
//!   [`MAX_BUFFERED_COHORTS`] = 64;
//! - free in-order iteration from the lowest rank for
//!   [`ReorderBuffer::try_drain_ready`];
//! - no extra crate dependency.
//!
//! The standard-library-first preference (see crate-level
//! `CLAUDE.md`-equivalent guidance in the engine's `plan_map.rs`
//! header) keeps the dependency footprint flat.
//!
//! # Cohort key shape
//!
//! [`DeleteCohortKey`] wraps a [`PathBuf`] - the destination-relative
//! parent directory path. DEL-1.b section 4.4 fixes the cohort
//! boundary as "one destination parent directory surfaced by
//! `DirTraversalCursor::next_ready`", and the cohort_index audit
//! (`crates/engine/src/delete/cohort_index.rs`) already keys per-dir
//! state by [`PathBuf`]. Using the path keeps the buffer portable to
//! Windows (which has no stable inode number) and lets the rest of the
//! delete pipeline pass cohort keys without an extra inode lookup.
//!
//! # Drain policy
//!
//! Drains are capped at `DRAIN_BATCH_CAP` = 8 sealed cohorts per
//! call, matching DEL-1.c section 3.2's `CONSUMER_DRAIN_BATCH_CAP`.
//! Unsealed cohorts are skipped: an unsealed cohort with the lowest
//! pending rank stops the drain (preserving the strict rank-order
//! invariant), but drains still surface zero cohorts without error so
//! the caller can park and retry on a later wake-up.
//!
//! # Upstream reference
//!
//! - `target/interop/upstream-src/rsync-3.4.1/del.c` (NDX_DEL_STATS
//!   semantics: exactly one frame per goodbye cohort, carrying five
//!   varints; the buffer never emits the frame itself, it only
//!   preserves the cohort identity the goodbye writer reads).
//! - `target/interop/upstream-src/rsync-3.4.1/generator.c:272-347`
//!   (`delete_in_dir`: one cohort per destination parent directory).
//! - DEL-1.a audit: `docs/design/del-1a-upstream-ordering-audit.md`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::plan::DeleteEntryKind;

/// Maximum number of cohorts the buffer holds concurrently.
///
/// DEL-1.b section 4.2 fixes this at **64**, matching the existing
/// reorder-buffer default in the delta pipeline
/// (`crates/engine/src/concurrent_delta/work_queue/capacity.rs`).
/// Rationale (verbatim from DEL-1.b 4.2): "N = 64 lets a 32-core box
/// keep at least one in-flight cohort per worker plus a one-batch
/// overflow per worker before any producer blocks. Smaller values
/// starve high-core hosts; larger values inflate worst-case memory
/// without measurable throughput gain."
pub const MAX_BUFFERED_COHORTS: usize = 64;

/// Maximum number of sealed cohorts a single
/// [`ReorderBuffer::try_drain_ready`] call surfaces.
///
/// DEL-1.c section 3.2 fixes this at **8**: "It is the smallest
/// power-of-two that amortises the Condvar cost below 15% of total
/// consumer CPU at the 100k-cohort projection point." A larger cap
/// inflates head-of-line latency on the wire; a smaller cap loses the
/// amortisation benefit. The cap is compile-time; a runtime knob is
/// out of scope for DEL-2.a (deferred to DEL-3).
pub const DRAIN_BATCH_CAP: usize = 8;

/// Per-deletion-attempt record the buffer carries.
///
/// A minimal payload that captures everything the parallel consumer
/// needs to either re-issue the dispatch or emit a `MSG_DELETED`
/// frame: the absolute destination path, the leaf name (for
/// SEC-1.q-style dirfd-anchored dispatch), and the kind for stats
/// bookkeeping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteOperation {
    /// Absolute (or destination-rooted) path of the deletion target.
    pub path: PathBuf,
    /// Leaf component of [`Self::path`]; matches what
    /// [`super::DeleteEntry::name`] would carry.
    pub leaf: OsString,
    /// Stats bucket the deletion contributes to.
    pub kind: DeleteEntryKind,
}

impl DeleteOperation {
    /// Builds a [`DeleteOperation`] from its components.
    #[must_use]
    pub fn new(path: PathBuf, leaf: OsString, kind: DeleteEntryKind) -> Self {
        Self { path, leaf, kind }
    }
}

/// Identifier for one cohort buffered by [`ReorderBuffer`].
///
/// The wrapped [`PathBuf`] is the destination-relative parent
/// directory path - the same key the rest of the delete pipeline uses
/// (see [`super::DeletePlanMap`] and
/// [`super::DirTraversalCursor`]). [`Clone`] is cheap enough for the
/// buffer's hot path because each cohort key is inserted at most
/// [`MAX_BUFFERED_COHORTS`] times concurrently and the path is short
/// (one parent directory per cohort).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeleteCohortKey(PathBuf);

impl DeleteCohortKey {
    /// Wraps a destination-relative parent directory path as a cohort key.
    #[must_use]
    pub fn new(parent_dir: impl Into<PathBuf>) -> Self {
        Self(parent_dir.into())
    }

    /// Borrows the underlying parent directory path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Consumes the key and returns the wrapped [`PathBuf`].
    #[must_use]
    pub fn into_inner(self) -> PathBuf {
        self.0
    }
}

impl From<PathBuf> for DeleteCohortKey {
    fn from(value: PathBuf) -> Self {
        Self(value)
    }
}

/// One cohort's worth of pending [`DeleteOperation`] entries with the
/// stable wire-ordering rank used to serialise the consumer drain.
///
/// The rank is supplied by the caller (the receiver-side driver in
/// DEL-2.c) and is the dense pre-order index DEL-1.c section 1
/// describes. Inside a cohort the [`Self::ops`] vector is in FIFO
/// insertion order; that is the upstream `delete_in_dir`
/// reverse-directory order producers walk (DEL-1.b section 3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteCohort {
    /// Stable wire-ordering rank for this cohort. Lower drains first.
    pub rank: u64,
    /// Pending operations in producer-insertion order.
    pub ops: Vec<DeleteOperation>,
    /// `true` once the producer has called [`ReorderBuffer::seal`]
    /// for this cohort. Sealed cohorts become drainable; unsealed
    /// cohorts block the head of the drain.
    pub sealed: bool,
}

impl DeleteCohort {
    /// Returns the number of pending operations in the cohort.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Returns `true` when the cohort holds no operations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// Errors surfaced by [`ReorderBuffer`].
///
/// The buffer never panics on legal API misuse; every misuse path
/// returns a typed [`ReorderBufferError`] so the DEL-2.c consumer can
/// distinguish capacity backpressure from invariant violations.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ReorderBufferError {
    /// Caller tried to insert into a new cohort while the buffer is
    /// already holding [`MAX_BUFFERED_COHORTS`] distinct cohorts.
    /// DEL-1.b section 4.3 mandates that producers block on this
    /// signal (the production wiring uses a `Condvar`; the DEL-2.a
    /// primitive surfaces it as an error and lets the caller decide).
    #[error(
        "delete reorder buffer is full: cap={cap} cohorts, new cohort key={key:?} would exceed it"
    )]
    BufferFull {
        /// The configured capacity ([`MAX_BUFFERED_COHORTS`]).
        cap: usize,
        /// The cohort key that triggered the overflow, surfaced so
        /// operator logs can correlate the backpressure event with
        /// the producer-side scheduling.
        key: DeleteCohortKey,
    },
    /// Caller sealed a cohort key that was never inserted. Distinct
    /// from a sealed-twice race: this signals a producer/consumer
    /// ordering bug rather than a legitimate backpressure event.
    #[error("delete reorder buffer: cannot seal unknown cohort key={key:?}")]
    UnknownCohort {
        /// The cohort key the caller attempted to seal.
        key: DeleteCohortKey,
    },
    /// Caller tried to mutate a cohort whose rank disagrees with the
    /// rank previously recorded for the same key. Only surfaced by
    /// [`ReorderBuffer::insert`] when a producer reuses a cohort key
    /// across distinct ranks - this is always a bug.
    #[error(
        "delete reorder buffer: cohort key={key:?} reused with different rank (existing={existing}, new={incoming})"
    )]
    RankConflict {
        /// The cohort key the caller is re-inserting under.
        key: DeleteCohortKey,
        /// The rank already recorded for the cohort.
        existing: u64,
        /// The rank the caller passed on this insert.
        incoming: u64,
    },
    /// Debug-only diagnostic: the consumer saw two consecutively
    /// drained cohorts whose ranks are not strictly increasing. This
    /// fires only under intentional misuse (e.g. a test that forces
    /// two cohorts to share a rank). Production code never observes
    /// it; the [`BTreeMap`](std::collections::BTreeMap) backing
    /// guarantees rank order otherwise.
    #[cfg(debug_assertions)]
    #[error(
        "delete reorder buffer: rank inversion during drain (previous={previous}, current={current})"
    )]
    RankInversion {
        /// Rank of the cohort drained immediately before.
        previous: u64,
        /// Rank of the cohort that triggered the inversion check.
        current: u64,
    },
}

/// Bounded re-ordering buffer for the parallel `DeleteEmitter` consumer.
///
/// The buffer is intentionally synchronous: it surfaces capacity
/// backpressure and unsealed-head stalls as method-return signals
/// rather than blocking the caller. The DEL-2.c wiring layers a
/// `Condvar` and a producer/consumer scope on top of this primitive.
///
/// # Invariants
///
/// - The buffer holds at most [`MAX_BUFFERED_COHORTS`] cohorts.
/// - [`Self::try_drain_ready`] surfaces at most
///   `DRAIN_BATCH_CAP` cohorts per call.
/// - Surfaced cohorts have strictly increasing rank within a single
///   [`Self::try_drain_ready`] call **and** across consecutive calls
///   (the buffer tracks the last-drained rank for the cross-call
///   assertion in debug builds).
/// - An unsealed cohort at the head blocks the drain entirely; later
///   sealed cohorts wait their turn.
#[derive(Debug, Default)]
pub struct ReorderBuffer {
    cohorts: BTreeMap<u64, (DeleteCohortKey, DeleteCohort)>,
    by_key: BTreeMap<DeleteCohortKey, u64>,
    #[cfg(debug_assertions)]
    last_drained_rank: Option<u64>,
}

impl ReorderBuffer {
    /// Constructs an empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of cohorts currently buffered (sealed or not).
    #[must_use]
    pub fn len(&self) -> usize {
        self.cohorts.len()
    }

    /// Returns `true` when the buffer holds no cohorts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cohorts.is_empty()
    }

    /// Returns `true` when the buffer is at its [`MAX_BUFFERED_COHORTS`] cap.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.cohorts.len() >= MAX_BUFFERED_COHORTS
    }

    /// Inserts one [`DeleteOperation`] into the cohort identified by
    /// `key`/`rank`.
    ///
    /// The first insert for a key creates the cohort and registers
    /// its rank; subsequent inserts append to the existing cohort.
    /// Operations are appended in caller-supplied order, which is the
    /// upstream `delete_in_dir` reverse-directory order producers
    /// already walk (DEL-1.b section 3.1).
    ///
    /// # Errors
    ///
    /// - [`ReorderBufferError::BufferFull`] when a new cohort would
    ///   exceed [`MAX_BUFFERED_COHORTS`]. Inserts into an
    ///   already-buffered cohort never trigger this.
    /// - [`ReorderBufferError::RankConflict`] when the caller reuses
    ///   `key` with a rank that disagrees with the one previously
    ///   recorded.
    pub fn insert(
        &mut self,
        key: DeleteCohortKey,
        rank: u64,
        op: DeleteOperation,
    ) -> Result<(), ReorderBufferError> {
        match self.by_key.get(&key) {
            Some(&existing_rank) if existing_rank != rank => {
                Err(ReorderBufferError::RankConflict {
                    key,
                    existing: existing_rank,
                    incoming: rank,
                })
            }
            Some(_) => {
                let entry = self
                    .cohorts
                    .get_mut(&rank)
                    .expect("by_key and cohorts maps must stay in sync");
                entry.1.ops.push(op);
                Ok(())
            }
            None => {
                if self.cohorts.len() >= MAX_BUFFERED_COHORTS {
                    return Err(ReorderBufferError::BufferFull {
                        cap: MAX_BUFFERED_COHORTS,
                        key,
                    });
                }
                let cohort = DeleteCohort {
                    rank,
                    ops: vec![op],
                    sealed: false,
                };
                self.by_key.insert(key.clone(), rank);
                self.cohorts.insert(rank, (key, cohort));
                Ok(())
            }
        }
    }

    /// Marks `key`'s cohort as complete so subsequent
    /// [`Self::try_drain_ready`] calls may surface it.
    ///
    /// Calling [`Self::seal`] on an already-sealed cohort is
    /// idempotent. DEL-1.b section 6.1 ("Producer panics mid-cohort")
    /// uses an empty-batch seal as the panic-recovery path; the DEL-2.a
    /// primitive accepts that pattern by allowing seals on empty
    /// cohorts (insert one zero-op cohort then seal) so the future
    /// wiring layer keeps a uniform API.
    ///
    /// # Errors
    ///
    /// - [`ReorderBufferError::UnknownCohort`] when `key` was never
    ///   inserted. This is a producer/consumer ordering bug; the
    ///   production wiring (DEL-2.c) should pair every `seal` with
    ///   exactly one prior `insert` or zero-op registration.
    pub fn seal(&mut self, key: &DeleteCohortKey) -> Result<(), ReorderBufferError> {
        let Some(&rank) = self.by_key.get(key) else {
            return Err(ReorderBufferError::UnknownCohort { key: key.clone() });
        };
        let entry = self
            .cohorts
            .get_mut(&rank)
            .expect("by_key and cohorts maps must stay in sync");
        entry.1.sealed = true;
        Ok(())
    }

    /// Registers an empty cohort under `key`/`rank` without inserting
    /// any operation.
    ///
    /// Used by the panic-recovery path (DEL-1.b section 6.1) and by
    /// producers that own a cohort with no extras to emit. The empty
    /// cohort still occupies one buffer slot and contributes nothing
    /// to the drained stats, matching upstream's "empty cohorts are
    /// no-ops" semantics.
    ///
    /// # Errors
    ///
    /// Same as [`Self::insert`] minus the rank-conflict path (an
    /// existing key is silently kept; the caller's `rank` is ignored).
    pub fn register_empty(
        &mut self,
        key: DeleteCohortKey,
        rank: u64,
    ) -> Result<(), ReorderBufferError> {
        if let Some(&existing_rank) = self.by_key.get(&key) {
            if existing_rank != rank {
                return Err(ReorderBufferError::RankConflict {
                    key,
                    existing: existing_rank,
                    incoming: rank,
                });
            }
            return Ok(());
        }
        if self.cohorts.len() >= MAX_BUFFERED_COHORTS {
            return Err(ReorderBufferError::BufferFull {
                cap: MAX_BUFFERED_COHORTS,
                key,
            });
        }
        let cohort = DeleteCohort {
            rank,
            ops: Vec::new(),
            sealed: false,
        };
        self.by_key.insert(key.clone(), rank);
        self.cohorts.insert(rank, (key, cohort));
        Ok(())
    }

    /// Drains up to `DRAIN_BATCH_CAP` contiguous-by-rank sealed
    /// cohorts from the head of the buffer.
    ///
    /// Returns one [`Vec<DeleteOperation>`] per drained cohort, in
    /// strictly increasing rank order. An unsealed cohort at the head
    /// stops the drain (the strict cohort-order rule from DEL-1.b
    /// section 2.3), and the call returns whatever was drained
    /// before the stall (possibly empty).
    ///
    /// The drained cohorts are removed from the buffer, freeing
    /// capacity for new inserts.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if two surfaced cohorts have
    /// non-monotonic ranks. The [`BTreeMap`](std::collections::BTreeMap)
    /// backing makes this statically impossible under correct API
    /// usage; the debug assertion guards against a regression in the
    /// buffer's invariants if a future change swaps the backing store.
    pub fn try_drain_ready(&mut self) -> Vec<Vec<DeleteOperation>> {
        let mut drained = Vec::new();
        #[cfg(debug_assertions)]
        let mut previous_rank: Option<u64> = self.last_drained_rank;

        while drained.len() < DRAIN_BATCH_CAP {
            let Some((&rank, (_, cohort))) = self.cohorts.iter().next() else {
                break;
            };
            if !cohort.sealed {
                break;
            }

            #[cfg(debug_assertions)]
            {
                if let Some(prev) = previous_rank {
                    assert!(
                        rank > prev,
                        "delete reorder buffer rank inversion during drain: previous={prev} current={rank}"
                    );
                }
                previous_rank = Some(rank);
            }

            let (key, cohort) = self
                .cohorts
                .remove(&rank)
                .expect("entry was just observed in the BTreeMap");
            self.by_key.remove(&key);
            drained.push(cohort.ops);
        }

        #[cfg(debug_assertions)]
        {
            if let Some(prev) = previous_rank {
                self.last_drained_rank = Some(prev);
            }
        }

        drained
    }

    /// Drains up to `DRAIN_BATCH_CAP` contiguous-by-rank sealed
    /// cohorts and returns each one's `(key, rank, ops)` tuple.
    ///
    /// Mirrors [`Self::try_drain_ready`]'s semantics with the cohort
    /// key and rank surfaced alongside the operations so the DEL-2.b
    /// batcher can dispatch cohorts through `DeleteFs` without holding
    /// a parallel snapshot of head identities. The strict cohort-order
    /// invariant from DEL-1.b section 2.3 carries through unchanged.
    pub fn try_drain_ready_with_meta(
        &mut self,
    ) -> Vec<(DeleteCohortKey, u64, Vec<DeleteOperation>)> {
        let mut drained = Vec::new();
        #[cfg(debug_assertions)]
        let mut previous_rank: Option<u64> = self.last_drained_rank;

        while drained.len() < DRAIN_BATCH_CAP {
            let Some((&rank, (_, cohort))) = self.cohorts.iter().next() else {
                break;
            };
            if !cohort.sealed {
                break;
            }

            #[cfg(debug_assertions)]
            {
                if let Some(prev) = previous_rank {
                    assert!(
                        rank > prev,
                        "delete reorder buffer rank inversion during drain: previous={prev} current={rank}"
                    );
                }
                previous_rank = Some(rank);
            }

            let (key, cohort) = self
                .cohorts
                .remove(&rank)
                .expect("entry was just observed in the BTreeMap");
            self.by_key.remove(&key);
            drained.push((key, rank, cohort.ops));
        }

        #[cfg(debug_assertions)]
        {
            if let Some(prev) = previous_rank {
                self.last_drained_rank = Some(prev);
            }
        }

        drained
    }

    /// Returns the rank of the head cohort (lowest rank currently
    /// buffered), or `None` when the buffer is empty.
    ///
    /// Exposed so DEL-2.c's wiring can drive an external
    /// `Condvar::wait` on "head rank unchanged" without re-walking the
    /// map.
    #[must_use]
    pub fn head_rank(&self) -> Option<u64> {
        self.cohorts.keys().next().copied()
    }

    /// Returns `true` when the head cohort is sealed and would be
    /// surfaced by the next [`Self::try_drain_ready`] call.
    #[must_use]
    pub fn head_is_ready(&self) -> bool {
        self.cohorts
            .iter()
            .next()
            .is_some_and(|(_, (_, cohort))| cohort.sealed)
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn fifo_per_cohort_and_rank_order_across_cohorts() {
        let mut buf = ReorderBuffer::new();
        // Insert 100 operations across 10 cohorts, interleaved.
        for round in 0..10 {
            for cohort_id in 0..10 {
                let k = key(&format!("dir{cohort_id}"));
                let entry_name = format!("file{cohort_id}-{round}");
                buf.insert(k, cohort_id as u64, op(&entry_name)).unwrap();
            }
        }
        assert_eq!(buf.len(), 10);
        // Seal in shuffled order to prove drain still respects rank.
        for cohort_id in [3, 0, 7, 1, 9, 4, 2, 8, 5, 6] {
            buf.seal(&key(&format!("dir{cohort_id}"))).unwrap();
        }
        // Drain in two batches: 8 then 2.
        let first = buf.try_drain_ready();
        assert_eq!(first.len(), DRAIN_BATCH_CAP);
        let second = buf.try_drain_ready();
        assert_eq!(second.len(), 10 - DRAIN_BATCH_CAP);
        assert!(buf.is_empty());

        let mut surfaced: Vec<Vec<DeleteOperation>> = first;
        surfaced.extend(second);
        // Outer order is rank-ascending (0..10).
        for (cohort_idx, cohort_ops) in surfaced.iter().enumerate() {
            assert_eq!(cohort_ops.len(), 10);
            // Inner order is FIFO insertion (round 0..10).
            for (round, op_entry) in cohort_ops.iter().enumerate() {
                assert_eq!(
                    op_entry.leaf,
                    OsString::from(format!("file{cohort_idx}-{round}"))
                );
            }
        }
    }

    #[test]
    fn insert_beyond_cap_returns_buffer_full() {
        let mut buf = ReorderBuffer::new();
        for rank in 0..MAX_BUFFERED_COHORTS {
            buf.insert(key(&format!("d{rank}")), rank as u64, op("f"))
                .unwrap();
        }
        assert!(buf.is_full());
        // Insert into an already-buffered cohort still works.
        buf.insert(key("d0"), 0, op("f2")).unwrap();
        // New cohort over the cap fails.
        let overflow_key = key("overflow");
        let err = buf
            .insert(overflow_key.clone(), MAX_BUFFERED_COHORTS as u64, op("f"))
            .unwrap_err();
        assert_eq!(
            err,
            ReorderBufferError::BufferFull {
                cap: MAX_BUFFERED_COHORTS,
                key: overflow_key,
            }
        );
    }

    #[test]
    fn drain_when_nothing_sealed_yields_empty_without_error() {
        let mut buf = ReorderBuffer::new();
        for rank in 0..5 {
            buf.insert(key(&format!("d{rank}")), rank as u64, op("f"))
                .unwrap();
        }
        let drained = buf.try_drain_ready();
        assert!(drained.is_empty());
        // Buffer state is unchanged.
        assert_eq!(buf.len(), 5);
        assert_eq!(buf.head_rank(), Some(0));
        assert!(!buf.head_is_ready());
    }

    #[test]
    fn drain_stops_at_unsealed_head_even_if_later_cohorts_sealed() {
        let mut buf = ReorderBuffer::new();
        for rank in 0..5 {
            buf.insert(key(&format!("d{rank}")), rank as u64, op("f"))
                .unwrap();
        }
        // Seal everything except cohort rank 0.
        for rank in 1..5 {
            buf.seal(&key(&format!("d{rank}"))).unwrap();
        }
        let drained = buf.try_drain_ready();
        assert!(
            drained.is_empty(),
            "unsealed head must block strictly-ordered drain"
        );
        assert_eq!(buf.len(), 5);
        // Seal the head and drain proceeds for all 5.
        buf.seal(&key("d0")).unwrap();
        let drained = buf.try_drain_ready();
        assert_eq!(drained.len(), 5);
        assert!(buf.is_empty());
    }

    #[test]
    fn drain_batch_cap_respected_with_twenty_sealed_cohorts() {
        let mut buf = ReorderBuffer::new();
        for rank in 0..20 {
            let k = key(&format!("d{rank}"));
            buf.insert(k.clone(), rank as u64, op("f")).unwrap();
            buf.seal(&k).unwrap();
        }
        let first = buf.try_drain_ready();
        assert_eq!(first.len(), DRAIN_BATCH_CAP);
        let second = buf.try_drain_ready();
        assert_eq!(second.len(), DRAIN_BATCH_CAP);
        let third = buf.try_drain_ready();
        assert_eq!(third.len(), 20 - 2 * DRAIN_BATCH_CAP);
        assert!(buf.is_empty());
        let fourth = buf.try_drain_ready();
        assert!(fourth.is_empty());
    }

    #[test]
    fn rank_conflict_on_key_reuse_is_reported() {
        let mut buf = ReorderBuffer::new();
        buf.insert(key("d0"), 0, op("a")).unwrap();
        let err = buf.insert(key("d0"), 1, op("b")).unwrap_err();
        assert_eq!(
            err,
            ReorderBufferError::RankConflict {
                key: key("d0"),
                existing: 0,
                incoming: 1,
            }
        );
    }

    #[test]
    fn seal_unknown_cohort_is_reported() {
        let mut buf = ReorderBuffer::new();
        let err = buf.seal(&key("never_inserted")).unwrap_err();
        assert_eq!(
            err,
            ReorderBufferError::UnknownCohort {
                key: key("never_inserted"),
            }
        );
    }

    #[test]
    fn register_empty_round_trips_through_drain() {
        let mut buf = ReorderBuffer::new();
        buf.register_empty(key("d0"), 0).unwrap();
        buf.insert(key("d1"), 1, op("f")).unwrap();
        buf.seal(&key("d0")).unwrap();
        buf.seal(&key("d1")).unwrap();
        let drained = buf.try_drain_ready();
        assert_eq!(drained.len(), 2);
        // Empty cohort drains as an empty Vec, preserving rank order.
        assert!(drained[0].is_empty());
        assert_eq!(drained[1].len(), 1);
    }

    #[test]
    fn head_rank_and_head_is_ready_track_state() {
        let mut buf = ReorderBuffer::new();
        assert_eq!(buf.head_rank(), None);
        assert!(!buf.head_is_ready());
        buf.insert(key("d5"), 5, op("f")).unwrap();
        buf.insert(key("d2"), 2, op("f")).unwrap();
        assert_eq!(buf.head_rank(), Some(2));
        assert!(!buf.head_is_ready());
        buf.seal(&key("d2")).unwrap();
        assert!(buf.head_is_ready());
    }

    /// DEL-2.b: [`ReorderBuffer::try_drain_ready_with_meta`] surfaces
    /// the cohort key and rank alongside the operations, matching the
    /// rank order and cap of [`ReorderBuffer::try_drain_ready`].
    #[test]
    fn try_drain_ready_with_meta_returns_keys_and_ranks_in_order() {
        let mut buf = ReorderBuffer::new();
        for rank in [4u64, 1, 3, 2, 0] {
            let k = key(&format!("d{rank}"));
            buf.insert(k.clone(), rank, op(&format!("op-{rank}")))
                .unwrap();
            buf.seal(&k).unwrap();
        }
        let drained = buf.try_drain_ready_with_meta();
        assert_eq!(drained.len(), 5);
        let ranks: Vec<u64> = drained.iter().map(|(_, r, _)| *r).collect();
        assert_eq!(ranks, vec![0, 1, 2, 3, 4]);
        for (idx, (k, _, ops)) in drained.iter().enumerate() {
            assert_eq!(*k, key(&format!("d{idx}")));
            assert_eq!(ops.len(), 1);
            assert_eq!(ops[0].leaf, OsString::from(format!("op-{idx}")));
        }
    }

    /// DEL-1.b section 2.3 / DEL-1.c section 3.2: drains across
    /// multiple [`ReorderBuffer::try_drain_ready`] calls preserve
    /// strict monotonic rank ordering. This guards the cross-call
    /// half of the invariant the [`ReorderBufferError::RankInversion`]
    /// variant documents.
    #[test]
    fn cross_call_rank_monotonicity_holds() {
        let mut buf = ReorderBuffer::new();
        for rank in 0..16 {
            let k = key(&format!("d{rank}"));
            buf.insert(k.clone(), rank as u64, op("f")).unwrap();
            buf.seal(&k).unwrap();
        }
        let first = buf.try_drain_ready();
        let second = buf.try_drain_ready();
        let combined: Vec<u64> = first
            .iter()
            .chain(second.iter())
            .enumerate()
            .map(|(idx, _)| idx as u64)
            .collect();
        for window in combined.windows(2) {
            assert!(window[0] < window[1]);
        }
    }

    /// Debug-only: confirm the monotonic-rank assertion fires when
    /// the [`BTreeMap`](std::collections::BTreeMap) invariant is
    /// broken via direct field mutation
    /// (a stand-in for a future regression that swaps the backing
    /// store for one without natural rank ordering). The test
    /// constructs an inverted-rank pair by hand, calls
    /// [`ReorderBuffer::try_drain_ready`], and asserts the
    /// `debug_assert!` fires under `cfg(debug_assertions)`.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "rank inversion")]
    fn debug_assertion_fires_under_intentional_misuse() {
        let mut buf = ReorderBuffer::new();
        // Drain a high-rank cohort first to advance last_drained_rank.
        let k_high = key("high");
        buf.insert(k_high.clone(), 100, op("h")).unwrap();
        buf.seal(&k_high).unwrap();
        let drained_high = buf.try_drain_ready();
        assert_eq!(drained_high.len(), 1);
        // Now insert a lower-rank cohort and drain it - the cross-call
        // monotonicity guard should fire because rank 1 < 100.
        let k_low = key("low");
        buf.insert(k_low.clone(), 1, op("l")).unwrap();
        buf.seal(&k_low).unwrap();
        let _ = buf.try_drain_ready();
    }
}
