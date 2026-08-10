# `Arc<Mutex<Vec<...>>>` contention under parallel stat: static analysis

Tracking issue: oc-rsync task #1192 (profile `Arc<Mutex<Vec>>` under
100K+ file parallel stat). Related: #1191 (replaced one `Mutex<Vec>`
with a lock-free `crossbeam_queue::ArrayQueue`, done), #1269 (buffer
pool profiled, done), #1615-#1620 (`drain_parallel` per-rayon-thread
sharding, done), and the just-merged static analysis of the drain-side
path in PR #3699
(`docs/audits/drain-parallel-contention-static-analysis.md:14-89`).

Last verified: 2026-05-05 against
`crates/transfer/src/parallel_io.rs:11-125`,
`crates/transfer/src/generator/file_list/{batch_stat.rs:38-51,walk.rs:262-267}`,
`crates/transfer/src/receiver/transfer/{candidates.rs:117-132,pipeline.rs:160-201}`,
`crates/transfer/src/receiver/directory/{creation.rs:118-128,deletion.rs:95-105}`,
`crates/engine/src/concurrent_delta/work_queue/drain.rs:14-155`,
`crates/engine/src/local_copy/buffer_pool/{mod,pool}.rs:1-50`,
`crates/engine/benches/{drain_parallel_benchmark,buffer_pool_benchmark}.rs:1-100`,
`crates/rsync_io/src/ssh/aux_channel.rs:39-247`, and
`crates/core/src/signal/cleanup.rs:280-326`.

## Summary

#1192 asks whether the parallel stat hot path used by the receiver
(`candidates.rs:117-132`) and the sender walker (`walk.rs:262-267`)
suffers from `Arc<Mutex<Vec<...>>>` contention at 100K+ files. Static
analysis says no: the path does not allocate any `Arc<Mutex<Vec<R>>>`
accumulator. It uses `map_blocking` (`parallel_io.rs:107-125`), which
delegates to rayon's `into_par_iter().map().collect()`. Rayon's
collect owns per-task buffers internally; user code never holds a
shared mutex-guarded `Vec`. The "shared collector" pattern #1192 was
filed to characterise lives in `drain.rs`, sharded already and
audited in PR #3699.

Three `Arc<Mutex<Vec<...>>>` sites remain in production outside
benches and tests:

1. `crates/rsync_io/src/ssh/aux_channel.rs:99,148` - SSH stderr drain,
   single producer thread, occasional reader.
