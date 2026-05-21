# PIP-6 - End-to-end parallel-vs-sequential receive-delta bench

Date: 2026-05-21
Scope: design + harness scaffold for the production-path bench that
backs the parallel-receive-delta promotion table
Status: scaffold only; numbers capture is the follow-up
Predecessors:
- PIP-3+5 (PR #4666, merged) wired the Path B heuristic
  (`file_count > 100 || total_size > 64 MiB`) into the receiver
  dispatch site.
- BR-3i.f (#2502, completed) shipped the apply-loop-level bench at
  `crates/engine/benches/parallel_receive_delta_perf.rs`.
- BR-3j.f (#2508, pending) is the post-DashMap apply-loop re-bench.
Tracker: PIP-6 (#2569)
Related promotion doc: `docs/design/parallel-receive-delta-default-on.md`
(section 4 names this bench as the remaining numbers-capture
follow-up).

## 1. The question

> How much faster is parallel-receive-delta on production-shaped
> workloads than the sequential apply path, when the production
> heuristic gets to make the dispatch decision?

PIP-3+5 already shipped the dispatch heuristic. The promotion table
in `docs/design/parallel-receive-delta-default-on.md` section 4 is
still empty because the apply-loop bench (BR-3i.f) measures the
scheduling cost in isolation - in-memory sinks, no real disk I/O,
no protocol framing, no checksum negotiation, no rayon pool warmup
amortised against actual transfer setup. The numbers from BR-3i.f
are necessary to gate the design (apply-loop is a winner) but not
sufficient to gate the **production** claim that the heuristic is
the right shape for the receivers operators actually run.

PIP-6 fills the gap by driving a real `oc-rsync` sender against a
real `oc-rsync` receiver over `rsync://` loopback, on a workload
matrix calibrated to the heuristic's decision boundary. Wall-clock
is the headline metric; the comparison is **same workload, parallel
vs sequential build**, not parallel-build vs upstream-rsync.

## 2. Why three benches, not one

The three benches in the parallel-receive-delta chain measure
different things; PIP-6 is the only one that exercises the
production code path end-to-end.

| Bench   | Scope                                | Drives                          | Sink         | Includes              |
|---------|--------------------------------------|---------------------------------|--------------|-----------------------|
| BR-3i.f | apply loop scheduling                | `ParallelDeltaApplier` directly | in-memory    | rayon dispatch only   |
| BR-3j.f | post-DashMap apply loop              | `ParallelDeltaApplier` directly | in-memory    | post-#2508 cleanup    |
| PIP-6   | production receiver dispatch + apply | `oc-rsync` binary over `rsync://` | real disk    | full transfer pipeline |

BR-3i.f answers "does the parallel apply loop have any throughput
to give?". BR-3j.f answers "did the DashMap rework regress the
apply loop or unlock more headroom?". PIP-6 answers "given a real
transfer pipeline - file-list build, signature exchange, delta
generation, dispatch, apply, fsync - does the parallel path's
apply-loop win translate into a wall-clock win that operators feel,
on the workload shapes the heuristic was tuned for?".

The three benches compose: if BR-3i.f shows no win, PIP-6 cannot
show one either (the apply loop is the bottleneck the parallel
path attacks). If BR-3i.f shows a 2x win but PIP-6 shows zero, the
real-world bottleneck is somewhere else in the pipeline (network
framing, signature exchange, fsync) and the dispatch decision is
landing the parallel-apply win in a cell where the apply step is
not the dominant cost.

## 3. Scope of the production path

The bench drives one full transfer per criterion iteration:

1. Sender starts: `oc-rsync` client connecting to `rsync://` daemon
   that points at the populated source tempdir as a module.
2. Receiver starts: a second `oc-rsync` process (in daemon role) or
   the same client when push direction; both halves are
   process-level so they exercise the same protocol multiplex
   framing operators see.
3. Sender builds the file list, transmits it.
4. Receiver decides parallel-vs-sequential via
   `ReceiverContext::dispatch_receiver_strategy` (the Path B
   heuristic landed in PIP-3+5).
5. Sender computes signatures, transmits deltas.
6. Receiver applies deltas, fsyncs, sends goodbye.
7. Both sides exit, criterion captures the wall.

The dispatch decision in step 4 is the variable PIP-6 isolates.
The bench compares the same workload run twice: once against an
`oc-rsync` binary built with default features (parallel-receive-
delta on; heuristic flips parallel above its thresholds), once
against an `oc-rsync` binary built **without** the
`parallel-receive-delta` feature (the dispatcher logs
`parallel_unavailable` per `crates/transfer/src/receiver/mod.rs:480`
and always picks sequential). Same workload, same wire framing,
same disk; only the apply strategy differs.

## 4. Workload matrix

Five workload shapes, calibrated to the dispatch boundary. The
heuristic is `file_count > 100 || total_size > 64 MiB`; every
shape is annotated with the exact decision the production
dispatcher takes.

| Shape              | Files  | Per-file size      | Total size | Heuristic decision  | Why this shape                                                                 |
|--------------------|--------|--------------------|------------|---------------------|--------------------------------------------------------------------------------|
| `single_large_file`| 1      | 1 GiB              | 1 GiB      | **parallel** (size) | Worst-case per-file-mutex regime; cross-file parallelism is zero, the apply loop has only one file slot. Sets the regression floor for the bench. |
| `many_small_files` | 10,000 | 4 KiB              | 40 MiB     | **parallel** (count)| Dispatch-bound shape that BR-3i.f calls out as the headline cell; tests whether apply-loop dispatch wins survive end-to-end framing overhead. |
| `boundary_under`   | 50     | ~655 KiB           | 32 MiB     | **sequential**      | Below both thresholds; runs the sequential path on both builds. Confirms the bench harness is comparing apples to apples (no spurious dispatch flip). |
| `boundary_over`    | 200    | 160 KiB            | 32 MiB     | **parallel** (count)| Just over the file-count threshold; total bytes are identical to `boundary_under` so the byte-count axis is held fixed. |
| `mixed_directory`  | 1,000  | 4 KiB - 4 MiB      | ~600 MiB   | **parallel** (both) | Typical project / media directory; sizes drawn deterministically from `{4 KiB, 16 KiB, 64 KiB, 256 KiB, 1 MiB, 4 MiB}` to match the `mixed` shape from BR-3i.f. |

The `boundary_under` cell is the control: both builds dispatch
sequential there, so wall-clock should be within bench noise.
A delta there is a bug in the harness, not a finding about
parallel-receive-delta.

The `single_large_file` cell is the **regression sentinel**: the
per-file `Mutex<FileSlot>` at
`crates/engine/src/concurrent_delta/parallel_apply.rs:248-258`
serialises all writes for the only file in the batch. The bench
expects this cell to be within +/-5% of sequential; a worse
showing is a finding that the heuristic should not steer
single-file transfers to parallel even when they cross the byte
threshold.

## 5. Metrics

Wall-clock is the headline number; the other four are sampled
when the platform exposes them and reported per workload shape.

| Metric                | Required | How captured                                              |
|-----------------------|----------|-----------------------------------------------------------|
| Wall-clock receive time | yes    | criterion `iter_custom` around the client invocation     |
| Total bytes received  | yes      | sum of source file sizes (deterministic per workload)    |
| Throughput (bytes/sec)| yes      | criterion `Throughput::Bytes` derives from the above     |
| Peak RSS (MiB)        | best-effort | rusage on Unix (`getrusage(RUSAGE_CHILDREN)`), Job Object on Windows; logged as a metadata sidecar, not a criterion measurement |
| CPU% over the run     | best-effort | rusage `utime + stime` / wall; logged as sidecar      |
| Disk write IOPS       | best-effort | iostat sampler on Linux when `/proc/diskstats` is present; logged as sidecar |

RSS, CPU%, and IOPS are sidecar metrics because criterion's
sampling model assumes a single numeric per iteration. Sidecars
go to a `target/pip-6-sidecar/` directory keyed by
`(workload_shape, build_variant)`; the design doc that consumes
the bench output (the section-4 table in
`docs/design/parallel-receive-delta-default-on.md`) is the right
place to surface them alongside the wall-clock table.

## 6. Comparison shape

Two binaries, both built locally before the bench runs:

- `target/release/oc-rsync` - default features, parallel apply
  available, heuristic flips parallel on shapes above its
  thresholds. This is the production binary.
- `target/release-no-parallel/oc-rsync` - built with
  `cargo build --release --target-dir target/release-no-parallel
  --no-default-features --features 'zstd lz4 xattr iconv'` (the
  default set minus `parallel-receive-delta`). The dispatcher in
  `crates/transfer/src/receiver/mod.rs:480-494` falls back to
  sequential with a `parallel_unavailable` debug line.

The bench resolves the two paths via env vars:

- `OC_RSYNC_BIN_PARALLEL` (default `target/release/oc-rsync`)
- `OC_RSYNC_BIN_SEQUENTIAL` (default
  `target/release-no-parallel/oc-rsync`)

The bench skips with a `eprintln!` and `return` (mirroring the
`BenchDaemon::start` pattern in
`crates/core/benches/transfer_benchmark.rs:48-51`) when either
binary is missing, so a fresh checkout that has not built both
variants does not panic. A README block in the bench file
documents the two `cargo build --release` invocations.

Same module path for both runs. Same workload tempdir, repopulated
between iterations via `daemon.clear()` and re-create, so the
receiver always sees an empty destination tree (full transfer,
not an incremental delta). For the delta-path cells, the
destination is pre-seeded with a deterministic basis that differs
from the source by 50% bytes; this is how BR-3i.f's `mixed` cell
already exercises the actual delta apply path rather than the
whole-file fast path.

## 7. Decision criteria

The Path B heuristic stays the default if, on the bench numbers
produced by this scaffold:

- **Win condition.** `parallel_wall / sequential_wall <= 0.9` on
  at least **two of the five** workload shapes. The `mixed` and
  `many_small_files` shapes are the headline cells; if neither
  wins by 10%+, the heuristic is steering work to a path that
  does not pay for itself.
- **Regression budget.** No shape regresses by more than **5%**
  (`parallel_wall / sequential_wall <= 1.05`). The
  `single_large_file` and `boundary_under` cells are the
  regression sentinels; the heuristic must not steer those into a
  worse path.
- **Boundary control.** `boundary_under` wall-clock must agree
  within +/-3% between the two builds (both run sequential there).
  Wider spread points at harness bias rather than at the apply
  strategy.

A failure on **either** of the first two reverts the heuristic
to opt-in (drops `parallel-receive-delta` from the
`default = [...]` set on `engine`, `transfer`, `core`, `cli`, and
the workspace binary). A failure on the third blocks the bench
report itself and triggers a harness audit before any conclusion
gets drawn from the cell-level numbers.

## 8. Out of scope

- **Numbers capture.** This PR ships the scaffold, not the
  numbers. Bench execution is hardware-sensitive (NVMe vs HDD,
  core count, ambient load); the right place to capture numbers
  is the `rsync-profile` and `oc-rsync-bench` containers
  documented in the project CLAUDE.md, on the same hardware that
  produces the BR-3i.f and BR-3j.f baselines.
- **SSH transport.** The bench drives `rsync://` daemon loopback
  because the comparison is about the receiver apply path, not
  about transport overhead. SSH adds process spawn, key
  negotiation, and userspace encryption to every iteration; that
  noise would mask the apply-strategy delta. A separate
  SSH-variant bench is a possible follow-up if telemetry from
  shipped Path B shows SSH receivers benefit differently than
  daemon receivers do.
- **`oc-rsync-bench` cross-version.** Comparing oc-rsync's
  parallel path against upstream rsync 3.4.1's sequential path is
  the job of the existing `crates/core/benches/transfer_benchmark.rs`,
  not this bench. PIP-6 isolates the parallel-vs-sequential
  decision inside oc-rsync; upstream comparison is a different
  question with a different bench.
- **Sender-side parallelism.** PIP-6 measures the receiver
  decision only. Sender-side delta parallelism is already
  pipelined per-file across rayon workers; that is not the path
  the heuristic touches.

## 9. Infrastructure needed before numbers can ship

The scaffold compiles and runs as a criterion bench harness; the
gating items below are about the inputs the bench needs to produce
publishable numbers, not about the harness itself.

1. **Two-variant build script.** A `tools/build_pip6_binaries.sh`
   (or equivalent xtask) that builds the two `oc-rsync` binaries
   into the expected target dirs. The current expectation is that
   the operator runs the two `cargo build --release` lines from
   the bench file's module doc; an xtask collapses the step.
2. **Container baseline.** The bench should run on both
   `rsync-profile` (NVMe + xxh3) and `oc-rsync-bench` (HDD-ish +
   MD5 software-aarch64) so the result set covers the two
   regimes the BR-3i.f cells stratify against (CPU-bound verify
   vs I/O-bound write).
3. **Sidecar metric collector.** RSS / CPU% / IOPS go to
   `target/pip-6-sidecar/`. A small helper crate or script that
   wraps the bench invocation and emits the sidecar JSON is the
   missing piece; the scaffold writes the directory but does not
   populate it yet (criterion does not have a hook for per-
   iteration auxiliary metrics).
4. **Numbers-capture commit.** The section-4 table in
   `docs/design/parallel-receive-delta-default-on.md` is the
   target for the wall-clock data; a follow-up `docs(design):`
   commit lands the numbers once the bench has been run on both
   containers.

None of items 1-4 block the scaffold. They block the
numbers-capture commit that consumes the scaffold's output.

## 10. References

- `docs/design/parallel-receive-delta-application.md` - umbrella
  design for the parallel apply loop.
- `docs/design/parallel-receive-delta-default-on.md` - promotion
  decision doc; section 4 is the table this bench fills.
- `docs/design/abw-2-pipelined-verify-write-deferred-2026-05-21.md` -
  closure note that defers the verify/write pipelining design
  pending BR-3j.f; PIP-6 is the production-path complement.
- `crates/engine/benches/parallel_receive_delta_perf.rs` -
  BR-3i.f apply-loop bench; PIP-6 is the end-to-end complement.
- `crates/core/benches/transfer_benchmark.rs` - the existing
  daemon-driven bench harness PIP-6's scaffold mirrors.
- `crates/transfer/src/receiver/mod.rs:96-510` - the heuristic
  constants (`PARALLEL_RECEIVE_FILE_COUNT_THRESHOLD = 100`,
  `PARALLEL_RECEIVE_BYTES_THRESHOLD = 64 MiB`) and the
  `ReceiverContext::dispatch_receiver_strategy` site that PIP-6
  exercises.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:248-258` -
  the per-file `Mutex<FileSlot>` ingest path; the
  `single_large_file` cell's regression sentinel.
- PIP-3+5 (PR #4666) - heuristic wiring; the production-path
  setup PIP-6 measures.
- BR-3i.f (#2502, completed) - apply-loop-level bench.
- BR-3j.f (#2508, pending) - post-DashMap apply-loop re-bench.
- `project_parallel_delta_apply_phase2.md` - project memory page
  tracking production cutover status.
