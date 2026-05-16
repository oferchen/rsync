# `BufferPool` current state - sharding-benchmark precondition (#1297)

Tracking task: #1297. Companion plan:
`docs/audits/bufferpool-sharded-benchmark-plan.md`. Related tickets:
#1295 (sharded design note), #1370, #1271, #1681 (gated refactor
followups). This document records the state of the production
`BufferPool` as of the date of this audit and explains why the
originally-scoped sharding microbenchmark cannot be authored against
the assumed `Mutex<Vec<Vec<u8>>>` baseline.

Last verified: 2026-05-16 against
`crates/engine/src/local_copy/buffer_pool/{mod,pool,thread_local_cache,
allocator,memory_cap,buffer_controller,pressure,byte_budget,guard,
global}.rs` and the existing benchmark
`crates/engine/benches/buffer_pool_contention.rs`.

## TL;DR

The production `BufferPool` does not use `Mutex<Vec<Vec<u8>>>` on
the buffer storage hot path. It uses a two-level lock-free
architecture: a `thread_local!` single-slot cache in front of a
`crossbeam_queue::ArrayQueue<Vec<u8>>` central queue with an atomic
soft-cap admission counter. The mutex-vs-sharded comparison the
#1297 task originally framed has no `Mutex` baseline to compare
against in the current tree.

The remaining mutexes in this module are unrelated to buffer
storage: `MemoryCap` uses a `Mutex<()>` only as the condvar's
companion lock (backpressure wakeups, not buffer access), and
`AdaptiveBufferController` uses `Mutex<ControllerState>` to guard
PID state updated on coarse throughput sampling intervals, not on
acquire/release.

## Current architecture

### Storage

```text
acquire() / acquire_from():
  thread_local_cache::try_take()       // zero-sync, ~2 ns
  -> ArrayQueue::pop()                 // wait-free CAS
  -> allocator.allocate()              // fresh allocation

return_buffer():
  thread_local_cache::try_store()      // zero-sync if slot empty
  -> admit_or_deallocate()             // CAS on central_count
       -> ArrayQueue::push()           // wait-free CAS
       -> allocator.deallocate()       // soft cap reached
```

### Fields on the storage hot path

From `pool.rs`:

```rust
pub struct BufferPool<A: BufferAllocator = DefaultAllocator> {
    buffers: ArrayQueue<Vec<u8>>,
    central_count: AtomicUsize,
    soft_capacity: AtomicUsize,
    buffer_size: usize,
    allocator: A,
    memory_cap: Option<MemoryCap>,
    byte_budget: Option<ByteBudget>,
    throughput: Option<ThroughputTracker>,
    pressure: Option<PressureTracker>,
    buffer_controller: Option<AdaptiveBufferController>,
    total_hits: AtomicU64,
    total_misses: AtomicU64,
    total_growths: AtomicU64,
}
```

No `Mutex<Vec<...>>`, no `RwLock<Vec<...>>`. Storage is the
`ArrayQueue` and the per-thread slot in `thread_local_cache.rs`.

### Thread-local fast path

`crates/engine/src/local_copy/buffer_pool/thread_local_cache.rs`:

```rust
thread_local! {
    static LOCAL_BUF: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}
```

Single-slot per thread, no synchronization, fully portable across
Linux, macOS, and Windows. Acquire and return check this first and
only fall through to the central queue on miss/overflow.

### Soft-cap admission

`admit_or_deallocate` uses `compare_exchange_weak` on
`central_count` to reserve a slot strictly below `soft_capacity`
before pushing to the queue. The queue's hard capacity is sized to
`max(soft_cap, DEFAULT_QUEUE_CAPACITY)` so a successful CAS
guarantees the subsequent `push` succeeds. Racing returners
serialize purely through the atomic CAS - no kernel-managed lock.

## Why the originally-scoped bench cannot be built

