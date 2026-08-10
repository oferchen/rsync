# 1M-file delete benchmark harness (DEL-4.b)

Status: Design (scale-up companion to DEL-4.a 100K bench; implementation
lands as a criterion harness plus a CI workflow once DEL-2.d wires the
parallel consumer into the receiver)
Audience: engine maintainers optimizing the parallel-deterministic-delete
pipeline at scale.
Scope: benchmark methodology for measuring delete throughput at
1,000,000 files - fixture generation, directory layouts, metrics,
comparison axes, and pass/fail criteria.

Out of scope: the parallel consumer implementation itself (DEL-2.c),
wire-byte parity verification (DEL-3), the 100K-scale harness (DEL-4.a),
and integration with the release benchmark workflow
(`.github/workflows/benchmark.yml`).

## 1. Goal

At 100K files (DEL-4.a), the sequential `DeleteEmitter` and the
parallel `ParallelDeleteEmitter` show measurable throughput differences
but memory pressure and kernel dentry-cache effects remain negligible.
At 1M files:

- **RSS** grows to hundreds of megabytes from `DeletePlanMap` +
  `CohortBatcher` + `ReorderBuffer` occupancy.
- **dentry/inode cache pressure** forces kernel-side eviction and
  reclaim under memory constraint.
- **Rayon parallelism payoff** scales non-linearly: at high file counts
  the unlink-per-syscall model amortises more work per cohort, and
  cross-core L3 thrashing becomes measurable.

This benchmark answers:

1. What is the delete throughput ceiling (ops/sec) at 1M files for the
   sequential emitter vs the parallel emitter?
2. How does peak RSS scale from 100K to 1M, and does it fit within the
   project's "< 10% vs upstream" memory target?
3. At what rayon thread count does throughput plateau or regress?
4. What fraction of wall-clock is spent in I/O wait vs userspace?

## 2. Fixture generation strategy

### 2.1 Constraints

A naive `for i in 1..1_000_000 { touch(file_i) }` approach is
unacceptable:

- **Inode exhaustion.** Default ext4 allocates one inode per 16 KiB of
  filesystem capacity. A 16 GiB tmpfs partition caps at roughly 1M
  inodes. The fixture must not exhaust inodes and must leave headroom
  for temp files the pipeline creates.
- **Wall-clock setup cost.** Sequential `open(O_CREAT)` + `close` for
  1M files takes 8-15 seconds on ext4, 3-5 seconds on tmpfs. Setup
  time must stay under 30 seconds to fit CI runner budgets.
- **Disk space.** Files are empty (zero bytes) - the benchmark measures
  unlink throughput, not read/write throughput. Total metadata overhead
  on tmpfs is approximately 200 bytes/file = 200 MB.

### 2.2 Implementation

The fixture generator is a standalone Rust binary
(`xtask/src/commands/benchmark/delete_fixture.rs`) that:

1. Creates the target directory tree structure (see section 3).
2. Spawns rayon workers that batch-create files using `openat2` (Linux)
   or `open(O_CREAT | O_EXCL)` with a dirfd anchor (portable). Each
   worker owns a contiguous range of file indices so no inter-worker
   coordination is needed.
3. Calls `syncfs` once at the end so the fixture is stable before
   timing begins.
4. Reports wall-clock setup time and file count to stderr.

Parallel creation with 8 workers on tmpfs achieves approximately
400K files/sec, bringing 1M fixture setup to under 3 seconds.

### 2.3 Filesystem choice

- **CI (Linux):** tmpfs mounted at the working directory with
  `size=4G,nr_inodes=1200000`. Tmpfs eliminates journal overhead and
  I/O scheduler noise; the benchmark measures the unlink syscall path
  in isolation.
- **Local profiling:** ext4 on NVMe is supported via an environment
  variable (`DEL_BENCH_FS=ext4`) for realistic I/O-wait measurements.

## 3. Directory layouts

Three layouts exercise different kernel code paths and reveal
parallelism scaling characteristics:

### 3.1 Flat (worst case for dentry hash collisions)

```
dest/
  file_000000 .. file_999999
```

All 1M files share one parent directory. The kernel's `dcache` hash
table bucket for this single directory inode becomes hot. Sequential
`unlink` must re-hash after every removal. The parallel emitter puts
all ops in a single cohort so intra-cohort parallelism is the only axis.

### 3.2 Deep (pathological path-length overhead)

```
dest/
  a/b/c/d/e/f/g/h/i/j/
    file_000000 .. file_099999
  a/b/c/d/e/f/g/h/i/k/
    file_100000 .. file_199999
  ... (10 leaf directories, 100K files each)
```

Deep nesting (10 levels) stresses `path_lookup` and `dentry_d_lockref`
contention in the VFS layer. Each leaf directory is one cohort, giving
10 cohorts of 100K ops each - enough to measure cross-cohort pipelining
but too few to saturate the `ReorderBuffer`.

### 3.3 Realistic (simulated project tree)

