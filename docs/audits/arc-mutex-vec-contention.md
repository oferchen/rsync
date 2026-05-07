# `Arc<Mutex<Vec<...>>>` contention under 100K+ file workloads (#1192)

Static audit of every `Arc<Mutex<Vec<...>>>` and `Mutex<Vec<...>>` site
reachable from the parallel-stat / parallel-drain hot paths in the
`transfer`, `engine`, and `flist` crates. Sets the measurement plan and
decision criteria for any future replacement work; ships no code change.

## 1. Static occurrences

`rg 'Arc<Mutex<Vec' crates/` and `rg 'Mutex<Vec' crates/` were run from
the worktree root. Production hits (excluding `tests.rs`,
`test_support.rs`, and benches) reduce to two sites:

| File | Line | Shape | Role |
|------|------|-------|------|
| `crates/engine/src/concurrent_delta/work_queue/drain.rs` | 63 | `Vec<Mutex<Vec<R>>>` (per-rayon-thread shards) | `drain_parallel` results buffer |
| `crates/rsync_io/src/ssh/aux_channel.rs` | 99, 148 | `Arc<Mutex<Vec<u8>>>` | bounded SSH stderr ring buffer |

Adjacent shared-state sites that do *not* match the literal pattern but
share the same contention question:

- `crates/flist/src/batched_stat/cache.rs:28-44` -
  `Arc<[Mutex<HashMap<PathBuf, Arc<Metadata>>>; 16]>`. The 16-shard hash
  cache hit by parallel stat at `flist/src/parallel.rs`.
- `crates/transfer/src/receiver/directory/deletion.rs:85` -
  `Arc<AtomicU64>` deletion counter (already lock-free; included as a
  known-good baseline, not a candidate for change).

All `Arc<Mutex<Vec<u8>>>` hits in `crates/transfer/src/{reader,writer}/tests.rs`
are recorder buffers in unit tests. Out of scope.

## 2. Hot paths where contention is plausible

1. **Parallel stat (>= 100K files).** `flist::parallel::collect_with_batched_stats`
   fans out across rayon, every worker locks one of 16 shards in
   `BatchedStatCache::{get, insert}`. With 100K paths and 16 shards the
   expected contention rate per shard is approximately `n_threads / 16`,
   so >= 32 rayon threads on a Mac Studio M2 Ultra cross the threshold
   where the FNV hash + `Mutex::lock` pair starts to dominate the stat
   syscall itself.
2. **`drain_parallel` result collection.** `WorkQueueReceiver::drain_parallel`
   uses `num_shards = rayon::current_num_threads()` so contention is one
   writer per shard *inside* the rayon pool. Threads outside the pool
   hash by `ThreadId`, which keeps the degenerate-shard-0 case off but
   does not give the contention-free guarantee the in-pool path enjoys.
3. **SSH stderr aux-channel drain.** `aux_channel.rs:208 drain_loop`
   appends bytes under `Mutex<Vec<u8>>`; readers pull via `snapshot`.
   Single producer + single consumer; contention is bounded but the lock
   spans the `Vec::extend_from_slice` allocation.

No production `Arc<Mutex<Vec<_>>>` sits on the receiver-side error
accumulation path: `io_error` is a `u32` field on `FileList`, mutated
sequentially by the generator (`memory/MEMORY.md` "Flist io_error
accumulation"). No measurement work needed there.

## 3. Measurement plan

Before any replacement lands, capture the following baselines on Linux
(rsync-profile container, `perf` available) and macOS (Instruments
"Lock Contention" template). All runs use a synthetic tree of 200K
files in 8K dirs so the stat path dominates.

1. **`perf record -e lock:contention_begin,lock:contention_end -g`**
   over `oc-rsync -a /src /dst --dry-run` to attribute wall-clock to
   each `Mutex::lock` site. Emit a flame graph; expect
   `BatchedStatCache` shards near the top.
2. **`perf stat -e cycles,context-switches,cs:u`** with `--threads`
   sweep `{1, 4, 8, 16, 32}`. Speed-up curve flattening below linear
   indicates lock or syscall saturation; pair with strace
   `-c -e statx` to subtract syscall cost.
3. **Criterion micro-bench**: extend
   `crates/engine/benches/buffer_pool_benchmark.rs` pattern with
   `crates/flist/benches/batched_stat_contention.rs` (new) that drives
   `BatchedStatCache::{get, insert}` from N rayon workers over a fixed
   path set, varying `SHARD_COUNT` in `{8, 16, 32, 64}`.
4. **`drain_parallel` micro-bench**: criterion harness already exists
   at `crates/engine/benches/drain_parallel_benchmark.rs`. Add a
   variant that reports `lock_acquire_ns` per shard via
   `parking_lot::Mutex` instrumentation when the `parking_lot` feature
   is enabled in the bench profile only.

## 4. Candidate replacements considered

| Candidate | Fits where | Cost / risk |
|-----------|-----------|-------------|
| `crossbeam::queue::ArrayQueue<T>` | `drain_parallel` shards (bounded, push-only) | Bounded capacity must be sized to worst-case rayon thread output; over-provisioning wastes memory, under-provisioning blocks workers. |
| `crossbeam::queue::SegQueue<T>` | `drain_parallel` shards (unbounded) | Lock-free MPSC; trades node-allocation cost for contention-free push. Heap traffic may regress small-N cases. |
| Thread-local `Vec` + post-merge | `drain_parallel`, `BatchedStatCache` warm-up | Zero contention during the parallel section; merge cost is `O(n)` and serial. Best when results are read once at the end. |
| `dashmap::DashMap` | `BatchedStatCache` | Drop-in for the 16-shard map; uses 64 shards by default and `RwLock` per shard. Adds a dep that the `metadata` and `flist` crates do not currently use. |
| `parking_lot::Mutex` | All sites | 5-15 ns acquire vs `std::sync::Mutex`'s 25-50 ns on uncontended path; same poisoning-free fast path on macOS and Linux. Mechanical change. |
| Per-thread shard via `ThreadLocal<Vec<R>>` (`thread_local` crate) | `drain_parallel` outside-pool path | Removes the `ThreadId` hash collision risk for non-rayon callers. Adds a dep. |

## 5. Decision criteria

A replacement ships only if all four hold for the workload `>= 100K
files, >= 16 rayon threads`:

1. **Wall-clock**: criterion bench shows >= 10% reduction at p50 vs the
   `Mutex<Vec>` baseline, with no regression > 2% at `threads = 1`.
2. **Lock contention**: `perf lock:contention_begin` events at the
   replaced site fall below 1% of cycles in the `--threads = 32` run.
3. **Memory**: peak RSS delta <= 5% (target from `CLAUDE.md` is < 10%
   total vs upstream; we keep half the budget for the change itself).
4. **Dependency budget**: any new crate (`dashmap`, `thread_local`,
   `parking_lot`) must clear an unsafe-policy review per
   `CLAUDE.md` "Unsafe Code Policy" - `flist` and `transfer` deny
   unsafe today; the dep's unsafe must stay encapsulated inside its
   own crate and the wrapper API must remain safe.

If criteria 1-2 fail, the simpler `parking_lot::Mutex` swap is the
fallback - it satisfies criterion 4 trivially (already a transitive
dep) and typically clears criterion 3 with no allocation change.

Tasks tracked under #1192. No follow-up PR ships until the criterion
benches above land and post a baseline on master.