2. `crates/engine/src/concurrent_delta/work_queue/drain.rs:63-64` -
   per-rayon-thread `Vec<Mutex<Vec<R>>>` shards (sharded under #1617 /
   #1680, audited in PR #3699).
3. None on the parallel stat path itself.

The only `Arc<Mutex<Vec>>` ever created on a 100K-file rayon hot path
was the buffer pool central queue prior to #1191. That site is now
`crossbeam_queue::ArrayQueue` (`pool.rs:12,22-30`); the
`Mutex<Vec<Vec<u8>>>` survives as a benchmark baseline at
`buffer_pool_benchmark.rs:30,38`. The static answer to "does parallel
stat hit a contended `Arc<Mutex<Vec>>` at 100K files?" is "no": the
pattern was profiled and removed under #1191 / #1269; the
`drain_parallel` path was profiled and sharded under #1615-#1620 and
audited in PR #3699; parallel stat itself never allocated such a
primitive.

This audit is read-only. It does not substitute for runtime profiling
tracked in #1192; it extracts the load-bearing facts profiling will
quantify.

## Methodology

Static-only. No `cargo` invoked (project rule "never run cargo
locally").

1. Workspace ripgrep for `Arc<Mutex<Vec` and `Arc::new(Mutex::new(Vec`
   over `crates/**/*.rs`. Hits classified production / integration
   test / unit test / bench. Test sites recorded but excluded from
   contention analysis.
2. Per production hit: trace producers and consumers. Frequency class
   per PR #3699
   (`drain-parallel-contention-static-analysis.md:50-58`): `H`, `B`,
   `S`, `R`.
3. Read the parallel stat path top-down from
   `parallel_io.rs::map_blocking`,
   `batch_stat.rs::batch_stat_dir_entries`,
   `build_files_to_transfer`, the signature batch loop in
   `pipeline.rs`, and directory creation/deletion modules.
4. Cross-check against the sharded pattern at `drain.rs:62-89`,
   already analysed in PR #3699; cite rather than restate.
5. Project shape at `100K` / `1M` using `C_lock_sharded` /
   `C_lock_single` from PR #3699
   (`drain-parallel-contention-static-analysis.md:218-247`).

## Inventory

### Production source

Every `Arc<Mutex<Vec<...>>>` or `Arc::new(Mutex::new(Vec::new()))`
that survives in non-test, non-bench source:

| # | Site | File:line | Lifetime | Frequency | Hot path? |
|---|------|-----------|----------|-----------|-----------|
| 1 | SSH stderr drain (pipe) | `crates/rsync_io/src/ssh/aux_channel.rs:99` | per `SshConnection` while child runs | `H` for drain thread, `R` for `collected()` callers | no - one writer per child, no rayon |
| 2 | SSH stderr drain (socketpair) | `crates/rsync_io/src/ssh/aux_channel.rs:148` | as above (`#[cfg(unix)]`) | `H` drain thread, `R` callers | no - same shape as site 1 |
| 3 | `drain_parallel` shards | `crates/engine/src/concurrent_delta/work_queue/drain.rs:63-64` | one `rayon::scope` | `H` per result, `S` at merge | yes, sharded - PR #3699 |

No production `Arc<Mutex<Vec<...>>>` sites exist on the parallel stat
path (`parallel_io.rs:107-125`, `batch_stat.rs:38-51`,
`candidates.rs:117-132`). Load-bearing static finding for #1192.

### Test and bench source - excluded from contention scope

Recorded for inventory completeness; not on any 100K-file hot path:

| Site | File:line | Purpose |
|------|-----------|---------|
| Reader recorder buffers | `crates/transfer/src/reader/tests.rs:583,611,633,668,693,702,753,798,834,846,894,935` | unit-test write recording |
| Writer recorder buffers | `crates/transfer/src/writer/tests.rs:532,557,608,638,667,698,724` | unit-test write recording |
| Multiplex reader handler | `crates/protocol/src/multiplex/reader.rs:339` (`#[cfg(test)]` mod at `:239-240`) | MUX frame capture |
| Keepalive integration | `crates/protocol/tests/keepalive.rs:182,215,447` | message capture |
| Timeout integration | `crates/protocol/tests/timeout_handling.rs:559,608` | message capture |
| Work queue tests | `crates/engine/src/concurrent_delta/work_queue/tests.rs:660` | baseline vs. shard variant |
| Bench baseline pool | `crates/engine/benches/buffer_pool_benchmark.rs:30,38` | `Mutex<Vec<Vec<u8>>>` baseline for #1269 comparison |
| Cleanup ordering | `crates/core/src/signal/cleanup.rs:304` (`#[cfg(test)]`) | LIFO assertion |

These sites are intentional. Not hot paths, not in #1192's profiling
target; recorded so a future ripgrep does not surface them as new.

## Per-site read/write ratio, hold-time bound, contention class

### Site 1 / 2: SSH stderr drain (`aux_channel.rs:99,148`)

- Producers: one drain thread per child
  (`aux_channel.rs:104-117` pipe, `:152-172` socketpair).
- Consumers: callers of `collected()`
  (`aux_channel.rs:121-123,177-179`), typically once on child exit.
- Read/write ratio: `~0` reads/sec from outside the drain thread;
  `~lines/sec` writes by the drain thread.
- Hold time on write (`append_bounded`, `aux_channel.rs:232-242`):
  `extend_from_slice` plus a possible `drain(..excess)` when the
  buffer exceeds `STDERR_BUFFER_CAP = 64 * 1024`
  (`aux_channel.rs:39`). Worst case `O(64 KiB)` memmove, otherwise
  `O(line length)` append. Lock released between lines.
- Hold time on `snapshot` (`aux_channel.rs:245-247`): one `.clone()`
  bounded by `STDERR_BUFFER_CAP`.
- Contention class: `0`. #1192 does not target this site; no rayon.

### Site 3: `drain_parallel` shards (`drain.rs:63-64`)

Already analysed in PR #3699
(`drain-parallel-contention-static-analysis.md:110-205`); compressed:

- Producers: one rayon worker per shard via
  `rayon::current_thread_index()` (`drain.rs:73-80`).
- Consumers: dispatcher thread, single-threaded, after `rayon::scope`
  joins (`drain.rs:86-89`).
- Read/write ratio: 1 `Vec::push` per `DeltaWork` (`drain.rs:81`),
  `into_inner` once per shard at shutdown.
- Hold time: amortised `O(1)` per push, `O(log2 K)` reallocs with
  `K` items per shard.
- Contention class: `H` per item, but each worker maps to a distinct
  shard, so steady-state contention is zero. Bench grid
  `{1,4,8,16} x {10K,100K}` confirmed safe in PR #3699.

### Sites in parallel stat: none

No `Arc<Mutex<Vec<...>>>` is allocated, captured, or `lock()`-ed
inside `map_blocking` (`parallel_io.rs:107-125`),
`batch_stat_dir_entries` (`batch_stat.rs:38-51`), the receiver's
parallel quick-check (`candidates.rs:124-132`), the parallel
basis-file finder (`pipeline.rs:182-201`), the parallel directory
metadata application (`creation.rs:118-128`), or the parallel
deletion scanner (`deletion.rs:95-105`).

The accumulator rayon needs for `.collect::<Vec<R>>()` is internal to
`ParallelExtend`. With the indexed `into_par_iter().map().collect()`
shape at `parallel_io.rs:124`, rayon uses split-and-merge over
per-task buffers. There is no user-visible mutex. The contention
#1192 was filed to investigate is, statically, not present.

## Parallel stat hot path

Receiver quick-check pipeline (`candidates.rs:117-132`):

```text
candidates: Vec<(usize, &FileEntry)>
   |  iter().map(...).collect()
   v
stat_paths: Vec<(usize, PathBuf)>
   |  parallel_io::map_blocking(stat_paths, parallel_thresholds.stat, |...| ...)
   v        # threshold = 64, candidates.rs:127, parallel_io.rs:16
stat_results: Vec<(usize, PathBuf, Option<Metadata>)>
   |  for (idx, file_path, dest_meta) in stat_results { ... }
   v        # candidates.rs:136
sequential post-processing
```

`map_blocking` (`parallel_io.rs:107-125`):

```rust
pub(crate) fn map_blocking<T, R, F>(items: Vec<T>, min_parallel: usize, f: F) -> Vec<R>
where T: Send + 'static, R: Send + 'static,
      F: Fn(T) -> R + Send + Sync + 'static,
{
    if items.is_empty() { return Vec::new(); }
    if items.len() < min_parallel {
        return items.into_iter().map(&f).collect();
    }
    items.into_par_iter().map(f).collect()
}
```

Static observations:

- Threshold gate (`parallel_io.rs:117`) is a length comparison. Below
  `min_parallel` (stat=64, signature=32, metadata=64, deletion=64 -
  `parallel_io.rs:16-33`) rayon is bypassed.
- Above threshold, `into_par_iter().map(f).collect()` is the only
  shared structure. Rayon's `IndexedParallelIterator::collect` uses
  bridge plus split-and-merge with per-task `Vec<R>` buffers; the
  final merge concatenates in index order. No `Mutex` is involved.
- Closure `f` returns `R` by value. It does not push into a shared
  accumulator. Per-item side effects are bounded to syscalls
  (`fs::metadata` / `fs::symlink_metadata`).

Walker side (`walk.rs:262-267`) calls `batch_stat_dir_entries` which
delegates to `map_blocking` (`batch_stat.rs:43-50`); same analysis.
The receiver also uses `par_iter` at `pipeline.rs:184` for the
basis-file finder; analysis identical.

## Why #1191 already replaced one and why others remain

### #1191 (replaced): buffer pool central queue

Before #1191 the buffer pool used `Mutex<Vec<Vec<u8>>>`, preserved
as the baseline at `buffer_pool_benchmark.rs:30,38-58`. Every
block-write acquired a buffer (`buffer_pool/mod.rs:30-40`). At 100K+
files with `W = 8`, `buffer_pool_contention.rs:1-30` confirmed
super-linear waiting.

The fix replaced `Mutex<Vec<Vec<u8>>>` with
`crossbeam_queue::ArrayQueue<Vec<u8>>`
(`buffer_pool/pool.rs:12,22-30`, `mod.rs:34-55`) plus a thread-local
single-slot cache (`thread_local_cache.rs`). Lock-free CAS push/pop
replaces `lock cmpxchg` plus futex queueing.

Justifying criteria: (1) site on the per-block hot path; (2) bench
at `W = 8 / 16` showed `Mutex::lock` in the perf top-10; (3) a
drop-in lock-free replacement existed (`crossbeam_queue::ArrayQueue`)
with matching bounded capacity.

### #1192 (this audit) - others remain because they are not hot

Remaining sites do not satisfy the same criteria:

- **SSH stderr buffer** (`aux_channel.rs:99,148`). One writer, bounded
  64 KiB. No rayon, no per-item lock acquisition. A lock-free swap
  provides no measurable benefit and would break the
  `STDERR_BUFFER_CAP` truncation contract (`aux_channel.rs:232-242`
  may `drain` the front; `ArrayQueue` does not support
  truncation-of-oldest).
- **`drain_parallel` shards** (`drain.rs:63-64`). Already sharded
  under #1617 / #1680. PR #3699 confirmed steady-state zero
  contention. Further swap (#1681) on hold pending evidence
  `T_lock` is materially visible at `W = 32 / 64`.
- **Test recorders and `MutexPool` baseline**. Single-thread tests;
  bench baseline retained to document the curve #1191 replaced.

The parallel stat path was built without an `Arc<Mutex<Vec>>`
accumulator. `map_blocking` returns `Vec<R>` by value via
`into_par_iter().map().collect()` (`parallel_io.rs:124`); rayon owns
the per-task buffer state, not user code.

## Predicted contention at 100K and 1M files

Notation matches PR #3699
(`drain-parallel-contention-static-analysis.md:209-217`):

- `N` = file count after threshold gate (`N >= 64`).
- `W` = `rayon::current_num_threads()`.
- `T_stat` = `fs::metadata` / `fs::symlink_metadata` syscall cost.
  Warm cache `~1-3 us`; cold cache `~50-200 us`.
- `T_collect` = rayon's per-task collect overhead. `O(N)` total.

Stat path (no shared mutex):
`T_total(N, W) ~= (N / W) * T_stat + N * T_collect + W * T_wakeup`.
Dominant term is `O(N / W)`. No super-linear term in `W`.

Hypothetical single `Arc<Mutex<Vec<R>>>`
(`drain-parallel-contention-static-analysis.md:226-235`):
`T_total_hyp(N, W) = (N / W) * T_stat + N * T_lock_contended(W) + W * T_wakeup`.
`T_lock_contended` at `W = 16` is `~50-500 ns` uncontended but
`~5-50 us` under heavy contention. A contended single-mutex
collector could double or quadruple wall time on cold-cache 100K
runs.

Predicted thresholds extending the bench grid PR #3699 covers
(`drain_parallel_benchmark.rs:20-23`) to 1M files and `W = 32 / 64`:

| N    | W       | parallel stat (current) | hypothetical single-Mutex |
|------|---------|--------------------------|---------------------------|
| 100K | 4 / 8   | `~25K / ~12.5K * T_stat`  | `+ 100K * T_lock_contended` |
| 100K | 16      | `~6.25K * T_stat`         | `+ 100K * T_lock_contended(16)` |
| 100K | 32 / 64 | `~3.1K / ~1.6K * T_stat`  | mutex dominates |
| 1M   | 16      | `~62.5K * T_stat`         | `+ 1M * T_lock_contended(16)` |
| 1M   | 32 / 64 | `~31K / ~16K * T_stat`    | catastrophic queueing |

Static prediction for #1192: parallel stat is in the "no super-linear
term" column. Runtime profiling must confirm `O(N / W)` shape and the
absence of `Mutex::lock` / `parking_lot_core::futex_wait` in the
stat-side perf top-10. If the prediction holds, #1192 closes
confirmed.

## Mitigations - if profiling surfaces a residual `Mutex<Vec>`

Static analysis cannot rule out a hidden contention source introduced
by a future change or a transitive call inside one of the per-item
closures. Mitigations are ordered by preference, mirroring PR #3699's
design space (`drain-parallel-contention-static-analysis.md:262-364`).

### A. Per-thread accumulator (#1617 mirror)

The pattern in `drain.rs:62-89` - one `Mutex<Vec<R>>` per rayon
thread, push keyed by `rayon::current_thread_index()`, flatten after
`rayon::scope`. PR #3699 confirmed contention-free in steady state.

For the parallel stat path this means wrapping `map_blocking`'s
collect with an explicit shard-based reducer instead of rayon's
internal collect. Static concern: rayon's
`into_par_iter().map().collect()` is already split-and-merge under
the hood, so an explicit shard adds atomic ops per item without
removing any. The drain path adopts this layer because its source is
a serial `WorkQueueIter::next()`
(`crates/engine/src/concurrent_delta/work_queue/iter.rs:33`); the
stat path is fed by `Vec<T>` and benefits from rayon's indexed split.
No payoff here.

### B. Lock-free MPSC channel

`drain_parallel_into` uses this pattern in production
(`drain.rs:136-155`); the streaming variant feeds the reorder thread
in `DeltaConsumer::spawn`
(`crates/engine/src/concurrent_delta/consumer.rs:138-143`). For a
batch result, `tx.clone() + rx.into_iter().collect()` is design C in
PR #3699 (`drain-parallel-contention-static-analysis.md:310-364`).

Static evidence against on the stat path: `crossbeam_channel::Sender`
clones an `Arc` per task spawn; rayon's collect does not. For
`N = 1 M`, `W = 16` that is `1 M` extra atomic increments, plus
`rx.into_iter().collect()` performs `N` atomic segment dequeues
whereas rayon's collect concatenates per-task `Vec<R>` with zero
atomic operations during merge. MPSC is correct for streaming;
strictly worse for batch collect.

### C. `DashMap` (#1620 evaluated, not adopted)

Considered for the work-queue results path and rejected because the
result set is index-ordered, not key-addressed. Same applies here:
stat results are returned in input order, and `DashMap<usize, R>`
would require `(0..N).map(|i| map.remove(&i)).collect()` - all `N`
insertions and removals atomic. No payoff at any `W`. `DashMap`
remains correct when consumers key by file index across phases
(e.g. delta-and-itemise re-correlation), not for ordered
map-and-collect.

### D. Avoid the collect entirely

Post-stat phase (`candidates.rs:134-203`) is sequential, so the
parallel collect's output is consumed in one pass. A
`par_iter().filter_map(...).collect_into_vec(...)` shaves one
allocation but does not remove locking. For the walk, the consumer is
also sequential (`walk.rs:269-310`). Replacing collect with
`par_iter() + for_each` breaks input-order pairing. No
allocation-saving micro-mitigation removes contention because there
is no contention to remove.

## Decision criteria - which sites are worth changing

For an `Arc<Mutex<Vec<...>>>` site to justify replacement under #1192,
all four conditions must hold:

1. Reachable from a rayon-pool worker on the per-item hot path of a
   workload with `N >= 100 K`. Inventory shows only `drain.rs:63-64`
   qualifies; SSH stderr sites do not.
2. Bench or profile data shows `Mutex::lock` /
   `parking_lot_core::futex_wait` (oc-rsync uses `std::sync::Mutex`
   exclusively per `docs/audits/mutex-implementation-policy.md:5-18`;
   the futex symbol still appears under heavy contention) above the
   worker closure body in the perf top-10.
3. A drop-in replacement exists with equivalent semantics: bounded
   capacity, FIFO or unordered drain, owned-by-one-thread shards.
   `crossbeam_queue::ArrayQueue` qualifies for fixed-size pools;
   `crossbeam_channel` for streaming MPSC; both already in the
   dependency tree.
4. The replacement does not add atomic operations that rayon's
   internal collect avoided. Rules out MPSC for batch collect and
   DashMap for index-ordered output.

By these criteria, #1192 closes without code change *if* runtime
profiling matches the prediction. If profiling shows contention at
`drain.rs:81`, the tree branches into #1681 (MPSC unification) or
`Vec::with_capacity` at `drain.rs:64` per PR #3699
(`drain-parallel-contention-static-analysis.md:367-402`). Contention
at any other site invalidates this analysis and is its own follow-up.

## What perf must measure for #1192 to close

Static analysis cannot answer:

- **Whether `T_stat` dominates at `N = 100K, W = 16`.** Tool:
  `perf stat -e cpu-clock,task-clock,context-switches` on
  `scripts/benchmark_100k.sh`, with `parallel_thresholds.stat` set to
  `64` (default) and `usize::MAX` (forces sequential). Prediction:
  parallel is `~W` times faster; ratio below `~0.7 * W` indicates a
  bottleneck elsewhere (likely `readdir`, not stat).
- **Whether rayon's collect shows up in perf.** Tool: `perf record -g`
  plus `perf report --stdio`. Look for
  `rayon::iter::collect::Collect::consume_iter` or
  `alloc::vec::Vec::extend_desugared`. Prediction: below
  `fs::metadata`; if higher, allocator pressure that
  `Vec::with_capacity` hints could fix.
- **Whether `lock` symbols appear at all.** Tool: `perf record`
  filtered to `Mutex|futex|park`. Prediction: zero hits on the stat
  path. The drain path shows `lock cmpxchg` per item on
  `drain.rs:81`, characterised as benign in PR #3699. Hits outside
  `drain.rs` invalidate this audit.
- **Whether 1M-file workloads scale linearly.** Tool: extend
  `scripts/benchmark_100k.sh` to `N = 1_000_000` on `rsync-profile`.
  Linear scaling confirms `O(N / W)`; super-linear (>1.2x per 10x `N`)
  indicates a hidden bottleneck.
- **Whether `W = 32` / `W = 64` runs scale.** Tool: pinned pool via
  `rayon::ThreadPoolBuilder::num_threads(W)`, mirror
  `drain_parallel_benchmark.rs:48-51`. Prediction: stat scales until
  `T_collect` dominates; that crossover is workload-specific.

If all five line up with the prediction, #1192 closes confirmed and
this audit is the closing artefact. Otherwise the deviation is the
new tracking issue and this audit becomes its prior art.

## References

- `crates/transfer/src/parallel_io.rs:11-125` - `map_blocking`.
- `crates/transfer/src/generator/file_list/{batch_stat.rs:38-51,walk.rs:262-267}` - sender-side parallel stat.
- `crates/transfer/src/receiver/transfer/{candidates.rs:117-132,pipeline.rs:160-201}` - receiver parallel quick-check and basis-file lookup.
- `crates/transfer/src/receiver/directory/{creation.rs:118-128,deletion.rs:95-105}` - other receiver-side parallel paths.
- `crates/engine/src/concurrent_delta/work_queue/drain.rs:14-155` - `drain_parallel` variants.
- `crates/engine/src/local_copy/buffer_pool/{mod.rs:1-50,pool.rs:1-30,thread_local_cache.rs}` - lock-free `ArrayQueue` pool from #1191.
- `crates/engine/benches/{drain_parallel_benchmark.rs:1-89,buffer_pool_benchmark.rs:1-100,buffer_pool_contention.rs:1-30}` - bench harnesses #1192 will exercise.
- `crates/rsync_io/src/ssh/aux_channel.rs:39-247` - SSH stderr drain.
- `crates/core/src/signal/cleanup.rs:280-326` - cleanup ordering recorder.
- Prior art: `docs/audits/drain-parallel-contention-static-analysis.md:14-451`, `docs/audits/profiling-100k-files.md:1-60`, `docs/audits/mutex-implementation-policy.md:1-60`.
