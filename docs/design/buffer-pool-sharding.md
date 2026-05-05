# Sharded BufferPool Layout for High Thread Counts (#1295)

## Summary

The current `BufferPool` is a two-level design: a single-slot
thread-local cache in front of one global lock-free
`crossbeam_queue::ArrayQueue`. The thread-local fast path absorbs the
common one-buffer-per-rayon-worker pattern, and the shared queue handles
overflow. At rayon worker counts of roughly 16 or fewer, this design is
effectively contention-free because the thread-local slot is hit for
nearly every acquire and return.

At 32 or more concurrent producers and consumers - for example a
64-core stat-walk feeding parallel signature generation, or a daemon
that reuses one pool across many concurrent connection threads - the
thread-local cache stops absorbing every acquire. When it misses, every
worker contends on the same `ArrayQueue` head and tail counters. The
hardware cost of that contention is cache-line ping-pong on the two
atomics that bound the ring's producer and consumer cursors.

This note designs an internal sharding layer that sits between the
thread-local cache and the lock-free queue. Sharding is selected at
pool construction time and only kicks in for pools created with a
worker count that exceeds the activation threshold; below the
threshold, a single `ArrayQueue` continues to win on memory and code
clarity. The public `BufferAllocator` trait and the `BufferGuard`
RAII pattern do not change. There is no wire-protocol impact.

