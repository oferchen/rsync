# Parallel Stat Batch Size Effectiveness

Tracking task: #1083.

This audit profiles the rayon-driven parallel stat paths used by the receiver
and the file-list walker. It documents the current thresholds, how rayon's
work-stealing scheduler chunks at common file counts, the trade-offs between
spawn overhead and filesystem cache locality, and proposes incremental
improvements that can be implemented without wire-protocol changes.

## 1. Current Thresholds and Chunk Sizing

Two layers participate in parallel metadata work.

### `crates/transfer/src/parallel_io.rs`

Defines the per-operation thresholds shared by the receiver, generator, and
directory paths. Defaults:

| Constant                       | Value | Use site                                  |
|--------------------------------|------:|-------------------------------------------|
| `DEFAULT_STAT_THRESHOLD`       |    64 | Receiver Phase B parallel `lstat`         |
| `DEFAULT_SIGNATURE_THRESHOLD`  |    32 | Pipeline basis-file lookup + checksums    |
| `DEFAULT_METADATA_THRESHOLD`   |    64 | Directory `chmod`/`chown`/`utimes`/xattr  |
| `DEFAULT_DELETION_THRESHOLD`   |    64 | Per-directory deletion scans              |

`map_blocking` is the single dispatch entry point. It performs a sequential
pass when `items.len() < min_parallel`, otherwise calls
`items.into_par_iter().map(f).collect()`. There is no explicit
`with_min_len` or `chunks` call - rayon's adaptive splitter picks the chunk
size at runtime based on stealer count and item budget.

### `crates/flist/src/batched_stat/`

Scoped to file-list construction. `cache.rs::stat_batch` and
`dir_stat.rs::stat_batch_relative` are gated on `feature = "parallel"`. Both
call `paths.par_iter().map(...).collect()` with no explicit threshold and no
chunk size: every batch handed to these helpers goes straight onto the rayon
pool. Callers (the file-list walker) decide whether the batch is worth
parallelising.

`BatchedStatCache` shards by FNV-1a hash across `SHARD_COUNT = 16`
mutexes to reduce contention when multiple workers fault paths into the
cache.

### Receiver Call Sites

Recorded at:

- `crates/transfer/src/receiver/transfer/candidates.rs:127` -
  parallel `fs::metadata` for the quick-check phase.
- `crates/transfer/src/receiver/transfer/pipeline.rs:179` - `signature`
  threshold gates the parallel basis-file resolver.
- `crates/transfer/src/receiver/directory/creation.rs:124` - `metadata`
  threshold gates parallel `apply_metadata_from_file_entry` + ACLs +
  xattrs.
- `crates/transfer/src/receiver/directory/deletion.rs:101` - `deletion`
  threshold gates parallel directory scans.

The receiver always passes the threshold from `ParallelThresholds`. The
flist parallel paths never inspect a threshold; they assume the caller has
already decided.

### Rayon Pool Construction

The pool is built once via `rayon::ThreadPoolBuilder::build_global` from
`crates/cli/src/frontend/execution/drive/thread_tunables.rs`. Default thread
count is `rayon::current_num_threads()` (the number of logical CPUs). The
audited code does not call `with_min_len`, `with_max_len`, or `chunks`, so
chunking is entirely up to rayon's splitter.

## 2. How Rayon Chunks at Different File Counts

Rayon's `IndexedParallelIterator::map().collect()` uses an adaptive splitter
that targets `~num_threads * splits_per_thread` work items, where
`splits_per_thread` defaults to `8`. The effective minimum work unit is
`max(1, len / (num_threads * 8))`.

Assume an 8-core host (typical CI runner) with `num_threads = 8`:

| Items | Threshold gate (stat=64)            | Approximate split size | Workers used | Notes                                     |
|------:|-------------------------------------|------------------------|------------:|-------------------------------------------|
|    16 | sequential (below 64)               | n/a                    |           1 | Iterator::map; no thread dispatch         |
|    64 | parallel boundary                   | 1 per item             |        ~8   | All workers, but each does ~1 stat        |
|   256 | parallel                            | ~4 per chunk           |           8 | Steady state; cache-locality is poor      |
|  1024 | parallel                            | ~16 per chunk          |           8 | Best-case throughput                      |
| 10000 | parallel                            | ~156 per chunk         |           8 | Limited by FS metadata bandwidth          |

