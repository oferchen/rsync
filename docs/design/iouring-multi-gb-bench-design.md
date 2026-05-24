# Multi-GB io_uring bench cell design (IUB-2)

Tracking: IUB-2 (#2855). Implementation lands under IUB-4 (decomposed).
Predecessor: IUB-1 (#2854 / PR #4879) inventory at
`docs/audit/iouring-bench-workload-inventory.md`.

## Goal

Validate the workload-scale hypothesis for io_uring on the file-data
path. Existing bench cells (per IUB-1) top out at 10 GiB across 10 files
and are env-gated; the headline release bench at 148 MB / 10 K files
shows ~1.00x vs stdlib (memory note
`project_iouring_marginal_at_small_bench_scale`). The hypothesis is that
io_uring's per-op cost is fixed but its queue-depth and batching wins
scale with payload, so a 2 GB / 10 GB / 50 GB single-file workload on
NVMe should expose a real speedup vs the stdlib pwrite/read loop.

A negative result is also useful: if the 2 GB cell shows < 1.05x we
should reconsider the scope of io_uring in oc-rsync rather than ship a
backend the bench grid does not justify.

## Non-goals

- No file-count scaling (that is IUB-3's 100 K-file design).
- No SQPOLL vs non-SQPOLL sweep (existing `iouring_sqpoll_vs_regular.rs`
  covers that shape; the new cells run with the default ring config so
  results map directly to a real-world transfer).
- No SEND_ZC / network-path work (covered by `ius_3_send_zc_vs_send.rs`
  and IUS-3 design).
- No reflink / copy_file_range comparison (handled by
  `per_op_thresholds.rs` and `platform_copy*.rs`).

## Workload spec

One bench file per cell-group lives at
`crates/fast_io/benches/iouring_multi_gb.rs` (added under IUB-4). Each
cell measures a single transfer of one file. The bench fixture is
pre-generated; the bench body times the transfer only.

| cell-id | size | mode | env gate | host requirement |
|---------|------|------|----------|------------------|
| `iouring_multi_gb/stdlib_read/2GiB` | 2 GiB | sender (read) | `OC_RSYNC_BENCH_IOURING_RING=1` | Linux 5.6+, NVMe, >= 8 GiB free |
| `iouring_multi_gb/iouring_read/2GiB` | 2 GiB | sender (read) | `OC_RSYNC_BENCH_IOURING_RING=1` | Linux 5.6+, NVMe, >= 8 GiB free |
| `iouring_multi_gb/stdlib_write/2GiB` | 2 GiB | receiver (write) | `OC_RSYNC_BENCH_IOURING_RING=1` | Linux 5.6+, NVMe, >= 8 GiB free |
| `iouring_multi_gb/iouring_write/2GiB` | 2 GiB | receiver (write) | `OC_RSYNC_BENCH_IOURING_RING=1` | Linux 5.6+, NVMe, >= 8 GiB free |
| `iouring_multi_gb/stdlib_read/10GiB` | 10 GiB | sender | `OC_RSYNC_BENCH_IOURING_RING=1` + `OC_RSYNC_BENCH_LARGE=1` | Linux 5.6+, NVMe, >= 30 GiB free |
| `iouring_multi_gb/iouring_read/10GiB` | 10 GiB | sender | same | same |
| `iouring_multi_gb/stdlib_write/10GiB` | 10 GiB | receiver | same | same |
| `iouring_multi_gb/iouring_write/10GiB` | 10 GiB | receiver | same | same |
| `iouring_multi_gb/stdlib_read/50GiB` | 50 GiB | sender | `OC_RSYNC_BENCH_IOURING_RING=1` + `OC_RSYNC_BENCH_LARGE=1` + `IUB_50GB=1` | Linux 5.6+, NVMe, >= 120 GiB free, NOT a ramdisk |
| `iouring_multi_gb/iouring_read/50GiB` | 50 GiB | sender | same | same |
| `iouring_multi_gb/stdlib_write/50GiB` | 50 GiB | receiver | same | same |
| `iouring_multi_gb/iouring_write/50GiB` | 50 GiB | receiver | same | same |

Notes on shape:

- **Single file** is deliberate. The IUB-1 inventory already covers
  many-file shapes (`iouring_per_file_vs_shared`,
  `iouring_sqpoll_vs_regular`). The hypothesis under test here is
  payload size, with file count fixed at 1 so queue depth and large
  contiguous I/O dominate.
- **Random fill** via `/dev/urandom` (Linux) so that any downstream
  compression in the rsync pipeline does not collapse the working set
  and skew results. The fixture is generated once and reused across
  iterations; only the transfer is timed.
- **Sender + receiver** modes per size keep the bench symmetric so we
  can see whether io_uring's payoff is asymmetric (e.g. writes win
  bigger because of fsync ordering, or reads win bigger because of
  fixed-buffer registration).
- The 50 GiB cell is `IUB_50GB=1` gated and refuses to run on a host
  that cannot satisfy the disk-free precondition - see Pre-flight
  checks below.

## Bench harness

**Recommendation: criterion**, not hyperfine.

Rationale:

- All existing io_uring cells (IUB-1 inventory rows 1-31) are criterion
  benches inside the workspace, gated on Linux + the `io_uring` feature.
  Adding criterion cells keeps the harness uniform: shared
  `--save-baseline` flow, shared HTML reports, shared filter syntax,
  and `scripts/benchmark_io_optimizations.sh` already knows how to drive
  them.
- Criterion's warm-up + statistical sampling is well suited to read/
  write latency where outliers from page-cache state matter. Hyperfine
  is designed for whole-process wall-time and would force a per-iter
  process spawn that pollutes the measurement at GB scale.
- Criterion lets us tag throughput per iter
  (`bench.throughput(Throughput::Bytes(size_bytes))`) so we get the
  MB/s metric the acceptance criteria require, without a separate
  post-processor.

Bench module layout:

```text
crates/fast_io/benches/iouring_multi_gb.rs
    fn read_cells(c: &mut Criterion) { ... }     # 2/10/50 GiB sender
    fn write_cells(c: &mut Criterion) { ... }    # 2/10/50 GiB receiver
    criterion_group!(...);
    criterion_main!(...);
```

The bench checks env gates up front and exits clean (no panic) if a
precondition is missing.

## Disk-class assumption

**NVMe required.** io_uring's queue depth and `IOSQE_ASYNC` only matter
when the device can sustain dozens of in-flight 4-128 KiB blocks; on
SATA SSD the queue collapses to ~32 outstanding and on HDD to ~1, so
the bench cannot distinguish io_uring from a pwrite loop.

Documented behaviour by disk class:

| disk class | 2 GiB expected | 10 GiB expected | 50 GiB expected |
|------------|----------------|-----------------|-----------------|
| NVMe (PCIe Gen3+) | > 1.1x speedup | > 1.3x | > 1.5x |
| SATA SSD | ~1.05x | ~1.1x | ~1.15x |
| HDD | ~1.00x | ~1.00x | ~1.00x |
| tmpfs / ramdisk | ~1.00x (no I/O wait to overlap) | n/a (size) | n/a (size) |

`tempfile::TempDir` on CI runners often lands on tmpfs - results from
such runs are not meaningful for the IUB-2 hypothesis and MUST be
discarded. The fixture-path selection (see Setup below) refuses tmpfs.

## Setup

Per-cell fixture generation runs once per benchmark invocation and is
amortised across criterion's iter loop. Generation is deterministic
under a fixed seed so re-runs hit the same bytes.

1. **Fixture path resolution.**
   - Use `OC_RSYNC_BENCH_NVME_PATH` if set; verify it is on a non-tmpfs
     filesystem via `statfs::f_type != TMPFS_MAGIC (0x01021994)`. If
     it is tmpfs, refuse with a printed warning and `return` (criterion
     skips the group cleanly).
   - Otherwise fall back to `std::env::temp_dir()` but only if
     `statfs::f_type` indicates a block-backed FS; otherwise refuse.
2. **Disk-free precondition.** Call `statvfs` on the fixture parent.
   Require `f_bavail * f_frsize >= 4 * size_bytes` (4x headroom for
   the source file, the destination file, criterion temp state, and
   page cache pressure). Below threshold, skip the cell.
3. **Generate fixture.** For `size` in `{2, 10, 50}` GiB:
   ```sh
   dd if=/dev/urandom of=$FIXTURE bs=1M count=$((size_mb)) iflag=fullblock
   ```
   In-bench equivalent: open `/dev/urandom`, loop with a 1 MiB buffer,
   write through a `BufWriter<File>`, `fdatasync()` at end. Skip on
   non-Linux hosts (urandom semantics differ).
4. **Cold-cache enforcement between runs.** Before each criterion
   sample, drop page cache for the fixture path:
   ```sh
   sync && echo 3 | sudo tee /proc/sys/vm/drop_caches
   ```
   In-bench equivalent: open the fixture with
   `OFlag::O_DIRECT` set on a probe handle to force readback (fallback
   when not root), or call
   `posix_fadvise(fd, 0, 0, POSIX_FADV_DONTNEED)` per iter. We pick
   `posix_fadvise` because it does not require root, works on every
   supported kernel, and is the same trick `dd iflag=nocache`
   uses internally.
5. **Warm-up amortisation.** Criterion's default warm-up runs at least
   one full transfer; at 50 GiB that is roughly 30 s on a Gen3 NVMe.
   Override warm-up to a single iteration via
   `Criterion::default().warm_up_time(Duration::from_secs(2))` and let
   the sample loop dominate; set `sample_size(10)` for the 10 GiB and
   `sample_size(5)` for the 50 GiB cells so the run finishes inside
   the IUB-4 wall-clock budget (target: < 30 min for the full
   gated grid).

## Tear-down

- Each criterion group registers a `Drop`-implementing fixture guard
  that `unlink()`s the source and destination paths even on panic.
- Because 50 GiB delete on a flash-translation-layer SSD can stall the
  device under TRIM pressure, the guard calls `posix_fadvise(...,
  POSIX_FADV_DONTNEED)` before `unlink()` to flush page cache first,
  then issues `unlink()` once per file (no recursive walk - the bench
  owns exactly two files per cell).
- Bench drivers should run `sync` after the bench completes; documented
  in the IUB-4 README block for `iouring_multi_gb.rs`.

## Metrics

Three metrics per cell:

1. **Throughput (MB/s)** - criterion's
   `Throughput::Bytes(size_bytes)` divided by sample mean. Reported
   directly in the bench output and the criterion HTML.
2. **Syscall count** - captured via an out-of-band run with
   `strace -c -e trace=read,write,pread64,pwrite64,readv,writev,io_uring_enter,io_uring_setup,io_uring_register,fsync,fdatasync`
   wrapping the bench. The IUB-4 implementation ships a sibling
   shell helper at `scripts/iouring_multi_gb_strace.sh` that runs each
   cell once with strace and dumps the summary into
   `target/iub-2-strace/{cell}.txt`. The bench itself does not invoke
   strace - that would invalidate the timing data.
3. **Wall time** - criterion's mean sample time. Used to cross-check
   throughput and detect outlier samples (e.g. background scrub).

Reported as a 3-column table in the IUB-4 results doc (one row per cell,
columns: throughput / syscalls / wall). Speedup ratios are computed
post-hoc as `iouring_* / stdlib_*` for each size and direction.

## Acceptance criteria

The IUB-4 results doc passes review iff:

- **2 GiB:** `iouring_* / stdlib_* >= 1.10x` on at least one of read or
  write, on NVMe, with the syscall count strictly lower for the
  io_uring variant. A result of `< 1.05x` for both directions is a
  **scope-reconsideration trigger** - file a follow-up to disable the
  io_uring data path by default and demote it to opt-in.
- **10 GiB:** `iouring_* / stdlib_* >= 1.30x` on at least one of read
  or write, on NVMe.
- **50 GiB:** `iouring_* / stdlib_* >= 1.50x` on at least one of read
  or write, on NVMe. A 50 GiB cell that comes in below 1.20x means
  the queue-depth ceiling is the bottleneck, not payload - log it
  and move to per-thread rings work (memory note
  `project_io_uring_shared_ring_bottleneck`).
- **Syscall counts:** io_uring cells must show strictly fewer
  `read/write/pread/pwrite` entries than stdlib cells (replaced by
  `io_uring_enter` / `io_uring_setup` / `io_uring_register`). If
  io_uring shows higher total syscalls, the bench is mis-configured
  (likely missing fixed-buffer registration).
- **SATA / HDD runs:** advisory only. Do not block IUB-4 merge on
  speedups that the disk class cannot deliver.

## Rollback

If the bench reveals io_uring is a regression at any tested size, the
following opcodes/paths are the candidates to disable, in order of
disable cost:

1. **Read path regression** (`iouring_read/*` slower than
   `stdlib_read/*`):
   - Disable `IORING_OP_READ_FIXED` and `IORING_OP_READV` dispatch in
     `crates/fast_io/src/iouring/` (search for `Op::ReadFixed`).
     Fall back to `pread64` loop.
   - ADR pointer: `docs/design/iouring-receive-data-path.md`.
2. **Write path regression** (`iouring_write/*` slower than
   `stdlib_write/*`):
   - Disable `IORING_OP_WRITE_FIXED` and `IORING_OP_WRITEV` dispatch
     (search for `Op::WriteFixed`).
   - Keep `IORING_OP_FSYNC` if it is still a win on its own.
   - ADR pointer: `docs/design/iouring-send-data-path.md`.
3. **Both directions regression** at 2 GiB but win at 10 GiB:
   - Add a payload-size guard in `fast_io::dispatch` that routes
     `< 4 GiB` total file size to stdlib and `>= 4 GiB` to io_uring.
   - ADR pointer: `docs/design/io-strategy-trait.md`.
4. **All sizes regress** (worst case):
   - Demote the io_uring data path to `--io-uring=force` opt-in only.
     Default builds use stdlib.
   - ADR pointer: `docs/design/init-time-backend-selection.md`.

Each rollback option is reversible; IUB-4 results determine which (if
any) we exercise.

## Open questions

- Should the 50 GiB cell ever run in CI, or only on a dev host? Current
  recommendation: dev host only, results pasted into the IUB-4 PR
  description.
- Do we need a `--checksum` variant of the bench to expose the
  XXH3-on-read code path? IUB-2 scope says no; consider as IUB-5
  follow-up if the io_uring read win at 10 GiB is marginal and
  checksum overhead is suspect.
- `posix_fadvise(POSIX_FADV_DONTNEED)` vs `drop_caches`: the bench
  uses fadvise to avoid sudo. If results show residual cache hits,
  IUB-4 may need a `drop_caches` opt-in via `IUB_DROP_CACHES=1` for
  bare-metal runs.
