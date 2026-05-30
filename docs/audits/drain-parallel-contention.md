# `drain_parallel` Mutex contention: profiling strategy

Tracking issue: oc-rsync task #1679. Companion to
`docs/audits/drain-parallel-contention-static-analysis.md` (PR #3699,
merged), which fixes the *shape* of the contention model from the
source. This document specifies the *measurements* a runner should
execute to fit numbers to that shape, the instrumentation points, and
the harness layout. Related: #1617 / #1680 (per-thread Vec accumulation,
done), #1681 (lock-free MPSC alternative, gated on the data this audit
prescribes), #1682 (Mutex vs per-thread Vec vs MPSC comparison,
pending), #1192 (profile under 100K+ files, pending).

Last verified: 2026-05-07 against
`crates/engine/src/concurrent_delta/work_queue/{drain,bounded,capacity,iter,mod}.rs`,
`crates/engine/src/concurrent_delta/{consumer,reorder}.rs`,
`crates/engine/benches/drain_parallel_benchmark.rs`,
`crates/transfer/src/delta_pipeline.rs`.

This is documentation of the testing strategy. It does not run any
benchmark, profiler, or `cargo` invocation. The runner who executes the
plan returns numeric results to a follow-up audit or to #1682.

## Implementation under measurement

`WorkQueueReceiver::drain_parallel`
(`crates/engine/src/concurrent_delta/work_queue/drain.rs:57-90`)
collects rayon worker results into `N` per-thread `Mutex<Vec<R>>`
shards, sized at scope entry by `rayon::current_num_threads()`. The
worker maps to a shard via `rayon::current_thread_index()` (with a
`DefaultHasher(ThreadId)` fallback for foreign threads). After
`rayon::scope` joins, the dispatcher flattens shards via `into_inner`.

Per-thread Vec is the *current* implementation, not a planned one - the
"per-thread Vec via rayon thread index" mitigation listed in the task
prompt landed in #1680. The static analysis (PR #3699) confirms the
hot-path lock site is `drain.rs:81` (`shards[idx].lock().unwrap().push`)
and the merge site is `drain.rs:86-89`. There are no other
`Mutex::lock()` sites on the result path.

The streaming variant `drain_parallel_into` (`drain.rs:136-155`) routes
results through a cloned `crossbeam_channel::Sender<R>`. It is the
variant exercised in production
(`crates/engine/src/concurrent_delta/consumer.rs:138-143`); the
shard-based `drain_parallel` is used by tests and the existing bench.
Both variants are in scope for #1679 because the choice between them is
a contention question.

## Lock acquisition sites in scope

| Site | File:line | Class | Frequency |
|------|-----------|-------|-----------|
| Per-item shard push (worker closure) | `drain.rs:81` | H | one acquire-release per item per worker |
| Final shard merge (dispatcher, post-scope) | `drain.rs:86-89` | S | one per shard, once per drain |
| `crossbeam_channel::Receiver::recv` (dispatcher iterator) | `iter.rs:33` | H, single-threaded | one per item, dispatcher only |
| `crossbeam_channel::Sender::send` (`drain_parallel_into`) | `drain.rs:149` | H | one per item per worker |

`H` = hot path, `S` = shutdown / merge. There are no batch-class (`B`)
acquisitions; results are not locally buffered before the shard push.

## Measurement axes

The bench grid in `crates/engine/benches/drain_parallel_benchmark.rs`
covers `THREAD_COUNTS = [1, 4, 8, 16]` and
`COUNTS = [10_000, 100_000]`. #1679 extends the worker axis to 32 and 64
to expose the queueing-delay term that scales with `W` past the
physical-core count.

Required axes:

| Axis | Values | Rationale |
|------|--------|-----------|
| Worker threads `W` | 1, 4, 8, 16, 32, 64 | covers SMT pair, single-socket, dual-socket, oversubscribed regimes |
| Item count `K_total` | 10_000, 100_000, 1_000_000 | 10K and 100K match existing grid; 1M extends to the #1192 envelope |
| Variant | `drain_parallel`, `drain_parallel_into` | shard vs MPSC contention models |
| Result size | scalar `u64`, `Vec<u8>` payload size 64 B and 4 KiB | tests cache-line interaction with the `Vec::push` realloc pattern |
| Pool topology | rayon default pool, custom pool size 32, custom pool size 64 | the harness already pins counts via `ThreadPoolBuilder::num_threads` (`bench:48-51`), but exercising sizes above hardware concurrency on a smaller box requires explicit oversubscription |

`W=32` and `W=64` should be run on a host with at least 32 physical
threads (e.g., a workstation-class Ryzen 9 / Threadripper or a 2-socket
Xeon). For machines smaller than 32 threads, the runs still produce
useful data on the *oversubscribed* regime - they will not characterise
the cross-socket regime, which must be tagged in the report.

## Instrumentation points

A run for #1679 must collect at least the following per data point.
Items 1-3 are mandatory; items 4-6 are required when item 1 indicates a
scaling cliff (per the trigger criteria in the static analysis at
`docs/audits/drain-parallel-contention-static-analysis.md:392`).