Implications:

- At 64 items the gate just barely fires, but each worker handles ~8 items.
  Wake-up latency dominates; we measured this overshoots sequential on warm
  caches in earlier benchmark runs (see `docs/audits/daemon-concurrency-bench-plan.md`).
- At 256-1024 items the splitter produces 4-16 entries per chunk, which is
  too small to keep an `openat`-cached directory hot across one worker.
  Adjacent file-list entries belonging to the same directory get distributed
  across cores, defeating the kernel's per-fd dirent cache.
- At 10K items we're metadata-bandwidth bound, not CPU bound. Rayon's split
  granularity is fine; the bottleneck moves to FS lock contention (ext4
  inode lock, APFS catalogue B-tree).

## 3. Trade-offs

### Thread Spawn Overhead

`map_blocking` does not spawn threads per call; rayon reuses its global
pool. The cost paid per call is:

1. Bridge from sequential to parallel: ~1 us setup for the splitter.
2. Steal handshake: lock-free, but each idle worker pays a cache-miss when
   it grabs a new chunk.
3. Result vector merge: `collect` reassembles per-thread vectors; this is
   `O(n)` and pinned to the calling thread.

For 64 items the bridge + steal cost is ~50-200 us depending on contention.
A warm sequential `lstat` loop on Linux ext4 is ~3 us per call, so 64
sequential stats take ~200 us. The break-even is `num_threads * spawn_cost`
divided by `per_item_savings`. On cold caches (~30 us per stat) the
break-even drops to ~16 items. On warm caches it can sit above 100 items.

### Filesystem Metadata Cache Locality

POSIX `lstat` resolves each path component, hitting the dentry cache (Linux)
or vnode cache (macOS). Adjacent file-list entries share directory prefixes,
so sequential traversal keeps the dentry hot. Splitting them across cores:

- Linux ext4: dentry is RCU-read; parallel reads scale, but inode-table
  fetches contend on the inode lock per group. Small chunks waste prefetch.
- macOS APFS: per-volume catalog B-tree lock; parallel readers serialise on
  the lock for inode lookups. Speedup tops out at ~2-3x even with 8 cores.
- Windows NTFS / ReFS (via `GetFileAttributesEx`): MFT record reads scale
  modestly, but the user-mode path goes through a single kernel transition
  per call - no batching, so chunking matters less.
- Network FS (NFS, SMB): RTT dominates. Parallel stats win up to the server
  concurrency limit, after which queueing overhead inverts the gain.
- io_uring `statx` (Linux 5.6+, optional via `fast_io`): batch submission
  amortises the kernel transition, so per-item parallelism is the wrong
  axis - we should be queueing requests, not splitting work.

### Result Ordering

Both `map_blocking` and the `batched_stat` helpers rely on rayon's
`par_iter().collect()` preserving index order. This is documented and
covered by the proptest in `parallel_io.rs:267
parallel_stat_preserves_ordering`. Switching to `for_each` or
`flat_map_iter` to reduce vector allocation would break this contract.

## 4. Proposed Improvements

The four improvements below are ordered by expected payoff per unit of
implementation risk. None require wire-protocol changes.

### 4.1 Adaptive Chunk Size via `with_min_len`

Pin the splitter's minimum chunk size based on observed per-item cost.
Replace:

```rust
items.into_par_iter().map(f).collect()
```

with

```rust
items.into_par_iter()
    .with_min_len(min_chunk_for(items.len(), num_threads))
    .map(f)
    .collect()
```

where `min_chunk_for(n, t) = clamp(n / (t * 4), 8, 256)`. This caps split
overhead at large `n` and keeps worker chunks large enough to retain
directory-cache locality. Implement once in `parallel_io::map_blocking` so
all four call sites benefit.

Risk: low. Behaviour is unchanged below threshold and equivalent at the
boundary. Add a benchmark in `crates/transfer/benches/` covering 64, 256,
1024, 10K item batches.

### 4.2 Per-FS-Type Threshold Tuning

