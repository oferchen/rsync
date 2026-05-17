# Per-Thread Buffer Slab for engine BufferPool (#1271, #1370)

## Summary

`BufferPool` today is a two-level cache: a single-slot `thread_local!`
cache (`thread_local_cache.rs`) in front of a single shared
`crossbeam_queue::ArrayQueue` (`pool.rs:103-110`). Issue #1295 proposes
sharding the shared queue into N independent `ArrayQueue` shards keyed
by thread index. Issue #1370 proposes growing the thread-local slot
into a small per-thread ring while keeping the shared queue as
overflow.

This note consolidates #1271 and #1370 into a single proposal: replace
the shared `ArrayQueue` as the *primary* storage with a per-thread
**slab** as the primary storage, and demote the shared queue to a
bounded global *overflow / balancer*. The slab is a thread-local LIFO
`Vec<Vec<u8>>` plus an aggregate byte counter; the global overflow
queue is the existing `ArrayQueue` retained at a much smaller fixed
capacity, used only to balance load across threads and to absorb the
cross-thread return problem (buffers allocated on thread A but dropped
on thread B).

The public surface (`BufferAllocator`, `BufferGuard`,
`BorrowedBufferGuard`, `PooledBuffer` Drop) does not change. The byte
budget (`byte_budget.rs`, #2245) and memory cap (`memory_cap.rs`)
continue to enforce process-wide caps. The wire protocol is untouched.

This is a paper design only. Landing is gated on profiling evidence
that the *current* (`ArrayQueue`-plus-TLS-slot) layout actually
contends at the worker counts and workloads documented in section 2,
not on the in-house benchmark alone.

## 1. Why Sharded Mutex (Or Sharded `ArrayQueue`) Is Not Enough At High Thread Counts

Issue #1295's sharding design (`docs/design/buffer-pool-sharding.md`)
proposes N per-thread-cluster `ArrayQueue` shards plus a global
fallback. Sharding is a real improvement over the single
`ArrayQueue`, but it does not eliminate cross-thread coordination:

1. **Per-shard atomics still ping-pong.** Each shard's `ArrayQueue`
   carries its own pair of `AtomicUsize` cursors. The shard is sized
   for `shard_capacity = max(soft_cap / shard_count, 2)` slots
   (`buffer-pool-sharding.md:215`). Two threads that hash to the same
   shard still race on those cursors. Index-aliasing under
   `rayon::current_thread_index()` causes this routinely once thread
   count exceeds shard count.
2. **Cross-shard pop is still a shared push to the fallback.** A
   producer-consumer asymmetry (signature builder on thread A,
   network writer on thread B) means thread B's shard fills up while
   thread A's shard empties. The overflow protocol promotes returns
   from B's saturated shard onto the global fallback, where every
   builder thread races for the fallback's two cursors plus the
   `central_count` `compare_exchange_weak` admission CAS
   (`pool.rs:790-836`).
3. **Shard count is fixed at construction.** The sharding design
   freezes `shard_count = clamp(num_cpus * 2, 4, 64)` at
   construction (`buffer-pool-sharding.md:201-209`). Daemon workloads
   that grow connection counts at runtime (one rayon pool per
   connection feeding a shared `global.rs` pool) hit shard
   underprovisioning the moment connections cross the construction-
   time worker estimate. The shard count cannot expand without
   reallocating the queue array.

The current `buffer_pool_contention.rs` bench tops out at 16 threads
(`THREAD_COUNTS = &[2, 4, 8, 16]`, line 17). The design hypothesis
that sharding pays at >= 32 threads is currently untested in-repo;
the contention bench needs an extension to 32, 64 (and ideally 128)
threads before any sharding scheme can be defended on data. The same
extension applies to the slab design proposed here. The two designs
share a test target.

For workloads where every worker holds and releases buffers in a
tight loop on its own thread (the dominant rayon fan-out pattern),
the *only* layout that takes the contention to zero on the hot path
is one where each thread's primary storage is private. That is a
per-thread slab. Sharding is a partial step in that direction
(threads cluster onto shards); the slab is the limit case (one shard
per thread).

## 2. Per-Thread Slab Design

### 2.1 Storage

```rust
struct LocalSlab {
    /// LIFO of pooled buffers. Newest entries on top so the hottest
    /// cache line wins.
    buffers: Vec<Vec<u8>>,
    /// Sum of `buf.capacity()` across `buffers`. Read on every
    /// admission to bound per-thread retention without scanning.
    retained_bytes: usize,
    /// Soft cap on this slab's `buffers.len()`. Overflow pushes the
    /// oldest entry onto the global overflow queue (FIFO eviction).
    slot_cap: usize,
    /// Soft cap on this slab's `retained_bytes`. Overflow pushes the
    /// oldest entry onto the global overflow queue.
    byte_cap: usize,
    /// Cumulative counters for telemetry.
    hits: u64,
    misses: u64,
    overflows: u64,
}

thread_local! {
    static LOCAL_SLAB: RefCell<LocalSlab> = RefCell::new(LocalSlab::new());
}
```

The slab is the **primary** storage. Acquire pops from the back of
`buffers`; return pushes to the back. Both operations are O(1),
non-atomic, single-cache-line.

Default slot cap: 8 (covers delta-apply two-buffer overlap, plus
prefetch, plus signature pipeline lookahead, plus headroom).

Default byte cap: `8 * COPY_BUFFER_SIZE = 1 MiB` per thread at the
default 128 KiB buffer size. Adjustable per-pool via
`with_per_thread_byte_cap(bytes)`.

### 2.2 Acquire Path

```text
1. LOCAL_SLAB.buffers.pop() -> Some(buf): hit, return.
2. GLOBAL_OVERFLOW.pop() -> Some(buf): rebalance hit, return.
3. allocator.allocate(buffer_size): fresh, return.
```

Step 2 is the new path. The global overflow queue is consulted only
when the thread's own slab is empty, so its cursors are touched at a
rate equal to the slab-miss rate, not the acquire rate. Under
balanced workloads (every thread allocates and releases its own
buffers), step 1 hits at >= 99% and the global queue is dead code.

### 2.3 Return Path (`PooledBuffer` Drop)

```text
1. LOCAL_SLAB.buffers.len() < slot_cap
   AND LOCAL_SLAB.retained_bytes + buf.cap() <= byte_cap:
       LOCAL_SLAB.buffers.push(buf); update retained_bytes; done.
2. GLOBAL_OVERFLOW under existing soft-cap admission
   (compare_exchange_weak on central_count, then ArrayQueue::push):
       admitted, done.
3. allocator.deallocate(buf): overflow, done.
```

Step 1 is the common case. Step 2 handles the cross-thread return
problem (section 3) and lets idle threads donate buffers to busy
threads. Step 3 is the byte-budget overflow path from #2245, kept
intact.

`PooledBuffer` Drop becomes:

```rust
impl<A: BufferAllocator> Drop for BufferGuard<A> {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            self.pool.return_buffer(buffer);
        }
    }
}
```

Unchanged from today (`guard.rs:58-64`). All slab logic lives inside
`BufferPool::return_buffer`, so guard callers see no API change.

### 2.4 Why LIFO, Not FIFO

LIFO keeps the most-recently-freed buffer on top. That buffer's
backing pages are warmest in the worker's L1/L2 (its capacity bytes
were just written by the previous transfer). LIFO is the standard
choice for free-lists in allocators (jemalloc tcache, tcmalloc
ThreadCache) for the same reason.