```
dest/
  dir_0000/
    file_000000 .. file_000099
  dir_0001/
    file_000100 .. file_000199
  ...
  dir_9999/
    file_999900 .. file_999999
```

10,000 directories with 100 files each. This mirrors a large
source-controlled repository or a package mirror. The 10K cohort count
exercises `MAX_BUFFERED_COHORTS = 64` back-pressure: producers must
park when the buffer fills, and the consumer must drain at full speed
to avoid becoming the bottleneck.

## 4. Measurement methodology

### 4.1 Primary metrics

| Metric | Source | Unit |
|--------|--------|------|
| **Throughput** | `files_deleted / wall_clock_seconds` | ops/sec |
| **Peak RSS** | `/proc/self/status` `VmHWM` sampled by a monitor thread | MiB |
| **I/O wait fraction** | `getrusage(RUSAGE_SELF)` voluntary context switches / total time | percent |
| **Syscall count** | `strace -c` in profiling mode (not in CI tight loop) | count |
| **Wall-clock** | criterion's built-in measurement | seconds |

### 4.2 Secondary metrics (profiling mode only)

| Metric | Source |
|--------|--------|
| dentry cache hit rate | `/proc/slabinfo` `dentry` active_objs delta |
| per-thread idle time | rayon `ThreadPoolBuilder::spawn_handler` hook |
| lock contention | `Mutex::try_lock` failure counter on `SharedBatcher` |

### 4.3 Timing methodology

- **Criterion harness** for the tight loop (throughput measurement).
  Each iteration:
  1. Create the 1M-file fixture (excluded from measurement via
     `iter_batched_ref` with `BatchSize::PerIteration`).
  2. Build the `DeletePlanMap` + `DirTraversalCursor` from a synthetic
     file-list (no actual rsync transfer).
  3. Run `emit_all` (sequential) or `ParallelDeleteEmitter::run`
     (parallel).
  4. Assert file count reaches zero.

- **Warm-up:** 1 iteration (fixture creation is expensive; criterion's
  default 3-second warm-up would only complete one iteration anyway).
- **Samples:** 10 iterations per bench group. Statistical significance
  at 1M files requires fewer samples because variance is lower (law of
  large numbers on per-file unlink latency).
- **Clock:** `Instant::now()` wall-clock. Not CPU-time - I/O wait is
  part of the signal.

### 4.4 RSS sampling

A dedicated monitor thread wakes every 10 ms and reads
`/proc/self/status` for `VmHWM` (Linux) or `task_info(MACH_TASK_BASIC_INFO)`
(macOS). The peak across all samples is reported alongside throughput.
On non-Linux/macOS platforms the monitor is a no-op.

## 5. Comparison axes

### 5.1 Sequential vs parallel

| Variant | Feature flag | Description |
|---------|-------------|-------------|
| Sequential | (default) | `DeleteEmitter::emit_all` - single-threaded drain |
| Parallel | `parallel-delete-consumer` | `ParallelDeleteEmitter::run` - cohort-coordinated rayon dispatch |

### 5.2 Rayon thread counts

The parallel variant runs with explicit `ThreadPoolBuilder::num_threads`:

- **1** - parallel overhead baseline (Condvar + Mutex cost with no
  actual concurrency).
- **4** - typical laptop / CI runner core count.
- **16** - server-class box; expected sweet spot for 10K-cohort layout.
- **64** - over-subscription stress; reveals lock contention and cache
  thrashing regressions.

Each thread count is a separate criterion benchmark group so regression
detection is per-configuration.

### 5.3 Layout cross-product

The full matrix is `{seq, par-1, par-4, par-16, par-64}` x
`{flat, deep, realistic}` = 15 benchmark points.

### 5.4 Upstream rsync comparison (profiling mode)

In profiling mode (`DEL_BENCH_UPSTREAM=1`), the harness also times
upstream rsync 3.4.1 performing an equivalent delete:

```sh
rsync --delete --recursive empty_dir/ dest/
```

This gives an absolute reference for ops/sec and RSS. The comparison
is not part of the criterion harness (upstream rsync is a subprocess)
but is captured in the same results directory with hyperfine.

## 6. Harness structure

```
crates/engine/benches/
  delete_1m_benchmark.rs        # criterion groups (requires --features parallel-delete-consumer)

xtask/src/commands/benchmark/
  delete_fixture.rs             # parallel fixture generator

tools/ci/
  bench_delete_1m.sh            # orchestration script for CI workflow

.github/workflows/
  bench-delete-1m.yml           # CI workflow (non-required, nightly + path trigger)
```

### 6.1 Criterion bench file

```rust
// Pseudocode structure (not literal implementation)
fn bench_sequential_flat(c: &mut Criterion) { ... }
fn bench_sequential_deep(c: &mut Criterion) { ... }
fn bench_sequential_realistic(c: &mut Criterion) { ... }
fn bench_parallel_flat(c: &mut Criterion) { ... }  // parameterised by thread count
// ... etc for each (variant, layout) pair
```

