# io_uring Benefit Model (IUM-1)

## Purpose

io_uring shipped in this codebase before anyone wrote down a falsifiable
prediction of where it would actually win. The real bottlenecks - statx
overhead during file-list build, and per-file metadata syscalls - surfaced
through a long chain of reactive audit commits, not from a model decided up
front.

This document forces the opposite discipline. Before any further io_uring
benchmarking or coding, each io_uring use site gets a written, falsifiable
benefit prediction:

- The expected win (qualitative and a rough magnitude).
- The precise mechanism that produces the win.
- The workload regime where it pays off (file count, file size, IOPS, queue
  depth, kernel version).
- The break-even point below which a plain syscall, `copy_file_range`,
  `splice`, or `sendfile` is at least as fast.

A prediction that cannot be falsified is not allowed here. Each section ends
with a concrete bench cell that would disprove it.

## Current io_uring surface

The de-facto live scope is metadata-only. The data-write path and the
zero-copy socket send both exist in the tree but are gated behind opt-in
cargo features and size thresholds, so a default build does not route file
data through io_uring.

Operations actually compiled and dispatched in a default build
(`default = ["io_uring", "iocp", "sqpoll-mlock-basis"]`):

| Operation | Opcode | Kernel floor | Wrapper | Default-on? |
|-----------|--------|--------------|---------|-------------|
| `STATX` | `IORING_OP_STATX` (21) | 5.11 | `io_uring_ops::try_statx_batch_via_io_uring` -> `submit_statx_batch` | yes (probe-gated) |
| `RENAMEAT2` | `IORING_OP_RENAMEAT` (35) | 5.11 | `io_uring_ops::try_rename_via_io_uring` -> `renameat2_blocking` | yes (probe-gated) |
| `LINKAT` | `IORING_OP_LINKAT` | 5.15 | `io_uring_ops::hard_link` -> `submit_linkat_blocking` | yes (probe-gated) |
| disk-batch writes | `IORING_OP_WRITE`/`WRITE_FIXED` | 5.6 | `IoUringDiskBatch` (receiver disk thread) | yes (Auto policy) |
| file-data slurp write | `IORING_OP_WRITE_FIXED` | 5.6 | `fast_io::write_file_with_io_uring` | no (`iouring-data-writes`) |
| basis-file slurp read | `IORING_OP_READ` | 5.6 | `IoUringFileReader` / `read_file_with_io_uring` | no (`iouring-data-reads`) |
| zero-copy socket send | `IORING_OP_SEND_ZC` | 6.0 | `ZeroCopySender::send_zc` / `try_send_zc` | no (`iouring-send-zc`) |

Every dispatch is best-effort. Availability is probed once per process and
cached (`is_io_uring_available`, plus per-opcode `*_supported` probes via
`IORING_REGISTER_PROBE`). Each `try_*` wrapper returns `Option<io::Result<_>>`
so the caller distinguishes "io_uring not available" (`None`) from "io_uring
tried and failed" (`Some(Err)`) and falls back to the plain syscall.

The default per-thread ring depth is 64 (`PER_THREAD_RING::DEFAULT_RING_DEPTH`,
matching `IoUringConfig::sq_entries`), overridable via `--io-uring-depth=N`.

## Per-use-site benefit model

### (a) Metadata ops - statx / renameat2 / linkat / unlinkat

Live call sites today:

- `STATX` batch: file-list build and quick-check. The generator stats many
  files during directory traversal; `submit_statx_batch` submits all paths as
  independent SQEs on one ring.
- `RENAMEAT2`: receiver temp-file commit (`receiver/transfer/sync.rs` tries
  `try_rename_via_io_uring` before `std::fs::rename`).
- `LINKAT`: hardlink creation (`io_uring_ops::hard_link`) used by
  `--link-dest` and the multi-component hardlink cohort path.
- `unlinkat`: removal during `--delete` and temp cleanup (same try-or-fallback
  shape as rename/link).

Predicted win: **the only metadata site with a real, measurable win is
batched STATX at high file count.** Single rename/link/unlink calls win
little to nothing.

Mechanism:

- STATX batch: syscall-count reduction plus submission batching. N synchronous
  `statx(2)` calls (N context switches) collapse to roughly
  `ceil(N / sq_entries)` `io_uring_enter` calls. At depth 64 that is a ~64x
  reduction in enter syscalls for the stat phase. With SQPOLL the enter count
  trends toward zero, leaving only completion reaping.
- rename / link / unlink: these wrappers submit a single SQE on a transient
  ring and block on the one CQE. There is no batching and no async overlap.
  The ring setup and teardown cost is paid per call. The mechanism here is
  effectively "an async syscall used synchronously", which has no upside over
  the direct syscall and can be slower.

