# Per-Thread Buffer Pools (#1370)

## Summary

`BufferPool` today is a two-level structure: a single-slot `thread_local!`
cache in front of a shared `crossbeam_queue::ArrayQueue` (#1329). The
fast path is contention-free for the one-buffer-per-worker case, but
multi-buffer workers and overflow returns still touch the shared queue's
producer/consumer atomics. This note proposes promoting the thread-local
slot into a small per-thread cache (a fixed-depth ring or short `Vec`)
so most acquire/return cycles stay entirely on the worker's own cache
line, with the existing shared `ArrayQueue` retained as the overflow
backstop. The goal is to drive shared-queue traffic toward zero in the
common rayon-fan-out workload without changing `BufferAllocator` or
`BufferGuard` semantics.

This is a paper design only; landing is gated on the contention
benchmark in `crates/engine/benches/buffer_pool_contention.rs` showing a
measurable win at 32+ workers. It supersedes the alternative sharded
central queue layout in #1271 (`docs/design/buffer-pool-sharding.md`),
which is retained as a fallback if per-thread caching proves unfit for
sustained overflow regimes.

## Current State

Source citations against `crates/engine/src/local_copy/buffer_pool/`:

- `pool.rs:50-64` documents the two-level model and the
  `DEFAULT_QUEUE_CAPACITY = 256` shared `ArrayQueue` (#1329).
- `pool.rs:459` is `acquire`; it calls `thread_local_cache::try_take()`
  before falling back to the shared queue.
- `pool.rs:520-573` is `return_buffer`; it tries `try_store()` first,
  then routes overflow to the shared queue.
- `thread_local_cache.rs:24-52` is the single-slot cache:
  `thread_local! { static LOCAL_BUF: RefCell<Option<Vec<u8>>> }` with
  `try_take()` / `try_store()`.

The single slot saturates as soon as a worker holds two buffers
concurrently (delta apply pipeline, signature-plus-basis I/O), forcing
the second acquire onto the shared queue.

## Design

Replace the single-slot `Option<Vec<u8>>` with a fixed-depth per-thread
cache:

```rust
const PER_THREAD_DEPTH: usize = 4;

thread_local! {
    static LOCAL_BUFS: RefCell<ArrayVec<Vec<u8>, PER_THREAD_DEPTH>>
        = const { RefCell::new(ArrayVec::new_const()) };
}
```

Acquire pops from the back (LIFO so the hottest cache line wins);
return pushes to the back. Both operations are O(1), zero-sync, and
touch only thread-private memory. Capacity-4 is chosen to cover the
delta-apply two-buffer case plus headroom for one prefetched basis
chunk and one returning chunk in flight, without inflating idle RSS
(four 128 KiB buffers per worker = 512 KiB peak per thread).

Public surface (`BufferAllocator`, `BorrowedBufferGuard`) is unchanged.
Only `thread_local_cache.rs` and the `acquire`/`return_buffer` paths in
`pool.rs` change.

## Trade-Off: Zero Contention vs Idle Memory

Per-thread caches eliminate cross-thread coordination on the hot path,
so acquire and return cost a single non-atomic `RefCell` borrow. The
cost is idle memory: in steady state each worker may park up to four
buffers, regardless of whether the fan-out has narrowed. With the
default 128 KiB block size and a 16-worker rayon pool that is 8 MiB
worst-case versus the current ~2 MiB ceiling. To bound this, the cache
opportunistically drains its tail entry to the shared `ArrayQueue` on
every Nth return (N tunable, default 16) so other workers can reclaim
under uneven load. A `Drop` hook on thread exit returns the residual
buffers to the shared pool, keeping long-running daemon RSS bounded.

## Overflow to Shared Pool

The existing `ArrayQueue` remains the overflow target for both
directions:

- **Acquire miss.** When `LOCAL_BUFS` is empty, fall through to
  `pop_buffer()` exactly as today (`pool.rs:520`). No code change in
  the cold path.
- **Return overflow.** When `LOCAL_BUFS` is full at `PER_THREAD_DEPTH`,
  push the buffer through `try_push` on the shared queue. If the queue
  is also full (above soft cap), drop the buffer and decrement
  `central_count` per current logic.

This preserves the soft-cap admission guarantee in `pool.rs:99` and the
hard-cap behaviour where over-cap buffers are deallocated rather than
queued. The shared pool continues to act as the cross-thread balancer
under load skew.

## Alternative Considered

The sharded central queue (#1271,
`docs/design/buffer-pool-sharding.md`) keeps the single-slot
thread-local cache and instead splits the shared `ArrayQueue` into N
independent shards keyed by thread id. That removes contention on the
central atomics but still walks off-thread on every overflow. Per-thread
caching short-circuits the overflow itself and is strictly cheaper on
the fast path, at the cost of higher idle memory. If profiling on real
daemon traffic shows the per-thread idle RSS is unacceptable, the
sharded layout in #1271 is the documented fallback.

## Implementation Steps

1. Replace the slot in `thread_local_cache.rs` with a depth-4
   `ArrayVec`; keep `try_take`/`try_store` signatures.
2. Add a `try_drain_one()` helper for the periodic drain to shared pool.
3. Wire the drain counter into `return_buffer` in `pool.rs:520-573`.
4. Add a `Drop` hook on thread exit to return residual buffers.
5. Re-run `crates/engine/benches/buffer_pool_contention.rs` at 4, 16,
   32, and 64 workers; gate landing on >= 20 % acquire-latency win at
   32 workers without RSS regression beyond the documented bound.

## References

- `crates/engine/src/local_copy/buffer_pool/pool.rs:50-64,459,520-573`
- `crates/engine/src/local_copy/buffer_pool/thread_local_cache.rs:24-52`
- `docs/design/buffer-pool-sharding.md` (#1271 alternative)
- Issue #1329 (current shared `ArrayQueue` design)
- Issue #1370 (this proposal)