The follow-up benchmark (#1297) is the gating signal for actually
landing the implementation. This design is the prerequisite plan.

## Current State

Source citations against `crates/engine/src/local_copy/buffer_pool/`:

- `mod.rs:24-43` documents the two-level model: thread-local
  single-slot cache in front of a `crossbeam_queue::ArrayQueue`.
- `pool.rs:9-13` imports `crossbeam_queue::ArrayQueue` and the
  `AtomicU64` / `AtomicUsize` counters used for soft-cap admission.
- `pool.rs:88-148` is the `BufferPool` struct. `buffers: ArrayQueue<Vec<u8>>`
  is the central queue; `central_count: AtomicUsize` is the admission
  counter; `soft_capacity: AtomicUsize` is the resize-target.
- `pool.rs:32-42` defines `DEFAULT_QUEUE_CAPACITY = 256` and the
  `queue_capacity()` helper that picks the larger of soft cap and
  default. At 128 KB per buffer this caps the central queue at 32 MiB
  of pooled memory before fresh allocation.
- `pool.rs:361-422` is `acquire_from` / `try_acquire_from`. They check
  the thread-local slot first via `thread_local_cache::try_take()`,
  fall through to `pop_buffer()`, which pops from the queue.
- `pool.rs:520-573` is `return_buffer`. It tries
  `thread_local_cache::try_store()` first, and on slot-occupied falls
  through to `admit_or_deallocate()` which owns the
  `compare_exchange_weak` admission protocol against `central_count`.
- `pool.rs:584-612` is the admission state machine: a CAS loop on
  `central_count` followed by `ArrayQueue::push`. Both the atomic and
  the queue's internal head index live on hot cache lines.
- `pool.rs:621-641` is `pop_buffer`: `ArrayQueue::pop()` then
  `central_count.fetch_sub(1, Relaxed)` plus pressure stats.
- `thread_local_cache.rs:24-52` implements the fast path: a
  `thread_local!` `RefCell<Option<Vec<u8>>>` slot per thread, with
  `try_take` / `try_store` returning `Option<Vec<u8>>`.
- `pressure.rs:24-78` defines the adaptive resize policy: 64-op
  evaluation interval, 20% miss rate triggers grow, 30% utilization
  triggers shrink, capacity bounds 2..256.
- `allocator.rs:25-54` is the `BufferAllocator` trait. The trait is
  the public abstraction we must keep stable.

The previous PR thread for this area (#1338, #1640, #1641, #1329) drove
the move from `Mutex<Vec<Vec<u8>>>` to `ArrayQueue` plus adaptive
resize. The contention regime that motivated #1329 was *mutex
acquisition* under heavy concurrency. That problem is solved. The
remaining concern is purely cache-line residency on the two atomic
cursors of the lock-free queue when the thread-local fast path misses.

## Contention Hypothesis

`crossbeam_queue::ArrayQueue` is a bounded MPMC ring. Internally it
holds a `Box<[Slot<T>]>` and two `AtomicUsize` cursors (`head` and
`tail`). Each `Slot<T>` carries its own per-slot stamp. Producers
`fetch_add(1)` on `tail`; consumers `fetch_add(1)` on `head`. The two
cursors share the cache line - the same allocation in the
`ArrayQueue` struct. Under coordinated bursts of pushes and pops the
two cursors get hammered by every active worker.

The thread-local cache (`thread_local_cache.rs:24-52`) absorbs this in
the steady state because each rayon worker is doing
`acquire/use/return` in a tight loop and finds its own slot
populated. There are three workloads where the thread-local slot
*structurally* misses:

1. **Producer-consumer asymmetry across threads.** A signature-builder
   worker checks out a buffer, fills it, and hands it via channel to a
   network writer thread that drops the guard. The writer's
   thread-local slot fills with buffers it never produced; the
   builder's slot stays empty. Both threads must touch the central
   queue every operation. With 32 builder threads and 32 writer
   threads this is 64 concurrent ring operations per file boundary.

2. **One-shot bursts at start-of-transfer.** When 64 rayon workers
   simultaneously call `acquire_from` for the first time, all 64 hit
   the empty thread-local cache and then race on the queue head. The
   first 64 pops serialize through the head cursor.

3. **Daemon thread-per-connection with shared pool.** The
   `global_buffer_pool` (see `global.rs`) is shared across all daemon
   connections. With 100 concurrent connections each transferring
   files, every connection's worker pool is racing for the same set of
   queue cursors.

The cache-line ping-pong cost is real but bounded: each contended
`fetch_add` on a shared atomic is ~30-100 ns on contemporary x86 and
ARM under heavy contention, vs ~3-5 ns uncontended. At 64 threads the
geometric throughput ceiling for any single shared atomic counter is
on the order of 10-20 M ops/s, which is plausible to hit with
ten-thousand-files-per-second stat workloads.

## Sharding Approaches Considered

### Approach A: per-thread shard keyed by rayon thread index

`rayon::current_thread_index()` returns `Option<usize>` for threads in
the current thread pool. We hash that index modulo `N` and use it to
pick a shard. Each shard is its own `ArrayQueue<Vec<u8>>`.

Pros: zero coordination on push (each shard sees a single producer).

Cons:

- Producer and consumer can be different threads (the
  builder/writer asymmetry above), so shards still need cross-shard
  steal on miss.
- `current_thread_index()` returns `None` for non-rayon threads
  (daemon listener, signal handler, panic-spawned helpers). The non-
  rayon path needs a fallback.
- Within a nested `rayon::scope`, the index numbering is local to that
  inner pool. A buffer admitted on outer index 5 can be popped on
  inner index 5 of a different inner scope - same shard, different
  thread, still contended.

### Approach B: per-CPU shard via `core_affinity` or `sched_getcpu`

Linux exposes `sched_getcpu()` cheaply (`vDSO` on most kernels).
Sharding by CPU number gives us NUMA locality automatically: a buffer
returned from CPU 7 stays on CPU 7's shard, which is on the same NUMA
node as CPU 7's L3 slice on most server hardware.

Pros: best cache locality. Aligns with `rayon`'s default policy of
work-stealing between cores.

Cons:

- Linux-only without a wrapper. macOS does not expose stable per-CPU
  affinity. Windows has `GetCurrentProcessorNumber` but the value is
  approximate. Cross-platform parity matters for this code.
- Cross-shard steal on miss has the same complexity as Approach A.
- Threads migrate between cores. A buffer admitted on CPU 7 may be
  consumed from CPU 3 a microsecond later. The shard mapping is
  inherently fuzzy.

### Approach C: two-level shard-then-fallback (recommended)

Each thread maps deterministically to a *primary* shard. On acquire,
the primary shard is tried first; on miss, a single global fallback
queue is tried; on miss again, allocate fresh. Symmetric on return.

This mirrors the partitioning strategy used by tcmalloc and jemalloc:
a thread-local cache, a per-CPU central cache, and a global heap. We
already have the thread-local layer; the question is what sits between
it and the global queue.

Pros:

- Producer and consumer asymmetry is naturally handled: a buffer
  admitted to shard 5 by the builder is popped from shard 5 by the
  builder again most of the time, because rayon's work-stealing keeps
  most file-boundary work on the same worker. When the consumer is on
  a different shard, it falls through to the global fallback.
- The global fallback is touched only when *every* primary shard has
  underflowed for that consumer, which under typical workloads is
  rare. Fallback cache-line contention is therefore bounded.
- Implementation is cleanly layered. The shard array is a private
  field; the global fallback is the existing `ArrayQueue`.

Cons:

- Memory overhead: each shard owns a small `ArrayQueue`. With shard
  count `N`, we need `N * shard_capacity` slots in the rings plus the
  global fallback's slots. At default `N = 2 * num_cpus` and
  `shard_capacity = 4` buffers, an 8-core machine pays for 64 buffer
  slots in shards plus the existing 256 global slots. Slot overhead
  is the `MaybeUninit<Vec<u8>>` plus a `u32` stamp = ~32 bytes per
  slot, so the per-shard tax is ~2 KiB on an 8-core machine. Tiny.

### Recommendation

Approach C. It is the only option that survives all three workloads
without per-platform plumbing or an unbounded steal protocol.

## Recommended Design (Approach C)

### Shard count

`shard_count = num_cpus * 2`, bounded above by 64 and below by 4.

The factor-of-two oversharding is a standard heuristic for hash-based
partitioning under non-uniform thread distributions. Below 4 shards
the variance from thread index aliasing dominates; above 64 the slot
overhead grows without measurable contention reduction.

The shard count is set once at `BufferPool` construction. It is
*never* recomputed at runtime. Each shard is fixed-size and the
global fallback is the existing `ArrayQueue` at the existing capacity.

### Shard capacity per shard

`shard_capacity = max(soft_capacity / shard_count, 2)`.

The shard is intentionally small because we expect underflow to be
common; the global fallback exists precisely to absorb that.
Small shards also keep wake-from-deallocation latency tight: a shard
that filled to capacity from a burst evacuates promptly when other
shards drain.

### Mapping function

```rust
fn shard_index(&self) -> usize {
    let raw = rayon::current_thread_index()
        .or_else(|| std::thread::current().id().as_u64().map(|n| n.get() as usize))
        .unwrap_or(0);
    raw & (self.shards.len() - 1)
}
```

The shard count is rounded up to a power of two so the modulus is a
mask. `current_thread_index` is the primary source because rayon
threads dominate the workload; non-rayon callers (daemon listener,
test threads) fall back to the OS thread id, which is a stable
process-lifetime number on all three platforms.

### Acquire path

```text
1. thread_local_cache::try_take() -> Some(buf): return.
2. self.shards[idx].pop() -> Some(buf): return.
3. self.fallback.pop() -> Some(buf): return.
4. allocator.allocate(buffer_size): return fresh.
```

Step 2 is the new step. Hits at step 2 do not touch the global
fallback or any cross-thread cache line - the shard is owned by a
single thread cluster with much lower contention than the existing
shared queue.

### Return path

```text
1. thread_local_cache::try_store(buf) -> stored, done.
2. self.shards[idx].push(buf) -> Ok(()): admitted, done.
3. self.fallback.push(buf) under the existing soft-cap
   `compare_exchange_weak` admission protocol: admitted or
   deallocated.
```

The soft-cap admission protocol stays attached to the *fallback*
queue, not the shards. Per-shard admission would need per-shard
counters and per-shard CAS loops, which reintroduces atomic
contention. Keeping the cap on the fallback is fine because the
shards are individually small (`shard_capacity` slots each), so the
total memory ceiling is `(shard_count * shard_capacity) +
soft_capacity`, which we expose via `max_buffers()` for telemetry.

### Activation threshold

```rust
fn should_shard(num_threads: usize) -> bool {
    num_threads >= 16
}
```

Below 16 rayon workers the existing single-queue layout wins on
memory and on code clarity. The construction call site is:

```rust
let workers = rayon::current_num_threads();
let pool = if should_shard(workers) {
    ShardedBufferPool::new(soft_cap, buffer_size)
} else {
    BufferPool::new(soft_cap)
};
```

The decision is frozen at construction. There is no runtime switch
because a hot-path branch on the strategy would defeat the point.

## Memory Cost

| Layout | Per-pool memory at default config |
|---|---|
| Today: `BufferPool` with 256 slots at 128 KB | 32 MiB ceiling |
| Sharded: 16 shards * 4 slots + 256 fallback slots at 128 KB | 32 MiB + 8 MiB = 40 MiB ceiling |
| Sharded: 64 shards * 4 slots + 256 fallback slots at 128 KB | 32 MiB + 32 MiB = 64 MiB ceiling |

The slot allocations themselves are cheap (the buffer `Vec<u8>` only
materializes on push; an empty slot is a `MaybeUninit` plus a stamp).
The "ceiling" line above is the worst case where every slot is
holding a maximally sized buffer. In practice the adaptive resizer
(`pressure.rs:24-78`) keeps the fallback near demand, and the shards
are bounded by `shard_capacity = 4`, so steady-state memory tracks
the existing pool plus a small constant.

## API Impact

### `BufferAllocator` (#1342)

No change. `allocate(size)` and `deallocate(buf)` are the only entry
points; sharding is internal to the pool. The trait remains the
dependency-inversion boundary that lets callers swap allocation
strategies without recompiling the pool.

### `BufferPool::acquire`, `acquire_from`, `try_acquire`,
`try_acquire_from`

No signature change. The four constructors stay
`new`, `with_buffer_size`, `with_allocator`, `with_memory_cap`. We add
one new constructor `with_sharding(num_threads: usize)` that selects
the sharded layout when `should_shard(num_threads)` returns true and
otherwise falls back to the existing single-queue path.

### Telemetry

`BufferPoolStats` (re-exported from `pool.rs:108`) gains two optional
fields:

- `shard_hits: Option<u64>` - hits at step 2 of the acquire path.
- `shard_overflows: Option<u64>` - returns that fell through to the
  fallback.

`None` for non-sharded pools, populated for sharded pools. Existing
counters (`total_hits`, `total_misses`, `total_growths`) are
unchanged.

## Wire-Compat

Zero impact. The `BufferPool` is internal to the local-copy and
delta-transfer paths. It does not appear in any wire format, capability
string, or persisted state. Sharding is invisible to the upstream
rsync 3.4.1 protocol.

This is documented for completeness because the project's commit
discipline requires every internal change to declare its protocol
position explicitly.

## Benchmark Plan Binding (#1297)

The follow-up benchmark task is the gating signal. It must measure
both the single-queue baseline and the sharded prototype on:

1. **Parallel stat at 100K+ files.** A directory tree with 100 K
   small files exercises the start-of-transfer burst and the
   producer-consumer asymmetry between stat-walk threads and
   per-file signature workers.
2. **Parallel signature generation on multi-GB files.** Several
   workers chunk the same large file and produce buffers that drop
   on the network-write side. This is the cleanest stress test for
   buffer asymmetry.
3. **Daemon thread-per-connection.** A daemon serving 100
   concurrent rsync clients, each pulling files. This exercises
   the global-pool sharing pattern (`global.rs`) and is the most
   sensitive workload to fallback-queue contention.

Expected speedup band, conservatively:

- 1-16 threads: negligible (< 5%) - sharding does not activate.
- 32 threads: 1.5-2x at the buffer-pool layer; less at the end-to-end
  layer because the pool is one component among many.
- 64 threads: 2-4x at the pool layer if the contention hypothesis
  holds; benchmark-noise-floor (within 5%) if it does not.

If the 64-thread number is in the noise, the implementation does not
land. The hypothesis must survive the benchmark.

## Risks

1. **Thread index instability under nested rayon scopes.** A
   `rayon::scope` spawned inside a worker has its own pool with
   different indices. A buffer admitted at outer index 5 may be
   consumed by inner index 5, which is a different thread. Mitigation:
   the shard count is small enough (factor-of-two oversharding) that
   index aliasing produces the same shard often enough to amortize
   admission cost; cross-shard misses fall through to the fallback,
   which is the worst-case existing behaviour.

2. **Shard skew under uneven worker activity.** A daemon connection
   that exits leaves its shard fully populated while no one consumes
   from it. The fallback absorbs this on the consumer side, but the
   shard's slots stay reserved. Mitigation: the adaptive resizer
   should periodically drain shards above the average occupancy and
   migrate the buffers to the fallback. Implementation detail for the
   follow-up work; not in this design.

3. **Debug instrumentation gap.** The existing tests in `tests.rs`
   inspect `pool.available()` and `pool.max_buffers()`, both of
   which need to aggregate across shards. The aggregation must be a
   snapshot read of every shard's `len()` plus the fallback's
   `len()`. This is a `O(shard_count)` operation but is only called
   from telemetry and tests, never the hot path.

4. **Memory cap interaction.** `MemoryCap` (`memory_cap.rs`) tracks
   outstanding bytes globally. Sharding does not change the cap
   semantics because outstanding bytes is the count of buffers
   *checked out by callers*, which is shard-independent. The
   `wait_and_reserve_memory` and `track_return` calls already happen
   at the pool boundary, not at the queue level.

5. **`zero-capacity` and `try_*` paths.** The existing return path
   has a zero-capacity short-circuit (`pool.rs:539-544`) that
   deallocates instead of admitting. Under sharding the same path
   must still apply, and the shard push must precede the fallback
   admission; the zero-capacity check happens once at the top.

## Tracking (Follow-Up Work)

These follow-ups exist in the project tracker; they are not added by
this design note, only listed for navigation:

- Implementation TODO: build the sharded pool behind the
  `should_shard` gate, mirror the existing test suite in
  `tests.rs`.
- Benchmark TODO (#1297): the gating measurement. Three workloads
  above; speedup band as stated.
- Profiling TODO: capture `perf c2c` (Linux) and `vmstat -h` (macOS)
  output before and after to demonstrate cache-line contention
  reduction in the contention-line histogram.
- Telemetry TODO: surface per-shard hit and overflow counters in
  `BufferPoolStats` and in the existing structured-logging hook
  (`global.rs`).

## Decision

Land the design note now. Implementation waits on benchmark
evidence from #1297. The trait surface is preserved. The wire
protocol is untouched. If #1297 shows the hypothesis is wrong,
this note documents *why* sharding is unnecessary - which is also
a valuable artifact.