The #1297 task brief assumed the baseline path was
`Mutex<Vec<Vec<u8>>>::lock().pop() / push()`. That layout was
replaced before this audit; see PRs #1338, #1640, #1641, #1329 in
the bufferpool-sharded-benchmark-plan history. Building a
`single_mutex_vs_sharded` Criterion bench against the current tree
would compare:

1. Current production path: TLS slot, then `ArrayQueue` (wait-free,
   no syscall) under a CAS admission counter.
2. A sharded prototype: N `Mutex<Vec<Vec<u8>>>` shards keyed by
   `thread_index() % N` (kernel locks).

The sharded prototype would be slower than the production code at
every measured shard count and thread count, because kernel mutex
contention strictly dominates a wait-free CAS once the TLS slot
misses. The bench would produce evidence only for a refactor
nobody is proposing, and would suggest the wrong action (revert
the lock-free queue to add shards).

The relevant remaining question is whether the lock-free central
queue itself becomes a contention point at very high thread counts
(64+) when the TLS slot misses, and whether sharding the
`ArrayQueue` would help. That is what
`docs/audits/bufferpool-sharded-benchmark-plan.md` already
specifies, and what the existing
`crates/engine/benches/buffer_pool_contention.rs` already
measures for the non-sharded baseline.

## What is already measured

`crates/engine/benches/buffer_pool_contention.rs` (in this tree)
already provides:

- Single-threaded acquire/release baseline.
- Multi-threaded contention at 2, 4, 8, 16 threads with constant
  total work via `rayon::ThreadPoolBuilder::num_threads`.
- Hit-vs-miss telemetry via `BufferPool::total_hits` and
  `BufferPool::total_misses`.
- Stat-workload pattern - many short borrows at the 4 KB buffer
  size.

Coverage gaps relative to the #1297 brief:

- No 1-thread or 64-thread point (current range is 2-16).
- No 4 KB / 64 KB payload sweep (uses the pool's default
  `COPY_BUFFER_SIZE` plus a single 4 KB stat variant).
- No sharded variant to compare against.

## Recommended next steps (out of scope for this audit)

1. Extend `buffer_pool_contention.rs` thread-count axis to include
   1 and 64, and parametrize payload size at 4 KB / 64 KB. This is
   a pure additive change, no new code path.
2. If contention at 64 threads is measurable (target: >5% of
   wall-clock from queue contention via `perf stat` cache-miss or
   atomic-contention counters), then author the sharded
   `ArrayQueue` comparison bench - sharding the
   `crossbeam_queue::ArrayQueue`, not a `Mutex<Vec<Vec<u8>>>`.
3. If contention is not measurable, close #1297 / #1295 / #1370 /
   #1271 / #1681 as not-needed and reference this audit.

These are tracked under the follow-up ticket batch and are not part
of #1297's deliverable.

## File and line references

- `crates/engine/src/local_copy/buffer_pool/pool.rs:110` - `buffers: ArrayQueue<Vec<u8>>`
- `crates/engine/src/local_copy/buffer_pool/pool.rs:118` - `central_count: AtomicUsize`
- `crates/engine/src/local_copy/buffer_pool/pool.rs:736` - `return_buffer` (TLS-first, then CAS-admission)
- `crates/engine/src/local_copy/buffer_pool/pool.rs:790` - `admit_or_deallocate` (CAS on central_count)
- `crates/engine/src/local_copy/buffer_pool/thread_local_cache.rs:24` - per-thread `RefCell<Option<Vec<u8>>>`
- `crates/engine/src/local_copy/buffer_pool/memory_cap.rs:19` - `Mutex<()>` (condvar partner, not storage)
- `crates/engine/src/local_copy/buffer_pool/buffer_controller.rs:183` - `Mutex<ControllerState>` (PID state, not storage)
- `crates/engine/benches/buffer_pool_contention.rs` - existing contention bench
- `docs/audits/bufferpool-sharded-benchmark-plan.md` - prior plan, gates the sharded refactor
