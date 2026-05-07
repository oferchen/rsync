# Sharded `BufferPool` benchmark plan (#1297)

Tracking task: oc-rsync task #1297 (gating measurement for sharded
`BufferPool` implementation). Companion design: `docs/design/buffer-pool-sharding.md`
(task #1295, landed via commit `904735d0d` -
`docs(design): sharded BufferPool layout for high thread counts`).
Related history: PR thread #1338, #1640, #1641, #1329 - the original
`Mutex<Vec<Vec<u8>>>` -> `crossbeam_queue::ArrayQueue` migration.
Adjacent contention work:
`docs/audits/drain-parallel-contention-static-analysis.md` (#1679,
#1681, #1682).

Last verified: 2026-05-07 against
`crates/engine/src/local_copy/buffer_pool/{mod,pool,thread_local_cache,pressure,allocator,memory_cap}.rs`
and `crates/engine/benches/buffer_pool_contention.rs`.

This is a read-only plan. No source files are modified. It defines the
criterion harness, axes, and decision criteria that gate whether the
sharded layout in `docs/design/buffer-pool-sharding.md` actually
ships.

## Summary

`#1295` (the sharded design note) is conditional. It explicitly
defers the implementation decision to `#1297`:

> "If the 64-thread number is in the noise, the implementation does
> not land. The hypothesis must survive the benchmark."
> -- `docs/design/buffer-pool-sharding.md:380`

This document specifies the experiment that produces that number. The
harness extends the existing
`crates/engine/benches/buffer_pool_contention.rs` grid
(`THREAD_COUNTS = [2, 4, 8, 16]`, `OPS_PER_ITER = 10_000`) to cover
`[1, 4, 16, 64]` workers and three workloads that exercise the
contention regimes the design note calls out: parallel-stat burst,
producer-consumer asymmetry, and shared-pool fan-in.

The output is a decision: ship the sharded prototype, or close `#1295`
as not-justified. Both outcomes are acceptable - the plan is
hypothesis-falsifying by construction.

## Current `BufferPool` shape

Source citations against `crates/engine/src/local_copy/buffer_pool/`:

- **Two-level cache, not sharded.** `mod.rs:24-43` documents the
  thread-local single-slot cache fronting one global lock-free
  `crossbeam_queue::ArrayQueue<Vec<u8>>`.
- **Central queue.** `pool.rs:96` declares
  `buffers: ArrayQueue<Vec<u8>>`. The queue is sized via
  `queue_capacity(max_buffers)` at `pool.rs:32-42` to
  `max(max_buffers, DEFAULT_QUEUE_CAPACITY=256)` with a floor of `1`.
- **No `Mutex` on the hot path.** The previous `Mutex<Vec<Vec<u8>>>`
  layout (PR #1329) was replaced. The only remaining mutex-like
  primitives are the `RefCell` inside `thread_local_cache.rs:24-27`
  (single-threaded, cannot contend) and the `Mutex<MemoryCapState>`
  in `memory_cap.rs` which only fires when `with_memory_cap` is set
  and outstanding bytes hit the cap.
- **Soft-cap admission.** `pool.rs:584-612` (`admit_or_deallocate`)
  uses `compare_exchange_weak` on `central_count: AtomicUsize`
  (`pool.rs:104`) to bound the central queue at `soft_capacity`. Both
  atomics share the `BufferPool` struct's cache line.
- **Acquire fast path.** `pool.rs:361-422` (`acquire_from`,
  `try_acquire_from`) checks the thread-local slot first via
  `thread_local_cache::try_take()` (zero synchronization), then falls
  through to `pop_buffer()` at `pool.rs:621-641`.
- **Return path.** `pool.rs:520-573` (`return_buffer`) tries
  `thread_local_cache::try_store()`, then `admit_or_deallocate()`.
- **Adaptive resizer.** `pressure.rs:24-78` evaluates hit/miss every
  64 operations and grows or shrinks `soft_capacity` between bounds
  `[2, 256]`.

There is no sharding today. The design note proposes a layer that
would sit between the thread-local cache and the global `ArrayQueue`,
keyed by `rayon::current_thread_index()` masked to a power-of-two
shard count.

## Sharded design under test (cited from #1295)

The reference implementation that the benchmark must compare against
is fully specified in `docs/design/buffer-pool-sharding.md`. Key
load-bearing decisions, cited by line:

- **Approach C (shard-then-fallback).**
  `docs/design/buffer-pool-sharding.md:160-197`. Each thread maps to
  a primary shard `ArrayQueue<Vec<u8>>`; on miss the existing global
  `ArrayQueue` is the fallback; on miss again, allocate fresh.
- **Shard count.** `bufferpool-sharding.md:202-212`.
  `shard_count = num_cpus * 2`, clamped to `[4, 64]`, rounded up to
  power of two so the modulus is a mask.
- **Shard capacity per shard.** `bufferpool-sharding.md:214-222`.
  `max(soft_capacity / shard_count, 2)`.
- **Mapping function.** `bufferpool-sharding.md:225-239`.
  `rayon::current_thread_index()` first, falling back to OS thread id.
- **Acquire path.** `bufferpool-sharding.md:241-253`.
  `try_take` (TLS) -> `shards[idx].pop()` -> `fallback.pop()` ->
  allocate.
- **Return path.** `bufferpool-sharding.md:255-263`.
  `try_store` (TLS) -> `shards[idx].push()` -> fallback `push` under
  the existing soft-cap admission protocol.
- **Activation gate.** `bufferpool-sharding.md:274-291`.
  `should_shard(num_threads) = num_threads >= 16`. Below 16 workers
  the sharded path is bypassed entirely; the existing single-queue
  `BufferPool` is used unchanged.
- **Telemetry additions.** `bufferpool-sharding.md:331-341`.
  `BufferPoolStats` gains `shard_hits: Option<u64>` and
  `shard_overflows: Option<u64>`.

The benchmark must measure both layouts at every thread count -
including below the activation gate - to confirm that gating to
`>= 16` is correct (i.e. that the sharded layout does not regress at
low thread counts).

## Existing benchmark harness

`crates/engine/benches/buffer_pool_contention.rs` (registered in
`crates/engine/Cargo.toml:144-146`) is the foundation. Today it has
four groups:

- `bench_single_threaded` (`buffer_pool_contention.rs:40-54`):
  baseline at one thread, expects ~100% TLS hit rate.
- `bench_multi_threaded_contention`
  (`buffer_pool_contention.rs:61-95`): rayon scope at
  `THREAD_COUNTS = [2, 4, 8, 16]`, fixed 10K ops total split evenly
  across threads.
- `bench_hit_miss_rate` (`buffer_pool_contention.rs:103-162`): same
  axes plus `pool.total_hits()` / `pool.total_misses()` reporting via
  `iter_custom`.
- `bench_stat_workload` (`buffer_pool_contention.rs:169-210`): 4 KiB
  buffers, 50K ops, simulates the parallel-stat shape with a minimal
  `black_box` borrow.

The harness already pins thread counts via
`ThreadPoolBuilder::new().num_threads(threads).build()` (so the
global rayon pool does not interfere) and reports
`Throughput::Elements`. Extending it follows the same pattern.

## Plan

### Axes

| Axis | Values | Rationale |
|---|---|---|
| Layout | `BufferPool` (today), `ShardedBufferPool` (prototype) | The decision under test. |
| Threads | `[1, 4, 16, 64]` | 1 = baseline, 4 = below-gate, 16 = at-gate, 64 = the contention hypothesis target from `bufferpool-sharding.md:91-115`. |
| Workload | `parallel_stat`, `producer_consumer`, `shared_pool_fanin` | Each maps to one of the three structural-miss workloads in `bufferpool-sharding.md:91-108`. |
| Buffer size | `4 KiB`, `128 KiB` | Tracks `bench_stat_workload` (4 KiB metadata reads) and `bench_multi_threaded_contention` (default 128 KiB). |
| Item count | `10_000`, `100_000` | 10K matches the existing harness; 100K stresses adaptive-resize and steady-state fallback contention. |

The cross-product is 2 layouts * 4 thread counts * 3 workloads *
2 sizes * 2 counts = 96 cells. Criterion reports them as nested
benchmark groups.

### Workloads

#### `parallel_stat`

Models `bufferpool-sharding.md:99-103` (one-shot start-of-transfer
burst). All `N` workers race to call `acquire_from` and immediately
drop the guard. Measures TLS-cold path contention on the central
queue's head cursor.

```text
rayon_pool.scope(|s| {
    for _ in 0..threads {
        s.spawn(|_| {
            for _ in 0..ops_per_thread {
                let g = BufferPool::acquire_from(Arc::clone(&pool));
                std::hint::black_box(&*g);
                drop(g);
            }
        });
    }
});
```

This is the closest analogue to `bench_stat_workload` and serves as
the "acquire/release in tight loop on same thread" baseline. The TLS
slot absorbs nearly everything; only the first acquire per worker
falls through to the shared queue.

#### `producer_consumer`

Models `bufferpool-sharding.md:91-98` (asymmetric thread roles). Half
the workers are producers, half are consumers. Producers acquire and
hand the guard via `crossbeam_channel::bounded` to consumers, who
drop it. Producer's TLS slot stays empty after each ship; consumer's
TLS slot fills with foreign buffers.

```text
let (tx, rx) = crossbeam_channel::bounded(threads);
rayon_pool.scope(|s| {
    for _ in 0..(threads / 2) {
        let pool = Arc::clone(&pool);
        let tx = tx.clone();
        s.spawn(move |_| {
            for _ in 0..ops_per_thread {
                let g = BufferPool::acquire_from(Arc::clone(&pool));
                tx.send(g).unwrap();
            }
        });
    }
    for _ in 0..(threads / 2) {
        let rx = rx.clone();
        s.spawn(move |_| {
            while let Ok(g) = rx.recv() {
                std::hint::black_box(&*g);
                drop(g);
            }
        });
    }
});
```

This is the workload most likely to differentiate the layouts. With
the current single-queue layout, every operation falls through to the
central `ArrayQueue` because the TLS slots structurally miss. Sharding
should keep producer-side admissions on the producer's shard and
consumer-side pops local most of the time (rayon work-stealing keeps
clusters of work co-located).

#### `shared_pool_fanin`

Models `bufferpool-sharding.md:105-108` (daemon thread-per-connection
fan-in to a single global pool). All `N` workers share one
`Arc<BufferPool>` initialized via the `global.rs` path
(`init_global_buffer_pool` + `global_buffer_pool`). Each worker holds
two concurrent guards at a time, so the second `acquire` per worker
always falls through TLS.

```text
let pool = global_buffer_pool();
rayon_pool.scope(|s| {
    for _ in 0..threads {
        let pool = Arc::clone(&pool);
        s.spawn(move |_| {
            for _ in 0..ops_per_thread {
                let g1 = BufferPool::acquire_from(Arc::clone(&pool));
                let g2 = BufferPool::acquire_from(Arc::clone(&pool));
                std::hint::black_box(&*g1);
                std::hint::black_box(&*g2);
                drop(g2);
                drop(g1);
            }
        });
    }
});
```

The two-buffer-per-worker pattern guarantees the central queue is
touched on every iteration. This is the upper-bound contention test.

### Telemetry

For each cell:

1. Wall time via `criterion::Criterion::iter_custom` (matches the
   existing `bench_hit_miss_rate` pattern at
   `buffer_pool_contention.rs:119-156`).
2. `pool.total_hits()` / `pool.total_misses()` after the inner loop;
   logged once per cell on the first criterion sample to avoid output
   spam.
3. `pool.total_growths()` to flag adaptive-resize churn.
4. For sharded runs, the new `shard_hits` / `shard_overflows`
   counters from `BufferPoolStats`
   (`bufferpool-sharding.md:331-341`).

Criterion's `Throughput::Elements(ops_per_iter)` produces an ops/sec
figure that is directly comparable across layouts.

### Profiling overlay

Per `bufferpool-sharding.md:425-434`, the benchmark run also captures
a contention-line histogram. The plan does not own that capture - it
is described here only so the result set is complete:

- **Linux**:
  `perf c2c record -- target/release/buffer_pool_contention --bench`,
  then `perf c2c report` to read the cache-line-bouncing histogram on
  `central_count` and on the `ArrayQueue` head/tail.
- **macOS**: `dtrace -n 'profile-1001 /pid == $target/ {...}'` on
  `BufferPool::pop_buffer` and `admit_or_deallocate`. Approximate
  L3-miss attribution via `vmstat -h`.

The cross-platform asymmetry is acceptable because the contention
hypothesis is a hardware-level effect; one-platform evidence either
way is sufficient to settle the design decision.

## Expected outcomes

The bands are stated by `bufferpool-sharding.md:372-382`:

| Threads | Expected sharded speedup at the buffer-pool layer |
|---|---|
| 1 | Within noise (< 5%). Sharding is gated off; both layouts use the same code path. |
| 4 | Within noise (< 5%). Below activation gate. |
| 16 | 1.0x to 1.3x. At activation gate; hits depend on workload. |
| 64 | 1.5x to 4x for `producer_consumer` and `shared_pool_fanin`; 1.0x to 1.2x for `parallel_stat`. |

End-to-end (full transfer) speedup will be a fraction of the pool-layer
speedup because the buffer pool is one component among many. Pool-layer
ops/sec is the load-bearing metric.

`parallel_stat` is expected to show the smallest sharded benefit
because the TLS slot already absorbs the steady-state hot path; only
the start-of-burst cold-cache phase is sensitive. This is intentional
- it confirms the harness is calibrated and that any large sharded
gain on the other two workloads is not an artefact.

## Decision criteria

The benchmark output gates `#1295` implementation. Decisions are
deterministic in the result set:

1. **Ship sharding.** All three conditions must hold:
   a. At 64 threads, `producer_consumer` shows >= 1.5x sharded speedup
      at both buffer sizes.
   b. At 64 threads, `shared_pool_fanin` shows >= 1.5x sharded speedup
      at both buffer sizes.
   c. At 1 and 4 threads, sharded layout regression is < 5% across all
      workloads. This validates the activation gate.

2. **Close `#1295` as not-justified.** Any of:
   a. At 64 threads, both `producer_consumer` and `shared_pool_fanin`
      sharded speedups are within +/- 5% noise. The contention
      hypothesis is falsified.
   b. At 1 or 4 threads, sharded regression exceeds 5% on any workload
      with no obvious harness fix. The cost-benefit is wrong.

3. **Iterate on the design.** Mixed signal:
   - 64-thread `producer_consumer` shows >= 1.5x but
     `shared_pool_fanin` does not. Indicates the global-fallback
     contention is the bottleneck, not the per-thread queue. A
     revised design needs per-shard admission counters; re-run the
     plan after the design is updated.
   - 64-thread speedups are present but adaptive-resize growth events
     (`total_growths`) explode under sharding. Indicates shard
     under-capacity; revise `shard_capacity` formula and re-run.

The decision must be recorded in the same audit directory as a
follow-up note (`docs/audits/bufferpool-sharded-benchmark-result.md`)
that cites the criterion JSON output and lists the chosen branch.

## Out of scope

- Single-instance optimization unrelated to sharding (e.g. `MemoryCap`
  re-tuning, adaptive resizer policy changes). Tracked separately
  under #1834 (adaptive sizing audit) and the recent `--max-alloc=N`
  work (PR #3749).
- Cross-process pool sharing (no current use case in oc-rsync).
- IO-side benchmarks. The sharding decision is purely a CPU-cache
  question; disk and network throughput do not enter the experiment.
- Replacing `crossbeam_queue::ArrayQueue` with a different lock-free
  structure. The fallback queue stays as-is per
  `bufferpool-sharding.md:181-184`.

## Risks

1. **Criterion pool re-construction overhead.** Running fresh
   `Arc<BufferPool>` per criterion sample (as `bench_hit_miss_rate`
   does at `buffer_pool_contention.rs:124`) skews the first iteration
   because TLS slots are cold. Mitigation: warm-up loop of
   `ops_per_thread` before the timed region, mirroring the existing
   `bench_multi_threaded_contention` shape which reuses one pool.
2. **Rayon pool reuse across cells.** The `ThreadPoolBuilder` cost is
   non-trivial. Mitigation: build one rayon pool per thread-count
   parameter, reuse across workloads. The existing harness already
   does this at `buffer_pool_contention.rs:71-74`.
3. **macOS thread-count mismatch.** CI runners cap at 4 logical CPUs;
   the 64-thread cell will oversubscribe and produce noisy timings.
   Mitigation: run the gating measurement on the `rsync-profile`
   container (Linux, > 16 cores) per the project's container policy
   in `CLAUDE.md`. Document the platform in the result note.
4. **TLS slot warm-up artefact.** After
   `thread_local_cache::try_store` succeeds once per thread, the slot
   stays warm for the rest of the benchmark. This means the second
   and subsequent criterion samples see different per-thread TLS
   state than a fresh production process would. Mitigation: this is
   an inherent property of the TLS design and is identical across
   layouts; the relative comparison stays valid.
5. **Adaptive resizer interaction.** `with_adaptive_resizing` is not
   on by default (see `pool.rs:288-308`). The benchmark must run with
   resizing off (default), then a separate cell with resizing on, so
   any sharded regression that shows up under resizing is
   attributable. Mitigation: a fifth axis `adaptive: bool` on the
   `producer_consumer` workload only; this is the workload most
   sensitive to resize churn.

## Tracking

- `#1295` (design note, landed at commit `904735d0d`) - prerequisite.
- `#1297` (this plan) - benchmark plan.
- Implementation TODO: gated on the decision in this plan. No tracker
  entry created until the decision is recorded.
- Result follow-up: `docs/audits/bufferpool-sharded-benchmark-result.md`,
  to be authored after the harness runs.

## Decision

Land this plan now. The benchmark harness extension and its three
workloads must run before any `ShardedBufferPool` code is written.
The decision criteria above are the contract: the implementation
ships only if the data supports it, and `#1295` closes as a
documented non-decision otherwise.