Workload regime where it pays off:

- STATX: high file count (thousands-plus entries per directory level), files
  small enough that metadata dominates wall time (the `--checksum`-mode and
  cold-cache file-list-build regimes). Kernel 5.11+. The win grows with file
  count and with queue depth up to the point where completion-reaping cost
  catches up with the syscall savings.
- rename / link / unlink: no regime where the transient-ring form wins. A win
  would require batching many renames/links onto a persistent ring and
  overlapping them, which the current single-SQE-transient-ring code does not
  do.

Break-even:

- STATX: below roughly a few hundred files per stat batch, the ring setup,
  SQE encoding, and completion-drain overhead are not amortized and plain
  `statx(2)` (or `rustix` fallback) wins. The exact crossover is the open
  measurement (IUM-2).
- rename / link / unlink: the break-even is "always" - the direct syscall is
  the baseline to beat, and the transient-ring path adds ring construction per
  call. Treat these as correctness-parity paths, not speed paths, until a
  batched persistent-ring design exists.

Falsifier: `iouring_high_file_count` bench. If io_uring STATX is not faster
than synchronous stat at >= 100K small files on kernel 5.11+, the STATX
prediction is wrong.

### (b) File-data writes - receiver write path

Live call sites today:

- `IoUringDiskBatch` on the receiver disk-commit thread (default Auto policy):
  reconstructed file blocks are submitted as batched `IORING_OP_WRITE` /
  `WRITE_FIXED` SQEs instead of one buffered `write(2)` per block.
- `write_file_with_io_uring` whole-file slurp write
  (`iouring-data-writes`, opt-in): gated at `IOURING_DATA_WRITES_MIN_BYTES`
  (1 MiB) in the engine local-copy executor.

Predicted win: **modest at best on common workloads; real only at multi-GB
single files or high sustained write IOPS on fast NVMe with deep queues.**

Mechanism:

- Submission batching and queue depth: many block writes become one
  `submit_and_wait`. With registered buffers (`WRITE_FIXED`) the kernel skips
  per-SQE page pinning. With SQPOLL the enter syscalls disappear.
- Async overlap: deep queues let the device keep multiple writes in flight,
  which matters only when the device can actually service them in parallel
  (NVMe with high queue depth), not on a single spinning disk or tmpfs.

Workload regime where it pays off:

- Large files (multi-GB) where one file produces enough block writes to fill
  the submission queue many times over, and the device sustains high IOPS at
  depth. Kernel 5.6+ for the base opcodes; registered buffers and SQPOLL add
  incremental benefit. The slurp-write path is deliberately gated at 1 MiB
  because below that the ring is pure overhead.

Break-even:

- For small and mid-size files, `copy_file_range` (server-local copy) and
  ordinary buffered `write(2)` win. `copy_file_range` does the copy entirely
  in-kernel with zero data crossing userspace, which io_uring writes do not.
  The break-even is workload-shaped: it is the file size and IOPS at which the
  amortized ring cost drops below the per-write syscall cost it replaces. The
  current 1 MiB gate is a first guess, not a measured crossover.
- Note the basis-mmap interaction: when the writer is io_uring-backed the
  applicator forces `BufferedMap` and never mmaps the basis, because an
  mmap-backed pointer in a `WRITE`/`READ` SQE risks cold-page faults inside
  the ring (`delta_apply/applicator.rs`). This is a correctness constraint
  that also caps the data-path win.

Falsifier: `iouring_multi_gb_scale` and `nvme_data_path` /
`nvme_data_path_production` benches. If io_uring data writes are not faster
than stdlib writes / `copy_file_range` at multi-GB on NVMe with deep queues,
the data-write prediction is wrong and the path should stay opt-in or be
removed.

### (c) Zero-copy socket send - SEND_ZC

Live call sites today: none in a default build. `ZeroCopySender::send_zc` and
`try_send_zc` exist behind the `iouring-send-zc` feature and are exercised
only by benches.

Predicted win: **CPU savings (avoided userspace-to-kernel copy), not latency,
and only for large contiguous sends from pinned registered buffers.**

Mechanism:

- Zero-copy page pinning: `SEND_ZC` DMA's payload pages directly; the kernel
  does not copy them into socket buffers. This trades one memcpy for two CQEs
  per send (a transfer CQE with `IORING_CQE_F_MORE`, then a notification CQE
  with `IORING_CQE_F_NOTIF` when the kernel releases the pages). The wrapper
  blocks on both CQEs, so callers see it as synchronous.
