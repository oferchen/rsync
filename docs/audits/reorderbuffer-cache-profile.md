# ReorderBuffer Cache Behavior Profile at 1M Files

Tracks issue #1854. Builds on the structural alternative evaluated in #1853
(VecDeque ring buffer vs BTreeMap). This audit specifies the measurement
plan; numbers land in a follow-up once the harness runs.

## 1. Current Implementation

`BoundedReorderBuffer<T>` lives in `crates/transfer/src/reorder_buffer.rs`
and is consumed by `crates/transfer/src/delta_pipeline.rs`. Storage is a
`BTreeMap<u64, T>` keyed by sequence number, with a `next_expected` cursor
and a `window_size` cap enforcing back-pressure on the producer.

```rust
pub struct BoundedReorderBuffer<T> {
    pending: BTreeMap<u64, T>,
    next_expected: u64,
    window_size: u64,
    // ...
}
```

Hot path on every dispatch: `pending.insert(seq, item)` then a drain loop
calling `pending.remove(&next_expected)` until the cursor stalls.

## 2. Profile Plan at 1M Files

Driver: a delta-dispatch microbenchmark feeding 1M sequence numbers through
`BoundedReorderBuffer::insert` with a payload sized to match `OpToken`.
Run under Linux (rsync-profile container) on a stable-frequency core; pin
with `taskset -c 2`.

- `perf stat -e cache-misses,cache-references,L1-dcache-load-misses,LLC-load-misses,cycles,instructions ./bench` over the full 1M dispatch.
- `valgrind --tool=cachegrind --cachegrind-out-file=cg.out ./bench` then `cg_annotate cg.out` to attribute D1/LLd misses to `BTreeMap::{insert,remove}` vs the ring-buffer index ops.
- Repeat both runs against the VecDeque prototype from #1853 with identical input traces. Capture three runs per variant; report median.

## 3. Hypothesis

`BTreeMap` allocates B-tree nodes from the global allocator; nodes scatter
across the heap, so every `remove(&next_expected)` chases pointers into
cold cache lines. Expected D1 miss rate ~30% on the drain loop, dominating
cycles once the working set exceeds L2.

A `VecDeque<Option<T>>` ring buffer indexed by `seq % capacity` keeps the
window contiguous: drains touch adjacent slots, so D1 miss rate should fall
under 5%, with prefetcher-friendly stride-1 access.

## 4. Workload

1,000,000 ops. 75% arrive in order (`seq == next_expected`); 25% arrive
out-of-order with a uniform gap of 1-100 ahead of `next_expected`, then
the missing slot is filled on a later step so the cursor advances.
`window_size = 256` to mirror the production back-pressure target. Payload
is a 32-byte struct standing in for `OpToken` to keep node-vs-slot size
parity. Seed the RNG (`StdRng::seed_from_u64(0xCA11_CACE)`) for repeatable
trace files committed alongside the bench.

## 5. Pass Criteria

The ring-buffer variant must beat the `BTreeMap` baseline by at least
2x on `cycles` from `perf stat`, with `cache-misses / cache-references`
also dropping by 2x or more. Either condition failing means cache-miss
pressure is not the dominant cost on this workload and the swap is
unjustified; record the result and close #1854 without changing the
implementation. Both conditions met clears the way for a follow-up PR
replacing the storage backend behind the existing public API.
