//! The [`ParallelDeltaApplier`] struct and its core register/apply/verify
//! impl.
//!
//! Extracted from `parallel_apply/mod.rs` as part of the module
//! decomposition. Holds the applier's data members (the per-file
//! [`SlotEntry`] map, concurrency limit, negotiated strong-checksum
//! strategy, and reorder-saturation telemetry), its constructors and
//! accessors, the single-chunk [`ParallelDeltaApplier::apply_one_chunk`]
//! path, the [`ParallelDeltaApplier::slot_for`] handle factory, and the
//! pure-CPU [`ParallelDeltaApplier::verify_chunk`] step. The batched apply
//! path lives in [`super::batch`] and the drain/finish primitives in
//! [`super::drain`]; both extend this same struct via additional `impl`
//! blocks.

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use checksums::strong::strategy::{
    ChecksumAlgorithmKind, ChecksumStrategy, ChecksumStrategySelector,
};
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;

use super::super::types::FileNdx;
use super::chunk::{DeltaChunk, VerifiedChunk};
use super::error::ParallelApplyError;
use super::file_slot::{FileSlot, IngestError};
use super::handle::SlotHandle;
use super::slot_barrier::{SlotBarrier, SlotEntry};
use super::{ring_cap_env, shard_sizing};

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
    /// instead of serialising every operation behind one [`std::sync::Mutex`]. Each
    /// slot value is a [`SlotEntry`] carrying the per-file payload
    /// (an [`Arc<slot_barrier::SlotData>`] wrapping
    /// [`std::sync::Mutex<FileSlot>`]) and the per-file bookkeeping
    /// (an [`Arc<slot_barrier::BarrierState>`] holding the in-flight
    /// counter and [`std::sync::Condvar`] that back FFB-2's
    /// `flush_workers` barrier). DG-3.b (#2569) swapped the value type
    /// from [`Arc<SlotBarrier>`] to [`SlotEntry`]; DG-3.c retyped
    /// [`DecrementGuard`] to consume an
    /// [`Arc<slot_barrier::BarrierState>`] sourced via
    /// [`SlotBarrier::barrier`], and [`SlotHandle`] keeps its
    /// [`Arc<SlotBarrier>`] adapter (minted by
    /// [`SlotBarrier::from_entry`]) until a follow-on DG-3.x task
    /// retypes the handle. The BR-3j.a audit (see
    /// `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md`) selected
    /// DashMap as the right fit for the access pattern: short guard
    /// windows, no iteration in the hot path, and write rates that scale
    /// with file count rather than chunk count.
    ///
    /// # Locking discipline
    ///
    /// All callers below must drop the DashMap shard guard returned by
    /// `get`/`entry` before doing any work longer than an
    /// [`SlotEntry::clone`] (two [`Arc::clone`] calls) or a small struct
    /// write. Holding a shard guard across a wait, `rayon::join`, or a
    /// per-file mutex lock would re-introduce the outer-map contention
    /// the migration was designed to eliminate. In particular, the
    /// barrier wait in `flush_workers` blocks on the slot's own
    /// [`std::sync::Condvar`] and never re-acquires a shard guard.
    ///
    /// [`slot_barrier::SlotData`]: super::slot_barrier::SlotData
    /// [`slot_barrier::BarrierState`]: super::slot_barrier::BarrierState
    /// [`DecrementGuard`]: super::decrement_guard::DecrementGuard
    /// [`SlotBarrier::barrier`]: super::slot_barrier::SlotBarrier::barrier
    /// [`SlotBarrier::from_entry`]: super::slot_barrier::SlotBarrier::from_entry
    /// [`Arc<SlotBarrier>`]: std::sync::Arc
    /// [`Arc<slot_barrier::SlotData>`]: std::sync::Arc
    /// [`Arc<slot_barrier::BarrierState>`]: std::sync::Arc
    pub(super) files: DashMap<FileNdx, SlotEntry>,
    /// Reorder-buffer capacity per file. Bounded so a stalled file does not
    /// pin unbounded memory.
    per_file_reorder_capacity: usize,
    /// Maximum number of chunks the applier dispatches to rayon in parallel.
    pub(super) concurrency: usize,
    /// Negotiated strong-checksum strategy used by [`Self::verify_chunk`].
    ///
    /// Held behind an [`Arc`] so rayon workers can clone the handle cheaply
    /// without re-boxing the trait object. The trait itself is `Send + Sync`
    /// (see `checksums::strong::strategy::ChecksumStrategy`), preserving the
    /// struct-level Send/Sync requirements documented above. BR-3i.b
    /// (#2498) plumbs the field; BR-3i.c (#2499) replaces the
    /// length-only verify stub with `strategy.compute(&chunk.data)`.
    pub(super) strategy: Arc<dyn ChecksumStrategy>,
    /// Cumulative count of per-file reorder-ring saturation events
    /// observed since the applier was constructed (ROB-2, #3667).
    ///
    /// Incremented exactly once per [`IngestError::ReorderSaturated`]
    /// returned by [`FileSlot::ingest`]. The per-file applier has no
    /// spill backend today, so saturation events surface as
    /// [`io::Error::other`] back to the caller; this counter lets
    /// operators observe the rate without parsing error strings.
    ///
    /// Exposed via [`Self::reorder_saturations`]. Pairs with
    /// [`reorder_saturated_warned`](Self::reorder_saturated_warned) so the
    /// first event also emits the ROB-3 one-shot warning.
    reorder_saturations: AtomicU64,
    /// One-shot guard ensuring the per-file ring-saturation warning fires
    /// at most once for the lifetime of this applier (ROB-3, #3667).
    ///
    /// The first [`IngestError::ReorderSaturated`] swaps this from `false`
    /// to `true` and emits a `tracing::warn!` (mirrored to stderr) that
    /// names the saturated file, the in-effect ring capacity, the
    /// `OC_RSYNC_REORDER_RING_CAP` env knob, and the registered file
    /// count. Subsequent saturations only bump
    /// [`reorder_saturations`](Self::reorder_saturations); the operator
    /// sees one warning per transfer rather than one per file.
    reorder_saturated_warned: AtomicBool,
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
    ///
    /// Operators can override this default (and any future adaptive sizing)
    /// by exporting `OC_RSYNC_REORDER_RING_CAP` to a positive integer; see
    /// `ring_cap_env` for the parser contract and ROB-11 (#3678) for the
    /// rationale.
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
    ///
    /// # DashMap shard sizing (DMC-CON.3, #3997)
    ///
    /// The internal `files: DashMap` is constructed with a shard count
    /// adapted to the applier's actual `concurrency` rather than DashMap's
    /// default `available_parallelism() * 4`. The heuristic
    /// (`(concurrency * 4).next_power_of_two().clamp(4, 1024)`) trades the
    /// default's CPU-relative sizing for one that tracks the applier's
    /// worker fan-out. See `shard_sizing` and
    /// `docs/design/dmc-con-adaptive-sharding.md` for the rationale, and
    /// the `OC_RSYNC_DASHMAP_SHARDS` env override for tuning.
    ///
    /// # Per-file ring capacity (ROB-11, #3678)
    ///
    /// The per-file reorder-ring capacity defaults to
    /// [`Self::DEFAULT_PER_FILE_REORDER_CAPACITY`] but is overridden when
    /// `OC_RSYNC_REORDER_RING_CAP` is set to a positive integer. The env
    /// var is read once per process via `ring_cap_env` and applies to
    /// every applier constructed afterwards, including ones built via
    /// [`Self::new`]. Callers that need a per-instance override should
    /// chain [`Self::with_per_file_reorder_capacity`] after construction
    /// (the explicit builder method takes precedence over both the env
    /// var and the default).
    #[must_use]
    pub fn with_strategy(concurrency: usize, strategy: Arc<dyn ChecksumStrategy>) -> Self {
        let shard_count = shard_sizing::resolve_shard_count(concurrency);
        let per_file_reorder_capacity =
            ring_cap_env::resolve_ring_capacity(Self::DEFAULT_PER_FILE_REORDER_CAPACITY);
        Self {
            files: DashMap::with_shard_amount(shard_count),
            per_file_reorder_capacity,
            concurrency,
            strategy,
            reorder_saturations: AtomicU64::new(0),
            reorder_saturated_warned: AtomicBool::new(false),
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

    /// Returns the per-file reorder-ring capacity in effect for this applier.
    ///
    /// Reflects (in precedence order) the most recent
    /// [`Self::with_per_file_reorder_capacity`] override, the
    /// `OC_RSYNC_REORDER_RING_CAP` env var captured at first applier
    /// construction in this process, or
    /// [`Self::DEFAULT_PER_FILE_REORDER_CAPACITY`] (64) when neither is set.
    /// Exposed primarily for diagnostics and the ROB-11 regression test.
    #[must_use]
    pub fn per_file_reorder_capacity(&self) -> usize {
        self.per_file_reorder_capacity
    }

    /// Returns the cumulative number of per-file reorder-ring saturation
    /// events observed since the applier was constructed (ROB-2, #3667).
    ///
    /// Granularity-invariant: one increment per
    /// `IngestError::ReorderSaturated` regardless of which file
    /// produced it. Pairs with the ROB-3 one-shot warning emitted by
    /// `Self::note_reorder_saturation`.
    #[must_use]
    pub fn reorder_saturations(&self) -> u64 {
        self.reorder_saturations.load(Ordering::Relaxed)
    }

    /// Records a per-file reorder-ring saturation event and, on the first
    /// observation per applier instance, emits the ROB-3 one-shot warning.
    ///
    /// Increments [`Self::reorder_saturations`] unconditionally and uses an
    /// [`AtomicBool::compare_exchange`] guard so the warning fires exactly
    /// once even when several files saturate concurrently from rayon
    /// workers. The warning text includes the file index, the in-effect
    /// per-file ring capacity, the offending `chunk_sequence`, the
    /// registered file count at the time of saturation, and a pointer to
    /// the `OC_RSYNC_REORDER_RING_CAP` env knob (ROB-11) so operators
    /// have an actionable next step.
    ///
    /// The warning goes through `tracing::warn!` (visible at `--info=ALL`
    /// minimum) and is mirrored to stderr via [`eprintln!`] so default
    /// builds without the `tracing` feature still surface it. Mirrors the
    /// SRO-6 pattern used by [`SpillableReorderBuffer`].
    ///
    /// [`SpillableReorderBuffer`]: super::super::reorder::SpillableReorderBuffer
    pub(crate) fn note_reorder_saturation(&self, ndx: FileNdx, chunk_sequence: u64) {
        self.reorder_saturations.fetch_add(1, Ordering::Relaxed);
        if self
            .reorder_saturated_warned
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            let capacity = self.per_file_reorder_capacity;
            let file_count = self.files.len();
            eprintln!(
                "warning: per-file reorder ring saturated during transfer \
                 (ndx={ndx}, chunk_sequence={chunk_sequence}, ring_capacity={capacity}, \
                 registered_files={file_count}); this indicates either an adversarial \
                 chunk ordering or undersized ring capacity. \
                 Set OC_RSYNC_REORDER_RING_CAP to a larger positive integer to widen the \
                 per-file ring. (one-time warning per applier)"
            );
            #[cfg(feature = "tracing")]
            tracing::warn!(
                ndx = %ndx,
                chunk_sequence,
                ring_capacity = capacity,
                registered_files = file_count,
                "per-file reorder ring saturated during transfer; \
                 set OC_RSYNC_REORDER_RING_CAP to widen the per-file ring \
                 (one-time warning per applier)"
            );
        }
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
        // entry into the map. Single shard write lock; no contention on
        // unrelated NDX values.
        let entry = SlotEntry::new(FileSlot::new(writer, self.per_file_reorder_capacity));
        match self.files.entry(ndx) {
            Entry::Occupied(_) => Err(io::Error::other(format!(
                "parallel applier file {ndx} already registered"
            ))),
            Entry::Vacant(vacant) => {
                vacant.insert(entry);
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
        match slot.ingest(verified.chunk) {
            Ok(()) => Ok(()),
            Err(err) => {
                if let IngestError::ReorderSaturated { chunk_sequence, .. } = &err {
                    self.note_reorder_saturation(ndx, *chunk_sequence);
                }
                Err(err.into())
            }
        }
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

    pub(super) fn slot_for(&self, ndx: FileNdx) -> io::Result<SlotHandle> {
        // Clone the per-file [`SlotEntry`] (two `Arc::clone` calls) while
        // the shard read guard is alive, then drop the guard at the end
        // of this expression. Callers never see the DashMap guard, so
        // they cannot accidentally hold it across the per-file mutex lock
        // or a rayon dispatch. The bridge below builds a fresh
        // [`Arc<SlotBarrier>`] adapter from the entry's two inner Arcs.
        // After DG-3.c the [`DecrementGuard`] no longer rides on the
        // adapter: [`SlotHandle::new`] sources the bookkeeping
        // [`Arc<BarrierState>`] through [`SlotBarrier::barrier`] and
        // hands that clone to the guard, leaving the adapter Arc to
        // bound the lock path only. The follow-on DG-3.x task retypes
        // [`SlotHandle`] and deletes the adapter entirely. The adapter
        // Arc is unique to this call site; the underlying [`SlotData`]
        // and [`BarrierState`] Arcs are shared with every other adapter
        // minted from the same entry, so the in-flight counter and
        // Condvar remain coherent.
        let entry = self
            .files
            .get(&ndx)
            .map(|guard| guard.value().clone())
            .ok_or_else(|| io::Error::other(format!("parallel applier file {ndx} unknown")))?;
        let barrier = Arc::new(SlotBarrier::from_entry(&entry));
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
    pub(super) fn verify_chunk(
        strategy: &dyn ChecksumStrategy,
        chunk: DeltaChunk,
    ) -> Result<VerifiedChunk, ParallelApplyError> {
        // ABW-5.a invariant 1: verify_chunk reads only owned chunk.data
        // and immutable shared strategy - no Mutex, no &self, no side
        // effects on shared state. Being a static method it structurally
        // cannot access the per-file Mutex map. Assert the digest length
        // from the strategy matches the algorithm's documented size as a
        // witness that we consume the immutable strategy correctly.
        debug_assert!(
            strategy.digest_len() > 0,
            "ABW-5.a invariant 1: verify_chunk requires a valid ChecksumStrategy \
             with non-zero digest length; received digest_len=0"
        );
        let digest = strategy.compute(&chunk.data);
        debug_assert_eq!(
            digest.as_bytes().len(),
            strategy.digest_len(),
            "ABW-5.a invariant 1: computed digest length ({}) does not match \
             strategy.digest_len() ({}); verify_chunk must produce a digest \
             consistent with the immutable shared strategy",
            digest.as_bytes().len(),
            strategy.digest_len(),
        );
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
