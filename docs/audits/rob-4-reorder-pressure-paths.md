# ROB-4: Reorder Buffer Pressure Path Inventory

Parent task: ROB-1 (spill-prevention initiative for the parallel-delta
`SpillableReorderBuffer`). Sibling docs:

- `docs/audits/rob-spill-prevention.md` (telemetry surface added by ROB-2/3,
  PR #5626).
- `docs/design/reorderbuffer-spill-to-tempfile.md` (spill backend contract).
- `docs/design/reorderbuffer-metrics-and-bypass.md` (metrics surface and
  bypass mode).

ROB-2 shipped the granularity-invariant `spill_activations` counter and ROB-3
wired the one-shot `tracing::warn!` so operators see the first spill event
per buffer lifetime. ROB-4 inventories the production code paths that feed
items into a reorder buffer so ROB-7 (adaptive ring sizing) knows which
producers to optimise for under normal-operation workloads. The audit is
descriptive only - no code changes ship with this PR.

## Phase 1: Producer inventory

The codebase exposes two distinct reorder buffer flavours. Only the
parallel-delta `ReorderBuffer<T>` / `SpillableReorderBuffer<T>` pair is in
scope for ROB-7's adaptive sizing; the delete `ReorderBuffer` is included
for completeness so future audits do not conflate the two surfaces.

### Parallel-delta producers (in scope for ROB-7)

Defined in `crates/engine/src/concurrent_delta/reorder/mod.rs` and wrapped
by `crates/engine/src/concurrent_delta/spill/buffer/mod.rs`. Public surface:
`ReorderBuffer::{insert, force_insert}` and `SpillableReorderBuffer::{insert,
force_insert}`. Direct callers in production source (tests excluded):

1. **`DeltaConsumer` reorder loop, bare backend** -
   `crates/engine/src/concurrent_delta/consumer/loops.rs::run_bare_loop`
   - line 37: `reorder.insert(result.sequence(), result.clone())`
   - line 43: `reorder.force_insert(result.sequence(), result.clone())`
     (deadlock breaker when the ring is full and `next_expected` is missing)

2. **`DeltaConsumer` reorder loop, spillable backend** -
   `crates/engine/src/concurrent_delta/consumer/loops.rs::run_spillable_loop`
   - line 93: `reorder.insert(result.sequence(), result.clone())`
   - line 119: `reorder.force_insert(result.sequence(), result.clone())`
     (capacity-exceeded path; spill layer displaces high-sequence items)

3. **`ParallelDeltaApplier` per-file `FileSlot::ingest`** -
   `crates/engine/src/concurrent_delta/parallel_apply/mod.rs::FileSlot::ingest`
   - line 254: `self.reorder.insert(seq, chunk)`
   (driven by `apply_batch_parallel` in `parallel_apply/batch.rs` after the
   rayon verify barrier resolves; the write-side loop runs serially under
   the per-file Mutex)

Both `(1)` and `(2)` share a single `DeltaResult` ring per `DeltaConsumer`
lifetime; `(3)` allocates one `ReorderBuffer<DeltaChunk>` per registered
file via `FileSlot::new`.

### Delete producers (out of scope for ROB-7)

`crates/engine/src/delete/reorder_buffer.rs::ReorderBuffer` is a separate
type with a different invariant set: cohort keys mapped to ranks via
`BTreeMap`, bounded by `MAX_BUFFERED_COHORTS = 64`, drained by the serial
`DeleteEmitter`. It does **not** spill to disk - the cap surfaces as a
`ReorderBufferError::BufferFull` for producers to back off on. Adaptive
sizing is unnecessary here because the consumer is single-threaded and the
producers already block via `Condvar` in `delete::cohort_batcher`
(DEL-1.b section 4.3 protocol).

ROB-7 does not need to consider this path. The DEL-3.c stress test
(`crates/engine/src/delete/parallel_consumer.rs`) confirms backpressure
holds even at adversarial cohort fan-out.

## Phase 2: Per-producer classification

Throughout this section "ring cap" refers to the parameter passed to
`ReorderBuffer::new(capacity)` and "byte threshold" refers to the
`SpillableReorderBuffer::new(capacity, threshold)` second argument that
drives `should_force_spill_for_rss() || memory_used > threshold` inside
`spill/buffer/insert.rs::insert`.

### Producer 1: `DeltaConsumer` reorder loop, bare backend

| Dimension | Value |
|-----------|-------|
| Source | `consumer/loops.rs::run_bare_loop` (line 37 / 43) |
| Item type | `DeltaResult` (per-file completion, not per-chunk) |
| Producer fan-out | One per active rayon worker in the delta-drain thread |
| Ring cap source | `ConcurrentDeltaConfig::reorder_capacity`, wired from `ParallelDeltaPipeline::with_capacity` (`delta_pipeline/parallel.rs::with_capacity`) |
| Default ring cap | `2 * worker_count` (large files) up to `8 * worker_count` (small files) via `adaptive_capacity` |
| Workload class | medium-to-large file counts; one slot per file completion |
| Insertion rate vs cap | one insert per `DeltaResult::with_sequence`; rate matches worker completion rate |
| Cohort spread | bounded by `worker_count` because at most that many items can be in flight ahead of `next_expected` |
| Adversarial sensitivity | low - the cap already tracks `worker_count`; `force_insert` only fires when one slow worker pins `next_expected` |
| Spill backend | bare ring with `force_insert` fallback; no disk path |

Bare backend has no spill semantics by definition - it doubles via
`ReorderBuffer::grow` on `force_insert` (line 614 in `reorder/mod.rs`).
This path is the historical default and is unaffected by ROB-4/7. It is
listed for completeness because operators may run with
`spill_policy.threshold_bytes == None`, in which case `force_insert` is the
only relief valve.

### Producer 2: `DeltaConsumer` reorder loop, spillable backend

| Dimension | Value |
|-----------|-------|
| Source | `consumer/loops.rs::run_spillable_loop` (line 93 / 119) |
| Item type | `DeltaResult` |
| Producer fan-out | Same as Producer 1 (single drain thread feeds the stream channel) |
| Ring cap source | `ConcurrentDeltaConfig::reorder_capacity` |
| Default ring cap | `2 * worker_count` to `8 * worker_count` (same `adaptive_capacity` heuristic) |
| Byte threshold | `SpillPolicy::threshold_bytes` (opt-in; absent means use bare backend) |
| Workload class | medium-to-large; only engaged when `threshold_bytes` is set |
| Insertion rate vs cap | one insert per worker completion; sustained rate equals worker throughput |
| Cohort spread | same as Producer 1; bounded by `worker_count` |
| Adversarial sensitivity | medium - if a single slow file stalls `next_expected`, the ring fills with high-sequence completions, then the byte threshold pushes them to disk |
| Spill backend | `SpillableReorderBuffer` with `SpillGranularity::WholeBatch` default |

This is the ROB-7-relevant DeltaResult path. The byte threshold is the
forcing function; ring cap is secondary. ROB-7 should focus on adjusting
the ring cap downward when a transfer fits entirely in memory and upward
when adversarial cohort spread emerges, deferring to the byte threshold for
hard memory pressure.

### Producer 3: `ParallelDeltaApplier::FileSlot::ingest`

| Dimension | Value |
|-----------|-------|
| Source | `parallel_apply/mod.rs::FileSlot::ingest` (line 254) |
| Item type | `DeltaChunk` (per-chunk-of-one-file, not per-file) |
| Producer fan-out | One reorder buffer per registered file; rayon dispatches verify in parallel and the per-file `Mutex<FileSlot>` serialises writes |
| Ring cap source | `ParallelDeltaApplier::per_file_reorder_capacity` |
| Default ring cap | `DEFAULT_PER_FILE_REORDER_CAPACITY = 64` (`parallel_apply/mod.rs` line 422) |
| Workload class | per-file; activated when delta apply runs in parallel mode |
| Insertion rate vs cap | rate equals chunk completion rate per file; depends on chunk size and rayon pool occupancy |
| Cohort spread | bounded by `concurrency` (typically `num_cpus`) - that is the max in-flight chunks per file |
| Adversarial sensitivity | medium-to-high at >32 workers + large files with many small chunks |
| Spill backend | bare `ReorderBuffer`; this path does **not** wrap `SpillableReorderBuffer` |

This producer is on the receive-side parallel apply path. Capacity 64 is a
hard default; only `with_per_file_reorder_capacity` (builder, not invoked in
production today) lets callers override it. There is no spill fallback - if
the ring fills, `ingest` returns `io::Error::other("parallel apply reorder
full: ...")` (line 255) and the receiver aborts the file.

ROB-7 should consider whether this path needs an adaptive cap given the
no-spill failure mode. See [[project_reorder_capacity_hard_default]] for
the matching memory note.

## Phase 3: Pressure matrix

| Producer | File:line | Hot under | Pressure mechanism | Spill likely (normal)? | Notes |
|---|---|---|---|---|---|
| `run_bare_loop` (DeltaResult) | `consumer/loops.rs:37` | delta pipeline default | ring fills if one worker stalls; `force_insert` doubles the ring | n/a (no spill) | adaptive cap already scales with `worker_count`; doubling absorbs tail latency |
| `run_spillable_loop` (DeltaResult) | `consumer/loops.rs:93` | delta pipeline with `SpillPolicy::threshold_bytes` set | byte threshold forces spill; ring cap drives `Capacity` errors that the loop converts to `force_insert` | no at typical `worker_count` and bounded cohort spread | one slow file plus high `worker_count` is the trigger; ROB-7 target |
| `FileSlot::ingest` (DeltaChunk) | `parallel_apply/mod.rs:254` | parallel-receive-delta apply path | per-file ring fills when verify dispatch outruns the serial write loop | no at default `concurrency`; possible at >32 workers and small chunks | no spill fallback; `ingest` errors out; PIP-10.b adversarial chunk-ordering stress already exercises this path |

## Phase 4: Normal vs adversarial classification

Per ROB-1 intent: adversarial-ordering reorder pressure is expected;
ROB exists to prevent *normal* operation from spilling. For each producer:

### Producer 1 (bare DeltaResult)

- **Normal**: workers complete roughly in submission order; ring stays
  shallow; `force_insert` rarely fires. The adaptive cap from
  `adaptive_capacity` is sufficient at any realistic `worker_count`.
- **Adversarial**: one large file pinned at `next_expected` while
  `worker_count`-many small files complete behind it. `force_insert`
  doubles the ring; the second slow file doubles again. No spill path -
  unbounded growth is the failure mode.

### Producer 2 (spillable DeltaResult)

- **Normal**: same arrival pattern as Producer 1. Byte threshold should
  not be crossed because at most `worker_count` `DeltaResult` items sit
  in the ring and each `DeltaResult` is small (`SpillCodec::estimated_size`
  is dominated by the path string and the stats tuple). A 64 MiB threshold
  is well above the steady-state footprint.
- **Adversarial**: one stalled file plus a large per-result payload (e.g.,
  long file paths in deeply nested directories, large error strings on
  failed results). Sustained pressure crosses the byte threshold; spill
  fires and `spill_activations` increments. ROB-7 should consider adapting
  the ring cap upward when sustained spill activations are observed, so
  the byte threshold stays the binding constraint instead of the ring cap
  bouncing inserts into `force_insert`.

### Producer 3 (per-file DeltaChunk)

- **Normal**: each file has at most `concurrency` chunks in flight; the
  default cap of 64 covers typical `num_cpus` values. No spill backend, so
  "normal" means the ring never fills.
- **Adversarial**: a single huge file with many small chunks plus a high
  `concurrency` setting. PIP-10.b adversarial chunk-ordering stress
  (`docs/audits/abw-1-apply-batch-verify-write-overlap-2026-05-21.md`)
  already covers the rayon verify path; the reorder ring is part of the
  same surface but is not exercised at >64 in-flight chunks per file by
  the existing stress test.

## Phase 5: ROB-7 priorities

Producers ROB-7 should adapt for, ranked by normal-operation spill risk:

1. **(top priority) `run_spillable_loop` DeltaResult ring** -
   `crates/engine/src/concurrent_delta/consumer/loops.rs:93`
   - Spills under sustained pressure when one slow file stalls
     `next_expected` and the byte threshold is crossed by the resulting
     pile-up. The ring cap already scales with `worker_count` via
     `adaptive_capacity`, but the cap is fixed for the consumer lifetime.
     Adaptive sizing would let the ring grow when `force_insert` fires
     repeatedly and shrink when steady-state is restored, keeping the byte
     threshold (not the ring cap) as the binding spill trigger.
   - Telemetry hookup: `spill_activations` (ROB-2) and
     `force_insert_count` already exposed via `ReorderMetrics`.

2. **(medium priority) `FileSlot::ingest` per-file DeltaChunk ring** -
   `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:254`
   - Hard cap of 64 (`DEFAULT_PER_FILE_REORDER_CAPACITY`) errors out
     rather than spilling. Adversarial workloads at high `concurrency`
     are the failure case. Adaptive sizing here would replace the hard
     error with graceful growth, but the path lacks a spill backend so
     unbounded growth is undesirable too. ROB-7 should evaluate whether
     this ring needs adaptive sizing at all or whether the upstream fix
     is to wrap it in `SpillableReorderBuffer` first.

3. **(low priority) `run_bare_loop` DeltaResult ring** -
   `crates/engine/src/concurrent_delta/consumer/loops.rs:37`
   - No spill backend by definition. `force_insert` doubles the ring on
     overflow; existing behaviour is acceptable. Adaptive shrink could
     reclaim memory after a transient stall but is not on the
     critical path.

## Phase 6: Cross-references

- Memory note [[project_reorder_capacity_hard_default]] - flags the bare
  in-memory ring's "errors instead of backpressuring" behaviour; this
  audit corroborates that for Producer 3 specifically.
- `docs/audits/rob-spill-prevention.md` (PR #5626) - telemetry surface
  consumed by this audit's prioritisation.
- `docs/audits/abw-1-apply-batch-verify-write-overlap-2026-05-21.md` -
  PIP-10.b adversarial chunk-ordering stress covering Producer 3's
  verify-write overlap contract.
- `docs/audits/dg-3d-finish-file-uncontended.md` - DG-3 stress test
  learnings for `finish_file` on the parallel applier; tangentially
  relevant because the same `FileSlot` is exercised.
- `docs/design/del-1b-reordering-buffer.md` and
  `crates/engine/src/delete/parallel_consumer.rs` DEL-3.c stress test -
  confirms the delete reorder buffer is out of scope for ROB-7.
- `docs/design/reorderbuffer-metrics-and-bypass.md` - prior-art metrics
  documentation; the `Metrics`/`spill_stats` surfaces it describes are the
  observability lever ROB-7 will read.
- `docs/design/reorderbuffer-spill-to-tempfile.md` - spill backend
  contract that ROB-7's adaptive policy must continue to honour.
