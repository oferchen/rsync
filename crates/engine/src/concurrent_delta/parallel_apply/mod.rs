//! Parallel receive-side delta apply scaffold (#1368).
//!
//! Gated behind the `parallel-receive-delta` feature so the production binary
//! continues to drive the sequential apply loop in
//! `crates/transfer/src/receiver/transfer.rs`. The design at
//! `docs/design/parallel-receive-delta-application.md` calls for this code
//! path to be opt-in until the parity-test gap (#4205 G2) closes and the
//! drain-parallel bench from #4214 shows a measurable win at receive-side
//! scale.
//!
//! # Shape
//!
//! [`ParallelDeltaApplier`] owns a configurable concurrency limit and a
//! per-file map of [`Mutex`]-guarded destination writers. Callers hand it
//! [`DeltaChunk`] values (one literal-or-block segment for one file) through
//! [`apply_one_chunk`](ParallelDeltaApplier::apply_one_chunk). The
//! checksum verify step runs on the rayon pool; the actual file-write happens
//! under the per-file mutex so per-file byte order is preserved.
//!
//! # Ordering preservation
//!
//! Two layers protect the wire-format invariants documented in section 2 of
//! the design doc:
//!
//! 1. **Per-file token order.** Each chunk carries a monotonic
//!    `chunk_sequence` per file. A per-file [`ReorderBuffer`] inside the
//!    applier replays chunks in submission order before they touch the
//!    destination writer, even though the rayon verify step completes out of
//!    order.
//! 2. **Per-file write exclusivity.** The destination writer for each file
//!    sits behind a [`Mutex`], so only one chunk ever holds the writer at a
//!    time. Combined with the reorder buffer above, the bytes hit the file
//!    in the exact sequence the producer submitted them.
//!
//! Cross-file ordering at the wire-output layer is the
//! [`super::ReorderBuffer`] caller's responsibility (the existing
//! `DeltaConsumer` pattern already covers that case).

use std::io::{self, Write};
use std::sync::{Arc, MutexGuard};

use checksums::strong::strategy::{
    ChecksumAlgorithmKind, ChecksumDigest, ChecksumStrategy, ChecksumStrategySelector,
};
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use rayon::prelude::*;
use thiserror::Error;

use super::reorder::ReorderBuffer;
use super::types::FileNdx;

mod slot_barrier;

use slot_barrier::SlotBarrier;

/// Typed error variants for [`ParallelDeltaApplier::finish_file`] shutdown
/// paths.
///
/// The audit at `docs/audits/arc-try-unwrap-classification.md` (ATU-3,
/// tracked in #2380) flagged the previous opaque `io::Error::other(...)`
/// message as user-visible but undiagnosable: it omitted the residual
/// [`Arc::strong_count`], the offending `FileNdx`, and the failure mode
/// (still-in-flight vs poisoned). Each variant below carries enough
/// context for an operator to locate the leaking worker or the
/// panicking holder.
///
/// [`Arc::strong_count`]: std::sync::Arc::strong_count
#[derive(Debug, Error)]
pub enum ParallelApplyError {
    /// The per-file slot's [`Arc`] still has outstanding clones; a
    /// worker has not yet released its reference. The applier cannot
    /// extract the writer until every clone has been dropped.
    #[error(
        "ParallelDeltaApplier::{kind}: file slot still referenced for ndx={ndx} (strong_count={strong_count})"
    )]
    ApplierStillReferenced {
        /// File index whose slot is still shared.
        ndx: FileNdx,
        /// Observed [`Arc::strong_count`] at the failure site.
        ///
        /// Always `>= 2` when this variant is constructed.
        ///
        /// [`Arc::strong_count`]: std::sync::Arc::strong_count
        strong_count: usize,
        /// Static tag identifying the call site (e.g. `"finish_file"`).
        kind: &'static str,
    },
    /// The per-file slot's mutex was poisoned by a panicking holder.
    /// The applier cannot reuse the writer; the caller must abort the
    /// transfer for `ndx`.
    #[error("ParallelDeltaApplier::{kind}: file slot mutex poisoned for ndx={ndx}")]
    SlotPoisoned {
        /// File index whose slot mutex was poisoned.
        ndx: FileNdx,
        /// Static tag identifying the call site (e.g. `"finish_file"`).
        kind: &'static str,
    },
    /// The per-file reorder buffer still holds chunks awaiting a
    /// missing sequence number when finish was requested. Indicates the
    /// producer dropped a chunk or stopped submitting before the
    /// stream completed.
    #[error(
        "ParallelDeltaApplier::{kind}: file {ndx} finished with chunks still buffered ({buffered})"
    )]
    UndrainedChunks {
        /// File index whose reorder buffer was non-empty at finish.
        ndx: FileNdx,
        /// Number of chunks still buffered.
        buffered: usize,
        /// Static tag identifying the call site (e.g. `"finish_file"`).
        kind: &'static str,
    },
    /// The strong checksum computed from `chunk.data` did not match the
    /// expected digest the producer attached to the chunk. The receiver
    /// must abort the chunk's file (or fall back to phase-2 redo) rather
    /// than commit corrupted bytes.
    #[error(
        "ParallelDeltaApplier::verify_chunk: checksum mismatch for ndx={ndx} sequence={chunk_sequence} algorithm={algorithm} expected={expected} actual={actual}"
    )]
    ChecksumMismatch {
        /// File index whose chunk failed verification.
        ndx: FileNdx,
        /// Per-file sequence number of the failing chunk.
        chunk_sequence: u64,
        /// Algorithm used for the digest comparison.
        algorithm: ChecksumAlgorithmKind,
        /// Expected digest as a hex string (from the chunk metadata).
        expected: String,
        /// Actual digest computed from `chunk.data`, as a hex string.
        actual: String,
    },
}

impl From<ParallelApplyError> for io::Error {
    /// Maps a [`ParallelApplyError`] to an [`io::Error`] so existing
    /// callers keep their `io::Result`-shaped API. The full typed
    /// message - including `ndx`, `strong_count`, and the call-site tag -
    /// is preserved as the `Display` payload.
    fn from(value: ParallelApplyError) -> Self {
        io::Error::other(value.to_string())
    }
}