1. **Wall-clock throughput** (`elements/sec`) via criterion's
   `Throughput::Elements` (already wired at `bench:46`). Required for
   every `(variant, W, K_total, payload, pool)` cell.
2. **CPU time per element** via `taskset` plus `/usr/bin/time -v`
   wrapping a single-iteration release run. Distinguishes wall-clock
   regressions caused by lock waiting (high wall, low CPU) from those
   caused by extra work (high wall, high CPU).
3. **Lock waiting time** via `perf stat -e
   syscalls:sys_enter_futex,task-clock` over the bench process. The
   ratio `futex_enter / task_clock` is the contention indicator;
   sharded design predicts this stays below 0.001 across the grid.
4. **Per-symbol CPU breakdown** via `perf record -F 999 -g --call-graph
   dwarf` then `perf report --no-children`. The success criterion: the
   user closure (`simulate_work` at `bench:31` or its production
   analogue) dominates the top-10. If `Mutex::lock`,
   `parking_lot_core::futex_wait`, or `crossbeam_channel::*::send`
   appear above it, design B has a residual cost worth fixing.
5. **Cache-line ping-pong** via `perf c2c record / report` on the bench
   binary. This is the right tool for the question "does each shard's
   `Mutex` word stay resident in the owning worker's L1?" - the static
   analysis predicts yes in steady state, but rayon work-stealing can
   rotate workers often enough to invalidate that prediction.
6. **Allocator pressure** via `dhat` (heap profiling) on a run with
   `K_total=100_000`. The shard `Vec<R>` grows by `Vec::push` with
   doubling reallocs; `O(log2 K)` reallocs per shard at `K=K_total/W`
   is `~14` reallocs at `W=8, K=100_000`. If `dhat` shows realloc
   traffic dominates worker time, the fix is `Vec::with_capacity` at
   `drain.rs:64` rather than a primitive change. The hint requires the
   dispatcher to know `K_total` ahead of time, which would be a new
   parameter on `drain_parallel`.

Do not collect with `valgrind --tool=cachegrind` for CPU breakdown; it
serialises the rayon pool to one core and produces meaningless numbers
for a contention bench. `perf` events sampled at 999 Hz preserve the
parallel scheduling.

## Bench harness shape

The existing `drain_parallel_benchmark.rs` is the right starting point.
The runner extends it without rewriting it:

1. **Extend the `THREAD_COUNTS` constant** at
   `crates/engine/benches/drain_parallel_benchmark.rs:23` to
   `[1, 4, 8, 16, 32, 64]`. The `ThreadPoolBuilder::num_threads` call
   at `bench:48-51` already accepts arbitrary values; rayon
   oversubscribes correctly when the count exceeds available cores.
2. **Extend the `COUNTS` constant** at `bench:20` to
   `[10_000, 100_000, 1_000_000]`. The 1M case at `W=1` runs in single
   seconds on modern hardware; criterion's default sample size handles
   this without configuration.
3. **Add a second benchmark group** for `drain_parallel_into` mirroring
   `bench_drain_parallel`. The new group spawns a consumer thread that
   drains the result channel into a `Vec`, replicating the production
   layout where `DeltaConsumer` reorders results
   (`consumer.rs:128-194`). The consumer must be in-scope for the
   measurement because backpressure between the channel and the
   reorder buffer is part of what #1679 needs to characterise.
4. **Parametrise on result size**. Replace the scalar `u64` return at
   `bench:69` with an enum or two parallel functions: `_u64`
   (current) and `_payload(size: usize)` returning `Vec<u8>`. The
   payload variant exercises the `Vec::push(Vec<u8>)` path that
   matters for real `DeltaWork` results, where `R` is a
   protocol-token-sized buffer rather than a primitive.
5. **Custom pool reuse**. Move the
   `rayon::ThreadPoolBuilder::new().num_threads(threads).build()` call
   out of the inner benchmark closure (`bench:48-51` is currently
   inside the `for` loop body, but outside `b.iter`). Building the pool
   once per parameter cell avoids spurious thread-creation cost at
   higher worker counts. This is a refactor, not a behaviour change;
   the existing bench already builds outside `b.iter`.
6. **Producer affinity**. Pin the producer thread to a CPU not in the
   rayon pool (`taskset -c <last_core>` wrapping the bench binary).
   With the producer free to migrate, on `W=64` runs the producer can
   end up on the same core as a worker and skew the result toward
   producer-side stalls instead of contention.
7. **Single-Mutex baseline**. Add a third benchmark group that uses
   `Arc<Mutex<Vec<R>>>` directly (the pre-#1617 baseline reproduced in
   `docs/audits/drain-parallel-contention-static-analysis.md` Design
   A). This is needed to validate the `T_lock_contended` curve - the
   sharded design's claim is "flat past `W = physical cores`", which is
   only meaningful if the contended baseline shows the expected
   linear-or-worse curve on the same hardware.

