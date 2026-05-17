# `spawn_blocking` Bridge for Rayon Work in the Async Daemon

Tracking issue: #1751. Companion to #1935 (async daemon impl),
#1367 (evaluate async daemon), #1594 (async migration plan).

Status: Design - opinionated. The async daemon does not exist in
production yet (#1935 in flight). This document fixes the bridge
contract so #1935 can land without re-deriving it, and so subsequent
phases (`docs/design/async-migration-plan.md` phase 2) can migrate
call sites against a stable target.

This document supersedes the narrower sketch at
`docs/design/tokio-spawn-blocking-rayon.md` for purposes of phase-2
implementation. The older note remains as historical context.

## 1. Why this document exists

`docs/design/async-migration-plan.md` commits the workspace to tokio
as the single async runtime and earmarks the daemon accept loop as
the first production-bound migration (phase 1, #1935). Once
`crates/daemon/src/daemon/async_session/listener.rs` is the default,
every `core::session()` call from inside the listener will execute on
a tokio worker thread. The transfer engine reachable from there still
dispatches CPU-heavy work through `rayon` (parallel stat, signature
generation, file hashing, block matching). A `par_iter` call from
inside an `async fn` blocks the calling tokio worker for the whole
parallel job; with `num_cpus` workers a few stalled jobs deny the
listener loop entirely.

The bridge is the single place the migration crosses from the async
runtime to the rayon thread pool. Get it wrong and the symptoms are
classic: rising accept latency, multiplex frame back-pressure,
keepalive deadlines missed, then `JoinError::is_cancelled()` storms
on shutdown. Get it right and the async daemon inherits the rayon
parallelism without any new lock taxonomy.

## 2. Inventory of rayon dispatches reachable from the daemon

Counted from `crates/daemon` -> `crates/core::session()` -> the
transfer engine. The daemon crate itself contains zero direct rayon
calls; every dispatch below is reached transitively through
`core::session()` once the daemon has accepted a connection and
handed work to the transfer pipeline.

All file paths are relative to the workspace root. Line numbers were
captured at the tip of `origin/master` at the time of writing; treat
them as anchors, not contracts.

### 2.1 Transfer pipeline (receiver-side, hottest path)

- `crates/transfer/src/receiver/transfer/pipeline.rs:186-188` -
  `batch.par_iter().map(|...| build_basis(...))` for signature
  generation per file in a batch. Bounded by `sig_threshold`
  (`ParallelOp::Signature`).
- `crates/transfer/src/parallel_io.rs:186` -
  `items.into_par_iter().map(f).collect()` in `map_blocking`. The
  generic helper that fans out stat/chmod/chown/metadata batches.
  Threshold short-circuits via `if items.len() < min_parallel`.
- `crates/transfer/src/generator/file_list/batch_stat.rs:43-50` -
  `batch_stat_dir_entries` calls `map_blocking` to batch `stat` /
  `lstat` per directory child. Reached during file list generation.

### 2.2 Engine concurrent delta pipeline

- `crates/engine/src/concurrent_delta/work_queue/drain.rs:67-84` -
  `rayon::scope` consuming a bounded `WorkQueueReceiver` and pushing
  results into per-thread sharded buffers.
- `crates/engine/src/concurrent_delta/work_queue/drain.rs:141-152` -
  `rayon::scope` variant that streams results through a `Sender<R>`
  for the streaming reorder buffer.
- `crates/engine/src/concurrent_delta/consumer.rs:160` -
  the `delta-drain` thread runs `drain_parallel_into` inside
  `rayon::scope`. This is the load-bearing rayon site for
  multi-file delta dispatch; everything else feeds into or out of
  it.

### 2.3 Engine local-copy executor

- `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:95`
  - `pairs.par_iter().map(...)` to prefetch source/destination
  checksums for quick-check candidate pairs.
- `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:110`
  - `rayon::join(|| compute_src, || compute_dst)` per pair.
- `crates/engine/src/local_copy/executor/directory/support.rs:106`
  - `pending.into_par_iter().map(symlink_metadata)` for directory
  entry metadata fetch. Sort applied after collection.
- `crates/engine/src/local_copy/executor/directory/parallel_planner.rs:100`
  - `entries.par_iter().enumerate().map(...)` to prefetch symlink
  targets and device metadata.

### 2.4 Checksums (`crates/checksums/src/parallel/`)

Pure CPU; reached whenever the transfer engine builds or verifies
checksums for a file batch.

- `parallel/files.rs:163` - `hash_files_parallel_with_config` over a
  slice of `PathBuf`. mmap or buffered read inside.
- `parallel/files.rs:202` - `hash_files_with_seed_parallel` variant.
- `parallel/files.rs:307` - `compute_file_signatures_parallel`
  builds per-file block signatures.
- `parallel/blocks.rs:117` - `compute_digests_parallel` over
  in-memory blocks.
- `parallel/blocks.rs:145` - `compute_digests_with_seed_parallel`.
- `parallel/blocks.rs:169` - `compute_rolling_checksums_parallel`.
- `parallel/blocks.rs:208` - `compute_block_signatures_parallel`
  (rolling + strong per block).
- `parallel/blocks.rs:251` - `process_blocks_parallel` generic
  worker.
- `parallel/blocks.rs:282` - `filter_blocks_by_checksum` predicate
  scan returning matching indices.

### 2.5 File list (`crates/flist/`)

- `flist/src/parallel.rs:83` - `process_entries_parallel` generic
  per-entry computation.
- `flist/src/parallel.rs:105` - `filter_entries_indices` parallel
  predicate scan.
- `flist/src/parallel.rs:132` - `collect_paths_then_metadata_parallel`
  parallel stat across pre-enumerated paths.
- `flist/src/parallel.rs:319` - `collect_paths_chunked_parallel`
  chunked variant with bounded memory.
- `flist/src/parallel.rs:388` - `resolve_metadata_parallel` for
  lazy entries.
- `flist/src/batched_stat/cache.rs:131` - `BatchedStatCache::stat_batch`
  parallel stat with sharded caching.
- `flist/src/batched_stat/dir_stat.rs:153` -
  `DirStatHandle::stat_batch_relative` (Linux `statx` via `openat`).

### 2.6 Signature crate

- `crates/signature/src/parallel.rs:139` - `par_chunks(BATCH_SIZE)`
  for SIMD-batched rolling + strong checksum generation. Combines
  thread-level and data-level parallelism per chunk.

### 2.7 fast_io parallel helpers

- `crates/fast_io/src/parallel.rs:136` -
  `BatchProcessor::process` fold/reduce across items, with an
  optional dedicated rayon `ThreadPoolBuilder` at line 159 (sized to
  `thread_count` for I/O-bound work).
- `crates/fast_io/src/parallel.rs:191` -
  `BatchProcessor::process_files` mirror for file paths, dedicated
  pool at line 217.
- `crates/fast_io/src/cached_sort.rs:114` -
  `items.par_iter().map(&key_fn).collect()` for cached sort key
  extraction.

### 2.8 Tally

| Subsystem | Direct rayon call sites |
|-----------|-------------------------|
| Transfer (`crates/transfer`) | 3 |
| Engine concurrent delta (`crates/engine/.../concurrent_delta`) | 3 |
| Engine local-copy directory (`crates/engine/.../directory`) | 4 |
| Checksums (`crates/checksums/src/parallel`) | 9 |
| File list (`crates/flist/src`) | 7 |
| Signature (`crates/signature/src`) | 1 |
| fast_io (`crates/fast_io/src`) | 3 |
| **Total reachable from daemon** | **30** |

All 30 sites were classified SAFE or GUARDED in the audit at
`crates/engine/src/concurrent_delta/mod.rs:55-165`. None are
RISK-class. The bridge does not change that classification; it only
controls *where* the rayon work runs relative to the tokio runtime.

## 3. The bridge pattern

A single helper, called from every async call site that reaches a
rayon dispatch. Implementation lands in `crates/transfer/src/async_compat.rs`
(new module) per `async-migration-plan.md` phase 2.

```rust
use std::future::Future;
use tokio::task::{JoinError, JoinHandle};

/// Bridge a rayon-dispatched CPU job onto the async runtime without
/// stalling a tokio worker. Falls back to direct invocation when the
/// workload is below the bridge threshold (`min_units > units`).
///
/// `job` runs on tokio's blocking pool. From inside `job` the caller
/// may freely use `rayon::par_iter`, `rayon::scope`, `rayon::join`,
/// or a pre-installed `rayon::ThreadPool`. The blocking thread parks
/// on the rayon join; async workers stay free for I/O.
///
/// Errors are kept narrow on purpose: a join failure is the only
/// new failure mode the bridge introduces. Inner errors are returned
/// by `job` itself (typically `io::Result<T>`).
pub async fn rayon_bridge<F, T>(min_units: usize, units: usize, job: F) -> Result<T, JoinError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    if units < min_units {
        return Ok(job());
    }
    tokio::task::spawn_blocking(job).await
}
```

Canonical use against an existing rayon site (the
`compute_block_signatures_parallel` call from a future async receiver):

```rust
let result = rayon_bridge(
    parallel_thresholds.for_op(ParallelOp::Signature),
    blocks.len(),
    move || compute_block_signatures_parallel::<Md5, _>(&blocks),
)
.await
.map_err(TransferError::from_join)?;
```

### 3.1 `rayon::scope` variant

`drain_parallel` and `drain_parallel_into` already hide their
`rayon::scope` inside a helper. The bridge wraps the helper, not the
scope itself:

```rust
let drained = rayon_bridge(
    1, // always bridge; scope cost dominates only for empty queues
    queue_len,
    move || work_rx.drain_parallel(|w| process(w)),
)
.await?;
```

The pattern preserves the `Send + 'static` requirement of
`spawn_blocking` and lets rayon manage the scope-internal thread
lifetime. Never combine `block_on` and `rayon::scope` on the same
thread; the closure escapes the scope's borrow guarantees through
the await point only because `move ||` re-establishes ownership at
the bridge boundary.

### 3.2 Error propagation

`JoinError` discriminates panic vs cancellation:

- `JoinError::is_panic()` -> map to `ExitCode::PROTOCOL` with a
  `[server]` role trailer. Mirrors upstream's fork-per-connection
  crash report.
- `JoinError::is_cancelled()` -> only possible if the join handle is
  dropped; never observed under `rayon_bridge` because we always
  `.await` the handle. Treat as `ExitCode::INTERRUPTED` defensively.

A `TransferError::from_join` adapter belongs alongside the helper.
It captures the panic payload via `into_panic()` for the log line
and stops there; the panic is not re-thrown.

### 3.3 What the bridge does *not* do

- Does not nest rayon pools. The closure runs on whichever rayon
  pool the call site selects: global pool by default, or a dedicated
  pool when the site uses `rayon::ThreadPoolBuilder::install` (e.g.
  `fast_io::parallel::BatchProcessor`).
- Does not retry. A panicked job is a programming bug; the bridge
  surfaces it, the daemon logs it, the session ends.
- Does not provide cancellation. `spawn_blocking` futures cannot be
  cancelled. Cooperative cancellation lives inside the job, not at
  the bridge.

## 4. Sizing the two pools

### 4.1 Defaults today

- Global rayon pool: lazy-initialised to `num_cpus` worker threads
  (rayon default). The workspace never calls
  `rayon::ThreadPoolBuilder::build_global`, so the default holds.
- tokio multi-thread runtime: `num_cpus` worker threads.
- tokio blocking pool: 512 threads (`max_blocking_threads` default).

With no intervention the daemon would run `num_cpus` tokio workers,
`num_cpus` rayon workers, and up to 512 transient blocking threads.
On a 16-core box that is up to 544 concurrent threads competing for
16 cores. Context-switch storm; rayon throughput collapses.

### 4.2 Recommendation

The recommendation is opinionated. Where it requires a benchmark
to confirm a number, the number is listed as a TODO instead of
shipped.

1. **Pin the rayon pool**, owned by `core`, sized to
   `max(1, num_cpus - tokio_workers)`. Build once at daemon startup
   via `rayon::ThreadPoolBuilder::new().num_threads(N).build()` and
   install it on each `rayon_bridge` call via `pool.install(job)`.
   This eliminates contention between the global rayon pool used by
   CLI invocations and the daemon's parallel work.
   - Avoid `build_global()`. The CLI shares the same binary; mutating
     the global pool breaks CLI runs that expect `num_cpus`.
2. **Cap the tokio blocking pool well below 512.**
   `tokio::runtime::Builder::max_blocking_threads` set to
   `2 * num_cpus`. The bridge does not need thousands of blocking
   slots; under fan-out we want pushback, not RSS spikes.
   - TODO: confirm with `daemon-tpc-benchmark-plan.md` 10k-connection
     run that `2 * num_cpus` does not bottleneck the listener.
     Owner: daemon maintainers. Trigger: phase 1 promotion gate.
3. **Bound concurrent bridges with a semaphore.** A
   `tokio::sync::Semaphore` sized to `max_blocking_threads` in front
   of every `rayon_bridge` call. Without this, a 10k-connection burst
   could schedule 10k pending blocking jobs and OOM the daemon
   before rayon even sees the first one.
4. **Tokio worker count: leave at `num_cpus`.** The daemon's
   primary job is I/O. Shrinking it to make room for rayon hurts
   accept-loop throughput more than it helps rayon under realistic
   workloads. Rayon shrinkage absorbs the cost.

### 4.3 What we are deliberately not doing

- Not adopting `tokio::task::block_in_place`. It saves one thread
  hop but only on the multi-thread runtime, cannot be used from
  `current_thread`, and composes badly with `select!`/`join!` arms
  that need true cancellation. The bridge must work on both
  runtimes (tests use `current_thread`); `spawn_blocking` does.
- Not lowering rayon's per-thread stack. Some block-match passes
  use sizeable on-stack buffers; the default rayon stack (8 MiB) is
  the safe baseline.
- Not exposing the pool sizes as user-visible CLI flags. Operators
  who need to tune get an env var
  (`OC_RSYNC_DAEMON_RAYON_THREADS`,
  `OC_RSYNC_DAEMON_BLOCKING_THREADS`) read once at daemon start.
  No man-page surface, no runtime reconfiguration. Phase-1 ships
  defaults only; the env vars are reserved names.

## 5. Anti-patterns to actively forbid

The bridge fails open: a wrong call site still compiles, still runs,
and may even pass tests at low load. The damage shows up under
production fan-out. Treat each item below as a review block, not a
guideline.

1. **`rayon::par_iter` from inside an `async fn`.** Stalls the
   calling tokio worker for the entire parallel job. Always wrap in
   `rayon_bridge`. Lint target: a `clippy::disallowed_methods`
   entry on `rayon::iter::IntoParallelIterator::into_par_iter` and
   `rayon::iter::ParallelIterator::par_iter` when the file is
   reachable from an `async fn`. Manual review until tooling
   catches up.
2. **`tokio::runtime::Handle::block_on` from a rayon worker.**
   Deadlocks deterministically if the runtime has `num_cpus` workers
   and all of them are blocked on the same `block_on`. Even with
   slack, this couples the two pools' liveness in a way debugging
   cannot untangle. Forbidden without exception.
3. **Nested `spawn_blocking` from inside a rayon worker.** Wastes a
   blocking-pool slot per nested call, then parks on a join from a
   thread that is itself a join target. The classic recipe for
   exhausting `max_blocking_threads` under fan-out. If a rayon
   worker needs async, the bridge is in the wrong place; restructure
   so the async call sits *outside* the rayon scope.
4. **Long-lived `rayon::scope` from a blocking thread.** A scope
   that runs for seconds or minutes holds a blocking-pool slot for
   the same duration. Cap the scope's wall-clock by chunking work
   into bridge-sized batches; consume them with a streaming receiver
   so the blocking slot turns over.
5. **Capturing non-`Send` state by reference.** `spawn_blocking`
   requires `F: Send + 'static`. The bridge takes `FnOnce() -> T`;
   any borrow must be `move`d through `Arc` or cloned. Compile-time
   error, but easy to mis-diagnose as a refactor-on-async-await
   issue. Document explicitly so reviewers spot it.
6. **Using the bridge for sub-microsecond work.** Each bridge hop
   costs roughly one context switch in each direction. Short-circuit
   below `min_units`; otherwise the hop dominates the work. The
   bridge already takes a threshold; reviewers must supply a
   meaningful one, not `0`.
7. **Mixing `tokio::task::yield_now` with rayon scope.** Yield
   points inside a `spawn_blocking` closure do nothing useful and
   confuse cancellation semantics. The blocking pool is not
   cooperatively scheduled.

## 6. Migration order

The order is driven by which call sites are reachable from the
daemon accept loop and which are also on hot paths. Source: section
2 inventory; ordering reflects #1935's incremental promotion.

### 6.1 Pre-phase: land the helper

Land `crates/transfer/src/async_compat.rs` with
`rayon_bridge` + `TransferError::from_join`. No call site migration.
Tests cover threshold short-circuit, panic mapping, and a
`current_thread` runtime smoke test. This is phase 2 of
`async-migration-plan.md`.

### 6.2 Wave 1: signature generation (highest call rate)

- `crates/transfer/src/receiver/transfer/pipeline.rs:186` - bridge
  the per-batch `par_iter` for signature build. Hot path during
  delta transfer; bridging here yields immediate accept-loop
  benefit.
- `crates/transfer/src/parallel_io.rs:186` - bridge inside
  `map_blocking`. Every stat / chmod / chown batch the async
  receiver dispatches lands here, so bridging once at the helper
  level covers many call sites with one change.
- `crates/signature/src/parallel.rs:139` - bridge from the async
  signature dispatcher. Reach via the helper above plus a thin
  shim in `signature::parallel`.

### 6.3 Wave 2: file-list build and stat batches

- `crates/transfer/src/generator/file_list/batch_stat.rs:43` -
  already routes through `map_blocking`. Inherits wave-1's bridge.
- `crates/flist/src/batched_stat/cache.rs:131` and
  `crates/flist/src/batched_stat/dir_stat.rs:153` - bridge from the
  async file-list builder. Daemon push transfers hit these on every
  enumeration pass.
- `crates/flist/src/parallel.rs:83,105,132,319,388` - bridge from
  async callers only. CLI sync callers continue calling rayon
  directly.

### 6.4 Wave 3: concurrent delta pipeline

- `crates/engine/src/concurrent_delta/work_queue/drain.rs:67,141`
  - bridge the `rayon::scope` entry points behind a helper on
  `WorkQueueReceiver` that returns a future. The async receiver
  awaits the future instead of spawning a thread.
- `crates/engine/src/concurrent_delta/consumer.rs:160` - the
  `delta-drain` thread. Either keep it as a dedicated `std::thread`
  (recommended; it is long-lived and has its own lifecycle) or
  bridge through `spawn_blocking` if the async receiver subsumes
  the thread.

### 6.5 Wave 4: checksums and local copy

- `crates/checksums/src/parallel/{files,blocks}.rs` - bridge from
  async callers. The functions stay sync; bridging happens at the
  call site.
- `crates/engine/src/local_copy/executor/directory/*.rs` - reached
  only when the async daemon serves a local-source pull. Bridge
  last; smallest hot-path impact.
- `crates/fast_io/src/parallel.rs` and
  `crates/fast_io/src/cached_sort.rs` - bridge from async callers
  only. The dedicated rayon `ThreadPoolBuilder` instances inside
  `BatchProcessor` stay; they compose with `pool.install(job)`
  inside the bridge.

### 6.6 Sync callers stay sync

The CLI never enters an async context (see
`async-migration-plan.md` section 2.6). Sync call sites continue
to invoke rayon directly. The bridge is conditional discipline:
introduce it only at the async boundary.

## 7. Test strategy without an async daemon

#1935 has not landed. The bridge must ship and be verifiable
independently. The migration plan promises this; the strategy below
delivers it.

### 7.1 Unit tests against the helper

- Threshold short-circuit: assert that with `units < min_units` the
  closure runs on the current thread (capture thread id pre- and
  post-call).
- Panic propagation: `spawn_blocking(|| panic!("x"))` returns a
  `JoinError` with `is_panic() == true`. Assert the
  `TransferError::from_join` adapter preserves the panic message.
- Result passthrough: closure returning `io::Result<T>` round-trips
  Ok and Err variants without wrapping.

### 7.2 Concurrency smoke under `current_thread`

The most common deadlock failure mode is "bridge runs on
`current_thread` and the closure tries to re-enter the runtime."
Build a `tokio::runtime::Builder::new_current_thread()` runtime,
issue a `rayon_bridge` that runs a 100ms rayon job, and assert that
a parallel `tokio::time::sleep(50ms)` task still fires on schedule.
If it does not, the bridge is starving the runtime.

### 7.3 Listener-progress invariant on the multi-thread runtime

A synthetic harness: `tokio::runtime::Builder::new_multi_thread()`
with `worker_threads(2)`, `max_blocking_threads(4)`. Spawn an
"accept loop" that records timestamps. In parallel, spawn N
`rayon_bridge` calls that each run a 50ms CPU job. Assert the
accept loop tick interval stays below 5ms p99 throughout. This is
exactly the behaviour the production async daemon needs; the
harness exercises it without touching daemon code.

### 7.4 Property test for ordering preservation

The audit at `concurrent_delta/mod.rs:55-165` proves rayon's
indexed `collect` preserves input order. The bridge must not lose
that property. Proptest harness: for random input vectors of length
1..1000, assert
`bridge(0, len, || vec.par_iter().map(f).collect::<Vec<_>>())`
produces the same `Vec<_>` as the sequential reference, in the same
order.

### 7.5 Fault injection under load

Use `loom` (already in the dev-deps tree via `crossbeam`) to model
the bridge's join-handle handshake. Specifically: a sender that
spawns a blocking job and a receiver that awaits the join handle;
assert no schedule produces a deadlock or lost wake-up.

For tokio runtime behaviour `loom` cannot model, fall back to a
soak test: 1000 iterations of the listener-progress invariant
above, with random think-time jitter on the rayon jobs. Failures
must produce a deterministic reproducer (record the seed).

### 7.6 Integration coverage when #1935 lands

When the async daemon promotes to default, add an end-to-end
interop run: `tools/ci/run_interop.sh` with
`OC_RSYNC_DAEMON_ASYNC=1` against upstream 3.0.9, 3.1.3, 3.4.1.
No new test file required; the existing harness exercises every
rayon call site in section 2 transitively.

### 7.7 Benchmark targets

Listed as TODOs for follow-up work; the bridge itself does not
benchmark.

- `bench: daemon accept p99 with N concurrent bridged jobs`. Owner:
  daemon maintainers. Trigger: phase 1 promotion gate.
- `bench: rayon_bridge throughput vs direct rayon call at varying
  job sizes`. Owner: transfer maintainers. Trigger: first wave-1
  call site lands.
- `bench: max_blocking_threads sensitivity sweep (2 * num_cpus,
  4 * num_cpus, default 512)`. Owner: daemon maintainers. Trigger:
  before promoting any default change to the blocking pool cap.

## 8. Open questions, owners, triggers

- **OQ1 - Should `rayon_bridge` accept a `&rayon::ThreadPool` to
  install into?** Recommendation: yes, behind an
  `Option<&'static ThreadPool>` parameter on a sibling helper
  (`rayon_bridge_in`). Default site uses the global pool; daemon
  call sites pass the daemon-owned pool. Owner: transfer
  maintainers. Trigger: when the daemon owns its rayon pool (phase
  1 promotion).
- **OQ2 - Should we adopt `tokio_util::task::TaskTracker` to count
  in-flight bridges?** Adds operability without changing
  semantics. Defer to when metrics scaffolding lands (#2136 actor
  pattern surfaces this anyway). Owner: daemon maintainers.
- **OQ3 - Is there ever a case for `block_in_place`?** Only inside
  a known-multi-thread runtime, only when the bridge cost matters,
  and only when no `current_thread` test target uses the call site.
  Recommendation: ship without `block_in_place`; revisit only if
  bridge-hop profiling shows it as a measurable cost. Owner:
  transfer maintainers. Trigger: bench result from section 7.7.
- **OQ4 - Should the bridge cap concurrent bridges itself, or
  expose a semaphore to the caller?** Recommendation: expose a
  semaphore, owned by the daemon, acquired before calling the
  bridge. Keeps the helper single-responsibility (Strategy: bridge
  policy lives at the call site; semaphore policy lives in the
  daemon supervisor). Owner: daemon maintainers. Trigger: wave-1
  migration.

## 9. Cross-references

| Tracker | Subject |
|---------|---------|
| #1367 | Daemon async migration (evaluation) |
| #1594 | Async migration plan (`docs/design/async-migration-plan.md`) |
| #1751 | This document |
| #1935 | Async daemon listener implementation |
| `docs/design/async-migration-plan.md` | Phase plan, runtime choice, bridge points (section 5.2) |
| `docs/design/tokio-spawn-blocking-rayon.md` | Historical sketch superseded by this document for phase-2 implementation |
| `docs/design/daemon-async-accept-sync-workers.md` | Hybrid accept+sync-worker topology |
| `docs/design/daemon-tpc-benchmark-plan.md` | Promotion benchmark plan |
| `docs/design/async-channel-abstraction.md` | Sync/async channel abstraction (#1591) |
| `crates/engine/src/concurrent_delta/mod.rs:55-165` | SAFE / GUARDED / RISK audit of every `par_iter` site |
