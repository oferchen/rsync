# Parallel stat batch size: threshold profiling plan

Tracking issue: oc-rsync #1083.

This note records the current rayon parallel-stat threshold, frames
the question of whether `64` is still optimal across filesystems with
very different syscall costs, lays out a profiling matrix to answer
it empirically, and enumerates the decision branches and risks for
the follow-up patch.

## 1. Current threshold

The receiver and generator no longer use a single global constant -
`PARALLEL_STAT_THRESHOLD` was promoted into a per-operation struct in
`crates/transfer/src/parallel_io.rs`:

- `DEFAULT_STAT_THRESHOLD = 64` (`parallel_io.rs:16`) - parallel
  `stat()` / `lstat()` / `quick_check_ok_stateless()`.
- `DEFAULT_SIGNATURE_THRESHOLD = 32` (`parallel_io.rs:22`) - parallel
  rolling + strong checksum computation.
- `DEFAULT_METADATA_THRESHOLD = 64` (`parallel_io.rs:27`) - parallel
  `chmod` / `chown` / `utimes` application.
- `DEFAULT_DELETION_THRESHOLD = 64` (`parallel_io.rs:33`) - parallel
  delete-side directory scans.

`map_blocking(items, min_parallel, f)` (`parallel_io.rs:107`) is the
single dispatch site: below `min_parallel` it runs `Iterator::map`
sequentially; at or above it dispatches `into_par_iter()` onto the
rayon global pool. Consumers thread the struct through:

- `crates/transfer/src/generator/file_list/batch_stat.rs:43` -
  generator-side bulk stat (`thresholds.stat`).
- `crates/transfer/src/receiver/transfer/candidates.rs:127` -
  receiver-side basis-candidate stat (`parallel_thresholds.stat`).
- Receiver directory scans / metadata pass use the same struct via
  `ParallelThresholds` plumbed through `ReceiverContext`.

## 2. Is 64 the right cutover?

`64` was picked as a coarse local-disk break-even: rayon dispatch +
work-stealing overhead is roughly tens of microseconds, while a
warm-cache `lstat()` on tmpfs / ext4 is sub-microsecond. Below ~64
items the sequential path wins; above it the parallel path amortises
the dispatch.

The constant ignores the syscall-cost dimension. On NFSv3/v4, FUSE,
SMB, and over-WAN SSHFS each `lstat` is a network round trip costing
hundreds of microseconds to low milliseconds - 100x to 1000x the
local cost. There the break-even drops sharply: even 8-16 items
benefit from parallelism, and at 64 we are leaving most of the
latency unhidden. Conversely, on a cold local disk with a deep dirent
cache miss, dispatching 32 stats across 16 cores can thrash the
elevator and slow the wall clock. A single number cannot fit both.

## 3. Profile plan

Vary `ParallelThresholds::stat` across `{16, 64, 256, 1024, 4096}`
against synthetic workloads of `{100, 1K, 10K, 100K}` files on:

- tmpfs (sub-microsecond stat, 16-core box).
- xfs on local NVMe (warm + cold page cache).
- xfs on local NVMe with `posix_fadvise(POSIX_FADV_DONTNEED)` to
  force cold dirents.
- NFSv4 mount over 1 GbE loopback (rsync-profile container).
- FUSE passthrough (`passthrough_hp` from libfuse examples).

Measurement: receiver-only `--list-only` and full pull of an
unchanged tree, both with `--no-W` so quick-check stat dominates.
Capture wall-clock (`hyperfine --warmup 3 --runs 10`) and per-syscall
cost (`strace -c -e lstat,statx`). The harness lives in
`scripts/benchmark_parallel_stat.sh` (to be added with the patch);
results land as a CSV under `target/bench/parallel_stat/` and a
markdown summary alongside this audit.

Threshold choice for each cell is the `min` arg to `map_blocking`,
overridden via a temporary `--parallel-stat-threshold` debug flag (or
env var if we keep the runtime knob private until #1554 lands).

## 4. Decision branches

Three outcomes are possible; the patch lands whichever the data
supports:

1. **Keep 64.** If tmpfs/xfs win at 64 and the NFS/FUSE wins at 16
   are inside benchmark noise (<5% wall-clock), leave the default
   alone and document the rationale here.
2. **Promote to runtime-tunable** (depends on #1554 - shared CLI
   tunability flags). Expose `--parallel-stat-threshold=<N>` and an
   `OC_RSYNC_PARALLEL_STAT_THRESHOLD` env var. Default stays 64;
   power users on remote filesystems lower it. Gate behind the same
   plumbing #1554 sets up so we do not grow a one-off knob.
3. **Filesystem-aware via statfs.** Call `statfs(2)` on the receiver
   root once per session; if `f_type` matches `NFS_SUPER_MAGIC`,
   `FUSE_SUPER_MAGIC`, `SMB_SUPER_MAGIC`, or `CIFS_MAGIC_NUMBER`,
   drop `thresholds.stat` to 16 (and `thresholds.deletion` likewise).
   On Windows, mirror with `GetVolumeInformation` / remote-path
   probing. macOS uses `statfs.f_fstypename`. Falls back to 64
   everywhere else. Adds ~one syscall per session.

Option 3 is the preferred direction if the data shows a >2x gap
between local and remote filesystems at any workload size; otherwise
option 2 is the lower-risk landing point.

## 5. Risks

- **Thread-pool warmup.** Short transfers (<1 s) absorb the cost of
  spinning up rayon workers the first time `into_par_iter()` runs.
  The current 64 cutover hides this; lowering to 16 makes warmup
  visible on every small transfer. Mitigation: keep a rayon
  `ThreadPoolBuilder::build_global()` warmup call in the receiver
  prelude, or memoize the first-dispatch latency.
- **Work-stealing imbalance on skewed dirs.** Stat cost is uniform
  per item, but cache-miss locality is not: a directory with one
  hot subtree and many cold ones can leave most workers blocked on
  the same inode lock. The risk is constant across thresholds but
  more visible at low ones because the parallel path runs on smaller
  batches where stragglers dominate.
- **NFS server overload.** Lowering the threshold to 16 on NFS sends
  bursts of parallel `LOOKUP` / `GETATTR` RPCs. A small home-NAS
  server can saturate at ~32 concurrent ops; we should cap the
  rayon worker count for stat dispatch (e.g. `min(num_cpus, 16)`)
  before lowering the entry threshold.
- **Heterogeneous workspaces.** A single transfer can cross
  filesystem boundaries (local source -> NFS destination). A
  per-receiver `statfs` probe captures the destination only. The
  generator's stat batch on the source side may need a separate
  probe.
- **Benchmark noise.** Tmpfs at 100 files finishes in <1 ms; hyperfine
  variance dominates. Use `--min-runs 50` and report 95% CI.