FIFO is used only for *eviction to overflow*: when the slab is full,
the oldest (coldest) buffer is the one shipped to the global queue.
That preserves the LIFO warmth on the hot path while keeping the
overflow protocol cheap.

## 3. Cross-Thread Return Problem

A buffer acquired on thread A and dropped on thread B is the
canonical hard case for per-thread allocators. There are two viable
strategies:

### 3.1 Global Overflow Queue (Recommended)

Retain the existing `ArrayQueue<Vec<u8>>` as the **global overflow
queue**, sized at a small fixed capacity (e.g. 64 slots). On
cross-thread drop, thread B simply pushes onto its own slab if there
is room, else onto the overflow queue, else deallocates. On acquire
miss, every thread first peeks at the overflow queue before
allocating fresh.

Pros:

- No bookkeeping of buffer provenance. The buffer is just a
  `Vec<u8>`; the slab does not care which thread originally allocated
  it.
- Reuses the existing `central_count` `compare_exchange_weak`
  admission protocol (`pool.rs:790-836`) with no changes.
- The byte budget (`byte_budget.rs`, #2245) is already a global
  counter and continues to bound total retention end-to-end.

Cons:

- Cross-thread drops touch the queue's cursors. Acceptable because
  cross-thread drops are *rare* in our workloads: rayon's
  work-stealing keeps most file-boundary work on a single worker; the
  producer-consumer asymmetry is a minority of total allocations.
- A pathological workload (every drop is cross-thread) degrades to
  the existing single-queue behaviour. This is the worst case, not
  the regression case.

### 3.2 Steal-From-Other-Thread

Each slab is registered in a `RwLock<Vec<Weak<LocalSlab>>>`. On
acquire miss, walk the registry and `try_pop` from a randomly-chosen
slab. On cross-thread drop, push to the dropping thread's own slab;
let the rebalancing happen lazily on the next acquire miss.

Pros:

- No global queue at all. Lowest possible aggregate cache-line
  contention in the balanced case.

Cons:

- Stealing requires `Mutex` or `RwLock` on the registry (or a
  hand-rolled epoch scheme), plus per-slab locking so two stealers
  do not race. This reintroduces the very contention the slab was
  meant to eliminate, just at a different layer.
- Slab teardown on thread exit becomes complex: the registry must
  prune dead entries without blocking active stealers. `Weak<T>` plus
  `Arc<Mutex<LocalSlab>>` per slab works, but adds two atomic ops
  per access.
- Debugging is harder: a buffer can sit indefinitely in a dead
  thread's slab until the next steal sweep walks the registry. The
  steal sweep is amortized but adds tail latency.

**Recommendation: 3.1.** The global overflow queue is the simpler
design, reuses existing admission code, and degrades to today's
behaviour in the worst case rather than to something worse.

## 4. Bounded Total Memory

Two budgets bound aggregate memory:

- **Per-thread byte cap** (`LocalSlab::byte_cap`): bounds each
  thread's slab retention. Default `8 * COPY_BUFFER_SIZE = 1 MiB`.
- **Global byte budget** (`byte_budget.rs`, #2245): bounds the
  global overflow queue's retention, exactly as today.

End-to-end ceiling at N threads, default config:

```text
N * 1 MiB         (slabs)
  + 64 * 128 KiB  (global overflow, fixed-cap)
  + memory_cap    (outstanding / checked-out, unchanged)
```

At N = 16: 16 + 8 = 24 MiB of pooled bytes. At N = 64: 64 + 8 = 72
MiB. The latter is larger than today's 32 MiB ceiling
(`buffer-pool-sharding.md:300`) but is bounded and configurable. The
trade is *aggregate idle memory vs hot-path contention*, exactly as
in jemalloc and tcmalloc.

Returning buffers above the per-thread cap is the cross-thread donor
path: the buffer enters the global queue (step 2 of section 2.3),
where another thread can claim it. The byte budget's overflow counter
(`byte_budget.rs:39-41`) increments whenever the global queue also
rejects. This is the existing failure mode from #2245 and is already
covered by `pool.rs:790-836`.

## 5. API Surface (Unchanged)

| Type | Today | After |
|---|---|---|
| `BufferPool::new(max)` | Pool with `max` central slots | Pool with `max` global-overflow slots; per-thread slabs created lazily |
| `BufferPool::acquire`, `acquire_from`, `try_acquire`, `try_acquire_from` | Returns `BufferGuard` / `BorrowedBufferGuard` | Same signatures |
| `BufferGuard` / `BorrowedBufferGuard` Drop | Calls `pool.return_buffer(buf)` | Same |
| `BufferAllocator` trait | Stable | Stable |
| `with_memory_cap`, `with_byte_budget`, `with_throughput_tracking`, `with_adaptive_resizing`, `with_buffer_controller` | Builder chain | Same |
| `BufferPoolStats` | Existing counters | New optional `slab_hits`, `overflow_hits`, `cross_thread_returns` fields; `None` on legacy pools, `Some` on slab pools |
| New: `with_per_thread_slab(slot_cap, byte_cap)` | n/a | Builder that switches the pool to slab mode |

Callers of `pool.acquire(...)` are unaware of the change. The slab
lives inside `pool.rs` and `thread_local_cache.rs`; everything else
calls the existing public methods.

## 6. Failure Modes

### 6.1 Thread Teardown

When a thread exits, its `LOCAL_SLAB` destructor fires (Rust's
`thread_local!` runs destructors at thread exit). The destructor
drains `buffers` into the global overflow queue if there is room,
else deallocates each buffer through the pool's allocator. Idle
buffers from short-lived rayon threads end up in the global queue
for reuse by surviving threads.

The destructor must not panic. Implementation: a `try_*` push loop
that ignores `ArrayQueue::push` errors and deallocates on failure;
no `unwrap`/`expect`.

### 6.2 Panicking Thread

Rust runs `thread_local!` destructors during panic unwind. The slab
destructor still fires and drains buffers via `try_*` push. There is
no `Mutex` to poison and no shared state that can be left in an
inconsistent state: the global queue is lock-free and the byte budget
uses atomic CAS.

The one risk is panicking during the buffer's own `Drop` (e.g. a
panic raised by the allocator's `deallocate`). Mitigation: the
allocator contract already forbids panics in `deallocate`
(`allocator.rs`), and `DefaultAllocator` simply drops the `Vec`,
which cannot panic.

### 6.3 Very-Long-Lived Buffers

A buffer pinned to thread A's slab can outlive useful reuse: thread
A goes idle while thread B repeatedly allocates fresh. Three
mitigations:

1. **Periodic donation.** Every Mth return (M = 64), the slab pops
   its *bottom* (oldest) entry and pushes to the global overflow
   queue. Cheap, amortized, breaks the pinning without adding
   complexity to the hot path.
2. **Idle-time drain.** If a slab has not seen an acquire in W
   seconds (W = 5), the next return triggers a full drain to the
   global overflow queue. Requires a per-slab `Instant`, one
   `Instant::now()` per return. Costs ~10 ns. Optional, behind an
   env flag.
3. **Bounded byte cap.** The per-thread byte cap (section 4) already
   bounds the damage: a slab cannot pin more than `byte_cap` bytes
   regardless of behaviour. Even with no donation, the worst case is
   `N * byte_cap` of pinned memory.

The recommended starting point is (1) only. (2) and (3) are
escape hatches if profiling shows (1) is insufficient.

## 7. Comparison Table

| Property | Today: single `ArrayQueue` + TLS slot | #1295: sharded `ArrayQueue` + TLS slot | This: per-thread slab + global overflow |
|---|---|---|---|
| Hot-path sync per acquire (cache hit) | 0 atomic (TLS slot hits) | 0 atomic (TLS slot hits) | 0 atomic (TLS slot hits, but slot is depth-8) |
| Hot-path sync per acquire (cache miss, balanced workload) | 2 atomics (queue cursors) + 1 CAS (central_count) | 2 atomics on the shard's cursors | 0 atomic; pop from local Vec |
| Hot-path sync per acquire (cross-thread donor) | Same as cache miss | Same as cache miss + cross-shard pop | 2 atomics on global overflow queue (1 CAS + cursor) |
| Memory ceiling at default config, N=16 | 32 MiB | 32 + 8 = 40 MiB | 16 + 8 = 24 MiB |
| Memory ceiling at default config, N=64 | 32 MiB | 32 + 32 = 64 MiB | 64 + 8 = 72 MiB |
| Adaptive resize | Yes (`pressure.rs`) | Yes (only on the fallback) | Yes (only on the global overflow) |
| Byte-budget cap (#2245) | Yes | Yes | Yes (global) + per-thread byte cap |
| Cross-thread return handling | Native (every return goes to the queue) | Native via fallback | Global overflow queue (rare path) |
| Implementation complexity | Baseline | +1 indirection, +shard array | +1 indirection, +per-thread slab struct |
| Code paths to test | Existing | Existing + per-shard | Existing + per-thread + cross-thread donation |
| Activation gating | None | `should_shard(workers) >= 16` | `with_per_thread_slab(...)` opt-in |

## 8. Trigger Conditions for Adoption

The slab lands only if **all** of the following are observed in
profiling on production-representative workloads, not just in the
contention bench:

1. **Sustained worker concurrency > 32 threads** on the same
   `BufferPool` instance. Daemon workloads with thread-per-connection
   sharing `global.rs` are the canonical case.
2. **`buffer_pool_contention.rs` extended to 32, 64, 128 threads
   shows >= 1.5x acquire-latency win for the slab vs sharded layout
   at 64 threads.** The bench must include a producer-consumer arm to
   stress cross-thread drops (existing harness, lines 26-33, only
   exercises same-thread acquire-and-release).
3. **p99 lock-wait or contention time on the existing `ArrayQueue`
   cursors exceeds 5 ms per acquire** under steady-state daemon
   load, measured via `perf c2c` (Linux) or `Instruments`
   (macOS / Counters).
4. **Hit rate from `BufferPoolStats::hit_rate()` exceeds 80% in
   steady state.** A low hit rate means the bottleneck is allocation
   rate, not cache-line contention; the slab does not address that
   case.
5. **Sharding (#1295) has been implemented or rejected.** The slab
   is strictly more invasive than sharding; if sharding suffices,
   the slab is unnecessary. If sharding lands and still fails the
   above thresholds at higher thread counts, the slab is the
   documented next step.

If **any** of (1) through (5) is not satisfied, the slab does not
land. The design note remains as the documented next-step plan.

## 9. Recommendation

**Defer until profiling proves need.** The existing two-level layout
(TLS slot + lock-free `ArrayQueue` + admission CAS) is already
contention-free for the dominant rayon-fan-out workload at the worker
counts the codebase tests against (4-16 in
`buffer_pool_contention.rs`). The two competing improvements
(#1295 sharding, this slab) both target the same hypothesis: that
contention becomes measurable above 32 threads.

The right order of work:

1. Extend `buffer_pool_contention.rs` to 32, 64, 128 threads and add
   a producer-consumer arm.
2. Run the extended bench against the current single-queue layout
   and decide whether *either* improvement is justified.
3. If yes: implement sharding first (#1295). It is the smaller
   change, preserves more of the existing code, and addresses the
   common case (balanced fan-out).
4. If sharding still fails at >= 64 threads on production workloads:
   implement the slab as described here.

The slab is the limit case of sharding (one shard per thread). It
is unambiguously stronger on the balanced hot path and unambiguously
weaker on memory ceiling at high thread counts. Both designs need
the same benchmark evidence to justify landing; the slab needs
strictly more evidence to justify its larger memory footprint.

## 10. Five-Step Implementation Sequencing (If Adopted)

1. **Extend the contention bench.** Add `THREAD_COUNTS = &[2, 4, 8,
   16, 32, 64, 128]` and a producer-consumer arm in
   `buffer_pool_contention.rs`. Run on a >= 32-core box. Land the
   bench separately from any pool changes so the baseline numbers are
   reproducible.
2. **Refactor `thread_local_cache.rs`.** Replace the single-slot
   `Option<Vec<u8>>` with the `LocalSlab` struct from section 2.1.
   Keep the existing `try_take` / `try_store` shape; the surface is
   `try_take()` (pops from slab) and `try_store(buf)` (pushes to
   slab, returning `Some(buf)` if full). Add a Drop hook for thread
   teardown that drains to the (still-existing) global queue. Keep
   `pool.rs` unchanged at this step - the slab is still acting as a
   cache.
3. **Promote the slab to primary storage.** Restructure
   `BufferPool::return_buffer` and the acquire fast paths so the slab
   is checked first, then the global overflow queue, then allocation.
   The global queue's capacity is reduced to a small fixed size
   (default 64) since it is now a balancer, not the primary store.
   Add `with_per_thread_slab(slot_cap, byte_cap)` builder and switch
   the global pool over behind a config flag.
4. **Add donation and metrics.** Implement the every-Mth-return
   donation hook (section 6.3 mitigation 1). Add `slab_hits`,
   `overflow_hits`, `cross_thread_returns` to `BufferPoolStats`. Wire
   them into the existing `OC_RSYNC_BUFFER_POOL_STATS=1` drop-time
   summary (`pool.rs:1107-1119`).
5. **Gate landing on the bench thresholds in section 8.** Run the
   extended contention bench against both the old layout and the new
   slab layout at every thread count. Reject if any threshold fails.
   Land behind a feature flag (`slab_buffer_pool`) for one release
   cycle so it can be disabled if production daemon traffic shows a
   regression on memory or tail latency.

## 11. References

- `crates/engine/src/local_copy/buffer_pool/pool.rs:103-110,488-1083`
- `crates/engine/src/local_copy/buffer_pool/thread_local_cache.rs:24-52`
- `crates/engine/src/local_copy/buffer_pool/byte_budget.rs:34-119` (#2245)
- `crates/engine/src/local_copy/buffer_pool/guard.rs:36-122`
- `crates/engine/src/local_copy/buffer_pool/memory_cap.rs`
- `crates/engine/src/local_copy/buffer_pool/global.rs:24-58`
- `crates/engine/benches/buffer_pool_contention.rs:14-219`
- `crates/engine/benches/buffer_pool_benchmark.rs`
- `docs/design/buffer-pool-sharding.md` (#1295)
- `docs/design/buffer-pool-sharding-bench.md` (#1297)
- `docs/design/per-thread-buffer-pools.md` (#1370 - cache-in-front-of-queue variant)
- Issue #1271 (this proposal: slab as primary storage)
- Issue #1370 (this proposal: per-thread slab consolidated)
- Issue #2245 (byte-budget cap on pool retention)