The harness stays criterion-based. Custom thread spawning is already
handled by `rayon::ThreadPoolBuilder` plus `pool.install`; raw
`std::thread::spawn` is not needed and would lose the work-stealing
behaviour the bench is meant to measure.

## Run sequence and reporting

Per `(variant, W, K_total, payload, pool)` cell:

1. Record the `git rev-parse HEAD` of the build so reports are
   reproducible.
2. Run criterion `cargo bench -p engine --bench drain_parallel_benchmark
   -- --save-baseline drain-1679-w<W>-k<K>-<variant>`.
3. Re-run a single iteration under `perf stat -e
   syscalls:sys_enter_futex,task-clock,context-switches,cpu-migrations`
   - one futex-enter per Mutex acquire on the contended path, near zero
   on the sharded path.
4. If items 2 or 3 trigger the criteria below, run `perf record -F 999
   -g --call-graph dwarf` then `perf report` plus `perf c2c record`
   then `perf c2c report`.

Trigger criteria for the deeper profile in step 4 (lifted from the
static analysis recommendation):

- Wall-clock throughput at `W=32` worse than `0.85x` of the `W=16`
  throughput at the same `K_total` (a clear scaling cliff), or
- `futex_enter / task_clock` ratio above `0.01` at any cell.

Output format. Each run produces:

- A criterion HTML report at `target/criterion/drain_parallel/...`.
- A CSV (or JSON) summary of `wall_ns_per_elem`, `cpu_ns_per_elem`,
  `futex_per_elem`, `cache_misses_per_elem` per cell. The runner
  appends to `docs/audits/drain-parallel-1679/results.csv`.
- A short markdown report capturing host topology
  (`lscpu | head -20`), kernel version, and the rayon version pinned
  in `Cargo.toml`. Hardware shape determines whether `W=32` crosses a
  socket boundary.

The follow-up to this audit is a quantitative report consuming that
CSV. It either confirms the static prediction (sharded design flat in
`W`, single-Mutex baseline super-linear past physical-core count) and
closes #1681, or identifies the cell where the prediction fails and
hands #1681 a concrete target.

## Container and host targets

The `rsync-profile` podman container (`rust:latest`, Debian, bind-mount
of the repo) is the canonical Linux profiling environment. It already
has `perf` (`apt install linux-perf` or
`linux-tools-common linux-tools-generic` on the host kernel). For the
`W=32` and `W=64` axes, the host running the container needs at least
32 hardware threads; if the test host is smaller, mark those rows as
"oversubscribed" in the report.

For NUMA-aware runs, prefix the bench invocation with
`numactl --cpunodebind=0 --membind=0` to isolate single-socket
behaviour, then re-run unbound to capture the cross-socket cost. This
matters because the cache-line ping-pong cost on a contended `Mutex` is
two orders of magnitude higher across QPI/UPI than within a socket;
the sharded design's contention story can only be falsified at scale
on a multi-socket host.

macOS (`darwin`) and Windows are out of scope for this profile pass.
The contention model is identical (same Rust `std::sync::Mutex` over
`os_unfair_lock` / `SRWLOCK`), but `perf` is Linux-only and the runner
should not produce dual-platform numbers without first matching the
single-platform shape.

## What this audit deliberately does not do

- It does not rerun or re-derive the static analysis. That was PR
  #3699 / `docs/audits/drain-parallel-contention-static-analysis.md`.
- It does not pre-judge the outcome. The shape of the result is
  predicted; the *numbers* are unknown until the runner executes.
- It does not invoke `cargo`, `criterion`, `perf`, or modify any
  source file. The harness extensions in "Bench harness shape" are
  prescriptive for the implementer of #1682, not changes shipped here.
- It does not commit to design C (lock-free MPSC, #1681) or to a
  `Vec::with_capacity` hint at `drain.rs:64`. Those decisions wait on
  the data this strategy collects.

## References

- `crates/engine/src/concurrent_delta/work_queue/drain.rs:57-155` -
  shard and streaming `drain_parallel` implementations.
- `crates/engine/src/concurrent_delta/work_queue/{bounded.rs:48-104,
  capacity.rs:8-76, iter.rs:12-35, mod.rs:1-110}` - SPMC types,
  capacity policy, iterator, ordering contract.
- `crates/engine/src/concurrent_delta/consumer.rs:97-194` -
  `DeltaConsumer::spawn`, production caller of `drain_parallel_into`.
- `crates/engine/src/concurrent_delta/reorder.rs:30-83` -
  `ReorderBuffer` ring-buffer design and capacity bound.
- `crates/engine/benches/drain_parallel_benchmark.rs:1-89` - existing
  bench harness, the starting point for the #1679 extensions.
- `crates/transfer/src/delta_pipeline.rs:146-258` -
  `ParallelDeltaPipeline` integration site.
- `docs/audits/drain-parallel-contention-static-analysis.md` -
  companion static analysis (PR #3699).
- `docs/audits/profiling-100k-files.md` - format and methodology
  reference for prior profiling audits.
- `docs/audits/mutex-implementation-policy.md` - workspace-wide policy
  on mutex selection.
