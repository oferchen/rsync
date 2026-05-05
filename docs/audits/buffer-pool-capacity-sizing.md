# BufferPool capacity selection and sizing assumptions

Last verified: 2026-05-05 against
`crates/engine/src/local_copy/buffer_pool/{mod,pool,global,pressure,memory_cap,allocator,thread_local_cache,throughput,guard}.rs`,
`crates/engine/src/local_copy/mod.rs`,
`crates/engine/src/local_copy/context_impl/state.rs`,
`crates/engine/src/local_copy/executor/file/copy/transfer/execute.rs`,
`crates/engine/src/local_copy/executor/directory/parallel_checksum.rs`,
`crates/engine/src/lib.rs`, `crates/transfer/tests/buffer_pool_cross_crate.rs`,
and benches under `crates/engine/benches/`.

Tracking issue: #1637. Related: #1010-#1012 (global pool, completed),
#1187-#1189 (memory cap, completed), #1265 (lock-free swap, completed),
#1295 (sharded design, completed), #1297 (benchmark, pending), #1329-#1330
(`ArrayQueue` migration, completed), #1336 (split guard, completed), #1342
(`BufferAllocator` trait, completed), #1363 (dynamic capacity, completed),
#1638-#1641 (adaptive sizing, completed), #1642 (adaptive bench, pending),
#1643 (CLI/env override, completed), #2045 (io_uring + adaptive design,
in flight).

## Scope

Document where the `BufferPool` capacity numbers come from, where they hold,
where they break down, what the adaptive policy added in #1638-#1641 does
and does not solve, and what knobs are exposed to operators today via the
`OC_RSYNC_BUFFER_POOL_SIZE` and `OC_RSYNC_BUFFER_POOL_STATS` environment
variables wired in #1643. The output is a sizing reference for the pending
#1642 benchmark and a pre-flight checklist for the #2045 io_uring fixed
buffer registration design.

## 1. Current default capacity

Two distinct quantities matter here. Conflating them is the most common
mistake in tuning the pool.

- **Buffer size (per-buffer byte length)**: 128 KiB, set by the constant
  `COPY_BUFFER_SIZE = 128 * 1024` at
  `crates/engine/src/local_copy/mod.rs:165`. This is also the value of
  `ADAPTIVE_BUFFER_MEDIUM` at `crates/engine/src/local_copy/mod.rs:172`
  and is the default `buffer_size` field initialized in
  `BufferPool::new()` at `crates/engine/src/local_copy/buffer_pool/pool.rs:172`.
- **Pool capacity (number of buffers retained)**: `available_parallelism()`
  with a fallback of 4, returned by `BufferPool::default()` at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:835-840` and used by
  `GlobalBufferPoolConfig::default()` at
  `crates/engine/src/local_copy/buffer_pool/global.rs:57-71`.

Two further constants govern the queue's underlying storage and the
adaptive resizer's hard limits:

- `DEFAULT_QUEUE_CAPACITY = 256` at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:32`. The lock-free
  `crossbeam_queue::ArrayQueue` is sized to the larger of `max_buffers`
  and this constant via `queue_capacity()` at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:40-42`. The soft cap
  is enforced separately on return.
- `MAX_CAPACITY = 256` and `MIN_CAPACITY = 2` at
  `crates/engine/src/local_copy/buffer_pool/pressure.rs:53` and
  `crates/engine/src/local_copy/buffer_pool/pressure.rs:47`. These bound
  the adaptive resizer's grow and shrink decisions.

The adaptive size table is at
`crates/engine/src/local_copy/mod.rs:168-180` and dispatched by
`adaptive_buffer_size()` at `crates/engine/src/local_copy/mod.rs:203-215`:
8 KiB, 32 KiB, 128 KiB, 512 KiB, 1 MiB, switching at 64 KiB, 1 MiB, 64 MiB,
and 256 MiB file-size thresholds.

## 2. Where the value came from

The numbers are not arbitrary but they were not chosen against a
representative workload either. The trail in the comments and history is:

- `COPY_BUFFER_SIZE = 128 * 1024` predates the buffer pool and matches the
  `read`/`write` block size used historically across the local-copy
  executor. The constant doubles as `ADAPTIVE_BUFFER_MEDIUM` and is the
  bucket selected for files between 1 MiB and 64 MiB. Tests at
  `crates/engine/src/local_copy/buffer_pool/tests.rs:290-292` assert the
  invariant `ADAPTIVE_BUFFER_MEDIUM == COPY_BUFFER_SIZE` so that the
  thread-local fast path can short-circuit when an adaptive request lands
  in the medium bucket (see `acquire_adaptive_from()` at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:432-449`).
