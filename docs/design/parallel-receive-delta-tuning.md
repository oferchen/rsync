# Parallel-receive-delta worker-pool tuning spec

Tracking: oc-rsync task PIP-9.h.a (#3016). Parent: PIP-9.h (#2603).
Follow-ups: PIP-9.h.b implementation (#3017) and PIP-9.h.c bench + defaults
selection (#3018).

This document scopes the tuning surface for the parallel-receive-delta
path. It enumerates the constants that already steer worker-pool
behaviour, specifies four new tuning knobs the implementation in
PIP-9.h.b should expose, and lists the telemetry the PIP-9.h.c bench
needs to land defaults from evidence.

It does **not** pick values. Defaults are PIP-9.h.c's deliverable; this
spec defines the ranges PIP-9.h.c is expected to sweep.

## 1. Scope

The parallel-receive-delta path is opt-in behind the
`parallel-receive-delta` cargo feature. It comprises three layers that
each carry their own hard-coded constants today:

1. **Dispatch threshold** - `ThresholdDeltaPipeline` buffers per-file
   `DeltaWork` items until the file count clears a fixed bar, then
   promotes the buffer into a `ParallelDeltaPipeline`. Below the bar,
   processing stays sequential.
2. **Worker / queue sizing** - `ParallelDeltaPipeline` constructs a
   bounded `work_queue::bounded_with_capacity()` channel sized off
   `rayon::current_num_threads()` and an adaptive multiplier keyed on
   the average target-file size.
3. **Per-file apply** - `ParallelDeltaApplier` holds a per-file
   `FileSlot` keyed on `FileNdx`, sized by
   `DEFAULT_PER_FILE_REORDER_CAPACITY` and a `concurrency` value the
   caller hands in.

Wire-format ordering and per-file byte exclusivity invariants are out
of scope; both are already enforced by the per-file reorder buffer and
the per-slot `Mutex<FileSlot>` lock discipline documented at
`crates/engine/src/concurrent_delta/parallel_apply/mod.rs:11`. The
present spec changes only the *sizing* of the primitives, never their
ordering contract. Cross-link:
[[project_parallel_interop_parity_gap]] - the parity gap predates this
spec and stays unchanged regardless of the tuning values selected.

The spec also excludes the spill-to-tempfile policy
(`SpillPolicy::threshold_bytes` and friends in
`crates/engine/src/concurrent_delta/config.rs`). Spill is a separate
backpressure surface with its own opt-in CLI flags
(`OC_RSYNC_SPILL_THRESHOLD_BYTES`, `OC_RSYNC_SPILL_DIR`) and is
intentionally orthogonal to worker-pool sizing.

## 2. Inventory of current tunables

Every hard-coded constant that steers the parallel-receive-delta path
today, with file and line.

### 2.1 Dispatch threshold

| Name | Value | Site |
|---|---|---|
| `DEFAULT_PARALLEL_THRESHOLD` | `64` files | `crates/transfer/src/delta_pipeline/mod.rs:54` |

Consumed by `ThresholdDeltaPipeline::with_default_threshold` at
`crates/transfer/src/delta_pipeline/threshold.rs:71-75`. The threshold
gate fires inside `submit_work` at
`crates/transfer/src/delta_pipeline/threshold.rs:130-141`: when the
buffered item count reaches `self.threshold`, the buffer is moved into
a `ParallelDeltaPipeline`.

### 2.2 Worker count

`ParallelDeltaPipeline` does not own a thread pool; it dispatches into
rayon's global pool. The effective worker count is whatever
`rayon::current_num_threads()` returns, which itself reads
`RAYON_NUM_THREADS` or the rayon default (`num_cpus::get()`).

`ThresholdDeltaPipeline::promote_to_parallel` reads
`rayon::current_num_threads()` directly at
`crates/transfer/src/delta_pipeline/threshold.rs:99` and passes the
result into `ParallelDeltaPipeline::new_adaptive`.

### 2.3 Work-queue capacity

| Name | Value | Site |
|---|---|---|
| `CAPACITY_MULTIPLIER` | `2` (xN cores) | `crates/engine/src/concurrent_delta/work_queue/capacity.rs:8` |
| `SMALL_FILE_THRESHOLD` | `64 KiB` | `crates/engine/src/concurrent_delta/work_queue/capacity.rs:15` |
| `LARGE_FILE_THRESHOLD` | `1 MiB` | `crates/engine/src/concurrent_delta/work_queue/capacity.rs:21` |
| `SMALL_FILE_MULTIPLIER` | `8` | `crates/engine/src/concurrent_delta/work_queue/capacity.rs:24` |
| `MEDIUM_FILE_MULTIPLIER` | `4` | `crates/engine/src/concurrent_delta/work_queue/capacity.rs:30` |
| `LARGE_FILE_MULTIPLIER` | `2` | `crates/engine/src/concurrent_delta/work_queue/capacity.rs:27` |

`default_capacity()` at
`crates/engine/src/concurrent_delta/work_queue/capacity.rs:36-38`
returns `rayon::current_num_threads() * CAPACITY_MULTIPLIER`.

`adaptive_queue_depth(avg_file_size)` at
`crates/engine/src/concurrent_delta/work_queue/capacity.rs:66-76`
selects between the three multipliers using the two file-size
thresholds.

A near-duplicate adaptive computation lives in
`crates/transfer/src/delta_pipeline/parallel.rs:146-159`
(`adaptive_capacity`) with its own inline `SMALL_FILE_THRESHOLD = 64
KiB` and `LARGE_FILE_THRESHOLD = 1 MiB` and the same 2 / 4 / 8
multiplier ladder. Any change to the adaptive policy must move both
sites in lockstep.

### 2.4 Per-file reorder capacity

| Name | Value | Site |
|---|---|---|
| `DEFAULT_PER_FILE_REORDER_CAPACITY` | `64` chunks | `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:426` |

Used to size the per-file `ReorderBuffer<DeltaChunk>` when
`ParallelDeltaApplier::register_file` builds a fresh `FileSlot` at
`crates/engine/src/concurrent_delta/parallel_apply/mod.rs:505-520`.
Overridable per-applier via
`with_per_file_reorder_capacity(capacity)` at
`crates/engine/src/concurrent_delta/parallel_apply/mod.rs:477-481`.

### 2.5 Per-file concurrency limit

| Name | Value | Site |
|---|---|---|
| `concurrency` constructor parameter | caller-supplied | `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:438-460` |

`ParallelDeltaApplier::new(concurrency)` and
`with_strategy(concurrency, strategy)` accept a `usize`; `0` means
"use the ambient rayon pool" (documented at lines 430 and 451). The
value bounds the rayon `into_par_iter().with_min_len(...)` chunking
inside `apply_batch_parallel` at
`crates/engine/src/concurrent_delta/parallel_apply/batch.rs:50-61`:
`min_len = total.div_ceil(cap.max(1)).max(1)`, with `cap =
concurrency.min(total)`.

Cross-link: [[project_parallel_delta_apply_phase2]] - the
`apply_one_chunk` entry point (`mod.rs:552-570`) currently fires
`rayon::join(verify, || ())` whose second closure is a no-op. The
worker-pool tuning we spec here applies to `apply_batch_parallel`'s
fan-out path; `apply_one_chunk`'s single-chunk shape is unaffected
until the receiver pipeline wires a real fan-out caller. See the call-
site catalogue at `docs/audits/rjn-1-apply-chunk-parallel-call-sites-
2026-05-21.md` (referenced at `mod.rs:540-543`).

### 2.6 Drain-parallel shard count

`WorkQueueReceiver::drain_parallel` shards results across
`rayon::current_num_threads()` mutex-guarded buckets at
`crates/engine/src/concurrent_delta/work_queue/drain.rs:62-65`. The
shard count is not user-tunable; it tracks the rayon pool size
directly. Listed for completeness; not in scope for new knobs.

### 2.7 Slot-barrier and decrement-guard primitives

`SlotBarrier`, `SlotEntry`, `BarrierState`, and `DecrementGuard`
(`crates/engine/src/concurrent_delta/parallel_apply/slot_barrier.rs`
and `decrement_guard.rs`) do not expose tunable parameters. The
in-flight counter is bookkeeping, not policy. The
spin-then-yield workaround in
`crates/engine/src/concurrent_delta/parallel_apply/drain.rs:84-103`
uses a `1_000` iteration cap. That ceiling is a correctness fence
against a real bug (caller raced `slot_for` against `finish_file`),
not a performance knob, and the DG-4 task tracks its removal once
DG-3.c lands. Out of scope for tuning.

## 3. Per-knob spec (new)

The four knobs below are the ones PIP-9.h.b should add. Each entry
follows the same template: CLI flag, env var, what it controls,
current value source, range to sweep in PIP-9.h.c, interactions, and
cost model.

### 3.1 Worker count

- **Name:** `--parallel-receive-workers=N` / `OC_RSYNC_PARALLEL_RECEIVE_WORKERS`
- **What it controls:** Number of rayon workers the parallel-receive-
  delta path is allowed to dispatch onto. Implemented as a dedicated
  rayon `ThreadPool` if non-zero, falling back to the ambient pool
  when unset.
- **Current source:** Implicit - `rayon::current_num_threads()`
  consumed at `crates/transfer/src/delta_pipeline/threshold.rs:99`
  and `crates/engine/src/concurrent_delta/work_queue/capacity.rs:37`.
  No per-feature override exists today.
- **Default range to sweep:** `1, 2, 4, 8, min(num_cpus, 16)`. The
  current implicit value is whatever rayon defaults to (commonly the
  full core count). Capping at 8 by default is a working hypothesis
  to validate against the cores-vs-throughput curve from BR-3i.f.
- **Interactions:**
  - Sets the floor for `queue-depth` (see 3.3). Default queue depth
    is `workers * capacity-multiplier`; raising `workers` widens the
    queue automatically unless `queue-depth` is also pinned.
  - Sets the floor for `batch-size` (see 3.2): a batch smaller than
    `workers` leaves cores idle.
  - Honours `RAYON_NUM_THREADS` as the upper bound when the user has
    already pinned a smaller pool; the parallel-receive worker count
    saturates at the ambient pool size.
- **Cost model:** Each worker holds at most one in-flight
  `DeltaChunk` (per the per-file mutex). Steady-state RSS scales
  linearly with `workers * average-chunk-size`. Syscall cost is
  amortised over the per-worker batch (see 3.2). Latency tail grows
  with `workers` because the per-file reorder buffer must wait for
  the slowest worker in the contiguous run.

### 3.2 Batch size

- **Name:** `--parallel-receive-batch-size=N` / `OC_RSYNC_PARALLEL_RECEIVE_BATCH_SIZE`
- **What it controls:** Maximum number of `DeltaChunk` items dispatched
  in a single `apply_batch_parallel` call. The applier's
  `into_par_iter().with_min_len(min_len)` at
  `crates/engine/src/concurrent_delta/parallel_apply/batch.rs:55-60`
  already derives a per-worker chunk size; this knob caps the outer
  batch the caller assembles before invoking the applier.
- **Current source:** Implicit. The receiver pipeline today calls
  `apply_one_chunk` per token, so the effective batch is 1. This is
  the optimisation target documented in
  [[project_parallel_delta_apply_phase2]].
- **Default range to sweep:** `1, 8, 32, 128, 512` chunks. The
  bench should run each value at three file-size profiles (small
  config files, mid-MB documents, multi-GB media) so PIP-9.h.c can
  see when batching dominates dispatch cost.
- **Interactions:**
  - Must satisfy `batch-size <= queue-depth`; the queue is the
    upstream backpressure surface and a batch larger than the queue
    would let the producer accumulate work unboundedly while waiting
    for the consumer.
  - Should be `>= workers` to keep all workers busy on a single
    dispatch; values below `workers` are bench-only baselines for
    measuring dispatch overhead.
  - Independent of `per-file-reorder-capacity` (see 3.5 below) but
    constrained by it: a batch may contain at most
    `per-file-reorder-capacity` chunks for any one file before the
    reorder buffer overflows.
- **Cost model:** Memory grows linearly with `batch-size *
  average-chunk-size`. Syscall amortisation improves until `batch-
  size` exceeds the rayon dispatch overhead curve, after which
  latency tail dominates and per-worker quanta become idle.

### 3.3 Queue depth

- **Name:** `--parallel-receive-queue-depth=N` / `OC_RSYNC_PARALLEL_RECEIVE_QUEUE_DEPTH`
- **What it controls:** Bounded slot count of the SPMC
  `crossbeam_channel` between the wire-reading producer and the
  rayon worker pool. Overrides the `workers * multiplier` default
  computed by `default_capacity()` and `adaptive_queue_depth()`.
- **Current source:** `default_capacity()` at
  `crates/engine/src/concurrent_delta/work_queue/capacity.rs:37`
  (`workers * 2`) and the adaptive `2x` / `4x` / `8x` ladder at
  lines 24-30 keyed on the file-size thresholds at lines 15 and 21.
- **Default range to sweep:** Express as a multiplier of
  `workers`, then sweep `1, 2, 4, 8, 16`. PIP-9.h.c picks the
  multiplier that minimises producer-block events on the
  `parallel_dispatch_overhead` and `parallel_receive_delta_perf`
  benches.
- **Interactions:**
  - Must satisfy `queue-depth >= batch-size` (see 3.2).
  - When `queue-depth` is hard-pinned, the adaptive average-file-
    size override (lines 66-76 in `capacity.rs`) is disabled - the
    user has already chosen the policy.
  - Interacts with the spill threshold
    (`SpillPolicy::threshold_bytes`): the spill layer triggers on
    reorder buffer pressure, which itself depends on queue depth.
    Out of scope here but called out so PIP-9.h.c can rule out
    accidental coupling.
- **Cost model:** Steady-state queue memory is `queue-depth *
  sizeof(DeltaWork)`, where `DeltaWork` is a `Ndx` + `PathBuf` +
  `u64` + optional sequence number (single-digit cache lines per
  slot). Backpressure events scale inversely with depth; tail
  latency under bursty wire input scales inversely too.

### 3.4 Threshold bytes

- **Name:** `--parallel-receive-threshold-bytes=N` / `OC_RSYNC_PARALLEL_RECEIVE_THRESHOLD_BYTES`
- **What it controls:** Minimum aggregate transfer size below which
  the receiver stays on the sequential path even if the file count
  exceeds `DEFAULT_PARALLEL_THRESHOLD`. Complements the existing
  file-count threshold at
  `crates/transfer/src/delta_pipeline/mod.rs:54` with a byte-volume
  gate so transfers of many tiny files do not pay the parallel
  dispatch tax when there is no real CPU work to fan out.
- **Current source:** Not implemented. The file-count threshold is
  the only gate today, evaluated at
  `crates/transfer/src/delta_pipeline/threshold.rs:130-141`.
- **Default range to sweep:** `0` (disabled, current behaviour),
  `1 MiB`, `16 MiB`, `128 MiB`, `1 GiB`. PIP-9.h.c picks the
  smallest value at which parallel mode pays for itself; setting
  too high would deny the optimisation to typical workloads.
- **Interactions:**
  - Logical AND with the file-count threshold: parallel mode
    activates only when **both** the file count meets
    `DEFAULT_PARALLEL_THRESHOLD` (or its successor knob) **and**
    the aggregate byte volume meets `threshold-bytes`.
  - The byte-volume estimate comes from the buffered `DeltaWork`
    items inside `ThresholdMode::Buffering`; the same accumulator
    that produces `average_target_size` (line 117-127 of
    `threshold.rs`) can produce the sum without an extra pass.
  - Pairs with the file-size adaptive multiplier in 3.3: a small-
    file transfer that does cross the byte threshold deserves the
    8x queue depth, not the 2x.
- **Cost model:** Pure additive guard - one `u64` accumulator on the
  hot `submit_work` path, no extra allocations. The cost-model win
  is on the negative side: avoids the rayon thread-spawn,
  consumer-thread spawn, and bounded-channel allocation when
  parallel mode would be wasted.

### 3.5 Optional companion knob (defer until evidence)

- **Name:** `--parallel-receive-per-file-reorder-capacity=N` /
  `OC_RSYNC_PARALLEL_RECEIVE_PER_FILE_REORDER_CAPACITY`
- **What it controls:** The `DEFAULT_PER_FILE_REORDER_CAPACITY`
  default (64 chunks) on `ParallelDeltaApplier`, surfaced as a CLI
  flag so callers can size the per-file ring up for very-large-file
  transfers or down to bound memory under many-file fan-out.
- **Current source:**
  `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:426`.
- **Why deferred:** PIP-9.h.c should bench whether the four knobs
  above are sufficient. If the reorder buffer is the dominant
  back-pressure surface on small-chunk fan-out, PIP-9.h.b adds this
  fifth knob. Otherwise it stays an internal default. Listed here so
  it is not re-derived later.

## 4. Telemetry hooks required by PIP-9.h.c

PIP-9.h.c cannot pick defaults without per-knob signal. The
implementation in PIP-9.h.b must surface the following counters and
histograms via the existing `logging_sink` feature gate (or an
equivalent bench-only collector behind `#[cfg(feature =
"parallel-receive-delta")]`). All metrics are per-transfer
aggregates unless noted.

1. **Per-worker dispatch latency** - histogram of `Instant::now() -
   submit_time` measured inside the rayon worker just before it
   begins `verify_chunk`. Reveals whether `queue-depth` is starving
   workers or whether dispatch overhead dominates per-chunk CPU.
2. **Drain wait** - histogram of `flush_workers` wait duration per
   file. Sourced from `BarrierState::wait_until_idle` at the entry
   in `crates/engine/src/concurrent_delta/parallel_apply/drain.rs:146-158`.
   Exposes whether the per-file mutex is the bottleneck or whether
   workers retire promptly.
3. **Queue-full backpressure events** - counter incremented when
   `WorkQueueSender::send` blocks. The `crossbeam_channel` `Sender`
   does not expose a hook directly; PIP-9.h.b wraps the send in a
   small helper that times the call and increments the counter when
   the wait exceeds a configurable floor (default 1 us).
4. **Batch sizing observed** - distribution of `chunks.len()` at
   each `apply_batch_parallel` call. Verifies the new
   `batch-size` knob is being honoured and that the producer's
   batching loop hits the target.
5. **Sequential fallback rate** - counter for how often
   `ThresholdDeltaPipeline::flush` exits via
   `ThresholdMode::Buffering` (below file-count threshold) versus
   `ThresholdMode::Parallel`. Demonstrates the byte-threshold knob
   (3.4) keeps small transfers on the sequential path.
6. **Per-file reorder buffer high-watermark** - max `buffered_count()`
   across the transfer. Tells PIP-9.h.c whether the deferred fifth
   knob (3.5) is worth promoting.

Each metric must be enabled and disabled by a single feature gate or
config bit so production builds pay zero cost when telemetry is off.
The bench harness in
`crates/engine/benches/parallel_receive_delta_perf.rs` and the
end-to-end driver in
`crates/core/benches/pip_6_end_to_end_parallel_vs_sequential.rs` are
the consumers; both already gate on `parallel-receive-delta`.

## 5. Rollback criterion

If PIP-9.h.c shows that any single knob dominates the response curve
(for example, `workers` alone explains > 90% of the throughput
variance across the swept ranges), then PIP-9.h.b should narrow the
implementation surface to just the dominant knob plus the byte
threshold (3.4), and drop the others. Cross-link:
[[project_apply_batch_write_serial]] - if the bench evidence shows
the write step is the bottleneck rather than verify dispatch, the
worker-count knob (3.1) becomes the only meaningful lever and 3.2 /
3.3 should not be promoted.

Specifically:
- If `batch-size` and `queue-depth` co-vary completely with
  `workers`, drop both and let them remain `workers * multiplier`
  derivations.
- If `threshold-bytes` never fires in production-shaped benches,
  fold it back into the file-count threshold by extending
  `DEFAULT_PARALLEL_THRESHOLD` rather than adding a new dimension.
- If telemetry shows the per-file reorder buffer never reaches its
  cap, do not promote the deferred 3.5 knob.

The bias is toward fewer knobs. Every flag we ship is a flag we must
test against the interop matrix forever, and surface area we cannot
walk back without a deprecation.

## 6. Interaction matrix (compact reference)

A small constraint table the implementation can encode in
config-build-time validation, mirroring the existing mutual-exclusion
checks called out in `feedback_no_upstream_patterns` /
`feedback_validate_state_first` style.

| Knob | Must be >= | Must be <= | Disables |
|---|---|---|---|
| `workers` | 1 | `RAYON_NUM_THREADS` (when set) | none |
| `batch-size` | `workers` | `queue-depth` | none |
| `queue-depth` | `batch-size` | 65_536 (sanity ceiling) | adaptive multiplier when explicit |
| `threshold-bytes` | 0 | u64::MAX | none |
| `per-file-reorder-capacity` (deferred) | 1 | 65_536 (sanity ceiling) | none |

The sanity ceilings exist to keep configuration errors from
allocating absurd channels; they are not the policy ranges to bench.

## 7. Out of scope

- Wire-protocol changes. The parallel-receive-delta path is
  wire-compatible with the sequential path by design; nothing in this
  spec changes that.
- Sender-side tuning. The sender owns its own thread budget through
  the work-queue producer; that path is governed by a separate spec
  not tracked under PIP-9.h.
- Spill-to-tempfile policy
  (`SpillPolicy::{threshold_bytes,dir,reclaim_mode,granularity,
  compression}`). Already covered by the existing
  `OC_RSYNC_SPILL_*` flags.
- Slot-barrier internals (`SlotBarrier`, `BarrierState`,
  `DecrementGuard`). Covered by the DG-3 / DG-4 / FFB-2 work; the
  tuning spec consumes those primitives and does not propose
  changes to them.

## 8. Acceptance signals for PIP-9.h.b

PIP-9.h.b ships when:

1. Each knob in section 3 has a CLI flag, an environment variable,
   and a config-field with validation matching the matrix in
   section 6.
2. The telemetry hooks in section 4 are present and disabled by
   default in production builds.
3. Existing default behaviour is preserved when no knob is set:
   `workers = rayon::current_num_threads()`, `batch-size = 1`
   (matching the current `apply_one_chunk` shape until the receiver
   pipeline wires a real fan-out caller), `queue-depth = workers *
   capacity-multiplier`, `threshold-bytes = 0`.
4. The bench harness in PIP-9.h.c can sweep every knob through the
   ranges in section 3 from a single invocation.