/// A single contiguous segment of a per-file delta apply.
///
/// One chunk corresponds to either a literal-data span (`is_literal = true`)
/// or a basis-file block reference (`is_literal = false`). Either way it
/// carries the bytes already resolved by the wire reader plus the
/// per-file sequence number assigned at submission time.
///
/// Chunks are CPU-light at this stage; the heavy step is the strong-checksum
/// rollup that [`ParallelDeltaApplier::verify_chunk`] runs on a rayon worker
/// using the negotiated [`ChecksumStrategy`].
#[derive(Debug, Clone)]
pub struct DeltaChunk {
    /// File this chunk belongs to.
    pub ndx: FileNdx,
    /// Monotonic per-file submission sequence number.
    ///
    /// The applier replays chunks for `ndx` in increasing `chunk_sequence`
    /// order, mirroring the per-file byte order the sender emitted.
    pub chunk_sequence: u64,
    /// Resolved bytes for this chunk.
    pub data: Vec<u8>,
    /// `true` for literal payloads, `false` for basis-file matches. The
    /// verify and write paths are identical today; the discriminator is kept
    /// so future stats reporting can split literal vs matched bytes without
    /// touching the public chunk shape.
    pub is_literal: bool,
    /// Optional expected strong-checksum digest for `data`.
    ///
    /// When `Some`, [`ParallelDeltaApplier::verify_chunk`] computes the
    /// digest of `data` using the negotiated strategy and compares it
    /// byte-for-byte against this value. A mismatch produces a typed
    /// [`ParallelApplyError::ChecksumMismatch`] so the receiver can fall
    /// back to a phase-2 redo or abort the transfer; the corrupt bytes
    /// never reach the destination writer.
    ///
    /// When `None`, the applier skips comparison but still computes the
    /// digest for parity with the verified path (keeping CPU cost stable
    /// across both shapes and exercising the strategy code path). Producers
    /// that have not yet wired the per-chunk expected digest into the
    /// receiver pipeline can leave this as `None` and the applier remains
    /// backward-compatible.
    pub expected_strong: Option<ChecksumDigest>,
}

impl DeltaChunk {
    /// Builds a literal-data chunk with no expected digest attached.
    #[must_use]
    pub fn literal(ndx: impl Into<FileNdx>, chunk_sequence: u64, data: Vec<u8>) -> Self {
        Self {
            ndx: ndx.into(),
            chunk_sequence,
            data,
            is_literal: true,
            expected_strong: None,
        }
    }

    /// Builds a basis-match chunk with no expected digest attached.
    #[must_use]
    pub fn matched(ndx: impl Into<FileNdx>, chunk_sequence: u64, data: Vec<u8>) -> Self {
        Self {
            ndx: ndx.into(),
            chunk_sequence,
            data,
            is_literal: false,
            expected_strong: None,
        }
    }

    /// Builder-style setter that attaches an expected strong-checksum
    /// digest to this chunk.
    ///
    /// The receiver pipeline uses this to opt each chunk into real
    /// per-chunk verification by [`ParallelDeltaApplier::verify_chunk`].
    /// Callers that have not negotiated per-chunk checksums (or are
    /// driving the applier from a bench/test that does not need the
    /// extra comparison) can leave the field unset.
    #[must_use]
    pub fn with_expected_strong(mut self, expected: ChecksumDigest) -> Self {
        self.expected_strong = Some(expected);
        self
    }
}

/// Per-file destination writer plus the reorder buffer that re-establishes
/// submission order after the rayon verify step completes out of order.
struct FileSlot {
    writer: Box<dyn Write + Send>,
    reorder: ReorderBuffer<DeltaChunk>,
    bytes_written: u64,
}

impl FileSlot {
    fn new(writer: Box<dyn Write + Send>, reorder_capacity: usize) -> Self {
        Self {
            writer,
            reorder: ReorderBuffer::new(reorder_capacity),
            bytes_written: 0,
        }
    }

    /// Inserts `chunk` into the reorder buffer and drains any contiguous run
    /// that is now ready, writing each ready chunk to the destination.
    ///
    /// The reorder buffer is the single source of truth for per-file
    /// sequencing; the writer only sees chunks once they have arrived in
    /// strict `chunk_sequence` order.
    fn ingest(&mut self, chunk: DeltaChunk) -> io::Result<()> {
        let seq = chunk.chunk_sequence;
        self.reorder
            .insert(seq, chunk)
            .map_err(|e| io::Error::other(format!("parallel apply reorder full: {e}")))?;
        let ready: Vec<DeltaChunk> = self.reorder.drain_ready().collect();
        for chunk in ready {
            self.write_chunk(chunk)?;
        }
        Ok(())
    }

    fn write_chunk(&mut self, chunk: DeltaChunk) -> io::Result<()> {
        self.writer.write_all(&chunk.data)?;
        self.bytes_written = self
            .bytes_written
            .checked_add(chunk.data.len() as u64)
            .ok_or_else(|| io::Error::other("parallel apply byte counter overflow"))?;
        Ok(())
    }

    /// Returns the bytes-written counter.
    fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Returns true if every submitted chunk for this file has hit the writer.
    fn drained(&self) -> bool {
        self.reorder.is_empty()
    }
}

/// RAII guard returned alongside a [`SlotHandle`] that decrements the
/// per-slot in-flight counter when dropped. Keeping the decrement in a
/// dedicated drop type makes the bookkeeping exception-safe: if the worker
/// panics mid-write or returns early via `?`, the counter still drops
/// back to its pre-handoff value and `flush_workers` unblocks.
struct DecrementGuard {
    barrier: Arc<SlotBarrier>,
}

impl Drop for DecrementGuard {
    fn drop(&mut self) {
        self.barrier.decrement_inflight();
    }
}

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
struct SlotHandle {
    barrier: Arc<SlotBarrier>,
    _decrement: DecrementGuard,
}

impl SlotHandle {
    /// Bumps the slot's in-flight counter and returns the handle. The
    /// counter is decremented when the returned handle is dropped.
    fn new(barrier: Arc<SlotBarrier>) -> Self {
        barrier.increment_inflight();
        let decrement = DecrementGuard {
            barrier: Arc::clone(&barrier),
        };
        Self {
            barrier,
            _decrement: decrement,
        }
    }

