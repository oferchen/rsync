# io_uring Submission Modes Benchmark Plan (#1626)

Tracking issue: oc-rsync task #1626. This is a documentation-only design
note; no code lands in this PR.

Sibling design notes and audits:

- `docs/design/iouring-session-ring-pool.md` (#1408 / #1409) - the
  session-level ring pool the SQPOLL mode is layered on top of.
- `docs/design/basis-file-io-policy.md` - keeps mmap pointers out of the
  io_uring data path; that invariant is assumed here.
- `docs/audits/iouring-socket-sqpoll-defer-taskrun.md` (#1267, #1622) -
  the original SQPOLL audit; concluded SQPOLL is a poor default for the
  daemon socket path. Kept as evidence for the kernel-thread cost model.
- `docs/audits/io_uring_sqpoll_mmap_pagefault.md` - SQPOLL kernel poller
  cannot fault user pages; relevant to the workload mix in section 2.
- `docs/audits/iouring-pbuf-ring.md` (#2043) - PBUF_RING (kernel 5.19+)
  evaluation; informs the kernel matrix in section 4.
- `docs/audits/disk-commit-iouring-batching.md` - disk-commit batching
  shape that the receiver path benchmarks must reproduce.

## 1. Goal

Quantify the end-to-end and microbenchmark difference between three
submission strategies on Linux for oc-rsync's hot disk-I/O paths:

- **(a) Regular submission with `submit_and_wait`.** The current default.
  Userspace pushes SQEs to the submission queue tail and calls
  `io_uring_enter(2)` with `IORING_ENTER_GETEVENTS` to advance the
  kernel-side head pointer and block until at least N completions arrive
  (`crates/fast_io/src/io_uring/file_writer.rs:190`,
  `crates/fast_io/src/io_uring/file_reader.rs:124`,
  `crates/fast_io/src/io_uring/file_reader.rs:248`,
  `crates/fast_io/src/io_uring/disk_batch.rs:253`,
  `crates/fast_io/src/io_uring/registered_buffers.rs:552`,
  `crates/fast_io/src/io_uring/registered_buffers.rs:672`,
  `crates/fast_io/src/io_uring/socket_reader.rs:61`,
  `crates/fast_io/src/io_uring/socket_reader.rs:98`,
  `crates/fast_io/src/io_uring/batching.rs:109`,
  `crates/fast_io/src/io_uring/batching.rs:213`,
  `crates/fast_io/src/io_uring/batching.rs:336`).

- **(b) SQPOLL with kernel-side polling.** `IORING_SETUP_SQPOLL` spawns a
  kernel thread that continuously polls the submission queue tail. After
  startup, userspace updates the SQ tail without an `io_uring_enter`
  syscall on the submit side; the kernel poller picks the SQE up and
  drives it. Wired today via `IoUringConfig::sqpoll`
  (`crates/fast_io/src/io_uring/config.rs:311`) with idle-timeout
  `sqpoll_idle_ms` (`config.rs:314`). Setup attempts
  `setup_sqpoll(idle_ms)` then falls back to a regular ring on
  `EPERM`/`ENOMEM` (`config.rs:381`-`config.rs:396`), recording the
  fallback in `SQPOLL_FALLBACK` (`config.rs:30`) which is observable via
  `sqpoll_fell_back()` (`config.rs:45`).

- **(c) Standard read/write/sync syscalls (the fallback path).** The
  factory layer at `crates/fast_io/src/io_uring/mod.rs:151`-`mod.rs:168`
  and `mod.rs:208`-`mod.rs:218` degrades to `BufReader` / `BufWriter`
  via `crate::traits::StdFileReader` / `StdFileWriter` whenever
  `is_io_uring_available()` is false or `build_ring()` errors. This is
  the baseline every modern kernel can reach, and the only path
  available on macOS, Windows, FreeBSD, kernels older than 5.6, and
  inside containers whose seccomp profile blocks `io_uring_setup(2)`
  (mod.rs:11-13, mod.rs:65-66).

The benchmark answers three questions: how much submission overhead
`submit_and_wait` actually costs versus an SQPOLL kernel thread; how
much both io_uring modes save over the fallback `read(2)`/`write(2)`
loop on identical workloads; and whether SQPOLL's CPU spin cost erodes
the savings on bursty or idle traffic patterns. The output drives the
default-policy decision in section 7.

## 2. Workload Mix

Three workloads, each run sequentially and in parallel. Sequential mode
uses one transfer at a time; parallel mode uses
`rayon::current_num_threads()` concurrent transfers, mirroring the
shape of `crates/transfer/src/parallel_io.rs` and the work-queue drain
in `crates/engine/src/concurrent_delta/work_queue/drain.rs`.

| Workload | Files | Per-file size | Total | Rationale |
|---|---|---|---|---|
| Small-file flood | 100,000 | ~4 KB random | ~400 MB | Stresses ring construction, fd registration, SQE push rate, per-completion overhead. Submission-side pressure dominates; this is where SQPOLL's no-syscall submit should pay off. |
| Single 1 GB file | 1 | 1 GB sequential | 1 GB | Stresses sustained throughput with `WRITE_FIXED` / `READ_FIXED` against `RegisteredBufferGroup` (`crates/fast_io/src/io_uring/registered_buffers.rs:69`-`registered_buffers.rs:110`). One ring, deep submission queue, completion-side dominated. |
| 10x100 MB files | 10 | 100 MB each | 1 GB | Mid-range: balances per-file open cost and steady-state batched I/O. Closest to typical oc-rsync release-tarball / log-archive transfers. |

For each workload, run two configurations:

- **Sequential.** One worker drives the transfer end to end. Measures
  per-op latency without contention.
- **Parallel.** `min(num_cpus, 4)` rayon workers drive disjoint slices.
  The shared-ring path exercises the session ring pool design from
  #1408 / #1409 (`docs/design/iouring-session-ring-pool.md`); the
  per-worker-ring path exercises model A from
  `docs/design/io-uring-rayon-composition.md` (where present in the
  active branch). The parallel run is the load case where SQPOLL's
  kthread-per-ring cost matters most.

Each mode (a / b / c) runs the full grid: 3 workloads x 2 concurrency
modes = 6 cells per mode, 18 cells total.

## 3. Privilege Matrix

The privilege matrix exists because SQPOLL is gated behind
`CAP_SYS_NICE` on every Linux release this benchmark targets
(`crates/fast_io/src/io_uring/mod.rs:67`-`mod.rs:67`). The CAP_SYS_NICE
audit and runtime check follow-ups (#1621, #1622, #1623) settled how
the fallback is detected and logged; the benchmark must measure each
of the following privilege contexts:

| Context | CAP_SYS_NICE | Expected behaviour |
|---|---|---|
| Privileged host | present (root or `setcap cap_sys_nice+ep`) | SQPOLL ring builds successfully; `sqpoll_fell_back()` returns `false` (`config.rs:45`). |
| Unprivileged host | absent | `IoUringConfig::build_ring()` retries without `setup_sqpoll`; `SQPOLL_FALLBACK` flips to `true` (`config.rs:381`-`config.rs:396`). The benchmark must record both the requested mode and the effective mode for every run. |
| Unprivileged container | absent (and seccomp may further block io_uring_setup) | Per #1624, the row that has not been measured. The benchmark must run inside the `localhost/oc-rsync-bench:latest` container without `--cap-add=SYS_NICE`, plus a second pass inside `rsync-profile` (rust:latest, Debian) without privileges. The probe at `crates/fast_io/src/io_uring/config.rs:271`-`config.rs:281` distinguishes "kernel too old", "syscall blocked", and "available" - the benchmark records which branch the probe took. |

The unprivileged-container row is the one #1624 leaves open. Without
that row, the SQPOLL recommendation in section 7 is over-fitted to a
privileged developer laptop, which is not the typical oc-rsync
deployment. The bench script must explicitly assert the SQPOLL state
seen by the process matches the state expected by the matrix row, and
must abort if `sqpoll_fell_back()` returns `true` in a row that
expects `false`.

## 4. Kernel Matrix

io_uring's opcode and feature surface evolves per minor kernel
release. The benchmark spans five points covering the gates that
matter for oc-rsync:

| Kernel | Feature gates crossed | Notes |
|---|---|---|
| 5.6 | Minimum (`MIN_KERNEL_VERSION`, `crates/fast_io/src/io_uring/config.rs:19`); `IORING_OP_READ`, `IORING_OP_WRITE`, `IORING_OP_SEND`, `IORING_REGISTER_FILES`, `IORING_SETUP_SQPOLL` | Floor of the supported set. SQPOLL works but lacks shared kthread mode (added 5.11). The probe at `config.rs:271`-`config.rs:281` returns `Available` here. |
| 5.11 | `IORING_SETUP_ATTACH_WQ`; SQPOLL shared-kthread; `CAP_SYS_NICE` requirement formalised | First version where multiple SQPOLL rings can share a single kernel thread. Reduces oversubscription at the kernel level when the session ring pool from #1409 holds more than one SQPOLL ring. |
| 5.15 | `IORING_OP_LINKAT`, `IORING_OP_RENAMEAT`, `IORING_OP_SYMLINKAT`, `IORING_OP_MKDIRAT`, `IORING_OP_UNLINKAT` | Receiver-side metadata ops for rename-into-place commit. The disk-commit audit (`docs/audits/disk-commit-iouring-batching.md`) targets this generation. |
| 6.0 | `IORING_OP_SEND_ZC` (zero-copy send), `IORING_RECVSEND_POLL_FIRST` | First kernel where the daemon socket path can plausibly skip a copy on send. Out of scope for this disk-only benchmark, but the kernel must still be tested because the receiver writes to disk while the sender path reads from it. |
| 6.6 | `IORING_REGISTER_PBUF_RING` (5.19); `IORING_SETUP_DEFER_TASKRUN` (6.1); `IORING_SETUP_SINGLE_ISSUER`; multishot accept/recv; mature task-work batching | The recommended target per the SQPOLL/DEFER_TASKRUN audit. PBUF_RING (#2043) lives here; the audit at `docs/audits/iouring-pbuf-ring.md` rules out PBUF_RING for the positional file reader (`file_reader.rs:30`) but flags it for stream paths. |

Kernel 5.19 (PBUF_RING) is intentionally subsumed by 6.6 since
PBUF_RING is not exercised by the file-I/O benchmark; the disk-only
data path uses `READ_FIXED` / `WRITE_FIXED` against the existing
`RegisteredBufferGroup`. If a follow-up #1626 phase-2 benchmarks the
socket path (`crates/fast_io/src/io_uring/socket_reader.rs`), 5.19
becomes a separate row.

The kernel matrix is realised via QEMU images keyed by version
suffix; the bench script consumes the image name and asserts
`uname -r` matches before running the workload. Kernel-mismatch is a
hard error: a 5.11 SQPOLL run executed on a 5.6 kernel produces
misleading numbers.

## 5. Existing Bench Harness Reuse

Two existing harnesses cover most of the fixture work; this benchmark
extends them rather than introducing a new one.

- `crates/fast_io/benches/io_optimizations.rs:156`-`io_optimizations.rs:223`
  - the `bench_io_uring` Criterion group already iterates 64 KB / 1 MB /
  10 MB read sizes, gates on `is_io_uring_available()`, and hoists ring
  allocation outside the inner iteration loop to avoid `RLIMIT_MEMLOCK`
  exhaustion (`io_optimizations.rs:195`-`io_optimizations.rs:202`). The
  comment block at `io_optimizations.rs:253`-`io_optimizations.rs:262`
  documents the same MEMLOCK lesson for writes. The submission-modes
  bench reuses this scaffolding and adds: a third sub-group keyed by
  `IoUringConfig::sqpoll = true`, a fourth keyed by the std-I/O
  fallback (already present implicitly via the `standard_io` baseline
  at `io_optimizations.rs:178`-`io_optimizations.rs:193` and
  `io_optimizations.rs:242`-`io_optimizations.rs:251`), and the per-mode
  metrics in section 6.
- `crates/fast_io/benches/platform_copy.rs` and
  `crates/fast_io/benches/splice_pipe.rs` - covered by the same
  Criterion harness; not extended for this work but used to confirm
  that platform-specific copy paths (sendfile, splice, copy_file_range)
  are not silently activated when io_uring is the path under test.
- `scripts/benchmark_io_optimizations.sh` - the kernel-version
  preflight and the `--save-baseline phase1_optimizations` invocation
  pattern at line 90 give the bench script its outer shell. The
  submission-modes script extends this with a privilege check
  (`capsh --print | grep cap_sys_nice`) before each SQPOLL run.
- `scripts/benchmark.sh`, `scripts/benchmark_hyperfine.sh`,
  `scripts/benchmark_remote.sh`, and the container image
  `localhost/oc-rsync-bench:latest` - end-to-end transfer harness.
  Used for the small-file-flood and 10x100 MB workloads where
  Criterion's per-iteration timing is too coarse for a transfer that
  involves rsync's full pipeline (file-list, signature, delta, commit).

The Criterion harness covers the microbenchmark layer; the shell
scripts cover the end-to-end layer. The submission-modes plan needs
both because the per-op syscall count is a microbench question and
the wall-clock gain on a 100,000-file transfer is an end-to-end
question, and they do not always agree.

## 6. Per-Mode Metrics

Each cell in the workload x privilege x kernel grid is evaluated
against the same metric set. Numbers below are illustrative only; the
benchmark records observed values, not target values.

- **Submission rate.** Average and p99 SQEs pushed per second on the
  submitting thread. For mode (a) this is bounded by the
  `io_uring_enter` syscall rate; for mode (b) it is bounded by
  userspace's tail-pointer write rate plus memory-barrier cost; for
  mode (c) it does not apply (no SQEs). Captured by instrumenting the
  submit path in `crates/fast_io/src/io_uring/file_writer.rs`-
  `crates/fast_io/src/io_uring/file_reader.rs` with a thread-local
  counter that does not perturb timing.
- **Completion latency p50 / p99.** Time from SQE push (or `write(2)`
  call for mode c) to the corresponding CQE observed by userspace (or
  `write(2)` return). Per-iteration histogram via `hdrhistogram` or
  Criterion's existing `BenchmarkId` plus `Throughput::Bytes`. The
  p99 tail is the more interesting number for SQPOLL: a hot poller
  delivers low p50 but its idle-wakeup behaviour shows up at p99.
- **Syscall count via `strace -c`.** Per-mode hard syscall budget on
  the same workload. The script runs the workload three times: once
  unwrapped for timing, once under `strace -c -f -o syscall.tsv` for
  the count breakdown, and once under `perf stat -e syscalls:sys_enter_io_uring_enter`
  for the io_uring-enter rate. The submission-modes split shows up
  here: mode (a) emits one `io_uring_enter` per submit batch; mode
  (b) emits zero on the steady-state submit path and only on
  ring-init / shutdown; mode (c) emits one `read`/`write` per
  buffer.
- **CPU usage.** `perf stat -e task-clock,cpu-cycles` for the
  submitting thread and a separate sample for the SQPOLL kthread when
  applicable. SQPOLL's hidden cost is the kthread spin: a benchmark
  that measures only userspace CPU under-counts mode (b) by exactly
  the kthread cost.
- **Kernel-thread idle wakes (mode b only).** `cat /proc/$(pidof
  iou-sqp-*)/status | grep voluntary_ctxt_switches` before and after
  the run. A high delta indicates the poller went to sleep and woke
  often, which is the worst case for SQPOLL.
- **End-to-end wall clock.** From `hyperfine` on the small-file flood
  and 10x100 MB workloads. Five runs, geometric mean reported, IQR
  reported alongside.
- **Fallback observability.** For every run, the script asserts the
  effective mode by reading `sqpoll_fell_back()` and the io_uring
  probe result. A run where the requested mode is (b) but
  `sqpoll_fell_back()` is `true` is logged as mode (a) with a
  fallback annotation, not silently rolled into the (b) row.

## 7. Decision Criteria

The decision the benchmark drives is the default value of
`IoUringConfig::sqpoll` in `crates/fast_io/src/io_uring/config.rs:336`,
currently `false`. Three outcomes are possible:

1. **Keep regular submission as the default.** Choose this if SQPOLL's
   end-to-end gain over regular submission is below 5% on the
   small-file flood and within the noise floor on the 1 GB and
   10x100 MB cases, OR if the unprivileged-container row in section 3
   shows the fallback path takes over for the majority of plausible
   deployments. The audit at
   `docs/audits/iouring-socket-sqpoll-defer-taskrun.md` already argues
   this position for the daemon socket path; this benchmark confirms
   or refutes it for the disk-I/O path. Today's default reflects this
   stance.
2. **Switch to SQPOLL by default on privileged hosts.** Choose this if
   SQPOLL beats regular submission by >= 15% wall clock on at least
   two of the three workloads, AND `sqpoll_fell_back()` correctly
   captures the unprivileged case so that the silent-degradation
   risk is bounded, AND the SQPOLL kthread CPU cost on the parallel
   runs does not exceed 10% over the regular-submit baseline.
3. **Adopt DEFER_TASKRUN as a middle ground.** The
   `IORING_SETUP_DEFER_TASKRUN` (kernel 6.1+) flag covered by the
   sibling audit at `docs/audits/iouring-socket-sqpoll-defer-taskrun.md`
   reduces per-completion task-work overhead without the
   `CAP_SYS_NICE` requirement. If the benchmark shows DEFER_TASKRUN
   recovers most of the SQPOLL gain on kernels >= 6.1 with no
   privilege cost, the default becomes DEFER_TASKRUN on supported
   kernels and regular submission elsewhere. This option is out of
   scope for #1626 (it lives in the sibling audit) but the benchmark
   harness must record kernel >= 6.1 timings cleanly so the follow-up
   has data to act on.

The decision is per-default, not per-workload; oc-rsync ships one
default and exposes `--io-uring-sqpoll` as a CLI override only if the
benchmark identifies a clear workload class that benefits from a
non-default choice.

## 8. Run Instructions and Expected Duration

Microbenchmark layer (Criterion), from the repo root on Linux 5.6+:

```text
cargo bench -p fast_io --features io_uring -- \
    --save-baseline submission_modes_1626 io_uring
```

The bench filter matches the existing `io_uring` and `io_uring_writes`
groups plus the new `io_uring_sqpoll` and `io_uring_stdio_baseline`
groups added under this plan. Runtime: ~12 minutes per kernel-privilege
cell on `localhost/oc-rsync-bench:latest`.

End-to-end layer (hyperfine):

```text
scripts/benchmark_hyperfine.sh \
    --workloads small-file-flood,single-1gb,ten-100mb \
    --modes regular,sqpoll,stdio \
    --concurrency seq,parallel \
    --output target/bench/submission_modes_1626.json
```

Runtime: ~25 minutes per cell, ~1.5 hours per workload across modes
and concurrency. The container row (#1624) reuses the same script
inside `localhost/oc-rsync-bench:latest` and `rsync-profile` without
`--cap-add=SYS_NICE` so the SQPOLL fallback exercises in the same
configuration production hits. Total grid (5 kernels x 3 privileges
= 15 cells x 1.5 hours) is ~22 hours per machine plus ~3 hours of
microbenches; ~5-7 days serially or ~2 days fanned across the kernel
matrix images via `benchmark_remote.sh`.

## 9. Cross-Reference: Registered-Buffer Adaptive Sizing (#2045)

The registered-buffer pool (`IORING_REGISTER_BUFFERS`,
`crates/fast_io/src/io_uring/registered_buffers.rs:69`-
`registered_buffers.rs:110`) is sized today by
`IoUringConfig::registered_buffer_count` (`config.rs:326`, default 8).
The adaptive-sizing follow-up #2045 proposes growing this count under
sustained pressure and shrinking it when slots stay idle. The
submission-modes benchmark must hold this dimension fixed so that the
mode-effect signal does not get blurred by adaptive resizing noise:

- All cells in this benchmark run with `registered_buffer_count = 8`
  for the small-file workload and `registered_buffer_count = 16` for
  the 1 GB and 10x100 MB workloads, matching the existing presets at
  `IoUringConfig::for_small_files()` (`config.rs:362`) and
  `IoUringConfig::for_large_files()` (`config.rs:347`).
- The #2045 design note will reuse this benchmark's per-mode
  fixtures to evaluate adaptive sizing as a separate dimension. The
  submission-modes baseline must therefore record the registered-
  buffer slot occupancy histogram (already exposed via
  `RegisteredBufferStats`,
  `crates/fast_io/src/io_uring/registered_buffers.rs`) per cell, so
  that #2045 can read this baseline without re-running the full grid.
- Conversely, if #2045 lands first, the submission-modes benchmark
  rerun must hold the adaptive-sizing parameters fixed at the
  documented preset values for the duration of the run, even if the
  production default changes after the fact.

The session ring pool design (#1408 / #1409) also intersects this
benchmark: SQPOLL's per-ring kthread cost is multiplied by the pool
depth, so a default of `min(num_cpus, 4)` rings on a 16-core host
produces 4 SQPOLL kthreads. The benchmark records SQPOLL kthread CPU
per ring and per pool, so the pool-size choice is informed by both
metrics.

## 10. Open Questions

The plan deliberately leaves the following questions open. Each blocks
on data the benchmark itself produces:

1. **Does SQPOLL's `sqpoll_idle_ms` interact with rsync's bursty
   pattern?** The default 1000 ms in `config.rs:337` was chosen
   without measurement. A bursty workload (e.g. file-list reception
   followed by a quiet period) might benefit from a shorter idle to
   let the kthread sleep, or a longer idle to keep the poller hot
   between bursts. The benchmark sweeps `sqpoll_idle_ms` at
   {100, 500, 1000, 2000, 5000} for the small-file flood only.
2. **Does kernel 5.11+ shared-kthread SQPOLL change the cost model
   when the session ring pool runs > 1 ring?** Today's design assumes
   each ring spawns its own kthread; on 5.11+ with `IORING_SETUP_ATTACH_WQ`
   they can share. The benchmark must run with and without
   `setup_attach_wq` on 5.11+ to settle this.
3. **What is the syscall-count gap on the unprivileged-container row
   when SQPOLL silently falls back?** The fallback to mode (a) means
   the syscall count is not zero; it is exactly the regular-submission
   count. The benchmark must confirm `sqpoll_fell_back()` plus the
   syscall histogram match the regular-submit row to one decimal
   place.
4. **Does mode (c) on a tmpfs filesystem outperform mode (a) for the
   small-file flood?** Some deployments run oc-rsync against tmpfs
   for CI fixtures. tmpfs has no real I/O scheduler, which removes
   io_uring's primary advantage (overlapping disk seeks with
   userspace work). The benchmark adds a tmpfs row to the small-file
   workload only.
5. **How does the benchmark interact with `IORING_SETUP_COOP_TASKRUN`
   (kernel 5.19+, default-on in modern kernels)?** Coop-taskrun
   already collapses CQE deliveries; SQPOLL on top may produce
   diminishing returns. The benchmark records whether
   `IoUring::params().features() & IORING_FEAT_COOP_TASKRUN` was
   active for each cell.
6. **What is the appropriate p99 latency target for the unprivileged
   container row?** The privileged-host SQPOLL p99 is the floor; the
   unprivileged row inherits regular-submit p99 by construction. The
   open question is whether the gap is acceptable or whether oc-rsync
   should expose a `--prefer-stdio-when-unprivileged` policy override
   that skips io_uring entirely on container hosts. This is a
   user-experience decision the benchmark informs but does not
   answer.
7. **Should the SSH-stdio path be benchmarked in the same plan?** The
   io_uring SSH-stdio audit (#1859,
   `docs/audits/iouring-pipe-stdio.md`) is on a separate timeline, but
   if its phase-1 reader lands before this benchmark runs, the SSH
   path should join the workload mix. As written, this plan covers
   only disk I/O on the receiver side.

## 11. References

- `crates/fast_io/src/io_uring/config.rs:19` -
  `MIN_KERNEL_VERSION = (5, 6)`.
- `crates/fast_io/src/io_uring/config.rs:30`,
  `config.rs:45` - `SQPOLL_FALLBACK` atomic and `sqpoll_fell_back()`
  accessor.
- `crates/fast_io/src/io_uring/config.rs:271`-`config.rs:281` -
  `check_io_uring_reason()` probe variants.
- `crates/fast_io/src/io_uring/config.rs:311`,
  `config.rs:314`, `config.rs:326`, `config.rs:336`,
  `config.rs:347`, `config.rs:362` - SQPOLL knobs, registered-buffer
  count, default and preset configs.
- `crates/fast_io/src/io_uring/config.rs:381`-`config.rs:396` -
  `build_ring()` and the SQPOLL fallback path.
- `crates/fast_io/src/io_uring/file_reader.rs:124`,
  `file_reader.rs:248`, `file_writer.rs:190`, `file_writer.rs:381`,
  `disk_batch.rs:253`, `batching.rs:109`, `batching.rs:213`,
  `batching.rs:336`, `registered_buffers.rs:552`,
  `registered_buffers.rs:672`, `socket_reader.rs:61`,
  `socket_reader.rs:98` - every `submit_and_wait` call site mode (a)
  exercises.
- `crates/fast_io/src/io_uring/registered_buffers.rs:69`-
  `registered_buffers.rs:110` - registered-buffer allocation and
  registration.
- `crates/fast_io/src/io_uring/buffer_ring.rs:50` -
  `MIN_PBUF_RING_KERNEL = (5, 19)`.
- `crates/fast_io/src/io_uring/mod.rs:11`-`mod.rs:13`,
  `mod.rs:62`-`mod.rs:70`, `mod.rs:151`-`mod.rs:168`,
  `mod.rs:208`-`mod.rs:218` - seccomp caveat, privilege table, and
  factory fallback to std I/O.
- `crates/fast_io/benches/io_optimizations.rs:156`-
  `io_optimizations.rs:283` - existing Criterion harness extended by
  this plan.
- `scripts/benchmark_io_optimizations.sh:61`-
  `benchmark_io_optimizations.sh:90`, `scripts/benchmark_hyperfine.sh`,
  `scripts/benchmark_remote.sh`, `scripts/benchmark.sh` - end-to-end
  harness.
- `docs/design/iouring-session-ring-pool.md` (#1408 / #1409),
  `docs/design/basis-file-io-policy.md`,
  `docs/audits/iouring-socket-sqpoll-defer-taskrun.md` (#1267,
  #1622), `docs/audits/io_uring_sqpoll_mmap_pagefault.md`,
  `docs/audits/iouring-pbuf-ring.md` (#2043),
  `docs/audits/disk-commit-iouring-batching.md` - sibling notes.
