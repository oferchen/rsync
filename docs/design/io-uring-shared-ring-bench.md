# Per-File vs Shared io_uring Ring Bench Plan (#1410)

Documentation-only design note. Companion to
`docs/design/iouring-session-ring-pool.md` (#1408 / #1409) and
`docs/design/io-uring-submission-modes-bench-plan.md` (#1626). This plan
quantifies the cost of the current per-file ring topology against a
session-level shared ring on a 100K small-file flood, and locks in a
pass/fail bar for the migration.

## 1. Current io_uring Usage (Per-File Ring)

Every io_uring-backed reader/writer constructs its own `RawIoUring` via
`IoUringConfig::build_ring()`. With N files transferred in a session,
the receiver path allocates N rings serially. Citations from
`crates/fast_io/src/io_uring/`:

- `config.rs:298`, `config.rs:440`, `config.rs:451` - `build_ring()`,
  the single ring construction primitive (wraps `IoUring::builder`).
- `file_writer.rs:54` - per-file ring for streaming writes.
- `file_reader.rs:60` - per-reader ring.
- `socket_reader.rs:32`, `socket_writer.rs:32` - per-socket rings.
- `disk_batch.rs:71` - per-batch ring on the receiver disk-commit path.
- `linkat.rs:128`, `linkat.rs:196` - per-call rings for `linkat(2)`.
- `buffer_ring.rs:808,853,872,902` - probe rings for PBUF_RING.
- `registered_buffers.rs:729,739,749,759,818,843,927,987,1045,1078,...`
  - probe rings for `IORING_REGISTER_BUFFERS` capability.
- `mod.rs:97,130` - `pub mod shared_ring; pub use SharedRing` exists
  but only for one reader+writer pair, not session-wide.

Each ring costs one `io_uring_setup(2)` (~80-150 us cold), one eventfd,
optional SQPOLL kthread, and `RLIMIT_MEMLOCK` pages for the SQ/CQ
mmap. On a 100K file flood this is the dominant per-file overhead.

## 2. Shared-Ring Design

One ring lives on a session-scoped `Arc<RingPool>` (see #1409). All
file writers, file readers, and disk-commit batches lease the same
ring(s). Demux is by SQE `user_data`: high 8 bits carry an `OpTag`
(extending `shared_ring::OpTag`, `mod.rs:130`), low 56 bits carry an
op_id that indexes into a per-file slot table.

Tag layout:

```text
 63        56 55             0
 +-----------+----------------+
 |  OpTag    |   op_id (56b)  |
 +-----------+----------------+
```

New tags vs `shared_ring.rs`: `FileWrite`, `FileRead`, `DiskCommit`,
`Linkat`, `Renameat`. The op_id is allocated from a free-list owned by
the lease holder, so completion routing is O(1). Buffer registration
uses one session-wide `RegisteredBufferGroup`; fd registration recycles
slots LRU when the table fills (see pitfall in #1409 doc lines 64-72).

## 3. Bench Harness Plan (100K x 4KB Writes)

Workload: 100,000 files, ~4 KB random content each (~400 MB total).
Mirrors the small-file flood row in
`io-uring-submission-modes-bench-plan.md`. Three configs:

- **per_file_ring** (baseline): today's path, one ring per file.
- **shared_ring_1**: session pool size 1, MPMC lease.
- **shared_ring_n**: session pool size `min(num_cpus, 4)`.

Driver: extend `crates/fast_io/benches/io_optimizations.rs` with a
`bench_io_uring_shared` Criterion group that hoists ring allocation
across the inner loop (mirrors lines 195-202). End-to-end timing via
`scripts/benchmark_hyperfine.sh` against the bench container.

Captured per cell:

- **Setup cost.** `io_uring_setup(2)` count + cumulative time; from
  `perf stat -e syscalls:sys_enter_io_uring_setup` and a wrapping
  `Instant::now()` inside `build_ring()`.
- **Throughput.** Bytes/sec wall clock; `hyperfine -r 5` geomean.
- **Latency p50/p99.** Per-write `submit -> CQE` latency via
  `hdrhistogram`. Tail latency is the load-bearing metric.
- **Syscalls.** `strace -c -f` totals; `io_uring_enter` rate via
  `perf stat -e syscalls:sys_enter_io_uring_enter`.
- **Memory.** Peak RSS + `RLIMIT_MEMLOCK` consumption via
  `/proc/self/status` snapshot at the workload midpoint.

## 4. Linux Setup

- Kernel >= 6.1 (for `IORING_SETUP_DEFER_TASKRUN` and mature task-work
  batching). Bench script asserts `uname -r` >= 6.1, hard-fails
  otherwise.
- CPU pin: `taskset -c 0-3` for sequential, `0-7` for parallel.
  Disable turbo boost (`echo 1 > intel_pstate/no_turbo`) to keep the
  p99 stable across runs.
- Cgroup memory limit: `systemd-run --user --scope -p
  MemoryMax=512M -p MemorySwapMax=0` so the 100K-file working set
  cannot leak into swap and skew the tail.
- File system: ext4 on a dedicated loopback file (`fallocate -l 4G`)
  to avoid noise from the host fs. Mount with `noatime,data=ordered`.
- Container: run inside `localhost/oc-rsync-bench:latest` (Arch) and
  `rsync-profile` (Debian) so seccomp behaviour matches production.

## 5. Pass/Fail

The migration ships only if all hold on kernel >= 6.1, parallel mode:

- **p99 latency reduction >= 30%** on the 100K x 4 KB workload
  (shared_ring_n vs per_file_ring). This is the headline bar.
- p50 latency no worse than per-file ring (regression guard).
- `io_uring_setup` syscall count drops by >= 99% (ring-construction
  amortisation guard; per-file path emits ~100K, shared pool emits
  <= num_cpus).
- Throughput +/- 5% or better; a slowdown is a hard fail even if
  tail latency improves.
- No new `RLIMIT_MEMLOCK` failures inside the 512 MB cgroup.

If any bar misses, the per-file path stays default and the shared
ring lands behind a `--io-uring-ring-pool` opt-in flag (matching the
sibling stabilisation strategy in `iouring-session-ring-pool.md` line
86).