- The win is the saved copy. The cost is the second CQE round trip plus the
  page-pin bookkeeping, which is fixed per send regardless of size.

Workload regime where it pays off:

- Large payloads where the saved copy exceeds the two-CQE overhead. The
  dispatch floor is `SEND_ZC_DISPATCH_MIN_BYTES = 4 KiB`; below that the
  notification-CQE cost dominates and a plain `IORING_OP_SEND` (one CQE) or
  `sendfile`/`splice` is cheaper. Kernel 6.0+. Best with a long-lived ring and
  a `RegisteredBufferGroup` so pages are already pinned.

Break-even:

- Below ~4 KiB per send, regular SEND wins (one CQE, no notification). For
  file-to-socket transfer where the data is already on disk, `sendfile` /
  `splice` win outright because they never bring bytes into userspace at all;
  SEND_ZC only helps when the payload already lives in a registered userspace
  buffer (for example a compressed or delta-encoded frame). The real crossover
  byte count is unmeasured.

Falsifier: `ius_3_send_zc_vs_send` bench. If SEND_ZC is not lower-CPU than
plain SEND at large registered-buffer payloads on kernel 6.0+, the SEND_ZC
prediction is wrong and the feature should not graduate from opt-in.

## Known evidence to confront

The predictions above are claims, not results. Existing evidence already
pushes back on the optimistic reading:

- At small bench scale (~148 MB), io_uring measures ~1.00x against standard
  I/O. The expected payoff is multi-GB / high-IOPS / high-file-count, which
  small benches do not exercise.
- The committed scope is metadata-only. The data-write and SEND_ZC paths are
  opt-in precisely because they have not demonstrated a default-on win.

These predictions must be checked against the IUB benchmark series - the
multi-GB single-file cell (`iouring_multi_gb_scale`), the high-file-count
STATX cell (`iouring_high_file_count`), the NVMe data-path cells
(`nvme_data_path`, `nvme_data_path_production`), the per-file-vs-shared and
SQPOLL-vs-regular cells - and against the SZC SEND_ZC benches
(`ius_3_send_zc_vs_send`). That cross-check is the follow-up task **IUM-2**:
run the cells, record the measured win or absence of win, and mark each
prediction above confirmed or falsified.

## Predictions vs evidence (IUM-2)

IUM-1 wrote falsifiable predictions. IUM-2 confronts each one with the
evidence already committed to this repo. The discipline here is conservative:
a number that was never captured is marked Untested and the bench cell that
would settle it is named; small-scale evidence that pushes against a "win"
claim is marked Contradicted. No numbers are invented below - where a result
table does not exist in the tree, that absence is itself the finding.

The recurring source of the small-scale datum is the project memory note
`project_iouring_marginal_at_small_bench_scale` (~1.00x at the 148 MB /
10 000-file release-bench shape). That datum is cited by the IUB-1 inventory
(`docs/audit/iouring-bench-workload-inventory.md` lines 24-34) but, per that
same inventory, it does not live in any tracked result file - no committed
`.rs` doc-comment baseline, no CHANGELOG number, no results doc.

