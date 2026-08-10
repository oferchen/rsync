# Parallel-stat result collection: what the code actually does (#1192)

Audit answers a single question raised by #1192:

> The receiver's parallel-stat path is suspected of using
> `Arc<Mutex<Vec<...>>>` to collect results from rayon workers. At 100K+
> files and 8+ cores this would be a contention bottleneck. Is it?

Short answer: **no production parallel-stat call site uses
`Arc<Mutex<Vec<...>>>` to collect results.** The collector is rayon's
built-in lock-free reducer, which split-and-merges per-thread vectors
without ever sharing a mutex. The two `Mutex<Vec<_>>` production sites
that do exist (SSH stderr capture and `WorkQueueReceiver::drain_parallel`)
are off the parallel-stat path or already sharded.

## Inventory of parallel-stat call sites

`grep -rn 'par_iter\|into_par_iter' crates/flist crates/transfer crates/engine`
returns the following metadata-collection call sites. Every one of them
collects through rayon's built-in reducer.

| Site | Shape | Collector |
|------|-------|-----------|
| `crates/flist/src/parallel.rs:83` (`map_entries_parallel`) | `entries.par_iter().map(f).collect()` | rayon reducer |
| `crates/flist/src/parallel.rs:105` (`collect_paths_chunked_parallel` inner stat) | `.par_iter().map(...).collect()` | rayon reducer |
| `crates/flist/src/parallel.rs:132` (`collect_paths_then_metadata_parallel`) | `paths.into_par_iter().map(stat).collect()` | rayon reducer |
| `crates/flist/src/parallel.rs:206` (`collect_lazy_parallel`) | `paths.into_par_iter().map(lazy).collect()` | rayon reducer |
| `crates/flist/src/parallel.rs:266` (`collect_with_batched_stats`) | `.into_par_iter().map(batched).collect()` | rayon reducer (cache shards live in `BatchedStatCache`, not on the collector) |
| `crates/transfer/src/parallel_io.rs:186` (`map_blocking`) | `items.into_par_iter().map(f).collect()` | rayon reducer (the single dispatcher used by the receiver and generator parallel-stat code) |
| `crates/transfer/src/receiver/transfer/pipeline.rs:188` (signature pre-fetch) | `.par_iter().map(...).collect()` | rayon reducer |
| `crates/engine/src/local_copy/executor/directory/support.rs:106` (parallel directory stat) | `.into_par_iter().map(...).collect()` | rayon reducer |
| `crates/engine/src/local_copy/executor/directory/parallel_planner.rs:100` (parallel planning) | `.par_iter().enumerate().map(...).collect()` | rayon reducer |

None of these allocate an `Arc<Mutex<Vec<_>>>` and push into it from
workers. Rayon's `collect::<Vec<_>>()` implementation builds per-thread
`Vec`s in `ParallelExtend::par_extend` and concatenates them in the
join-tree. Workers never share a mutex during the collection step.

## Production `Mutex<Vec<_>>` sites that exist (none on parallel-stat path)

`grep -rn 'Arc<Mutex<Vec\|Mutex<Vec\|Mutex::new(Vec' crates/ --include='*.rs' | grep -v tests`
finds two non-test producers in shipping code:

| File | Role | Hot path? |
|------|------|-----------|
| `crates/rsync_io/src/ssh/aux_channel.rs:99,148` | Bounded SSH stderr ring buffer (cap 64 KiB) | No - one drain thread per SSH child; not in any parallel-stat path |
| `crates/engine/src/concurrent_delta/work_queue/drain.rs:63` | `WorkQueueReceiver::drain_parallel` per-rayon-thread shards | Yes for delta work items, but sharded by `rayon::current_thread_index()` so contention is structurally near zero |

Neither sits on the receiver's parallel-stat path. Both are documented
in `docs/audits/arc-mutex-vec-parallel-stat-contention.md`.

## Why the rayon reducer wins by default

`Vec::<R>::par_extend` operates as follows in rayon 1.x (see `rayon/src/iter/extend.rs`):

1. Each worker accumulates into its own `Vec<R>` allocated from the local
   heap. No coordination.
2. When two workers join via `rayon::join`, the smaller `Vec` is
   `Vec::append`-ed onto the larger one by the joining thread.
3. The join tree concatenates upward until the root holds the full result.

The only synchronisation cost is rayon's own work-stealing deque (one
atomic CAS per task steal, not per item). The collector contributes
zero locks per item, which is what makes the `par_iter().collect()`
pattern strictly faster than `Arc<Mutex<Vec<R>>>` for any non-trivial
worker count.

## Adjacent shared-state under parallel stat

The one shared mutable structure that workers do touch during
`collect_with_batched_stats` is the metadata cache:

- `crates/flist/src/batched_stat/cache.rs:28-44` -
  `Arc<[Mutex<HashMap<PathBuf, Arc<Metadata>>>; 16]>`.

Sixteen-shard hash cache, locked per shard. Expected contention per shard
is approximately `n_threads / 16`. At 32 rayon threads the FNV hash plus
`Mutex::lock` cost begins to compete with the underlying stat syscall
(see audit doc, section 2.1). This is **not** a `Vec` collector, so it
is out of scope for #1192, but the same measurement plan applies and is
captured in `docs/audits/arc-mutex-vec-contention.md`.

## Microbench backing this audit

`crates/transfer/benches/parallel_stat_collector_contention.rs` parametrises
four append strategies (`Arc<Mutex<Vec>>`, sharded `Mutex<Vec>`,
`crossbeam_queue::SegQueue`, `crossbeam_channel::unbounded`) over 100K
items at 1, 4, 8, and 16 rayon workers. Run with:

```sh
cargo bench -p transfer -- parallel_stat_collector_contention
```

Throughput is reported in elements/sec. The numbers are the baseline
against which any future proposal to introduce a shared `Mutex<Vec>` on
this path must be measured.

## Conclusion

The premise of #1192 - "Arc<Mutex<Vec>> is the parallel-stat collector"
- is incorrect against current master. The collector is rayon's lock-free
reducer; no shared mutex sits on the per-item path. The microbench
quantifies what the cost would be if a future change introduced one.

The remaining contention candidate under parallel stat is the 16-shard
`BatchedStatCache`, tracked separately by the measurement plan in
`docs/audits/arc-mutex-vec-contention.md` and the dashmap/parking_lot
comparisons in `docs/audits/arc-mutex-vec-parallel-stat-static.md`.

## References

- `crates/transfer/src/parallel_io.rs` - single dispatcher for parallel stat.
- `crates/flist/src/parallel.rs` - flist build-time parallel collectors.
- `crates/flist/src/batched_stat/cache.rs` - 16-shard metadata cache.
- `crates/transfer/benches/parallel_stat_collector_contention.rs` - the
  matching microbench produced for #1192.
- `docs/audits/arc-mutex-vec-parallel-stat-contention.md` - per-site
  contention analysis.
- `docs/audits/arc-mutex-vec-contention.md` - static audit and decision
  criteria for any future replacement.
- `docs/audits/parallel-stat-batch-size-profile.md` - threshold tuning
  for the parallel-stat dispatcher.
- Issue #1192 - tracking item.