The current single threshold ignores filesystem class. Provide a runtime
classifier (`ext4`, `xfs`, `btrfs`, `apfs`, `ntfs`, `nfs`, `smb`, `tmpfs`,
unknown) that selects from a tuned profile:

| FS class | stat | signature | metadata | deletion |
|----------|-----:|---------:|---------:|---------:|
| tmpfs    |   16 |       16 |       16 |       16 |
| ext4/xfs |   64 |       32 |       64 |       64 |
| apfs     |  128 |       32 |      128 |      128 |
| ntfs     |  128 |       32 |      128 |      128 |
| nfs/smb  |    8 |       32 |        8 |        8 |
| unknown  |   64 |       32 |       64 |       64 |

Detection sources: `statvfs(2)` `f_basetype` on Linux, `getmntinfo(3)` on
BSD/macOS, `GetVolumeInformationW` on Windows. Cache the result per
destination root in `core::session()`. NFS/SMB profile favours aggressive
parallelism because RTT dominates. APFS/NTFS profile pushes the threshold
up because the metadata B-tree lock penalises small chunks.

Risk: medium. The classifier needs cross-platform tests and a fallback for
unknown filesystems.

### 4.3 Work-Stealing Across the Walker and Receiver

Today the file-list walker and the receiver's quick-check are separate
batch boundaries; the walker finishes before the receiver schedules its
parallel stat. Both phases could share a rayon pool and `Iter` so that
walker producers feed receiver consumers as soon as a directory's entries
are ready. Concretely, replace the walker's `Vec<FileEntry>` output with a
`crossbeam::SegQueue<FileEntry>` drained by `par_bridge` on the receiver
side. This collapses two synchronous parallel passes into one work-stealing
pipeline.

Risk: medium-high. Result ordering is no longer trivial; the receiver must
sort by index after completion or use `Vec<Option<T>>` with index slots.

### 4.4 Statx Batching on Linux

Where available, prefer the existing `statx_relative` path
(`flist::batched_stat::dir_stat`) and switch the parallel layer from
"map across files" to "io_uring submission queue". Each rayon worker
becomes an io_uring submitter that queues `IORING_OP_STATX` ops up to the
queue depth, then drains completions. Per-FS thresholds (4.2) determine
queue depth: NFS gets a deep queue, ext4 gets a shallow one.

Risk: medium. Requires `fast_io` plumbing and `io_uring` feature gating;
the path already exists for writes. Falls back to today's parallel `lstat`
on non-Linux or when `io_uring` is disabled.

### 4.5 Per-Call-Site Override for Empirically-Hot Paths

The receiver's directory metadata pass (`creation.rs:124`) and deletion
scan (`deletion.rs:101`) have very different per-item costs (xattr + ACL
write vs. directory listing). Allow each call site to scale the global
threshold by a constant rather than sharing one. Concretely, add
`ParallelThresholds::with_*_scale(f32)` and let each receiver subsystem
ship a tuned scale baked in via `Default`. Cheaper than a full FS-aware
classifier and isolates regressions to one site.

Risk: low. Configuration-only change.

## Pointers

- `crates/transfer/src/parallel_io.rs` - thresholds and `map_blocking`.
- `crates/flist/src/batched_stat/cache.rs` - sharded `stat_batch`.
- `crates/flist/src/batched_stat/dir_stat.rs` - `fstatat`/`statx` batch.
- `crates/transfer/src/receiver/transfer/candidates.rs` - quick-check stat.
- `crates/transfer/src/receiver/transfer/pipeline.rs` - basis lookup.
- `crates/transfer/src/receiver/directory/creation.rs` - metadata apply.
- `crates/transfer/src/receiver/directory/deletion.rs` - deletion scan.
- `crates/cli/src/frontend/execution/drive/thread_tunables.rs` - rayon
  global pool bootstrap.
- `crates/fast_io/src/parallel.rs` - per-task rayon pools used by I/O.

## References

- Upstream rsync issues only single-threaded `lstat` walks; this audit
  describes oc-rsync extensions, not protocol behaviour.
- `target/interop/upstream-src/rsync-3.4.1/flist.c` - reference walker;
  no parallelism, used as ordering ground truth.
- Task tracker: #1083.