| Prediction (use site) | Evidence source | Verdict | Notes |
|-----------------------|-----------------|---------|-------|
| (a) STATX batch wins at high file count | `crates/fast_io/benches/iouring_high_file_count.rs` (IUB-5 cell, env-gated `BENCH_HIGH_FILE_COUNT` / `_1M`); `docs/audit/iouring-bench-workload-inventory.md` lines 109-129 | Untested | The 100K / 1M-file STATX bench is implemented but has no committed result. IUB-1 observation 4 states "No baseline numbers ... are committed to the repo." The break-even (the doc's "a few hundred files") is explicitly the open measurement (IUM-1 line 104). Settled by running `iouring_high_file_count` at 100K and 1M small files on kernel 5.11+. |
| (a) STATX overhead is real in `--checksum` mode (the regime the win targets) | `docs/audit/checksum-statx-overhead.md` (STX-1/STX-4) | Corroborated (problem), Untested (io_uring fix) | strace shows oc-rsync issuing 6,691 statx vs upstream 2,006 on a 500-file `--checksum` corpus (3.34x). This confirms metadata syscalls dominate in the targeted regime. But the audit's root causes (BufReader EOF probe STX-6, redundant fstat STX-8) are syscall-count bugs whose fix is sized reads, not io_uring STATX batching. No evidence that io_uring STATX is the lever that closes this gap. |
| (a) rename / link / unlink transient-ring form has no regime where it wins | `docs/design/iur-3f-shared-rings-decision.md` (sections 2-3); `docs/design/io-uring-shared-ring-audit.md` lines 180-198 (IUR-1 section 3.4) | Corroborated (qualitatively), Untested (numerically) | IUR-1/IUR-3.f find the probe-ring acquire "below the flame-graph noise floor" with "no SharedRing contention to measure today" - consistent with the "always break-even, treat as correctness-parity" claim. But this is a contention model, not a head-to-head bench of transient-ring renameat2/linkat vs the plain syscall. No bench in `crates/fast_io/benches/` times single rename/link/unlink against `std::fs::rename` / direct syscall. The prediction is plausible and unmeasured. |
| (b) Receiver disk-batch writes win only at multi-GB / high sustained IOPS | `crates/fast_io/benches/nvme_data_path.rs`, `nvme_data_path_production.rs` (10x1GiB, env-gated, IUD-4/IUD-9); `docs/design/iouring-multi-gb-bench-design.md` (IUB-2, 2/10/50 GiB cells); `docs/audit/iouring-bench-workload-inventory.md` lines 66-71, 125-129 | Untested | The disk-batch path is default-on (Auto policy) yet has no committed measurement. IUB-1 states the IUD-4/IUD-9 NVMe benches carry "not committed (#4381 / #2364)" / "(#4398 / #2369)" numbers. The IUB-2 multi-GB cells that would test the payload-scaling hypothesis are designed but, per IUB-2 status, land under IUB-4 and have not been run. Settled by `iouring_multi_gb` (2/10/50 GiB) and `nvme_data_path*` on NVMe with deep queues. |
| (b) File-data slurp read/write (opt-in) wins only at multi-GB on fast NVMe | `crates/fast_io/benches/nvme_data_path_production.rs` (IUD-9, `iouring-data-writes`+`iouring-data-reads`); `docs/design/iouring-receive-data-path.md`; `docs/audit/iouring-bench-workload-inventory.md` lines 68-71 | Untested | The slurp paths stay opt-in precisely because no default-on win was shown - but the underlying magnitude (multi-GB payoff) is unmeasured. No numeric baseline is committed for the IUD-9 production wrapper. The 1 MiB / 64 KiB gates in the engine and `iouring-receive-data-path.md` are described in their own docs as a "first guess, not a measured crossover." Settled by the same `iouring_multi_gb` / `nvme_data_path*` cells. |
| (b) At small/mid file size, `copy_file_range` / buffered `write(2)` win over io_uring | `project_iouring_marginal_at_small_bench_scale` (via `docs/audit/iouring-bench-workload-inventory.md` lines 24-34); `docs/design/iouring-multi-gb-bench-design.md` disk-class table (lines 117-126) | Corroborated (small scale) | The ~1.00x measurement at 148 MB on a CI runner (page-cache-resident, likely tmpfs) is consistent with "no win below the break-even." IUB-2's own disk-class table predicts ~1.00x on tmpfs/ramdisk and HDD. This is the one place small-scale evidence actively supports a prediction in the model - the "no win at small scale" half. The crossover above which io_uring wins remains unmeasured. |
| (c) SEND_ZC saves CPU only for large registered-buffer payloads | `crates/fast_io/benches/ius_3_send_zc_vs_send.rs`; `docs/design/ius-4-decision-2026-05-22.md`; `docs/design/szc-a-send-zc-bench-workload.md`, `szc-b-send-zc-10gb-bench.md`, `szc-d-send-zc-concurrent-bench.md` | Untested | The IUS-4 decision doc records the IUS-3 throughput input as "**MISSING**" - "No multi-kernel hardware run has been captured ... only the bench harness has shipped." The SZC.a/b/d successor docs are all "design spec; implementation is a follow-up PR" with no captured numbers. The 4 KiB dispatch floor and the two-CQE-overhead claim are unmeasured. Settled by `ius_3_send_zc_vs_send` (and SZC.b 10 GiB) on kernel 6.0+ with a registered buffer group. |
| (c) Below ~4 KiB plain SEND wins; `sendfile`/`splice` win for on-disk data | `docs/design/ius-4-decision-2026-05-22.md` section 1 (default builds use `sendfile`/`splice`/`copy_file_range`); `crates/fast_io/benches/ius_3_send_zc_vs_send.rs` (4 KiB-1 MiB chunk shapes) | Untested | The default-build posture (plain SEND + sendfile/splice) is an architectural choice consistent with the prediction, but no bench compares SEND_ZC against plain SEND at sub-4 KiB chunks with committed numbers. The bench harness has the chunk shapes (`small_chunks_16KiB`, `mixed_chunks_4KiB_to_1MiB`) but, per IUB-1, results are "not committed." |

Tally: of the eight rows, 5 are Untested, 2 are Corroborated (the "no win at
small scale" half of the data-path prediction, and the qualitative
no-regime-to-win finding for transient-ring rename/link/unlink), 1 is split
(the `--checksum` STATX problem is corroborated as real, but io_uring STATX as
its fix is untested). None of the predictions is Contradicted: no committed
measurement shows an io_uring path losing where the model claims a win,
because the magnitude benches that would test the win claims have not been
run. The single piece of measured data (~1.00x at 148 MB) sits below every
predicted break-even and therefore neither confirms nor refutes the
payoff claims - it only confirms the model's "no win at small scale" guard.

Implication for IUM-3: the evidence base justifies only the metadata-only
default scope that ships today, and even there the STATX win is asserted, not
measured. Every magnitude prediction (data-path writes, slurp read/write,
SEND_ZC) rests on assumption until the IUB-4 / SZC.b benches run on NVMe and
kernel 6.0+ hardware. IUM-3 is the go/no-go decision doc; on this evidence it
should keep the data-path and SEND_ZC paths opt-in, decline to default-flip
anything new, and gate any scope expansion on running the named cells above.

## Decision gate

Future io_uring code or scope changes must be gated on a measured break-even
threshold, not on open-ended auditing. The rule:

- An io_uring use site is enabled by default only after a bench cell shows it
  beats the best non-io_uring alternative (plain syscall, `copy_file_range`,
  `splice`, `sendfile`) by a margin that survives noise, and the doc records
  the file-size / file-count / IOPS / queue-depth / kernel regime where that
  holds.
- Below the measured break-even, the path stays behind a size threshold or an
  opt-in feature, or is removed.
- A prediction in this document that the IUM-2 benches falsify is deleted or
  rewritten - it does not linger as aspiration.

Choosing those thresholds from the IUM-2 measurements is the follow-up
decision task **IUM-3**.

## Cross-references

io_uring surface (`crates/fast_io/src/io_uring/`):

- `mod.rs` - module overview, factory dispatch, fallback chain.
- `statx.rs` - `IORING_OP_STATX` wrapper, `submit_statx_batch`.
- `renameat2.rs` - `IORING_OP_RENAMEAT` wrapper, `renameat2_blocking`.
- `linkat.rs` - `IORING_OP_LINKAT` wrapper, `submit_linkat_blocking`.
- `cancel.rs` - `IORING_OP_ASYNC_CANCEL` for in-flight SQE cancellation.
- `send_zc.rs` - `IORING_OP_SEND_ZC` zero-copy send, `ZeroCopySender`,
  `SEND_ZC_DISPATCH_MIN_BYTES`.
- `disk_batch.rs` - `IoUringDiskBatch` batched receiver writes.
- `buffer_ring/` - PBUF_RING provided-buffer rings.
- `registered_buffers.rs` - `READ_FIXED` / `WRITE_FIXED` page-pinned buffers.
- `per_thread_ring.rs` - per-thread ring, `DEFAULT_RING_DEPTH = 64`.
- `session_pool.rs` - long-lived ring pool shared across consumers.
- `io_uring_common.rs` - `*_MIN_KERNEL` floors, `IoUringConfig`.

Dispatch wrappers and consumers:

- `crates/fast_io/src/io_uring_ops.rs` - `try_*` / `hard_link` fallback
  wrappers.
- `crates/transfer/src/disk_commit/` - `IoUringDiskBatch` wiring.
- `crates/transfer/src/receiver/transfer/sync.rs` - rename fast path.
- `crates/transfer/src/delta_apply/applicator.rs` - io_uring-vs-mmap basis
  constraint.
- `crates/engine/.../execute/iouring.rs` - opt-in 1 MiB data-write gate.

Benches (`crates/fast_io/benches/`):

- `iouring_high_file_count.rs` - STATX batching at scale (IUM-2 cell a).
- `iouring_multi_gb_scale.rs` - multi-GB single-file write (IUM-2 cell b).
- `nvme_data_path.rs`, `nvme_data_path_production.rs` - NVMe write path.
- `iouring_per_file_vs_shared.rs` - per-file vs shared ring topology.
- `iouring_sqpoll_vs_regular.rs` - SQPOLL vs regular submission.
- `ius_3_send_zc_vs_send.rs` - SEND_ZC vs SEND (IUM-2 cell c).
- `iocp_vs_iouring_matched.rs` - cross-platform IOCP/io_uring comparison.

Related design docs: `iouring-send-zc.md`, `iouring-receive-data-path.md`,
`iur-2-per-thread-rings.md`.
