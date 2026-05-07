# BufferPool capacity selection and sizing assumptions

Tracking issue: #1637. Related, completed: #1010-#1012 (global pool),
#1187-#1189 (memory cap), #1265 (lock-free swap), #1295 (sharded
design), #1329-#1330 (`ArrayQueue` migration), #1336 (split guard),
#1342 (`BufferAllocator` trait), #1363 (dynamic capacity),
#1638-#1641 (adaptive sizing), #1643 (env-var override). In flight:
#1297 (benchmark), #1642 (adaptive bench), #2045 (io_uring + adaptive
design).

This audit documents the current `BufferPool` capacity selection logic,
the sizing assumptions baked into the defaults, the trade-offs they
encode, and a short list of improvements worth pursuing. The pool was
originally a single file at `crates/engine/src/local_copy/buffer_pool.rs`
but was decomposed into a module under
`crates/engine/src/local_copy/buffer_pool/` (mod.rs, pool.rs, global.rs,
pressure.rs, memory_cap.rs, allocator.rs, thread_local_cache.rs,
throughput.rs, guard.rs). All file references below point at the
current module layout.

## 1. BufferPool implementation

The pool is a two-level cache designed to amortize the cost of
allocating the per-file copy buffer used by the local-copy executor and
the parallel checksum walker.

- Module entry point and public re-exports:
  `crates/engine/src/local_copy/buffer_pool/mod.rs:94-108`.
- Core `BufferPool<A>` struct with the lock-free
  `crossbeam_queue::ArrayQueue` central queue, atomic admission counter,
  soft-capacity field, optional memory cap, optional pressure tracker,
  optional throughput tracker, and telemetry counters:
  `crates/engine/src/local_copy/buffer_pool/pool.rs:87-148`.
- Thread-local single-slot fast path keyed on `RefCell<Option<Vec<u8>>>`:
  `crates/engine/src/local_copy/buffer_pool/thread_local_cache.rs`.
- Acquire hot path checks the thread-local slot, then pops from the
  central queue, then falls through to a fresh allocation:
  `crates/engine/src/local_copy/buffer_pool/pool.rs:361-422`.
- Return hot path tries the thread-local slot first, then admits to the
  central queue under a soft-cap CAS, then deallocates:
  `crates/engine/src/local_copy/buffer_pool/pool.rs:534-612`.
- Process-wide singleton with lazy init:
  `crates/engine/src/local_copy/buffer_pool/global.rs:30-130`.
- Adaptive resizer driven by hit/miss rates:
  `crates/engine/src/local_copy/buffer_pool/pressure.rs`.
- Hard memory cap with backpressure (CAS fast path, condvar slow path):
  `crates/engine/src/local_copy/buffer_pool/memory_cap.rs`.

Buffer ownership is exclusively via RAII guards
(`BufferGuard` / `BorrowedBufferGuard`) returned by `acquire_from`,
`acquire_adaptive_from`, and `acquire`. The `Drop` impls call
`return_buffer` to push the buffer back into the pool exactly once,
even on panic unwind.

## 2. Default capacity, per-buffer size, total memory cap

Two distinct quantities drive sizing. They are independently
configurable and conflating them is the most common tuning mistake.

### Per-buffer size

- `COPY_BUFFER_SIZE = 128 * 1024` at
  `crates/engine/src/local_copy/mod.rs:165`. This is the default
  `buffer_size` field set by `BufferPool::new()` at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:172`.
- Adaptive table at `crates/engine/src/local_copy/mod.rs:168-180` and
  selector at `crates/engine/src/local_copy/mod.rs:203-215`: 8 KiB,
  32 KiB, 128 KiB, 512 KiB, 1 MiB at file-size thresholds 64 KiB,
  1 MiB, 64 MiB, 256 MiB.
- The medium adaptive bucket equals `COPY_BUFFER_SIZE`, so an adaptive
  acquire for files between 1 MiB and 64 MiB takes the fast path.
  `acquire_adaptive_from` at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:432-449`.

### Pool capacity (number of buffers retained in the central queue)

- Default soft cap is `available_parallelism()`, falling back to 4:
  `crates/engine/src/local_copy/buffer_pool/pool.rs:835-840` and the
  global config `Default` at
  `crates/engine/src/local_copy/buffer_pool/global.rs:58-79`.
- Underlying `ArrayQueue` is sized to the larger of `max_buffers` and
  `DEFAULT_QUEUE_CAPACITY = 256`:
  `crates/engine/src/local_copy/buffer_pool/pool.rs:32`,
  `crates/engine/src/local_copy/buffer_pool/pool.rs:40-42`.