- `available_parallelism()` was introduced in PR #2979 (commit
  `dfeba4e3a`, "feat: implement global bounded buffer pool singleton")
  and motivated by the rayon thread pool topology: one buffer per worker
  thread is the saturation point for the dominant workload (one file per
  worker at a time).
- `DEFAULT_QUEUE_CAPACITY = 256` was sized to match `MAX_CAPACITY = 256`
  in the adaptive resizer landed by PR #3248 (commit `a1c4ec3cf`,
  "feat: add adaptive BufferPool resizing based on allocation pressure").
  The comment at `crates/engine/src/local_copy/buffer_pool/pool.rs:30-31`
  cites 8 MiB of pooled memory at 64 buffers x 128 KiB; the upper bound
  reaches 32 MiB at 256 buffers x 128 KiB, matching the budget cited at
  `crates/engine/src/local_copy/buffer_pool/pressure.rs:51-52`.
- The env var `OC_RSYNC_BUFFER_POOL_SIZE` and the telemetry counters
  printed via `OC_RSYNC_BUFFER_POOL_STATS=1` landed in PR #3253
  (commit `6edfd1667`, "feat: add BufferPool telemetry counters and env
  var pool sizing"). See
  `crates/engine/src/local_copy/buffer_pool/global.rs:49`,
  `crates/engine/src/local_copy/buffer_pool/global.rs:61-65`, and the
  drop-time print at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:849-861`.
- The throughput tracker came in PR #3032 (commit `9687f9434`,
  "feat: add EMA throughput tracker and dynamic buffer sizing to
  BufferPool"). It is opt-in via `with_throughput_tracking()` at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:270-273` and is not
  active by default.

## 3. Workloads it was tuned for

The defaults assume a steady-state local-copy run that looks like a rayon
parallel walk with one in-flight buffer per worker thread:

- **100k small files in parallel**: `parallel_checksum.rs:92` pulls the
  global pool once and threads `Arc<BufferPool>` into `hash_file_contents`
  at `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:142,151,153`.
  Each worker holds one buffer, returns it on file end, and the
  thread-local slot at
  `crates/engine/src/local_copy/buffer_pool/thread_local_cache.rs:24-27`
  serves the next acquire with zero synchronization. The central queue
  is touched only on the first acquire per thread.
- **1 GiB single-file local copy**: the executor at
  `crates/engine/src/local_copy/executor/file/copy/transfer/execute.rs:376-379`
  takes one buffer via `BufferPool::acquire_adaptive_from()` for the
  duration of the copy. With `available_parallelism()` >= 1, that fits
  trivially. The 1 MiB `ADAPTIVE_BUFFER_HUGE` bucket at
  `crates/engine/src/local_copy/mod.rs:175-180` halves the syscall count
  on the read/write fallback path versus `ADAPTIVE_BUFFER_LARGE`.
- **Multi-thread parallel stat / parallel checksum**: same pattern as the
  100k case. Workers process candidates sequentially and recycle a single
  buffer through the thread-local slot. The lock-free `ArrayQueue` only
  comes into play when a thread retires its slot while another thread's
  slot is full, an uncommon event in steady state.

In all three cases the buffer pool's role is amortizing the
`vec![0u8; 128 * 1024]` that `DefaultAllocator::allocate()` at
`crates/engine/src/local_copy/buffer_pool/allocator.rs:51-53` would
otherwise issue per file. The hot-path per-acquire cost in steady state
is one `RefCell` borrow plus one `set_len()` (see the unsafe block at
`crates/engine/src/local_copy/buffer_pool/pool.rs:550-556` that elides
the `resize(size, 0)` memset that profiling measured at 26 % of runtime
before #3253).

## 4. Where it underprovisions

The default capacity equals hardware parallelism, which is exactly enough
for the "one buffer per worker" assumption and nothing more. Workloads
that violate that assumption see allocator pressure that the pool cannot
absorb until the adaptive resizer reacts:

- **1M small files in parallel**: rayon spawns `available_parallelism()`
  workers but the thread-local cache is per OS thread, not per rayon
  task. Bursts of `par_iter()` work can route returns through the
  central queue. With `max_buffers = N_cpus`, every return past the
  thread-local slot lands directly at the soft cap. Subsequent acquires
  on a cold thread allocate fresh through `pop_buffer()` at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:621-641`. Miss rate
  spikes during workload acceleration until the adaptive resizer fires
  at the next 64-op boundary.
- **Sub-tasking inside a single file**: e.g. the parallel checksum
  pipeline acquiring a second buffer for verify or hash-strong rehash.
  With one slot per thread, the second acquire goes to the central
  queue or a fresh allocation. At `max_buffers = N_cpus` the central
  queue is empty in steady state, so the second-buffer path is
  fresh-allocate every time.
- **Memory-cap'd configurations with `try_acquire_from()`**: the
  non-blocking variant at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:391-422` returns
  `None` at the cap. Callers that retry without backoff (e.g. the
  io_uring submission loop in #2045) will burn CPU spinning. There is
  no test coverage today for the rate-limited acquire pattern; #1642
  should add it.

The grow path doubles capacity at most once every 64 acquires (see
`CHECK_INTERVAL = 64` at
`crates/engine/src/local_copy/buffer_pool/pressure.rs:29`). Going from 8
buffers to the 256 ceiling takes five doublings, which is 320 acquires
of accumulated pressure, plus the 64 ops between checks. On a 1M-file
workload this is invisible. On a short burst it is the entire workload.

## 5. Where it overprovisions

The other failure mode is keeping memory pinned that nothing will reuse.
The pool's idle footprint is bounded but not negligible:

- **Sequential single-threaded transfers**: a `oc-rsync src dst` invocation
  with `RAYON_NUM_THREADS=1` still picks `max_buffers = N_cpus` from
  `available_parallelism()`. On a 16-core host that is 16 x 128 KiB =
  2 MiB of central pool capacity that the workload will never fill,
  plus one TLS slot in active use.
- **Long-lived process with bursty workloads**: the soft cap is the
  retention target for the central queue, not a high-water mark. Once
  the adaptive resizer has grown the pool to a peak, the shrink path at
  `crates/engine/src/local_copy/buffer_pool/pressure.rs:155-163` only
  fires when `utilization < 30 %` and `miss_rate < 10 %` simultaneously.
  A workload that oscillates between idle and saturated misses the
  shrink window and stays at peak.
- **Daemon mode with short-lived sessions**: each session inherits the
  process-wide singleton (see
  `crates/engine/src/local_copy/context_impl/state.rs:36`), so the pool
  retains the worst-case sizing across sessions. There is no per-session
  reset.

The hard ceiling at 256 buffers x 128 KiB caps idle memory at 32 MiB,
which is safe for a server-class deployment but not negligible on a
constrained embedded target. The 4 KiB / 256 KiB clamp in
`recommended_buffer_size()` at
`crates/engine/src/local_copy/buffer_pool/pool.rs:326-339` interacts
poorly with `ADAPTIVE_BUFFER_HUGE = 1 MiB`: a throughput-tracked pool
would never recommend a 1 MiB buffer even when the file size adaptive
table would.

## 6. Adaptive grow/shrink policy from #1638-#1641

Implemented in `crates/engine/src/local_copy/buffer_pool/pressure.rs`
and wired into the acquire path via `pop_buffer()` at
`crates/engine/src/local_copy/buffer_pool/pool.rs:621-641` and
`maybe_resize()` at
`crates/engine/src/local_copy/buffer_pool/pool.rs:650-678`.

Policy summary:

- **Check cadence**: every 64 acquires
  (`CHECK_INTERVAL` at `crates/engine/src/local_copy/buffer_pool/pressure.rs:29`,
  power of two for bitwise modular check at
  `crates/engine/src/local_copy/buffer_pool/pressure.rs:107-110`).
- **Grow trigger**: `miss_rate > 20 %`
  (`MISS_RATE_GROW_THRESHOLD` at
  `crates/engine/src/local_copy/buffer_pool/pressure.rs:35`). New
  capacity is `min(current * 2, 256)` per
  `crates/engine/src/local_copy/buffer_pool/pressure.rs:136-142`.
- **Shrink trigger**: `utilization < 30 %` AND `miss_rate < 10 %`
  (`UTILIZATION_SHRINK_THRESHOLD` at
  `crates/engine/src/local_copy/buffer_pool/pressure.rs:41` combined with
  the `MISS_RATE_GROW_THRESHOLD / 2` guard at
  `crates/engine/src/local_copy/buffer_pool/pressure.rs:155-156`). New
  capacity is `max(current / 2, 2)`.
- **Shrink reclamation**: excess buffers above the new cap are popped
  and deallocated immediately at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:664-676`, decrementing
  `central_count` per reclamation.

What it solves:

- Mismatched defaults on hosts where `N_cpus` is much smaller than the
  effective parallelism (e.g. a workload that fans out via
  `rayon::scope` and exceeds `available_parallelism()` briefly).
- Long-running daemon processes whose workload mix changes shape over
  time. Periodic re-evaluation eventually converges to the new
  steady-state size.

What it does not solve:

- **Cold-start bursts**: the first 64 ops on a fresh pool are evaluated
  but cannot trigger growth because there are no prior samples; the
  resizer's first useful evaluation is at op 128.
- **Churn under uneven workloads**: a workload that alternates between
  bursts and idle in cycles shorter than `CHECK_INTERVAL` ops will not
  reach a stable size. The shrink path's dual threshold (utilization
  AND low miss rate) prevents the worst thrashing, but the pool can
  still oscillate between sizes that are both wrong for the average
  workload.
- **Per-thread starvation**: the resizer adjusts the central queue's
  soft cap. It cannot influence which thread holds a TLS slot, so a
  workload with fewer hot threads than buffers leaves the queue full
  while a few threads spin acquire-allocate-deallocate.
- **Hard memory cap interaction**: the resizer has no view of
  `MemoryCap::outstanding()`. Growing the soft cap when checked-out
  memory is already at the hard cap is a no-op against acquire
  blocking, but it does increase eventual idle memory after returns.

## 7. CLI/env override surface (#1643)

There is no `--buffer-pool-size` CLI flag today. The override surface is
two environment variables, both consumed by the engine crate without
proxying through `cli` or `core`.

- **`OC_RSYNC_BUFFER_POOL_SIZE`** at
  `crates/engine/src/local_copy/buffer_pool/global.rs:49`. Parsed at
  `GlobalBufferPoolConfig::default()` at
  `crates/engine/src/local_copy/buffer_pool/global.rs:61-65`:
  - Type: positive `usize`. Zero, negative, and non-numeric values are
    silently ignored and the pool falls back to
    `available_parallelism()`. The behaviour is fixed by tests at
    `crates/engine/src/local_copy/buffer_pool/global.rs:243-289`.
  - Default: `available_parallelism()` with a fallback of 4.
  - Bounds: lower bound 1 (zero is rejected). Upper bound is whatever
    the OS allocator can serve x the per-buffer 128 KiB cost. The
    adaptive resizer's `MAX_CAPACITY = 256` does not cap the env-var
    value because the env var sets the soft cap directly. A user who
    sets `OC_RSYNC_BUFFER_POOL_SIZE=10000` gets a pool that retains up
    to 10000 buffers (1.25 GiB at 128 KiB each), although the underlying
    `ArrayQueue` will be sized accordingly via
    `queue_capacity()` at `crates/engine/src/local_copy/buffer_pool/pool.rs:40-42`.
  - Read once at first access. Setting the variable after the singleton
    has been initialized has no effect.
- **`OC_RSYNC_BUFFER_POOL_STATS`** at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:850`. Boolean (`"1"`
  enables, anything else is off). Checked only at pool drop, prints
  `reuses`, `allocations`, `growths`, and `hit_rate` on stderr. No
  effect on pool behaviour, telemetry only.

There is no programmatic flag plumbed through `core::CoreConfig` or
`cli::Args`. The `init_global_buffer_pool()` function at
`crates/engine/src/local_copy/buffer_pool/global.rs:112-120` is exposed
publicly for embedders but the binary entry points do not call it.

## 8. Memory cap interaction (#1188)

The memory cap is a hard upper bound on outstanding (checked-out) bytes
implemented in `crates/engine/src/local_copy/buffer_pool/memory_cap.rs`.
It is opt-in via `with_memory_cap()` at
`crates/engine/src/local_copy/buffer_pool/pool.rs:253-257` and is not
configured by default. The default `BufferPool` and the global singleton
both run uncapped.

Interaction surface:

- **What the cap counts**: bytes outstanding (checked out by callers).
  Idle buffers in the central queue or in TLS slots do not count, since
  they are immediately reusable. The accounting is at
  `crates/engine/src/local_copy/buffer_pool/memory_cap.rs:14-22`.
- **Backpressure semantics**: `wait_and_reserve()` at
  `crates/engine/src/local_copy/buffer_pool/memory_cap.rs:56-109` uses a
  CAS fast path then a condvar slow path. Returners notify all waiters
  via `track_return()` at
  `crates/engine/src/local_copy/buffer_pool/memory_cap.rs:142-152`.
- **Adaptive growth interaction**: the adaptive resizer can grow the
  soft cap above what the memory cap will admit. When this happens,
  `acquire_from()` blocks at the cap regardless of the soft cap value.
  No deadlock is possible because returns are unconditional, but
  throughput collapses to the rate of returns. This is the failure
  mode that #1642 must measure.
- **`recommended_buffer_size()` clamp**: at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:326-339`, the
  recommended size is capped at `memory_cap / 4`. This protects against
  a single buffer pinning a quarter of the cap, but it also caps the
  recommendation below `ADAPTIVE_BUFFER_HUGE = 1 MiB` for any cap below
  4 MiB.

The cap and the adaptive resizer were designed independently. They
compose correctly (no shared mutable state) but not optimally (the
resizer cannot detect cap-induced misses, since cap waits do not
register as pool misses; only fresh allocations do).

## 9. Recommended sizing rules of thumb for users

Given the analysis above, here are the heuristics worth documenting in
operator-facing notes:

- **Default is correct for most local copies**. If `OC_RSYNC_BUFFER_POOL_SIZE`
  is unset and the workload is one rsync invocation per minute or less,
  the pool's idle memory is bounded by 256 x 128 KiB = 32 MiB and the
  hot path is amortized. Do not tune.
- **Override when `N_cpus` >> hot threads**. On a 64-core host running
  oc-rsync with `--no-detach` and a single-threaded workload, set
  `OC_RSYNC_BUFFER_POOL_SIZE=4` or `8` to cap idle memory.
- **Override when sub-tasking is heavy**. Workloads using
  `--checksum` plus `--whole-file` plus a deep parallel verify can hold
  more than one buffer per thread. Set
  `OC_RSYNC_BUFFER_POOL_SIZE=2*N_cpus` and verify with
  `OC_RSYNC_BUFFER_POOL_STATS=1` that `hit_rate > 95 %` and `growths == 0`.
- **Avoid setting above 256 unless you have measured the win**. The
  adaptive resizer caps growth at 256 for a reason; an env-var override
  larger than that just disables the implicit safety bound on the
  central queue.
- **Memory cap is for adversarial environments**. Containerized
  deployments with strict cgroups limits should plumb a memory cap via
  `init_global_buffer_pool()` plus a custom `with_memory_cap()` call;
  the env var alone does not expose this.
- **Use stats output before tuning**. Run with
  `OC_RSYNC_BUFFER_POOL_STATS=1` and inspect the stderr line at process
  exit. A `hit_rate > 95 %` means the default is fine; below 80 % means
  the workload is allocating fresh buffers more often than the resizer
  can react. A non-zero `growths` count means the workload exceeded the
  initial capacity at least once; if it grows every run, raise the env
  var to the steady-state size.

## 10. Open questions for #1642 benchmark

The benchmark under #1642 should answer the following, none of which are
settled by the existing micro-benchmarks at
`crates/engine/benches/buffer_pool_benchmark.rs` or
`crates/engine/benches/buffer_pool_contention.rs`:

1. What is the cold-start miss rate on a 1M-file workload at
   `available_parallelism()` capacity, and how many `CHECK_INTERVAL`
   boundaries elapse before the resizer reaches steady-state? The
   theoretical lower bound is 5 doublings x 64 ops = 320 acquires; the
   observed value depends on rayon scheduling.
2. Does the grow path ever actually fire on the dominant local-copy
   workload, or is the global singleton's startup capacity already
   sufficient? Use `total_growths()` at
   `crates/engine/src/local_copy/buffer_pool/pool.rs:779-781`.
3. Does shrink ever fire in long-running daemon mode, or does idle
   memory accrete monotonically? Add a workload alternation (saturate,
   idle, saturate) and verify the resizer reaches the shrink threshold.
4. What is the cap-blocked acquire rate when `with_memory_cap()` is set
   below `max_buffers x buffer_size`? This is not measurable from
   `total_misses()` because cap waits do not record as misses; the
   benchmark must instrument `MemoryCap::outstanding()` directly.
5. What is the measured win of the unsafe `set_len()` shortcut at
   `crates/engine/src/local_copy/buffer_pool/pool.rs:550-556` over the
   `resize(size, 0)` it replaces? The 26 % CPU figure cited in the
   comment was measured pre-#3253; revalidate on the current code path
   to justify keeping the unsafe block.
6. How does the adaptive policy interact with the throughput tracker's
   `recommended_buffer_size()` when both are enabled? The recommendation
   targets 10 ms of data per buffer
   (`TARGET_BUFFER_DURATION_SECS = 0.01` at
   `crates/engine/src/local_copy/buffer_pool/throughput.rs:48`), which
   for a 1 GiB/s sustained throughput recommends 10 MiB clamped to the
   `MAX_BUFFER_SIZE = 256 * 1024` ceiling at
   `crates/engine/src/local_copy/buffer_pool/throughput.rs:42`. The
   adaptive table for >= 256 MiB files would prefer 1 MiB buffers; the
   tracker forces 256 KiB. Which wins on real hardware?
7. How does the soft cap interact with the lock-free `ArrayQueue`'s
   fixed hard capacity when `OC_RSYNC_BUFFER_POOL_SIZE > 256`? The
   queue is sized via `queue_capacity()` at
   `crates/engine/src/local_copy/buffer_pool/pool.rs:40-42` to
   `max(max_buffers, 256)`, so a value of 10000 produces a 10000-slot
   queue and matching idle memory. Confirm via stress test that the
   admission CAS at
   `crates/engine/src/local_copy/buffer_pool/pool.rs:584-612` does not
   regress when capacity is two orders of magnitude above the default.
8. What is the right default for the pending io_uring registered-buffer
   path under #2045? The fixed-buffer registration cost amortizes only
   if the pool's churn rate is low; the benchmark should establish a
   baseline reuse rate against which #2045 can claim a win.

The answers feed back into whether the 128 KiB / `N_cpus` defaults
should change, whether `OC_RSYNC_BUFFER_POOL_SIZE` should be promoted to
a `--buffer-pool-size` CLI flag, and whether the adaptive resizer's
thresholds need workload-specific overrides.
