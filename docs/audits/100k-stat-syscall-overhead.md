# Profile 100K Files Stat Syscall Overhead

Tracking task: #1045.

This audit profiles the per-entry stat syscall cost when oc-rsync walks a tree
of 100K files. It locates each call site, computes the theoretical floor,
defines a runnable profile plan, lists optimization paths already in flight,
and gives decision criteria for choosing io_uring statx over the existing
parallel sync stat fan-out.

## 1. Stat Syscalls in oc-rsync

The walk and quick-check phases issue one metadata syscall per entry. Call
sites grouped by phase:

- **File-list walk (`lstat`)** - one metadata lookup per directory child.
  - `crates/flist/src/file_list_walker.rs:39,109` - `fs::symlink_metadata`
    on the root and on each visited entry.
  - `crates/flist/src/lazy_metadata.rs:107` - lazy `symlink_metadata`
    fetcher used by builders.
  - `crates/flist/src/parallel.rs:137,324,434` - rayon-parallel
    `symlink_metadata` inside `collect_with_batched_stats` and the
    deletion scanner.
  - `crates/transfer/src/generator/file_list/walk.rs:267` -
    `batch_stat_dir_entries` is the receiver-side hot path.
  - `crates/transfer/src/generator/file_list/batch_stat.rs:43,47` -
    rayon dispatch to `fs::symlink_metadata` (or `metadata` when
    `--copy-links` is set), gated by
    `ParallelThresholds::stat = 64`.

- **Quick-check (`stat`/`lstat`)** - a second metadata fetch per file when
  the generator decides whether to skip a transfer.
  - `crates/transfer/src/generator/file_list/mod.rs:140,168` -
    `std::fs::symlink_metadata` on each generator entry to compare
    size+mtime against the file-list metadata.
  - `crates/engine/src/local_copy/...` - the local-copy executor calls
    `metadata` again on the destination during quick-check (see audit
    `quick-check-stat-overhead.md`).

- **statx fast path (Linux 4.11+)** - lightweight metadata when available.
  - `crates/flist/src/batched_stat/statx_support.rs:17,67,84` -
    `has_statx_support`, `statx`, `statx_mtime`, `statx_size_and_mtime`
    wrappers around `SYS_statx`.
  - `crates/flist/src/batched_stat/dir_stat.rs:89-140` -
    `DirectoryStatBatch::statx_relative` issues `statx` against an
    open directory fd, returning a packed `StatxResult` without
    constructing `fs::Metadata`.
  - `crates/flist/src/batched_stat/dir_stat.rs:50-77` -
    `stat_relative` uses `fstatat` for the same dir-relative path
    when statx is unavailable.

Reproduce with:
`rg -n "lstat|statx|symlink_metadata|::metadata\(" crates/flist/ crates/transfer/`.

## 2. 100K Files Theoretical

A single `lstat`/`statx` round-trip on a warm dentry cache costs roughly 1
microsecond on Linux (kernel mode + return). Cold-cache lookups on ext4 cost
50-200 microseconds depending on inode locality.

- **Warm cache (sequential)**: 100,000 calls x 1 us = 100 ms minimum.
- **Cold cache (sequential)**: 100,000 x ~80 us = 8 s, dominated by
  blocking on the inode read.
- **Sequential walk + quick-check**: 200,000 syscalls = 200 ms warm, 16 s cold.
- **Parallel (8-way) warm**: 100 ms / 8 = ~12.5 ms, plus rayon dispatch.

Real measured numbers are not yet captured in CI; the perf workflow stops at
1K files. The next benchmark run should record `oc-rsync --dry-run` against a
100K-file tree on tmpfs (warm) and ext4 (cold) and compare against
`rsync --dry-run`.

## 3. Profile Plan

Run on the `rsync-profile` container against a 100K-file fixture
(`scripts/make_fixture.sh 100000`):

1. Build a fresh tree on tmpfs and ext4.
2. Capture syscall counts and totals:
   - `strace -c -e trace=lstat,statx,fstatat,newfstatat oc-rsync -anr src/ dst/`
   - `strace -c -e trace=lstat,statx,fstatat,newfstatat rsync -anr src/ dst/`
3. Capture per-thread distribution with `perf trace -s` to see how rayon
   batches dispatch.
4. Drop caches between runs (`echo 3 > /proc/sys/vm/drop_caches`) to measure
   cold-cache cost; rerun warm.
5. Record results in `bench/data/stat_syscall_overhead.csv` with columns
   `tool,fs,cache,n_files,syscalls,total_us,p50_us,p99_us`.

## 4. Optimization Paths

- **io_uring `IORING_OP_STATX` chains** (#1833 pending). Submit a batch of
  statx SQEs against an open directory fd and reap completions in one
  `io_uring_enter`. Removes the per-call ~200 ns syscall entry/exit overhead
  and lets the kernel issue inode reads without per-thread blocking.
- **`openat2` with `O_PATH` for dir-relative stat**. Already partially
  implemented via `DirectoryStatBatch` (`fstatat`/`statx` against a kept
  dir fd). Extending to `openat2(RESOLVE_NO_SYMLINKS)` would harden the
  walk against TOCTOU symlink races and skip the path-resolution scan.
- **Parallel sync stat batches** (#1252 done). `ParallelThresholds::stat =
  64` (`crates/transfer/src/parallel_io.rs:16`) routes batches >= 64 to
  rayon. The 16-shard `BatchedStatCache` removes contention on the result
  map (`crates/flist/src/batched_stat/cache.rs:16-66`).

## 5. Decision Criteria: io_uring statx vs Parallel Sync stat

io_uring statx is preferred when **all** of the following hold:

- Linux kernel 5.6+ with io_uring enabled (`fast_io::io_uring_status_detail`
  reports `available`).
- Batch size >= 256 entries from the same directory, so SQE submission
  amortizes the `io_uring_enter` cost.
- Cold-cache scenario (cold ext4, NFS, FUSE) where syscall blocking is the
  dominant cost; queueing many in-flight statx hides the per-call latency.
- Workload is metadata-heavy (`--dry-run`, `--list-only`,
  initial transfer). For mixed read+stat workloads io_uring is already
  occupied by file I/O and adding statx contends for SQEs.

Prefer parallel sync stat when:

- Kernel < 5.6 or io_uring is disabled by policy.
- Batch < 64 entries (rayon dispatch dominates).
- Warm cache; sequential stat already runs at full memory bandwidth.
- macOS or Windows targets (no io_uring).

The runtime decision should consult `ParallelThresholds::stat` for the lower
bound, `fast_io::io_uring_status_detail()` for kernel support, and a new
`stat_batch_size_hint` carried by `DirectoryStatBatch` for the upper bound.