- Adaptive resizer bounds the soft cap to `[2, 256]`:
  `MIN_CAPACITY` and `MAX_CAPACITY` at
  `crates/engine/src/local_copy/buffer_pool/pressure.rs:47,53`.
- The thread-local cache is one extra slot per OS thread, not counted
  against the soft cap:
  `crates/engine/src/local_copy/buffer_pool/thread_local_cache.rs`.

### Total memory cap

- No hard memory cap by default. Opt-in via `with_memory_cap()` at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:253-257`.
- When set, accounts for outstanding (checked-out) bytes only; idle
  buffers do not count:
  `crates/engine/src/local_copy/buffer_pool/memory_cap.rs:14-22`.
- Implicit ceiling on idle memory at default settings: the soft cap can
  grow to 256 buffers, so 256 x 128 KiB = 32 MiB of pooled memory. The
  comment at `crates/engine/src/local_copy/buffer_pool/pressure.rs:50-53`
  states this budget explicitly.
- Operator override of pool count is via `OC_RSYNC_BUFFER_POOL_SIZE`
  env var (#1643): `crates/engine/src/local_copy/buffer_pool/global.rs:56`,
  parsed at `crates/engine/src/local_copy/buffer_pool/global.rs:64-72`.

## 3. Sizing assumptions

The defaults assume the dominant local-copy workload: a rayon parallel
walk where each worker holds one buffer for the duration of one file,
returns it on file end, and immediately re-acquires for the next file.

- **Target concurrency**: equal to `available_parallelism()`. Each rayon
  worker is expected to hold at most one buffer at a time. The
  thread-local cache absorbs the steady-state acquire/return traffic at
  zero synchronization cost. The central queue is touched only on the
  first acquire per OS thread and on cross-thread imbalance.
- **Per-thread reuse**: the TLS slot is single-occupancy. The first
  acquire per thread comes from the central queue (or fresh
  allocation); subsequent acquire/return pairs cycle through the TLS
  slot. This matches the saturation point of the parallel checksum
  walker
  (`crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:142,151,153`)
  and the file-copy executor
  (`crates/engine/src/local_copy/executor/file/copy/transfer/execute.rs:376-379`).
- **Memory budget**: the pool retains at most `soft_capacity` buffers
  in the central queue. With `soft_capacity = N_cpus` and 128 KiB
  buffers, idle memory is bounded by `N_cpus * 128 KiB`. With the
  adaptive ceiling at 256, the worst-case retained memory is 32 MiB
  regardless of host parallelism.
- **No hard memory cap by default**. The cap is an embedder-only knob
  intended for memory-constrained deployments. The default daemon and
  CLI entry points run uncapped.
- **Burst tolerance**: the underlying `ArrayQueue` is pre-allocated to
  `max(max_buffers, 256)` slots
  (`crates/engine/src/local_copy/buffer_pool/pool.rs:32-42`). This
  headroom allows the adaptive resizer to grow the soft cap up to 256
  without reallocating the queue.

## 4. Trade-offs

The defaults trade off allocation pressure against pinned idle memory.

- **Smaller pool (lower `max_buffers`)**:
  - Less idle memory. A pool of 4 buffers caps idle memory at 512 KiB.
  - More misses under burst. Sub-tasking patterns where a single thread
    holds two buffers simultaneously bypass the TLS slot and either pop
    the central queue or allocate fresh.
  - Higher allocator and zero-init pressure when the central queue
    drains. Profiling against the pre-pool implementation showed
    `vec![0; 128 * 1024]` consuming approximately 26 % of CPU; that is
    the cost the pool is amortizing.
- **Larger pool (higher `max_buffers`)**:
  - More memory pinned even when idle. A 64-core host with the
    adaptive cap maxed reaches 32 MiB of central-queue memory plus one
    TLS slot per active OS thread.
  - Diminishing reuse benefit past `N_cpus + sub-task fanout`. Buffers
    above that threshold sit idle indefinitely until the shrink path
    fires (`utilization < 30 %` and `miss_rate < 10 %`,
    `crates/engine/src/local_copy/buffer_pool/pressure.rs:155-163`).
  - Fixed-buffer io_uring registration (planned under #2045) cost
    scales with pool size; an oversized pool extends warm-up time.
- **Single shared global pool vs per-subsystem pools**:
  - The global singleton (#1010-#1012) gives one shared memory budget
    across receiver, generator, parallel checksum, and disk commit.
    Per-subsystem pools would each independently size to
    `available_parallelism()`, multiplying retained memory.
  - The drawback is no per-subsystem accounting; telemetry counters are
    aggregated process-wide.
- **Adaptive resizer cadence vs reactivity**:
  - `CHECK_INTERVAL = 64`
    (`crates/engine/src/local_copy/buffer_pool/pressure.rs:29`)
    amortizes the resize evaluation. A smaller interval reacts faster
    to bursts but costs more atomic load/swap operations on the hot
    path.
  - The current threshold combination (grow at 20 % miss, shrink at
    30 % utilization plus 10 % miss) tolerates oscillation but
    converges slowly for cold-start bursts (cannot trigger growth until
    at least 128 acquires have elapsed).

## 5. Proposed improvements

### (a) Workload-derived sizing at startup

Replace the bare `available_parallelism()` default with a heuristic
that factors in the expected workload shape. The CLI entry point
already knows the recurse depth, the parallel-stat threshold, and
whether `--checksum` or parallel verify is enabled; that information
should seed the initial soft cap.

- A single-file `oc-rsync src dst` invocation should start at 2 buffers
  (one in flight, one warm). The current default of `N_cpus` over-
  provisions for non-parallel workloads.
- A recursive copy with parallel checksum should start at
  `min(N_cpus, expected_files / 4)`, clamped to `[2, 32]`.
- Plumb the heuristic through `core::CoreConfig` so the embedder can
  override it.

### (b) Promote `OC_RSYNC_BUFFER_POOL_SIZE` to a CLI flag

The env var landed in #1643 and exposes the soft cap directly with
documented bounds and silent rejection of invalid values
(`crates/engine/src/local_copy/buffer_pool/global.rs:64-72`,
tests at `crates/engine/src/local_copy/buffer_pool/global.rs:243-289`).
Operators have to remember an env var name and set it before
invocation. A `--buffer-pool-size N` CLI flag with the same semantics,
validated by clap and threaded through `core::CoreConfig`, would make
the knob discoverable via `--help` and consistent with other tuning
flags. Also expose `--buffer-pool-stats` to flip the telemetry print
path on demand.

### (c) Instrumentation hooks

The pool already exposes telemetry via `BufferPoolStats` (returned by
`stats()` at `crates/engine/src/local_copy/buffer_pool/pool.rs:791-797`)
and a stderr summary printed on drop when
`OC_RSYNC_BUFFER_POOL_STATS=1`. This is one-shot and only useful for
post-hoc tuning. Add:

- A periodic sampler that emits `total_hits`, `total_misses`,
  `total_growths`, and `available()` to the logging crate at the
  `debug` level, gated by a config flag. Useful for long-running
  daemons.
- An internal metrics export (Prometheus-shaped counters) when the
  daemon is started with a metrics endpoint. Counters already exist;
  only the exporter wiring is missing.
- Track `central_count` peak and TLS-slot hit ratio explicitly to
  distinguish "TLS absorbed it" from "central queue absorbed it" in
  the hit count.

### (d) Drain on idle

Add a periodic drain task that shrinks the central queue when the
process has been idle for longer than a threshold (e.g. 60 seconds).
The adaptive shrink path requires both `utilization < 30 %` and
`miss_rate < 10 %` measured over `CHECK_INTERVAL` ops; a long-idle
process never accumulates new operations and so never re-evaluates.
The result is monotonic memory growth in daemon mode after the first
busy burst. A wall-clock-driven shrink complements the op-count-driven
shrink and addresses the failure mode where the singleton is shared
across short-lived sessions
(`crates/engine/src/local_copy/context_impl/state.rs:36`).

### (e) Adaptive ceiling tied to memory cap

When `with_memory_cap()` is configured, the adaptive resizer's
`MAX_CAPACITY = 256` ceiling is irrelevant; the binding constraint is
`memory_cap / buffer_size`. Wire the resizer to read the cap and clamp
its grow target to the cap-implied maximum so that the soft cap never
exceeds what the memory budget can actually admit. This avoids the
failure mode where the resizer grows the soft cap past the cap and
subsequent acquires block on `wait_and_reserve()`
(`crates/engine/src/local_copy/buffer_pool/memory_cap.rs:56-109`)
without the resizer noticing.

## Summary

The default sizing (one buffer per hardware thread, 128 KiB each, soft
cap bounded to `[2, 256]` by the adaptive resizer, 32 MiB worst-case
idle footprint) is correct for the dominant local-copy workload and
defensible for daemon mode. The known gaps are cold-start
underprovisioning, oversizing on high-core hosts running
single-threaded copies, and the absence of CLI plumbing for the env-var
override. The five improvements above are sequenced from highest
leverage (workload-derived sizing) to most narrowly scoped (memory-cap-
aware adaptive ceiling) and can be pursued independently.
