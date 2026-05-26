# PIP-9.h.b: Worker-pool tuning knobs implementation

Tracking: PIP-9.h.b (#3017). Parent: PIP-9.h (#2603).
Predecessor: PIP-9.h.a tuning spec (`docs/design/parallel-receive-delta-tuning.md`).
Successor: PIP-9.h.c bench + defaults selection (#3018).

This document specifies how to implement the four tuning knobs defined in
PIP-9.h.a. It covers config struct changes, env-var wiring, auto-tuning
logic, memory budget enforcement, rayon pool strategy, and the testing
plan. It does not pick default values - that is PIP-9.h.c's deliverable.

## 1. Knob inventory

Each knob below is specified in PIP-9.h.a section 3. The table
summarises the contract PIP-9.h.b must implement.

| # | Name | Type | Default | Valid range | Env var | CLI flag |
|---|------|------|---------|-------------|---------|----------|
| 1 | Worker count | `usize` | `rayon::current_num_threads()` | 1..=ambient pool size | `OC_RSYNC_PARALLEL_RECEIVE_WORKERS` | `--parallel-receive-workers` |
| 2 | Batch size | `usize` | 1 (current `apply_one_chunk` shape) | 1..=queue depth | `OC_RSYNC_PARALLEL_RECEIVE_BATCH_SIZE` | `--parallel-receive-batch-size` |
| 3 | Queue depth | `usize` | `workers * capacity_multiplier` | batch size..=65536 | `OC_RSYNC_PARALLEL_RECEIVE_QUEUE_DEPTH` | `--parallel-receive-queue-depth` |
| 4 | Threshold bytes | `u64` | 0 (disabled) | 0..=u64::MAX | `OC_RSYNC_PARALLEL_RECEIVE_THRESHOLD_BYTES` | `--parallel-receive-threshold-bytes` |

The deferred fifth knob (per-file reorder capacity, PIP-9.h.a section
3.5) is excluded until PIP-9.h.c bench evidence shows the reorder buffer
is the dominant backpressure surface. `ParallelDeltaApplier` already
exposes `with_per_file_reorder_capacity()` for programmatic override.

## 2. Config struct changes

### 2.1 New struct: `WorkerPoolConfig`

Add `crates/engine/src/concurrent_delta/worker_pool_config.rs`. The
struct lives in the engine crate alongside `ConcurrentDeltaConfig` so
both the transfer crate and the bench harness can consume it without
a dependency inversion.

```rust
/// Tuning knobs for the parallel-receive-delta worker pool.
///
/// Constructed by the CLI layer or by bench harnesses. The default value
/// preserves the historical behaviour: all fields are `None`, meaning
/// the pipeline auto-selects from the ambient rayon pool and the
/// adaptive capacity heuristic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkerPoolConfig {
    /// Explicit worker count. `None` = use `rayon::current_num_threads()`.
    pub workers: Option<usize>,
    /// Maximum chunks per `apply_batch_parallel` call. `None` = 1
    /// (current `apply_one_chunk` shape).
    pub batch_size: Option<usize>,
    /// Explicit bounded queue depth. `None` = `workers * multiplier`
    /// from the adaptive heuristic.
    pub queue_depth: Option<usize>,
    /// Minimum aggregate transfer bytes before parallel mode activates.
    /// `None` or `Some(0)` = disabled (file-count threshold only).
    pub threshold_bytes: Option<u64>,
}
```

### 2.2 Validation

`WorkerPoolConfig::validate()` enforces the constraint matrix from
PIP-9.h.a section 6. Called at config-build time - before the pipeline
is constructed - so constraint violations surface as user-facing errors
rather than runtime panics.

```rust
impl WorkerPoolConfig {
    /// Validates the constraint matrix and returns an error message on
    /// violation. Called at config-build time.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(w) = self.workers {
            if w == 0 {
                return Err("--parallel-receive-workers must be >= 1".into());
            }
        }
        if let Some(bs) = self.batch_size {
            if bs == 0 {
                return Err("--parallel-receive-batch-size must be >= 1".into());
            }
            if let Some(qd) = self.queue_depth {
                if bs > qd {
                    return Err(format!(
                        "--parallel-receive-batch-size ({bs}) must be \
                         <= --parallel-receive-queue-depth ({qd})"
                    ));
                }
            }
        }
        if let Some(qd) = self.queue_depth {
            if qd == 0 {
                return Err("--parallel-receive-queue-depth must be >= 1".into());
            }
            if qd > 65_536 {
                return Err(format!(
                    "--parallel-receive-queue-depth ({qd}) exceeds \
                     sanity ceiling 65536"
                ));
            }
        }
        Ok(())
    }
}
```

### 2.3 Wiring into `ConcurrentDeltaConfig`

Add a `worker_pool` field to `ConcurrentDeltaConfig`:

```rust
pub struct ConcurrentDeltaConfig {
    pub spill_policy: SpillPolicy,
    /// Worker-pool tuning knobs for the parallel-receive-delta path.
    /// Default leaves all fields `None` (auto-select).
    pub worker_pool: WorkerPoolConfig,
}
```

The `Default` impl stays compatible: both `spill_policy` and
`worker_pool` default to their respective `Default` values, preserving
all existing call sites.

## 3. Env-var wiring

### 3.1 New module: `worker_pool_env.rs`

Add `crates/engine/src/concurrent_delta/worker_pool_env.rs` following
the `spill/env.rs` pattern exactly. Four const env-var names, one
`apply_env_overrides(config: &mut WorkerPoolConfig)` function.

Each variable is parsed independently. Absent variables leave the
corresponding field unchanged. Invalid values emit `tracing::warn!` and
leave the field at its prior value - never a panic. The `tracing` gate
mirrors `spill/env.rs`.

```rust
pub const ENV_WORKERS: &str = "OC_RSYNC_PARALLEL_RECEIVE_WORKERS";
pub const ENV_BATCH_SIZE: &str = "OC_RSYNC_PARALLEL_RECEIVE_BATCH_SIZE";
pub const ENV_QUEUE_DEPTH: &str = "OC_RSYNC_PARALLEL_RECEIVE_QUEUE_DEPTH";
pub const ENV_THRESHOLD_BYTES: &str =
    "OC_RSYNC_PARALLEL_RECEIVE_THRESHOLD_BYTES";
```

### 3.2 Application order

Env overrides are applied inside
`ThresholdDeltaPipeline::promote_to_parallel` before the pool and queue
are constructed - the same position where `DeltaConsumer::spawn`
applies `OC_RSYNC_SPILL_*` overrides today. The precedence chain is:

1. CLI flag (highest)
2. Env var
3. Programmatic `WorkerPoolConfig` field
4. Auto-select (lowest)

The CLI parser sets the `WorkerPoolConfig` fields from flag values. Then
`apply_env_overrides` fills any remaining `None` slots from the
environment. The pipeline reads resolved values and auto-selects for any
slot still `None`.

## 4. Runtime auto-tuning logic

### 4.1 Worker count resolution

```
resolved_workers = config.workers
    .unwrap_or_else(|| rayon::current_num_threads())
    .min(rayon::current_num_threads())
```

When `workers` is explicit, it is clamped to the ambient rayon pool
size. The parallel-receive path dispatches into rayon's global pool -
there is no benefit to requesting more workers than rayon owns.

Rationale for not spawning a dedicated `rayon::ThreadPool`: the
parallel-receive-delta path coexists with the sender-side signature
generation path, which also uses the global rayon pool. A separate pool
would double thread count and compete for CPU cache residency. If bench
evidence from PIP-9.h.c shows the global pool is the bottleneck (e.g.
sender and receiver contend), the pool-isolation question reopens as a
follow-on task.

### 4.2 Queue depth resolution

```
resolved_queue_depth = config.queue_depth
    .unwrap_or_else(|| adaptive_queue_depth(resolved_workers, avg_file_size))
```

When `queue_depth` is explicit, the adaptive file-size heuristic is
bypassed entirely - the user has chosen the policy. When `queue_depth`
is `None`, the existing `adaptive_queue_depth` function in
`crates/engine/src/concurrent_delta/work_queue/capacity.rs` drives the
decision. The only change is that `adaptive_queue_depth` now accepts
`resolved_workers` instead of calling `rayon::current_num_threads()`
internally, so the worker-count knob flows through.

**Deduplicate adaptive capacity.** The near-duplicate
`adaptive_capacity` function in
`crates/transfer/src/delta_pipeline/parallel.rs:146-159` must be
replaced with a call to the engine crate's `adaptive_queue_depth`. Both
sites must move in lockstep (PIP-9.h.a section 2.3). PIP-9.h.b
eliminates the duplication by parameterising the engine function on
worker count and removing the transfer-crate copy.

### 4.3 Batch size resolution

```
resolved_batch_size = config.batch_size
    .unwrap_or(1)
    .min(resolved_queue_depth)
```

The default of 1 preserves the current `apply_one_chunk` shape until the
receiver pipeline wires a real fan-out caller. When the caller begins
batching (the PIP-9.b.3 feed loop), it assembles up to
`resolved_batch_size` chunks before calling `apply_batch_parallel`.

### 4.4 Threshold bytes resolution

```
resolved_threshold_bytes = config.threshold_bytes.unwrap_or(0)
```

Zero disables the byte-volume gate, preserving the current file-count-
only threshold. When non-zero, `ThresholdDeltaPipeline::submit_work`
accumulates a `u64` running sum of `work.target_size()` alongside the
existing item count. Parallel promotion fires only when **both** the
file count meets `DEFAULT_PARALLEL_THRESHOLD` **and** the byte sum meets
`resolved_threshold_bytes`.

Implementation site: `ThresholdDeltaPipeline` gains a
`accumulated_bytes: u64` field, initialised to 0, updated in the
`ThresholdMode::Buffering` arm of `submit_work`:

```rust
ThresholdMode::Buffering(buf) => {
    self.accumulated_bytes = self.accumulated_bytes
        .saturating_add(work.target_size());
    buf.push(work);
    if buf.len() >= self.threshold
        && self.accumulated_bytes >= self.threshold_bytes
    {
        let buffered = std::mem::take(buf);
        self.promote_to_parallel(buffered)?;
    }
    Ok(())
}
```

The `saturating_add` prevents overflow for very long file lists; the
cost is one `u64` add per `submit_work` call on the hot path.

## 5. Memory budget enforcement

### 5.1 Steady-state memory model

The parallel-receive path's in-flight memory consumption is bounded by
three independent surfaces:

1. **Work queue** - `queue_depth * sizeof(DeltaWork)`. `DeltaWork`
   carries a `FileNdx` (u32), `PathBuf`, `u64` size, and an optional
   sequence number. Approximately 80-120 bytes per slot depending on
   path length. At the sanity ceiling of 65536 slots, this is under
   8 MiB.

2. **In-flight chunks** - `workers * average_chunk_size`. Each rayon
   worker holds at most one `DeltaChunk` during the verify step. At 8
   workers and 64 KiB chunks, peak is 512 KiB.

3. **Per-file reorder buffers** - `active_files *
   per_file_reorder_capacity * average_chunk_size`. With the default
   capacity of 64 and 8 concurrent files, peak is 32 MiB at 64 KiB
   chunks.

### 5.2 Budget validation

`WorkerPoolConfig` gains an optional `memory_budget_bytes: Option<u64>`
field (not a user-facing knob in PIP-9.h.b - an internal safety rail).
When set, the pipeline refuses to promote to parallel mode if the
projected memory consumption exceeds the budget:

```
projected = queue_depth * WORK_ITEM_SIZE
          + workers * avg_chunk_size
          + concurrent_files * per_file_reorder_capacity * avg_chunk_size
```

The concurrent-files estimate comes from the buffered items in
`ThresholdMode::Buffering` at promotion time (the number of distinct
NDX values). `avg_chunk_size` is derived from the average target file
size and the block-size heuristic.

When the projected memory exceeds the budget, the pipeline stays on the
sequential path and logs a `tracing::info!` explaining why. This is a
soft guard - the budget is never user-facing - so transfers complete
correctly regardless.

## 6. Rayon pool strategy

### 6.1 Global pool (default)

PIP-9.h.b dispatches all parallel-receive work into rayon's global
thread pool. The `workers` knob caps the concurrency of the
`apply_batch_parallel` fan-out via the existing `min_len` calculation
in `batch.rs:70`:

```rust
let cap = if self.concurrency == 0 {
    total
} else {
    self.concurrency.min(total)
};
let min_len = total.div_ceil(cap.max(1)).max(1);
```

The `concurrency` field on `ParallelDeltaApplier` is set to
`resolved_workers` at construction time. This bounds the number of
rayon tasks that run concurrently for any one batch without creating a
separate pool.

### 6.2 Scoped pool (deferred)

A dedicated `rayon::ThreadPool` is not constructed in PIP-9.h.b. The
rationale is documented in section 4.1. If PIP-9.h.c shows pool
contention between sender and receiver paths, a follow-on task can
introduce a scoped pool behind a `--parallel-receive-dedicated-pool`
flag without changing the `WorkerPoolConfig` interface.

## 7. CLI flag wiring

### 7.1 Clap argument definitions

Add the four flags to the existing `transfer_behavior_options` section
in `crates/cli/src/frontend/command_builder/sections/transfer_behavior_options.rs`,
grouped under a new `help_heading("Advanced (parallel receive)")`. The
pattern mirrors the existing `--spill-dir` / `--spill-threshold-bytes`
flags.

Each flag accepts a value parser that produces a `usize` (or `u64` for
threshold-bytes) and is gated behind `#[cfg(feature =
"parallel-receive-delta")]` so production builds without the feature
do not expose flags that cannot take effect.

### 7.2 Parsed args

Add corresponding fields to `ParsedArgs` in
`crates/cli/src/frontend/arguments/parsed_args/mod.rs`:

```rust
#[cfg(feature = "parallel-receive-delta")]
pub parallel_receive_workers: Option<usize>,
#[cfg(feature = "parallel-receive-delta")]
pub parallel_receive_batch_size: Option<usize>,
#[cfg(feature = "parallel-receive-delta")]
pub parallel_receive_queue_depth: Option<usize>,
#[cfg(feature = "parallel-receive-delta")]
pub parallel_receive_threshold_bytes: Option<u64>,
```

### 7.3 Config assembly

The drive layer in `crates/cli/src/frontend/execution/drive/` reads the
parsed fields into a `WorkerPoolConfig` and passes it through
`ConcurrentDeltaConfig` into the pipeline construction path. This is the
point where CLI > env > default precedence is enforced: CLI values
populate `WorkerPoolConfig` fields, then `apply_env_overrides` fills
remaining `None` slots.

## 8. Integration points

### 8.1 `ThresholdDeltaPipeline`

The `promote_to_parallel` method at
`crates/transfer/src/delta_pipeline/threshold.rs:98-111` changes to
accept a `WorkerPoolConfig` and resolve all four knobs before
constructing the `ParallelDeltaPipeline`:

```rust
fn promote_to_parallel(
    &mut self,
    buffered: Vec<DeltaWork>,
    mut pool_config: WorkerPoolConfig,
) -> io::Result<()> {
    worker_pool_env::apply_env_overrides(&mut pool_config);
    pool_config.validate().map_err(io::Error::other)?;

    let workers = pool_config.workers
        .unwrap_or_else(rayon::current_num_threads)
        .min(rayon::current_num_threads());
    let avg_target_size = average_target_size(&buffered);
    let queue_depth = pool_config.queue_depth
        .unwrap_or_else(|| adaptive_queue_depth(workers, avg_target_size));
    let batch_size = pool_config.batch_size
        .unwrap_or(1)
        .min(queue_depth);

    let mut parallel = if self.bypass_reorder {
        ParallelDeltaPipeline::with_explicit_capacity(
            workers, queue_depth, true,
        )
    } else {
        ParallelDeltaPipeline::with_explicit_capacity(
            workers, queue_depth, false,
        )
    };
    for item in buffered {
        parallel.submit_work(item)?;
    }
    self.mode = ThresholdMode::Parallel(parallel);
    Ok(())
}
```

### 8.2 `ParallelDeltaPipeline`

Add `with_explicit_capacity(workers: usize, capacity: usize,
bypass_reorder: bool)` constructor that accepts pre-resolved values
instead of computing them internally. The existing `new`,
`new_adaptive`, and `new_bypass_adaptive` constructors become thin
wrappers that resolve defaults and delegate.

### 8.3 `ParallelDeltaApplier`

The `concurrency` constructor parameter is set to `resolved_workers`
from the `WorkerPoolConfig`. No structural changes needed - the field
already exists and is wired into `apply_batch_parallel`.

### 8.4 Adaptive capacity deduplication

Remove the `adaptive_capacity` function at
`crates/transfer/src/delta_pipeline/parallel.rs:146-159`. Replace all
call sites with `work_queue::adaptive_queue_depth` from the engine
crate, which is parameterised on an explicit `worker_count` argument
instead of reading `rayon::current_num_threads()` internally.

Update the engine function signature:

```rust
pub fn adaptive_queue_depth(worker_count: usize, avg_file_size: u64) -> usize
```

This eliminates the lockstep maintenance risk called out in PIP-9.h.a
section 2.3.

## 9. Telemetry hooks

PIP-9.h.a section 4 requires six telemetry hooks for PIP-9.h.c to
sweep defaults from evidence. All metrics are gated behind `#[cfg(feature
= "parallel-receive-delta")]` so production builds pay zero cost.

### 9.1 Per-worker dispatch latency

Stamp each `DeltaChunk` with `submit_instant: Option<Instant>` at the
point the producer hands it to the work queue. Inside the rayon worker,
just before `verify_chunk`, compute `Instant::now() - submit_instant`.
Accumulate into a `Histogram<Duration>` on a per-transfer
`TelemetryCollector`.

### 9.2 Drain wait

Instrument `BarrierState::wait_until_idle` in
`parallel_apply/drain.rs:146-158` to record the wait duration per file
into the same `TelemetryCollector`.

### 9.3 Queue-full backpressure events

Wrap `WorkQueueSender::send` in a timed helper. When the send blocks
longer than 1 microsecond, increment an `AtomicU64` counter on the
`TelemetryCollector`. The threshold is configurable via a const so
PIP-9.h.c can adjust it.

### 9.4 Batch sizing observed

Record `chunks.len()` at each `apply_batch_parallel` entry into a
histogram. Verifies that the batch-size knob is being honoured.

### 9.5 Sequential fallback rate

Increment an `AtomicU64` in `ThresholdDeltaPipeline::flush` when the
pipeline exits via `ThresholdMode::Buffering` rather than
`ThresholdMode::Parallel`. Demonstrates the threshold-bytes knob
keeping small transfers sequential.

### 9.6 Per-file reorder buffer high-watermark

Track `max(buffered_count())` across the transfer in
`ParallelDeltaApplier::slot_for`. Tells PIP-9.h.c whether the deferred
fifth knob (per-file reorder capacity) is worth promoting.

### 9.7 Telemetry collector shape

```rust
/// Per-transfer telemetry for the parallel-receive-delta path.
///
/// All fields are lock-free atomics or thread-local histograms merged at
/// transfer end. The collector is constructed once per transfer and
/// exposed through `ParallelDeltaPipeline::telemetry()`.
#[cfg(feature = "parallel-receive-delta")]
pub struct TelemetryCollector {
    pub dispatch_latency: Mutex<Vec<Duration>>,
    pub drain_wait: Mutex<Vec<Duration>>,
    pub backpressure_events: AtomicU64,
    pub batch_sizes: Mutex<Vec<usize>>,
    pub sequential_fallbacks: AtomicU64,
    pub reorder_high_watermark: AtomicUsize,
}
```

The `Mutex<Vec<_>>` histograms are cheap at the per-transfer
granularity (one lock acquisition per worker retire, not per chunk). If
contention shows up in PIP-9.h.c, replace with thread-local accumulators
merged on `flush()`.

## 10. Testing strategy

### 10.1 Unit tests for `WorkerPoolConfig`

In `crates/engine/src/concurrent_delta/worker_pool_config.rs`:

- `default_config_is_all_none` - all fields `None`.
- `validate_rejects_zero_workers` - `workers = Some(0)` errors.
- `validate_rejects_zero_batch_size` - `batch_size = Some(0)` errors.
- `validate_rejects_zero_queue_depth` - `queue_depth = Some(0)` errors.
- `validate_rejects_queue_depth_exceeds_ceiling` - `queue_depth =
  Some(70_000)` errors.
- `validate_rejects_batch_exceeds_queue` - `batch_size = Some(64)`,
  `queue_depth = Some(32)` errors.
- `validate_accepts_valid_config` - all constraints satisfied.

### 10.2 Unit tests for env-var overrides

In `crates/engine/src/concurrent_delta/worker_pool_env.rs`, following
the `spill/env.rs` test pattern with `ENV_LOCK`, `EnvGuard`, and
`reset_env`:

- `no_env_vars_leaves_config_unchanged`
- `env_workers_sets_value`
- `env_workers_invalid_leaves_unchanged`
- `env_batch_size_sets_value`
- `env_queue_depth_sets_value`
- `env_threshold_bytes_sets_value`
- `env_threshold_bytes_invalid_leaves_unchanged`

### 10.3 Integration tests for knob resolution

In `crates/transfer/tests/`:

- `threshold_pipeline_respects_worker_count` - set `workers = Some(2)`,
  verify `ParallelDeltaPipeline` uses capacity derived from 2 workers.
- `threshold_pipeline_byte_threshold_prevents_promotion` - set
  `threshold_bytes = Some(1 GiB)` with small files, verify the pipeline
  stays sequential.
- `threshold_pipeline_byte_threshold_allows_promotion` - set
  `threshold_bytes = Some(1 KiB)` with large files, verify promotion.
- `explicit_queue_depth_overrides_adaptive` - set `queue_depth =
  Some(42)`, verify the work queue capacity is 42.

### 10.4 Bench harness extensions for PIP-9.h.c

Extend `crates/engine/benches/parallel_receive_delta_perf.rs` so
PIP-9.h.c can sweep every knob from a single invocation:

- Add env-var reading at bench setup time: `OC_RSYNC_BENCH_WORKERS`,
  `OC_RSYNC_BENCH_BATCH_SIZE`, `OC_RSYNC_BENCH_QUEUE_DEPTH`. Each
  overrides the corresponding bench parameter when set.
- Add a `sweep` benchmark group that iterates over the cross-product of
  workers {1, 2, 4, 8} x batch-size {1, 8, 32, 128} x queue-depth
  multiplier {1, 2, 4, 8} at the `mixed` workload profile. This
  produces 64 cells per invocation, enough for PIP-9.h.c to identify
  the dominant knob.
- Extend the existing `small_files`, `mixed`, and `large_files` groups
  to accept the configured `WorkerPoolConfig` instead of hard-coded
  `PARALLEL_WORKERS = 8`.

### 10.5 Regression guard

Add a test in `crates/transfer/tests/` that constructs a
`ThresholdDeltaPipeline` with default `WorkerPoolConfig`, submits 128
work items (above `DEFAULT_PARALLEL_THRESHOLD`), and verifies all
results are delivered in submission order. This guards that the knob
wiring does not break the existing default behaviour.

## 11. File inventory

New files:

| Path | Purpose |
|------|---------|
| `crates/engine/src/concurrent_delta/worker_pool_config.rs` | `WorkerPoolConfig` struct + validation |
| `crates/engine/src/concurrent_delta/worker_pool_env.rs` | Env-var override application |
| `crates/engine/src/concurrent_delta/telemetry.rs` | `TelemetryCollector` (feature-gated) |

Modified files:

| Path | Change |
|------|--------|
| `crates/engine/src/concurrent_delta/config.rs` | Add `worker_pool: WorkerPoolConfig` field |
| `crates/engine/src/concurrent_delta/mod.rs` | Re-export new modules |
| `crates/engine/src/concurrent_delta/work_queue/capacity.rs` | Parameterise `adaptive_queue_depth` on `worker_count` |
| `crates/transfer/src/delta_pipeline/threshold.rs` | Accept `WorkerPoolConfig`, add byte accumulator, resolve knobs |
| `crates/transfer/src/delta_pipeline/parallel.rs` | Add `with_explicit_capacity`, remove `adaptive_capacity` |
| `crates/transfer/src/delta_pipeline/mod.rs` | Thread `WorkerPoolConfig` through `ThresholdDeltaPipeline` |
| `crates/cli/src/frontend/command_builder/sections/transfer_behavior_options.rs` | Add four CLI flags |
| `crates/cli/src/frontend/arguments/parsed_args/mod.rs` | Add four parsed fields |
| `crates/cli/src/frontend/execution/drive/` | Assemble `WorkerPoolConfig` from parsed args |
| `crates/engine/benches/parallel_receive_delta_perf.rs` | Sweep harness for PIP-9.h.c |

## 12. Acceptance criteria

PIP-9.h.b ships when:

1. Each knob in section 1 has a CLI flag, an env var, and a config field
   with validation matching the constraint matrix.
2. Telemetry hooks from section 9 are present and disabled by default in
   production builds (zero cost when the feature gate is off).
3. Existing default behaviour is preserved when no knob is set: workers
   = `rayon::current_num_threads()`, batch size = 1, queue depth =
   workers * capacity multiplier, threshold bytes = 0.
4. The bench harness in PIP-9.h.c can sweep every knob through the
   ranges in PIP-9.h.a section 3 from a single invocation.
5. The adaptive capacity duplication between the engine and transfer
   crates is eliminated.
6. All existing tests continue to pass with no knobs set.

## 13. Rollback criterion

Inherited from PIP-9.h.a section 5. If PIP-9.h.c shows that a single
knob (likely `workers`) explains > 90% of throughput variance, the other
knobs should be demoted to internal constants and their CLI flags
removed before the feature gate is promoted to default-on. The struct
and env-var machinery stay in place for bench reproducibility, but the
user-facing surface narrows.