    /// Locks the per-file [`FileSlot`] for the duration of the returned
    /// guard. The in-flight counter remains held by `self`; the lock
    /// covers only the per-file write critical section.
    fn lock_slot(&self, ndx: FileNdx, kind: &'static str) -> io::Result<MutexGuard<'_, FileSlot>> {
        self.barrier.lock_slot(ndx, kind)
    }
}

/// CPU-bound verification result handed back from the rayon worker so the
/// owning thread can run the serial per-file write under the per-file mutex.
#[derive(Debug)]
struct VerifiedChunk {
    chunk: DeltaChunk,
    /// Strong-checksum digest computed for `chunk.data` using the
    /// negotiated strategy. Equal to the chunk's `expected_strong` value
    /// (when one was attached) by construction: a mismatch is reported as
    /// a [`ParallelApplyError::ChecksumMismatch`] before this type is
    /// constructed, so a `VerifiedChunk` is only ever produced for data
    /// that has cleared verification.
    digest: ChecksumDigest,
}

/// Parallel receive-side delta applier.
///
/// Fans the CPU-bound verify step across rayon workers while keeping the
/// per-file destination writer serial. The struct is `Send + Sync` so a
/// single instance can back the whole receiver pipeline.
///
/// # Concurrency limit
///
/// The applier respects [`Self::concurrency`] when sharding chunk batches
/// through [`rayon::ThreadPoolBuilder`]'s ambient pool. Callers can size
/// this from [`rayon::current_num_threads`] or from a CLI override.
pub struct ParallelDeltaApplier {
    /// Per-file slots keyed by NDX. The outer map is a [`DashMap`] so the
    /// register/lookup path shards under a fixed number of internal locks
    /// instead of serialising every operation behind one [`Mutex`]. Each
    /// slot value is an [`Arc<SlotBarrier>`] that wraps the per-file
    /// [`Mutex<FileSlot>`] alongside the in-flight counter and [`Condvar`]
    /// that back FFB-2's `flush_workers` barrier. The BR-3j.a audit (see
    /// `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md`) selected
    /// DashMap as the right fit for the access pattern: short guard
    /// windows, no iteration in the hot path, and write rates that scale
    /// with file count rather than chunk count.
    ///
    /// # Locking discipline
    ///
    /// All callers below must drop the DashMap shard guard returned by
    /// `get`/`entry` before doing any work longer than an [`Arc::clone`]
    /// or a small struct write. Holding a shard guard across a wait,
    /// `rayon::join`, or a per-file mutex lock would re-introduce the
    /// outer-map contention the migration was designed to eliminate. In
    /// particular, the barrier wait in `flush_workers` blocks on the
    /// slot's own [`Condvar`] and never re-acquires a shard guard.
    files: DashMap<FileNdx, Arc<SlotBarrier>>,
    /// Reorder-buffer capacity per file. Bounded so a stalled file does not
    /// pin unbounded memory.
    per_file_reorder_capacity: usize,
    /// Maximum number of chunks the applier dispatches to rayon in parallel.
    concurrency: usize,
    /// Negotiated strong-checksum strategy used by [`Self::verify_chunk`].
    ///
    /// Held behind an [`Arc`] so rayon workers can clone the handle cheaply
    /// without re-boxing the trait object. The trait itself is `Send + Sync`
    /// (see `checksums::strong::strategy::ChecksumStrategy`), preserving the
    /// struct-level Send/Sync requirements documented above. BR-3i.b
    /// (#2498) plumbs the field; BR-3i.c (#2499) replaces the
    /// length-only verify stub with `strategy.compute(&chunk.data)`.
    strategy: Arc<dyn ChecksumStrategy>,
}

impl std::fmt::Debug for ParallelDeltaApplier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParallelDeltaApplier")
            .field("file_count", &self.files.len())
            .field("per_file_reorder_capacity", &self.per_file_reorder_capacity)
            .field("concurrency", &self.concurrency)
            .field("strategy", &self.strategy.algorithm_kind())
            .finish()
    }
}

impl ParallelDeltaApplier {
    /// Default per-file reorder buffer capacity. Sized to hold a handful of
    /// rayon workers' worth of in-flight chunks per file without forcing
    /// the producer to block.
    pub const DEFAULT_PER_FILE_REORDER_CAPACITY: usize = 64;

