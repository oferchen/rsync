# BufferPool Sharding Benchmark Plan (#1297)

## Summary

Issue #1295 designs a sharded `BufferPool` layout for high thread
counts. Landing the implementation requires hard evidence that the
single-queue layout becomes a bottleneck at 32+ threads. This note
specifies the criterion benchmark that gates the sharding work.

## 1. Current `BufferPool` Implementation

Source: `crates/engine/src/local_copy/buffer_pool/`.

- `pool.rs:88-148` defines the struct. The hot fields are
  `buffers: ArrayQueue<Vec<u8>>` (central lock-free MPMC queue),
  `central_count: AtomicUsize` (soft-cap admission counter), and
  `soft_capacity: AtomicUsize` (resize target).
- `pool.rs:32-42` sizes the `ArrayQueue` to
  `max(soft_cap, DEFAULT_QUEUE_CAPACITY=256)`.
- `pool.rs:361-422` is the acquire path. Step 1 is
  `thread_local_cache::try_take()` (zero sync); step 2 is
  `ArrayQueue::pop()` plus `central_count.fetch_sub`; step 3 is
  fresh allocation.
- `pool.rs:520-573` is the return path. Step 1 is
  `thread_local_cache::try_store()`; step 2 is the
  `compare_exchange_weak` admission protocol on `central_count`
  followed by `ArrayQueue::push`.
- `thread_local_cache.rs:24-52` is the single-slot per-thread cache.
- The contention surface is the two `ArrayQueue` cursor atomics plus
  the `central_count` admission CAS, all on shared cache lines.

## 2. Sharding Design Under Test

Approach C from `buffer-pool-sharding.md`: per-thread primary shard
(rayon thread index masked to a power-of-two shard count) with a
single global `ArrayQueue` fallback.

- Acquire: thread-local slot, then `shards[idx].pop()`, then
  `fallback.pop()`, then allocate.
- Return: thread-local slot, then `shards[idx].push()`, then
  fallback admission under the existing `compare_exchange_weak` cap.
- Default `shard_count = num_cpus * 2`, clamped to `[4, 64]` and
  rounded up to a power of two so the index is a mask.
- Per-shard capacity `max(soft_cap / shard_count, 2)`.
- Activation gate `should_shard(workers) = workers >= 16`. Below the
  gate the existing single-queue path is used unchanged.

## 3. Bench Harness Plan

`crates/engine/benches/buffer_pool_sharding.rs` (new file, criterion).

- Two pool variants: `BufferPool::new(soft_cap)` baseline and the
  prototype `ShardedBufferPool::new(soft_cap, buffer_size)`.
- Thread counts: 1, 4, 16, 64. Workers spawn via `rayon::ThreadPool`
  built with `num_threads(N)`. Each worker runs a tight loop of
  `acquire -> touch first byte -> drop guard` to exercise the pool
  surface without I/O noise.
- Buffer sizes: a mixed workload draws sizes uniformly from
  `[8 KiB, 32 KiB, 128 KiB, 512 KiB, 1 MiB]` (the `ADAPTIVE_BUFFER_*`
  ladder from `mod.rs:113-121`) so the adaptive-size hot path
  (`pool.rs:432-449`) is also covered. A second arm pins the size to
  the pool default for a clean atomic-throughput measurement.
- Producer-consumer arm: half the workers acquire and ship buffers
  via `crossbeam_channel` to the other half, which drops them.
  This isolates the cross-thread asymmetry from `buffer-pool-sharding.md`
  scenario 1.
- Warm-up: 100 ms warm-up plus 1 s measurement per criterion sample,
  100 samples per data point. Throughput reported as ops/sec.
- Runner: `cargo bench -p engine --bench buffer_pool_sharding`. The
  CI hook lives in `tools/ci/run_benches.sh`; this bench is gated
  behind `OC_RSYNC_RUN_SHARDING_BENCH=1` to keep nightly time
  bounded.

## 4. Pass / Fail Criteria

The sharded layout lands only if every condition holds.

| Condition | Threshold |
|---|---|
| 1-thread overhead vs baseline | within 3% (no regression at low concurrency) |
| 4-thread throughput | within 5% of baseline (sharding inactive below 16) |
| 16-thread throughput | >= 1.5x baseline; linear scaling target |
| 64-thread throughput | >= 2x baseline; ideally 2-4x per #1295 hypothesis |
| Producer-consumer arm at 16-64 threads | >= 1.4x baseline |
| Memory ceiling growth | <= 30% over baseline at default soft cap |

Linear scaling to 16 threads means the 16-thread number is
within 20% of `4 * (4-thread number)`. If the 64-thread improvement
falls inside the 5% noise floor the hypothesis is rejected and the
implementation does not land - the design note is preserved as the
record of why.

## 5. Risks

1. **False sharing on cache lines.** A shard's `ArrayQueue` cursors
   land on the same 64-byte line as adjacent shards if the shard
   array is `Vec<ArrayQueue<...>>`. Mitigation: wrap each shard in
   `crossbeam_utils::CachePadded` so every shard occupies its own
   cache line. The bench must cover both padded and unpadded
   variants to confirm the padding pays for itself.
2. **Hot-shard imbalance.** Rayon thread indices are not uniform
   under nested scopes; a small subset of shards may absorb most
   traffic. Mitigation in the bench: report per-shard hit
   distribution (max/min/stddev across shards) alongside aggregate
   throughput. A stddev/mean ratio above 0.5 on the 64-thread arm
   counts as a failure even if aggregate throughput passes - it
   signals the index hash collapses under load.
3. **Bench noise from thermal throttling on long runs.** Pin
   criterion to 100 samples and reject runs where the standard
   deviation exceeds 10% of the mean.
4. **`current_thread_index` returning `None`.** Non-rayon test
   threads degrade to shard 0 and skew the distribution. Mitigation:
   the bench harness always runs inside `rayon::ThreadPoolBuilder`
   so every worker has a stable index.