Groups use `criterion::BenchmarkGroup::throughput(Throughput::Elements(1_000_000))`
so criterion reports ops/sec natively.

### 6.2 CI workflow

Mirrors the `bench-daemon-coldstart.yml` pattern:

- Trigger: nightly cron (06:17 UTC), path changes to
  `crates/engine/src/delete/**`, manual dispatch.
- Runner: `ubuntu-latest` with tmpfs mount.
- Build: `cargo build --release -p engine --features parallel-delete-consumer`.
- Artifact: JSON results uploaded as workflow artifact; step summary
  table with throughput and RSS per configuration.
- Non-required check; promotion tracked as DEL-4.c.

### 6.3 Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `DEL_BENCH_FS` | `tmpfs` | Filesystem type (tmpfs or ext4) |
| `DEL_BENCH_LAYOUT` | `all` | Which layout(s) to run (flat, deep, realistic, all) |
| `DEL_BENCH_THREADS` | `1,4,16,64` | Comma-separated rayon thread counts |
| `DEL_BENCH_UPSTREAM` | `0` | Enable upstream rsync comparison via hyperfine |
| `DEL_BENCH_FILE_COUNT` | `1000000` | Override file count (useful for local quick-checks) |

## 7. Pass / fail criteria

The benchmark is advisory (non-required) during bake-in. Once promoted
(DEL-4.c), the following gates apply:

| Condition | Threshold | Rationale |
|-----------|-----------|-----------|
| Parallel (4 threads, realistic) throughput vs sequential | >= 2.0x speedup | Parallel overhead must justify the complexity at scale |
| Parallel (16 threads, realistic) throughput vs sequential | >= 3.0x speedup | Diminishing returns must still yield material gain |
| Parallel (64 threads) throughput vs parallel (16 threads) | no regression (>= 0.9x) | Over-subscription must not regress |
| Sequential throughput vs DEL-4.a (100K) extrapolation | within 20% of linear scaling | Algorithmic complexity must remain O(n) |
| Peak RSS (sequential, realistic) | < 800 MiB | Memory budget for 1M-entry `DeletePlanMap` |
| Peak RSS (parallel, realistic) | < 1000 MiB | Parallel bookkeeping overhead bounded |
| Upstream rsync comparison (profiling mode) | informational only | No gate; data collection for roadmap prioritization |

## 8. Known risks and mitigations

### 8.1 CI runner inode limits

GitHub-hosted runners use ext4 with a default inode ratio that may cap
below 1.2M inodes on smaller disk images. Mitigation: the workflow
mounts a dedicated tmpfs with explicit `nr_inodes=1200000`. If the
runner's `/dev/shm` is too small, the workflow falls back to a
loopback ext4 image with `mkfs.ext4 -N 1500000`.

### 8.2 Fixture teardown time

After the benchmark, deleting the 1M-file fixture tree for cleanup can
itself take 5-10 seconds. The harness uses `rm -rf` in a background
process with a 60-second timeout rather than blocking the criterion
iteration loop.

### 8.3 Variance from dentry cache state

Cold-cache vs warm-cache unlink latency differs by 2-3x. The harness
drops the dentry cache between iterations on Linux
(`echo 3 > /proc/sys/vm/drop_caches` when running as root, skipped in
non-root CI). Results sections note whether cache drops were active.

### 8.4 tmpfs vs real filesystem divergence

tmpfs `unlink` is ~5x faster than ext4 `unlink` because it skips the
journal commit. The CI harness measures tmpfs for reproducibility; the
profiling mode documents the ext4 multiplier so operators can
extrapolate to production scenarios.

## 9. Relationship to other DEL tasks

| Task | Relationship |
|------|-------------|
| DEL-1.a/b/c | Design specs this bench validates at scale |
| DEL-2.a | `ReorderBuffer` - buffer occupancy is a primary metric |
| DEL-2.c | `ParallelDeleteEmitter` - the subject under test |
| DEL-3 | Wire-byte parity gate - must pass before this bench is meaningful |
| DEL-4.a | 100K predecessor; shares fixture-generation code |
| DEL-4.c | Promotion of this bench to a required CI check |

## 10. Implementation plan

1. **DEL-4.b.1** - Fixture generator in xtask (parallel file creation,
   layout selection, inode pre-check).
2. **DEL-4.b.2** - Criterion harness with sequential-only groups
   (validates fixture and measurement before parallel wiring).
3. **DEL-4.b.3** - Parallel groups parameterized by thread count;
   requires `parallel-delete-consumer` feature.
4. **DEL-4.b.4** - RSS monitor thread integrated into criterion custom
   measurement.
5. **DEL-4.b.5** - CI workflow (`bench-delete-1m.yml`) with tmpfs mount
   and artifact upload.
6. **DEL-4.b.6** - Upstream rsync comparison arm (hyperfine, profiling
   mode only).