    /// Builds a new applier with the supplied concurrency limit.
    ///
    /// `concurrency == 0` is treated as "use the ambient rayon pool".
    ///
    /// The strong-checksum strategy defaults to MD5 (seed `0`), matching the
    /// protocol >= 30 fallback that
    /// `crates/transfer/src/shared/checksum.rs::ChecksumFactory::from_negotiation`
    /// resolves when no `NegotiationResult` is present. Callers that own a
    /// negotiated algorithm should use [`Self::with_strategy`] instead.
    #[must_use]
    pub fn new(concurrency: usize) -> Self {
        let strategy: Arc<dyn ChecksumStrategy> = Arc::from(
            ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Md5, 0),
        );
        Self::with_strategy(concurrency, strategy)
    }

    /// Builds a new applier with an explicit strong-checksum strategy.
    ///
    /// Used by the receiver pipeline to thread the negotiated algorithm into
    /// the per-chunk verify step. The trait object is held behind an
    /// [`Arc`] so each rayon worker can clone the handle without re-boxing.
    ///
    /// `concurrency == 0` is treated as "use the ambient rayon pool".
    #[must_use]
    pub fn with_strategy(concurrency: usize, strategy: Arc<dyn ChecksumStrategy>) -> Self {
        Self {
            files: DashMap::new(),
            per_file_reorder_capacity: Self::DEFAULT_PER_FILE_REORDER_CAPACITY,
            concurrency,
            strategy,
        }
    }

    /// Returns the configured strong-checksum strategy.
    ///
    /// Exposed so callers (and the BR-3i.c follow-up) can read back the
    /// negotiated algorithm kind for logging and parity assertions.
    #[must_use]
    pub fn strategy(&self) -> &Arc<dyn ChecksumStrategy> {
        &self.strategy
    }

    /// Builder-style override for the per-file reorder buffer capacity.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    #[must_use]
    pub fn with_per_file_reorder_capacity(mut self, capacity: usize) -> Self {
        assert!(capacity > 0, "per-file reorder capacity must be non-zero");
        self.per_file_reorder_capacity = capacity;
        self
    }

    /// Returns the configured concurrency limit.
    #[must_use]
    pub fn concurrency(&self) -> usize {
        self.concurrency
    }

    /// Registers a destination writer for `ndx`.
    ///
    /// Must be called before the first chunk for `ndx` reaches
    /// [`apply_one_chunk`](Self::apply_one_chunk). Returns an
    /// error if `ndx` already has a writer (the receiver opens each file
    /// exactly once).
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if `ndx` is already registered.
    pub fn register_file(
        &self,
        ndx: impl Into<FileNdx>,
        writer: Box<dyn Write + Send>,
    ) -> io::Result<()> {
        let ndx = ndx.into();
        // Pre-build the slot OUTSIDE the shard guard so the reorder-buffer
        // allocation never widens the lock window. Then the entry guard
        // only holds long enough to check vacancy and move the prebuilt
        // Arc into the map. Single shard write lock; no contention on
        // unrelated NDX values.
        let slot = Arc::new(SlotBarrier::new(FileSlot::new(
            writer,
            self.per_file_reorder_capacity,
        )));
        match self.files.entry(ndx) {
            Entry::Occupied(_) => Err(io::Error::other(format!(
                "parallel applier file {ndx} already registered"
            ))),
            Entry::Vacant(vacant) => {
                vacant.insert(slot);
                Ok(())
            }
        }
    }

    /// Applies one chunk, dispatching the CPU-bound verify step to rayon.
    ///
    /// The verify step runs on a rayon worker via [`rayon::join`] so the
    /// ambient pool (or the worker that owns the current thread) handles
    /// the work without spinning up a new pool. The serial write step then
    /// runs under the per-file mutex so per-file byte order is preserved.
    ///
    /// # Scheduling shape
    ///
    /// This entry point schedules a single chunk's verify on a rayon worker
    /// via `rayon::join(verify, || ())`. The second closure is a no-op, so
    /// the caller still blocks until that one verify returns. This is a
    /// single-chunk scheduling primitive, **not** cross-chunk parallelism.
    /// For multi-chunk parallel verify across the rayon pool use
    /// [`apply_batch_parallel`](Self::apply_batch_parallel), which collects a
    /// `Vec<DeltaChunk>` through `into_par_iter` and fans the verifies out
    /// subject to [`Self::concurrency`].
    ///
    /// See `docs/audits/rjn-1-apply-chunk-parallel-call-sites-2026-05-21.md`
    /// for the call-site catalogue and design rationale behind keeping this
    /// per-chunk shape until the receiver pipeline wires a real fan-out
    /// caller (tracked under RJN-3).
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if the file is unknown, a slot mutex is
    /// poisoned, the destination writer fails, the per-file reorder
    /// buffer overflows (a stalled file with unbounded out-of-order
    /// submissions), or the per-chunk strong-checksum verification fails
    /// when [`DeltaChunk::expected_strong`] was attached.
    pub fn apply_one_chunk(&self, chunk: DeltaChunk) -> io::Result<()> {
        // `slot_for` returns a [`SlotHandle`] (RAII guard around an
        // `Arc<SlotBarrier>` clone) and drops the DashMap shard guard
        // before returning, so the rayon verify below never blocks
        // shard-mates on unrelated NDX values. The handle's drop fires
        // `flush_workers`-visible decrement once this call returns.
        let ndx = chunk.ndx;
        let handle = self.slot_for(ndx)?;
        // `rayon::join` schedules the verify on a worker thread when the
        // caller is inside the rayon pool; outside the pool it falls back
        // to the calling thread, which keeps single-threaded callers cheap.
        let strategy = Arc::clone(&self.strategy);
        let (verified, _) = rayon::join(|| Self::verify_chunk(strategy.as_ref(), chunk), || ());
        let verified = verified?;

        let mut slot = handle.lock_slot(ndx, "apply_one_chunk")?;
        let _ = verified.digest; // reserved for future stats wiring
        slot.ingest(verified.chunk)
    }

    /// Applies a batch of chunks, fanning the verify step across the rayon
    /// pool subject to [`Self::concurrency`]. Order-preserving per file.
    ///
    /// Chunks belonging to different files run independently; chunks for the
    /// same file are merged back through the per-file reorder buffer before
    /// they reach the destination writer.
    ///
    /// # Errors
    ///
    /// Returns the first [`io::Error`] encountered while applying the
    /// batch, including any per-chunk strong-checksum mismatch surfaced by
    /// [`Self::verify_chunk`].
    pub fn apply_batch_parallel(&self, chunks: Vec<DeltaChunk>) -> io::Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        let total = chunks.len();
        let cap = if self.concurrency == 0 {
            total
        } else {
            self.concurrency.min(total)
        };
        let min_len = total.div_ceil(cap.max(1)).max(1);
        let strategy = Arc::clone(&self.strategy);
        let verified: Result<Vec<VerifiedChunk>, ParallelApplyError> = chunks
            .into_par_iter()
            .with_min_len(min_len)
            .map(|chunk| Self::verify_chunk(strategy.as_ref(), chunk))
            .collect();
        let verified = verified?;

        for v in verified {
            let ndx = v.chunk.ndx;
            let handle = self.slot_for(ndx)?;
            let mut slot = handle.lock_slot(ndx, "apply_batch_parallel")?;
            slot.ingest(v.chunk)?;
        }
        Ok(())
    }

    /// Returns the total bytes written to `ndx` so far.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if `ndx` is unknown or the per-file slot
    /// mutex is poisoned.
    pub fn bytes_written(&self, ndx: impl Into<FileNdx>) -> io::Result<u64> {
        let ndx = ndx.into();
        let handle = self.slot_for(ndx)?;
        let slot = handle.lock_slot(ndx, "bytes_written")?;
        Ok(slot.bytes_written())
    }

    /// Finalises a file's writer once every submitted chunk has applied.
    ///
    /// Returns the destination writer so the caller can run its own
    /// finalisation step (checksum verify, temp-file rename, metadata
    /// apply). Errors if any chunks remain buffered awaiting a missing
    /// `chunk_sequence`.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if `ndx` is unknown, the slot is still
    /// referenced by another caller, the slot mutex is poisoned, or the
    /// per-file reorder buffer still holds undelivered chunks.
    pub fn finish_file(&self, ndx: impl Into<FileNdx>) -> io::Result<Box<dyn Write + Send>> {
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
        let (_, slot_arc) = self
            .files
            .remove(&ndx)
            .ok_or_else(|| io::Error::other(format!("parallel applier file {ndx} unknown")))?;
        // Post-barrier release-race window: `flush_workers` waits for
        // `inflight==0` via the Condvar, which fires from
        // `DecrementGuard::drop` *before* the guard's own
        // `Arc<SlotBarrier>` clone has been released (the notify happens
        // inside the drop body; the inner Arc only drops after the body
        // returns). The window is typically nanoseconds but is reliably
        // observable on Windows under load. Spin-then-yield until the
        // worker's drop completes; the worker is past the notify and its
        // drop fn is just about to return so the wait is bounded.
        let mut spin = 0u32;
        while Arc::strong_count(&slot_arc) > 1 {
            spin = spin.saturating_add(1);
            if spin >= 1_000 {
                // Past the typical drop window - surface the typed error
                // so a real bug (e.g. caller raced a new `slot_for`
                // against `finish_file`) does not hide forever.
                return Err(ParallelApplyError::ApplierStillReferenced {
                    ndx,
                    strong_count: Arc::strong_count(&slot_arc),
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
        let barrier = Arc::try_unwrap(slot_arc).map_err(|still_shared| {
            ParallelApplyError::ApplierStillReferenced {
                ndx,
                strong_count: Arc::strong_count(&still_shared),
                kind: "finish_file",
            }
        })?;
        let slot = barrier
            .slot
            .into_inner()
            .map_err(|_| ParallelApplyError::SlotPoisoned {
                ndx,
                kind: "finish_file",
            })?;
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
    /// the slot's [`Condvar`] until that counter is observed to be zero.
    /// Spurious wakeups are filtered by the wait-while predicate.
    ///
    /// Returns [`Ok`] immediately if `ndx` is not registered (or has
    /// already been finalised through [`Self::finish_file`]); the absence
    /// of a slot is the same observable outcome as a fully-drained slot.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] only if the slot's in-flight mutex was
    /// poisoned by a panicking worker. In that case the typed
    /// [`ParallelApplyError::SlotPoisoned`] variant carries the offending
    /// `ndx` and the `"flush_workers"` call-site tag.
    pub fn flush_workers(&self, ndx: impl Into<FileNdx>) -> io::Result<()> {
        let ndx = ndx.into();
        // Look up the slot, clone the `Arc<SlotBarrier>`, drop the shard
        // guard before waiting. This keeps the DashMap shard available to
        // other NDX values while the caller blocks on the slot's own
        // condvar, preserving the BR-3j.c shard-discipline contract.
        let barrier = match self.files.get(&ndx) {
            Some(guard) => Arc::clone(guard.value()),
            None => return Ok(()),
        };
        barrier.wait_until_idle(ndx, "flush_workers")
    }

    /// Blocks until every registered slot in the applier has zero in-flight
    /// workers.
    ///
    /// Implemented as a thin loop over [`Self::flush_workers`] (FFB-1
    /// Option B). Used by panic/abort/shutdown paths that need to retire
    /// every slot in one shot; normal per-file completion goes through
    /// [`Self::finish_file`]'s baked-in barrier instead.
    ///
    /// Snapshots the current set of registered file indices before
    /// iterating so no shard guard is held across a wait. Files
    /// registered after the snapshot are intentionally skipped: the
    /// caller asked to drain the workers that exist now, not to chase
    /// new submissions arriving after the call.
    ///
    /// # Errors
    ///
    /// Returns the first [`io::Error`] surfaced by [`Self::flush_workers`]
    /// (poisoned inflight mutex on a slot).
    pub fn drain_inflight(&self) -> io::Result<()> {
        // Snapshot the keys without holding any shard guard during the
        // subsequent waits.
        let keys: Vec<FileNdx> = self.files.iter().map(|entry| *entry.key()).collect();
        for ndx in keys {
            self.flush_workers(ndx)?;
        }
        Ok(())
    }

    fn slot_for(&self, ndx: FileNdx) -> io::Result<SlotHandle> {
        // Clone the per-file `Arc` while the shard read guard is alive,
        // then drop the guard at the end of this expression. Callers
        // never see the DashMap guard, so they cannot accidentally hold
        // it across the per-file mutex lock or a rayon dispatch. The
        // returned [`SlotHandle`] bumps the slot's in-flight counter for
        // the duration of its lifetime so `flush_workers` can wait on it.
        let barrier = self
            .files
            .get(&ndx)
            .map(|guard| Arc::clone(guard.value()))
            .ok_or_else(|| io::Error::other(format!("parallel applier file {ndx} unknown")))?;
        Ok(SlotHandle::new(barrier))
    }

    /// Pure CPU step that the rayon worker runs.
    ///
    /// Computes the strong checksum of `chunk.data` using the negotiated
    /// `strategy` (see [`Self::with_strategy`]). When the chunk carries a
    /// [`DeltaChunk::expected_strong`] digest, the computed value is
    /// compared byte-for-byte against the expected one; a mismatch
    /// produces a [`ParallelApplyError::ChecksumMismatch`] so the
    /// receiver can abort the file or fall back to a phase-2 redo
    /// without committing the corrupt bytes to the destination writer.
    ///
    /// When `expected_strong` is `None` the comparison is skipped, but
    /// the digest is still computed so the parallel verify step has a
    /// stable CPU cost regardless of whether the producer attached an
    /// expectation. This preserves the cross-core amortisation the
    /// design doc relies on while keeping the verified-vs-unverified
    /// shapes interchangeable for backward-compatible callers.
    ///
    /// Implements BR-3i.c (#2499); the strategy plumbing landed in
    /// BR-3i.b (#2498).
    fn verify_chunk(
        strategy: &dyn ChecksumStrategy,
        chunk: DeltaChunk,
    ) -> Result<VerifiedChunk, ParallelApplyError> {
        let digest = strategy.compute(&chunk.data);
        if let Some(expected) = chunk.expected_strong.as_ref() {
            // `ChecksumDigest` carries both bytes and len; rely on its
            // `PartialEq` impl which compares the active byte ranges and
            // ignores the unused tail of the fixed-capacity buffer.
            if expected != &digest {
                return Err(ParallelApplyError::ChecksumMismatch {
                    ndx: chunk.ndx,
                    chunk_sequence: chunk.chunk_sequence,
                    algorithm: strategy.algorithm_kind(),
                    expected: hex_lower(expected.as_bytes()),
                    actual: hex_lower(digest.as_bytes()),
                });
            }
        }
        Ok(VerifiedChunk { chunk, digest })
    }
}

/// Lowercase-hex formatter used in [`ParallelApplyError::ChecksumMismatch`]
/// messages. Avoids pulling in a hex crate for a single, error-path use.
fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from_digit((b >> 4) as u32, 16).expect("hi nibble"));
        out.push(char::from_digit((b & 0x0f) as u32, 16).expect("lo nibble"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashMap;
    use std::io::Cursor;

    /// In-memory sink that records every byte written so tests can compare
    /// parallel vs sequential output.
    struct VecSink {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl VecSink {
        fn new() -> (Self, Arc<Mutex<Vec<u8>>>) {
            let buf = Arc::new(Mutex::new(Vec::new()));
            (Self { buf: buf.clone() }, buf)
        }
    }

    impl Write for VecSink {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            let mut guard = self.buf.lock().expect("sink mutex poisoned");
            guard.extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn sequential_apply(chunks: &[DeltaChunk]) -> Vec<u8> {
        let mut by_file: HashMap<FileNdx, Vec<&DeltaChunk>> = HashMap::new();
        for c in chunks {
            by_file.entry(c.ndx).or_default().push(c);
        }
        let mut ndxs: Vec<FileNdx> = by_file.keys().copied().collect();
        ndxs.sort();
        let mut out = Vec::new();
        for ndx in ndxs {
            let mut per_file = by_file.remove(&ndx).expect("present");
            per_file.sort_by_key(|c| c.chunk_sequence);
            for c in per_file {
                out.extend_from_slice(&c.data);
            }
        }
        out
    }

    fn collect_file(
        applier: &ParallelDeltaApplier,
        ndx: FileNdx,
        buf: Arc<Mutex<Vec<u8>>>,
    ) -> Vec<u8> {
        let _writer = applier.finish_file(ndx).expect("finish_file");
        buf.lock().expect("sink mutex").clone()
    }

    #[test]
    fn single_file_in_order_matches_sequential() {
        let applier = ParallelDeltaApplier::new(2);
        let (sink, buf) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();

        let chunks: Vec<DeltaChunk> = (0..16)
            .map(|i| DeltaChunk::literal(0u32, i, vec![i as u8; 8]))
            .collect();
        let expected = sequential_apply(&chunks);

        for c in chunks {
            applier.apply_one_chunk(c).unwrap();
        }
        assert_eq!(collect_file(&applier, FileNdx::new(0), buf), expected);
    }

    #[test]
    fn single_file_out_of_order_preserves_byte_order() {
        let applier = ParallelDeltaApplier::new(4);
        let (sink, buf) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();

        let chunks: Vec<DeltaChunk> = (0..32)
            .map(|i| DeltaChunk::literal(0u32, i, vec![i as u8; 4]))
            .collect();
        let expected = sequential_apply(&chunks);

        let mut shuffled = chunks.clone();
        // Deterministic non-trivial permutation: rotate by 7.
        shuffled.rotate_left(7);

        for c in shuffled {
            applier.apply_one_chunk(c).unwrap();
        }
        assert_eq!(collect_file(&applier, FileNdx::new(0), buf), expected);
    }

    #[test]
    fn batch_apply_matches_sequential_byte_for_byte() {
        let applier = ParallelDeltaApplier::new(8);
        let (sink_a, buf_a) = VecSink::new();
        let (sink_b, buf_b) = VecSink::new();
        applier.register_file(0u32, Box::new(sink_a)).unwrap();
        applier.register_file(1u32, Box::new(sink_b)).unwrap();

        let mut chunks = Vec::new();
        for i in 0..24u64 {
            let payload: Vec<u8> = (0..16).map(|b| (i as u8).wrapping_add(b)).collect();
            chunks.push(DeltaChunk::literal(0u32, i, payload.clone()));
            chunks.push(DeltaChunk::matched(1u32, i, payload));
        }
        let expected_a = sequential_apply(
            &chunks
                .iter()
                .filter(|c| c.ndx == FileNdx::new(0))
                .cloned()
                .collect::<Vec<_>>(),
        );
        let expected_b = sequential_apply(
            &chunks
                .iter()
                .filter(|c| c.ndx == FileNdx::new(1))
                .cloned()
                .collect::<Vec<_>>(),
        );

        applier.apply_batch_parallel(chunks).unwrap();
        assert_eq!(collect_file(&applier, FileNdx::new(0), buf_a), expected_a);
        assert_eq!(collect_file(&applier, FileNdx::new(1), buf_b), expected_b);
    }

    #[test]
    fn missing_file_registration_errors() {
        let applier = ParallelDeltaApplier::new(1);
        let err = applier
            .apply_one_chunk(DeltaChunk::literal(7u32, 0, vec![1, 2, 3]))
            .unwrap_err();
        assert!(err.to_string().contains("unknown"));
    }

    #[test]
    fn double_registration_errors() {
        let applier = ParallelDeltaApplier::new(1);
        let (sink_a, _) = VecSink::new();
        let (sink_b, _) = VecSink::new();
        applier.register_file(3u32, Box::new(sink_a)).unwrap();
        let err = applier.register_file(3u32, Box::new(sink_b)).unwrap_err();
        assert!(err.to_string().contains("already registered"));
    }

    #[test]
    fn finish_file_with_pending_chunks_errors() {
        let applier = ParallelDeltaApplier::new(1);
        let (sink, _) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();

        // Submit out-of-order chunk; sequence 0 never arrives.
        applier
            .apply_one_chunk(DeltaChunk::literal(0u32, 1, vec![0u8; 4]))
            .unwrap();
        match applier.finish_file(0u32) {
            Ok(_) => panic!("finish_file should fail with pending chunks"),
            Err(e) => assert!(e.to_string().contains("still buffered")),
        }
    }

    #[test]
    fn bytes_written_tracks_in_order_writes() {
        let applier = ParallelDeltaApplier::new(2);
        let (sink, _) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();
        applier
            .apply_one_chunk(DeltaChunk::literal(0u32, 0, vec![1u8; 100]))
            .unwrap();
        assert_eq!(applier.bytes_written(0u32).unwrap(), 100);
        applier
            .apply_one_chunk(DeltaChunk::literal(0u32, 1, vec![2u8; 50]))
            .unwrap();
        assert_eq!(applier.bytes_written(0u32).unwrap(), 150);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn random_chunk_sizes_and_permutations_match_sequential(
            sizes in prop::collection::vec(1usize..=64usize, 1..=48),
            seed in 0u64..512,
        ) {
            let chunks: Vec<DeltaChunk> = sizes
                .iter()
                .enumerate()
                .map(|(i, &len)| {
                    let payload: Vec<u8> = (0..len)
                        .map(|b| ((i as u64 ^ seed ^ b as u64) & 0xff) as u8)
                        .collect();
                    DeltaChunk::literal(0u32, i as u64, payload)
                })
                .collect();
            let expected = sequential_apply(&chunks);

            // Permute deterministically by `seed` to simulate parallel-completion order.
            let mut order: Vec<usize> = (0..chunks.len()).collect();
            // Fisher-Yates with a small xorshift seeded by `seed`.
            let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
            for i in (1..order.len()).rev() {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let j = (state as usize) % (i + 1);
                order.swap(i, j);
            }
            let permuted: Vec<DeltaChunk> = order.into_iter().map(|i| chunks[i].clone()).collect();

            let applier = ParallelDeltaApplier::new(((seed % 8) + 1) as usize);
            let (sink, buf) = VecSink::new();
            applier.register_file(0u32, Box::new(sink)).unwrap();
            for c in permuted {
                applier.apply_one_chunk(c).unwrap();
            }
            let actual = collect_file(&applier, FileNdx::new(0), buf);
            prop_assert_eq!(actual, expected);
        }
    }

    #[test]
    fn cursor_writer_round_trip() {
        // Smoke test that the trait-object writer wraps anything `Write + Send`.
        let applier = ParallelDeltaApplier::new(1);
        let cursor: Cursor<Vec<u8>> = Cursor::new(Vec::new());
        applier.register_file(0u32, Box::new(cursor)).unwrap();
        applier
            .apply_one_chunk(DeltaChunk::literal(0u32, 0, vec![9u8; 32]))
            .unwrap();
        let _writer = applier.finish_file(0u32).unwrap();
    }

    #[test]
    fn flush_workers_returns_immediately_when_no_inflight() {
        // FFB-2: with no apply calls outstanding, `flush_workers` must
        // observe zero in-flight handles and return without blocking.
        let applier = ParallelDeltaApplier::new(1);
        let (sink, _) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();
        let start = std::time::Instant::now();
        applier.flush_workers(0u32).expect("flush_workers");
        // Generous bound so loaded CI hosts do not flake; the call must
        // be effectively instant because the inflight counter starts at
        // zero and no worker is registered.
        assert!(
            start.elapsed() < std::time::Duration::from_millis(50),
            "flush_workers should not block when nothing is in flight"
        );
    }

    #[test]
    fn flush_workers_returns_ok_for_unknown_ndx() {
        // FFB-2: absent slot is the same observable outcome as
        // fully-drained slot; the API contract is "wait until idle", and
        // a slot that does not exist is idle by definition.
        let applier = ParallelDeltaApplier::new(1);
        applier.flush_workers(99u32).expect("no-op flush_workers");
    }

    #[test]
    fn flush_workers_blocks_until_worker_drops_arc() {
        // FFB-2: a worker thread holds a SlotHandle clone for ~50ms;
        // flush_workers must not return until the handle drops. Uses
        // raw `slot_for` to exercise the barrier directly without going
        // through `apply_one_chunk` (which internally bounds the
        // handle lifetime to the call itself).
        let applier = Arc::new(ParallelDeltaApplier::new(1));
        let (sink, _) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();

        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel::<()>();
        let worker_applier = Arc::clone(&applier);
        let hold_duration = std::time::Duration::from_millis(50);
        let worker = std::thread::spawn(move || {
            let handle = worker_applier
                .slot_for(FileNdx::new(0))
                .expect("slot registered");
            acquired_tx.send(()).expect("handshake send");
            std::thread::sleep(hold_duration);
            drop(handle);
        });

        // Wait for the worker to acquire its handle deterministically.
        // The sleep-based barrier raced on macOS nightly when the OS
        // didn't schedule the worker before the main thread started the
        // timer, causing flush_workers to return immediately (inflight=0)
        // and the elapsed-time assertion to fire.
        acquired_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("worker acquired handle");

        let start = std::time::Instant::now();
        applier.flush_workers(0u32).expect("flush_workers");
        let elapsed = start.elapsed();
        worker.join().expect("worker thread");
        assert!(
            elapsed >= std::time::Duration::from_millis(40),
            "flush_workers returned too early: {elapsed:?}"
        );
    }

    #[test]
    fn drain_inflight_drains_all_files() {
        // FFB-2: register N files, hand a SlotHandle clone out to a
        // worker per file, call drain_inflight, assert it blocks until
        // every worker drops its handle.
        const N: u32 = 6;
        let applier = Arc::new(ParallelDeltaApplier::new(2));
        for ndx in 0..N {
            let (sink, _) = VecSink::new();
            applier.register_file(ndx, Box::new(sink)).unwrap();
        }

        let hold_duration = std::time::Duration::from_millis(40);
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel::<()>();
        let mut handles = Vec::with_capacity(N as usize);
        for ndx in 0..N {
            let worker_applier = Arc::clone(&applier);
            let acquired_tx = acquired_tx.clone();
            handles.push(std::thread::spawn(move || {
                let handle = worker_applier
                    .slot_for(FileNdx::new(ndx))
                    .expect("slot registered");
                acquired_tx.send(()).expect("handshake send");
                std::thread::sleep(hold_duration);
                drop(handle);
            }));
        }
        drop(acquired_tx);

        // Wait for every worker to grab its handle before the drain call.
        // Replaces a sleep-based barrier that raced on macOS where workers
        // had not yet entered slot_for when drain_inflight was invoked.
        for _ in 0..N {
            acquired_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("worker acquired handle");
        }

        let start = std::time::Instant::now();
        applier.drain_inflight().expect("drain_inflight");
        let elapsed = start.elapsed();
        for h in handles {
            h.join().expect("worker thread");
        }
        assert!(
            elapsed >= std::time::Duration::from_millis(30),
            "drain_inflight returned before workers dropped: {elapsed:?}"
        );
    }

    #[test]
    fn finish_file_calls_flush_workers_internally() {
        // FFB-2 Option D: finish_file bakes the barrier in. A worker
        // that holds a SlotHandle clone for a bounded duration must not
        // cause finish_file to return ApplierStillReferenced; instead
        // finish_file blocks until the worker drops the handle, then
        // succeeds.
        //
        // The handshake replaces the previous sleep-based "let the
        // worker get going" coordination, which raced on macOS where
        // the main thread reached finish_file before the worker had
        // acquired the SlotHandle (inflight stayed 0, the barrier
        // returned immediately, and the elapsed-time assertion fired).
        let applier = Arc::new(ParallelDeltaApplier::new(1));
        let (sink, _) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();

        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel::<()>();
        let worker_applier = Arc::clone(&applier);
        let worker = std::thread::spawn(move || {
            let handle = worker_applier
                .slot_for(FileNdx::new(0))
                .expect("slot registered");
            acquired_tx.send(()).expect("handshake send");
            std::thread::sleep(std::time::Duration::from_millis(40));
            drop(handle);
        });
        acquired_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("worker acquired handle");

        let start = std::time::Instant::now();
        let _writer = applier.finish_file(0u32).expect("finish_file");
        let elapsed = start.elapsed();
        worker.join().expect("worker thread");
        assert!(
            elapsed >= std::time::Duration::from_millis(30),
            "finish_file returned before worker dropped: {elapsed:?}"
        );
    }

    #[test]
    fn flush_workers_survives_spurious_wakeup() {
        // Condvars are permitted to wake spuriously; the wait_while
        // predicate in `SlotBarrier::wait_until_idle` must re-check
        // under the mutex and continue waiting. We exercise the
        // predicate by notifying the slot's condvar manually while the
        // inflight counter is still > 0, then verifying flush_workers
        // only returns once the counter actually reaches zero. The
        // AtomicBool gate proves the flusher did not exit until the
        // handle drop fired the real (non-spurious) decrement.
        use std::sync::atomic::{AtomicBool, Ordering};

        let applier = Arc::new(ParallelDeltaApplier::new(1));
        let (sink, _) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();

        // Grab a handle to bump inflight to 1, then arrange for spurious
        // notifications to land while flush_workers is waiting.
        let handle = applier.slot_for(FileNdx::new(0)).expect("slot registered");

        // Snapshot the inner Arc so a sibling thread can notify the
        // slot's condvar without going through the apply path.
        let barrier = applier
            .files
            .get(&FileNdx::new(0))
            .map(|guard| Arc::clone(guard.value()))
            .expect("slot present");

        let notifier_barrier = Arc::clone(&barrier);
        let notifier = std::thread::spawn(move || {
            for _ in 0..5 {
                std::thread::sleep(std::time::Duration::from_millis(8));
                notifier_barrier.notify.notify_all();
            }
        });

        // Tracks whether the flusher returned before we released the
        // handle. If the wait predicate was wrong and a spurious wakeup
        // shipped through `wait_while`, the flusher would join before
        // `released` flipped to true.
        let released = Arc::new(AtomicBool::new(false));
        let released_for_flusher = Arc::clone(&released);
        let flush_applier = Arc::clone(&applier);
        let flusher = std::thread::spawn(move || {
            flush_applier.flush_workers(0u32).expect("flush_workers");
            assert!(
                released_for_flusher.load(Ordering::SeqCst),
                "flush_workers returned before the slot handle was released - \
                 spurious wakeup escaped the wait_while predicate"
            );
        });

        // Let the notifier fire several spurious wakeups, then release
        // the handle so the predicate finally evaluates to false.
        std::thread::sleep(std::time::Duration::from_millis(60));
        released.store(true, Ordering::SeqCst);
        drop(handle);

        notifier.join().expect("notifier thread");
        flusher.join().expect("flusher thread");
    }

    #[test]
    fn parallel_apply_error_display_carries_ndx_and_strong_count() {
        let err = ParallelApplyError::ApplierStillReferenced {
            ndx: FileNdx::new(7),
            strong_count: 3,
            kind: "finish_file",
        };
        let msg = err.to_string();
        assert!(msg.contains("finish_file"));
        assert!(msg.contains("ndx=7"));
        assert!(msg.contains("strong_count=3"));
    }

    #[test]
    fn parallel_apply_error_converts_into_io_error_with_typed_message() {
        let err: io::Error = ParallelApplyError::SlotPoisoned {
            ndx: FileNdx::new(2),
            kind: "finish_file",
        }
        .into();
        assert_eq!(err.kind(), io::ErrorKind::Other);
        let msg = err.to_string();
        assert!(msg.contains("poisoned"));
        assert!(msg.contains("ndx=2"));
    }

    #[test]
    fn new_defaults_strategy_to_md5() {
        // BR-3i.b: `new(concurrency)` must default to the protocol >= 30
        // fallback (MD5) so existing test/bench callers keep working without
        // observing a behaviour change.
        let applier = ParallelDeltaApplier::new(1);
        assert_eq!(
            applier.strategy().algorithm_kind(),
            ChecksumAlgorithmKind::Md5
        );
        assert_eq!(applier.strategy().digest_len(), 16);
    }

    #[test]
    fn with_strategy_threads_negotiated_algorithm() {
        // BR-3i.b: `with_strategy(concurrency, strategy)` is the constructor
        // the receiver pipeline will use once the negotiated algorithm is
        // wired in. Verify it stores and exposes the supplied trait object.
        let strategy: Arc<dyn ChecksumStrategy> = Arc::from(
            ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Xxh3, 0),
        );
        let applier = ParallelDeltaApplier::with_strategy(4, Arc::clone(&strategy));
        assert_eq!(
            applier.strategy().algorithm_kind(),
            ChecksumAlgorithmKind::Xxh3
        );
        // The applier shares the strategy by Arc, so cheap clones reach
        // rayon workers without re-boxing.
        assert!(Arc::ptr_eq(applier.strategy(), &strategy));
    }

    #[test]
    fn unverified_chunk_preserves_writer_byte_stream() {
        // BR-3i.c: when a chunk carries no `expected_strong`, the applier
        // still computes a digest (so the parallel verify path has stable
        // CPU cost) but skips comparison, leaving the writer byte stream
        // unchanged. Backward-compatible callers must keep working.
        let strategy: Arc<dyn ChecksumStrategy> = Arc::from(
            ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Xxh3, 0),
        );
        let applier = ParallelDeltaApplier::with_strategy(1, strategy);
        let (sink, buf) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();
        applier
            .apply_one_chunk(DeltaChunk::literal(0u32, 0, vec![0xAB; 64]))
            .unwrap();
        let _writer = applier.finish_file(0u32).unwrap();
        assert_eq!(buf.lock().unwrap().len(), 64);
    }

    /// Helper: builds a chunk whose `expected_strong` matches the digest
    /// the supplied strategy will compute over `data`. Used by the BR-3i.c
    /// happy-path tests so the fixture stays in lockstep with the
    /// negotiated algorithm.
    fn chunk_with_correct_digest(
        strategy: &dyn ChecksumStrategy,
        ndx: u32,
        sequence: u64,
        data: Vec<u8>,
    ) -> DeltaChunk {
        let digest = strategy.compute(&data);
        DeltaChunk::literal(ndx, sequence, data).with_expected_strong(digest)
    }

    #[test]
    fn verify_chunk_accepts_matching_digest_md5() {
        // BR-3i.c happy path: MD5 (protocol >= 30 fallback) chunk with the
        // correct expected digest applies cleanly and writes the original
        // bytes to the sink.
        let applier = ParallelDeltaApplier::new(1);
        let strategy = Arc::clone(applier.strategy());
        let (sink, buf) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();
        let chunk = chunk_with_correct_digest(strategy.as_ref(), 0, 0, vec![0x42; 128]);
        applier.apply_one_chunk(chunk).unwrap();
        let _writer = applier.finish_file(0u32).unwrap();
        assert_eq!(buf.lock().unwrap().len(), 128);
        assert!(buf.lock().unwrap().iter().all(|&b| b == 0x42));
    }

    #[test]
    fn verify_chunk_accepts_matching_digest_xxh3() {
        // BR-3i.c happy path under the XXH3 negotiated algorithm: confirms
        // the dispatch routes through the configured strategy, not a
        // hard-coded MD5 path.
        let strategy: Arc<dyn ChecksumStrategy> = Arc::from(
            ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Xxh3, 0),
        );
        let applier = ParallelDeltaApplier::with_strategy(2, Arc::clone(&strategy));
        let (sink, buf) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();
        let chunk = chunk_with_correct_digest(strategy.as_ref(), 0, 0, vec![0xAA; 200]);
        applier.apply_one_chunk(chunk).unwrap();
        let _writer = applier.finish_file(0u32).unwrap();
        assert_eq!(buf.lock().unwrap().len(), 200);
    }

    #[test]
    fn verify_chunk_rejects_mismatched_digest_and_does_not_write() {
        // BR-3i.c error path: a chunk with a deliberately wrong expected
        // digest must fail verification, surface the typed
        // `ChecksumMismatch`, and never reach the destination writer.
        let applier = ParallelDeltaApplier::new(1);
        let (sink, buf) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();
        // Bogus expected digest: all-zero MD5 (16 bytes) will not match any
        // non-empty payload's real digest.
        let bogus = ChecksumDigest::new(&[0u8; 16]);
        let chunk = DeltaChunk::literal(0u32, 0, vec![0x99; 64]).with_expected_strong(bogus);
        let err = applier.apply_one_chunk(chunk).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("checksum mismatch"), "msg was: {msg}");
        assert!(msg.contains("ndx=0"), "msg was: {msg}");
        assert!(msg.contains("sequence=0"), "msg was: {msg}");
        assert!(msg.contains("algorithm=md5"), "msg was: {msg}");
        // The writer must remain untouched: the verify failure happens
        // before the per-file mutex is taken.
        assert!(buf.lock().unwrap().is_empty());
    }

    #[test]
    fn verify_batch_rejects_mismatched_digest() {
        // BR-3i.c error path under the batch entry point. The rayon
        // parallel `collect` short-circuits on the first error, surfacing
        // the typed `ChecksumMismatch` instead of any successful write.
        let applier = ParallelDeltaApplier::new(4);
        let strategy = Arc::clone(applier.strategy());
        let (sink, _) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();

        let good_a = chunk_with_correct_digest(strategy.as_ref(), 0, 0, vec![1u8; 32]);
        let bad = DeltaChunk::literal(0u32, 1, vec![2u8; 32])
            .with_expected_strong(ChecksumDigest::new(&[0u8; 16]));
        let good_b = chunk_with_correct_digest(strategy.as_ref(), 0, 2, vec![3u8; 32]);

        let err = applier
            .apply_batch_parallel(vec![good_a, bad, good_b])
            .unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"));
    }

    #[test]
    fn parallel_apply_with_real_digests_matches_sequential_byte_for_byte() {
        // BR-3i.e regression test: parallel apply with real per-chunk
        // strong-checksum verification produces the same destination byte
        // stream as the sequential reference path. Guards against future
        // regressions where the verify path mutates `chunk.data` or
        // reorders writes when the strategy short-circuits.
        let strategy: Arc<dyn ChecksumStrategy> = Arc::from(
            ChecksumStrategySelector::for_algorithm(ChecksumAlgorithmKind::Md5, 0),
        );
        let applier = ParallelDeltaApplier::with_strategy(4, Arc::clone(&strategy));
        let (sink, buf) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();

        let chunks: Vec<DeltaChunk> = (0..32u64)
            .map(|i| {
                let payload: Vec<u8> = (0..64u8).map(|b| b.wrapping_add(i as u8)).collect();
                chunk_with_correct_digest(strategy.as_ref(), 0, i, payload)
            })
            .collect();
        let expected = sequential_apply(&chunks);

        // Deterministic non-trivial permutation: rotate by 5 so workers
        // see chunks out of submission order; the reorder buffer must
        // still emit them in `chunk_sequence` order.
        let mut shuffled = chunks;
        shuffled.rotate_left(5);
        applier.apply_batch_parallel(shuffled).unwrap();
        let _writer = applier.finish_file(0u32).unwrap();
        assert_eq!(buf.lock().unwrap().clone(), expected);
    }

    #[test]
    fn checksum_mismatch_error_converts_into_io_error_with_typed_message() {
        let err: io::Error = ParallelApplyError::ChecksumMismatch {
            ndx: FileNdx::new(9),
            chunk_sequence: 42,
            algorithm: ChecksumAlgorithmKind::Md5,
            expected: "deadbeef".to_string(),
            actual: "cafef00d".to_string(),
        }
        .into();
        assert_eq!(err.kind(), io::ErrorKind::Other);
        let msg = err.to_string();
        assert!(msg.contains("checksum mismatch"));
        assert!(msg.contains("ndx=9"));
        assert!(msg.contains("sequence=42"));
        assert!(msg.contains("algorithm=md5"));
        assert!(msg.contains("expected=deadbeef"));
        assert!(msg.contains("actual=cafef00d"));
    }
}
