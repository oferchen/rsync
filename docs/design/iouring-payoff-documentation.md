# io_uring payoff documentation design (IUB-11)

Tracking: IUB-11. Depends on IUB-10 (payoff matrix synthesis from bench
results). Predecessors: IUB-1..5 (bench cell design and implementation),
IUB-6..9 (pending bench runs on Linux hardware), IUM-1..4 (benefit model
predictions).

## Purpose

Users need clear guidance on when io_uring acceleration helps their
workload and when it does not. Without this, operators either leave
performance on the table (disabling io_uring unconditionally) or expect
gains that do not materialize (small transfers on slow storage).

This document specifies the format, content, and placement of the
user-facing io_uring payoff documentation - both in release notes
(concise, actionable) and in the user guide (detailed, with tuning
advice).

## Background

### Current io_uring scope in oc-rsync

The io_uring backend in a default build is metadata-only:

| Operation | Opcode | Default-on? |
|-----------|--------|-------------|
| `STATX` batch (file-list build, quick-check) | `IORING_OP_STATX` | yes (probe-gated, kernel 5.11+) |
| Temp-file rename | `IORING_OP_RENAMEAT` | yes (probe-gated, kernel 5.11+) |
| Hardlink creation | `IORING_OP_LINKAT` | yes (probe-gated, kernel 5.15+) |
| Disk-batch writes (receiver) | `IORING_OP_WRITE` / `WRITE_FIXED` | yes (Auto policy, kernel 5.6+) |
| File-data slurp write | `IORING_OP_WRITE_FIXED` | no (`iouring-data-writes` feature) |
| Basis-file read | `IORING_OP_READ` | no (`iouring-data-reads` feature) |
| Zero-copy socket send | `IORING_OP_SEND_ZC` | no (`iouring-send-zc` feature, kernel 6.0+) |

The data path uses `copy_file_range` (same-filesystem reflinks),
`splice`/`vmsplice` (cross-fd pipes), and buffered writes. io_uring data
paths are opt-in features that require explicit Cargo feature flags.

### IUM benefit model predictions

From `docs/design/iouring-benefit-model.md`:

- **Batched STATX** is the only metadata site with a predicted real win -
  at high file counts (thousands+), the syscall-count reduction from
  batching N `statx(2)` calls into `ceil(N / sq_entries)` ring enters
  produces measurable speedup.
- **Single rename/link/unlink** calls via transient rings have no predicted
  win over direct syscalls.
- **Disk-batch writes** show modest win only at multi-GB single files or
  sustained high IOPS on fast NVMe with deep queues.
- **SEND_ZC** provides CPU savings (avoided memcpy) but not latency
  reduction; real only for large contiguous sends from registered buffers.

### Measured baseline

At 148 MB / 10K files on NVMe: ~1.00x io_uring vs standard I/O. This
workload sits in the crossover zone - too few files for IOPS dominance,
too few bytes for throughput dominance.

## 1. Workload categories

Three categories define the payoff boundary:

### Category A: metadata-heavy (many small files)

Characteristics:
- 100K+ files, per-file size under 64 KiB
- Per-file syscall cost dominates wall time
- Examples: `node_modules`, OCI image layers, package caches, mail
  spools, source trees

io_uring mechanism: batched STATX reduces `statx(2)` syscall count by up
to 64x per batch (at default ring depth 64). Per-file rename/link/unlink
contribute negligible savings individually but may compound at scale.

Expected payoff (IUM prediction):
- 100K files, 4 KiB each (~400 MiB total): **1.1x-1.3x** (stat-bound)
- 1M files, 1 KiB each (~1 GiB total): **1.2x-1.5x** (stat-bound)
- Below 10K files: **~1.00x** (ring setup cost not amortized)

Primary beneficiary: file-list build phase, `--checksum` mode, cold-cache
initial sync.

### Category B: data-heavy (few large files)

Characteristics:
- 1-100 files, per-file size 2 GiB+
- Byte throughput dominates wall time
- Examples: database dumps, VM images, container base layers, large
  archives, media files

io_uring mechanism: disk-batch writes via submission-queue batching keep
the NVMe device queue saturated. Registered buffers (`WRITE_FIXED`) skip
per-SQE page pinning. SQPOLL eliminates enter syscalls entirely.

Expected payoff (IUM prediction):
- 2 GiB single file on NVMe: **1.05x-1.15x** (write-bound)
- 10 GiB single file on NVMe: **1.10x-1.25x** (queue depth amortized)
- 50 GiB single file on NVMe: **1.15x-1.30x** (deep pipeline saturates device)
- On SATA SSD or HDD: **~1.00x** (device is the bottleneck, not syscalls)

Primary beneficiary: receiver-side file reconstruction, initial full copy
of large files.

### Category C: mixed (typical workloads)

Characteristics:
- Thousands to tens of thousands of files with a mix of sizes
- Neither metadata nor data throughput cleanly dominates
- Examples: web application deployments, home directories, general backup

Expected payoff (IUM prediction):
- 10K files, 148 MiB total: **~1.00x** (measured baseline)
- 50K files, 1 GiB total: **1.02x-1.08x** (stat batching begins to show)
- 100K files, 10 GiB total: **1.05x-1.15x** (both mechanisms contribute)

Primary beneficiary: transfers above the crossover threshold where
combined stat batching and write batching overcome ring overhead.

## 2. Crossover thresholds

The break-even points below which io_uring adds overhead rather than
removing it:

| Dimension | Approximate crossover | Below this, prefer standard I/O |
|-----------|-----------------------|---------------------------------|
| File count (stat batching) | ~500 files per batch | Ring setup cost exceeds saved syscalls |
| File size (data writes) | ~1 MiB per file | `copy_file_range` or buffered write wins |
| Device class | NVMe (multi-queue, high IOPS) | SATA SSD/HDD cannot exploit queue depth |
| Kernel version | 5.11+ for STATX, 5.6+ for writes | Fallback to direct syscalls is automatic |
| Total payload | ~500 MiB+ | Below this, total wall time is too short for amortization |

## 3. The `--io-uring` flag

oc-rsync exposes io_uring policy via `--io-uring=<mode>`:

| Mode | Behavior |
|------|----------|
| `auto` (default) | Probe kernel at startup; use io_uring for supported ops when available. Falls back silently on unsupported kernels or ops. |
| `enabled` | Require io_uring; fail with an error if the kernel does not support the minimum opcode set. Useful for CI validation. |
| `disabled` | Never use io_uring even if available. Useful for isolating performance regressions or on kernels with known io_uring bugs. |

Additional tuning:
- `--io-uring-depth=N` - set submission queue depth (default 64; higher
  values can improve throughput on very high file counts or large files
  but consume more kernel memory)

## 4. Release notes format

Release notes must be concise and actionable. The io_uring payoff section
should appear under a "Performance" heading and be no more than 10-15
lines.

### Template

```markdown
### Performance

**io_uring acceleration payoff** (Linux 5.11+):

io_uring is on by default (`--io-uring=auto`) and benefits workloads above
the crossover threshold. Measured speedups on NVMe:

| Workload | Speedup vs standard I/O |
|----------|-------------------------|
| 100K small files (4 KiB each) | {{BENCH_100K_RESULT}} |
| 1M small files (1 KiB each) | {{BENCH_1M_RESULT}} |
| 2 GiB single file | {{BENCH_2G_RESULT}} |
| 10 GiB single file | {{BENCH_10G_RESULT}} |

Below ~500 files or ~500 MiB total, io_uring adds negligible benefit.
Use `--io-uring=disabled` to bypass on kernels with known io_uring issues
(pre-5.11, or specific 5.x backports with bugs).

See [io_uring performance guide](docs/user/iouring-performance.md) for
detailed tuning.
```

### Guidance for filling the template

- Replace `{{BENCH_*_RESULT}}` with measured values from IUB-6..9 runs.
- If a measured result is below 1.02x, report it as "~1.00x (no
  measurable gain)" - do not inflate marginal results.
- If a predicted category shows no gain, include it anyway with the
  measured number to set correct expectations.
- Add a "Kernel requirement" note only if the minimum kernel version
  changed from the prior release.

## 5. User guide format

The user guide entry should be a standalone document at
`docs/user/iouring-performance.md` following the established pattern
(see `docs/user/checksum-performance.md`, `docs/user/parallel-receive-delta.md`).

### Proposed structure

```
# io_uring performance guide

## When io_uring helps

[Payoff table: workload category -> expected speedup range]
[Crossover thresholds table]
[Kernel/hardware requirements]

## Configuration

[`--io-uring` flag documentation]
[`--io-uring-depth` tuning]
[Cargo feature flags for opt-in paths]

## Workload-specific recommendations

### Many small files (container images, package caches)
[Specific advice: default config is optimal, SQPOLL if on NVMe]

### Large file transfers (databases, VM images)
[Specific advice: consider `iouring-data-writes` feature, ensure NVMe]

### General backup / sync
[Specific advice: auto mode handles crossover, no tuning needed]

## Diagnosing io_uring behavior

[How to confirm io_uring is active: `--verbose` output]
[How to measure if io_uring is helping: compare with --io-uring=disabled]
[Known kernel issues and version-specific caveats]

## Benchmark methodology

[Link to bench design docs for reproducibility]
[How to run the payoff bench cells locally]
```

### Content principles

- Lead with "when does this help you" - not implementation details.
- Include a decision tree or quick-reference table at the top.
- Provide a one-command comparison for users to measure their own
  workload: run once with `--io-uring=auto`, once with
  `--io-uring=disabled`, compare wall time.
- Do not recommend io_uring unconditionally - be honest about the
  crossover where it adds nothing.
- Reference upstream rsync's lack of io_uring as context (upstream uses
  direct syscalls exclusively).

## 6. Relationship to other documentation

| Document | Scope | Status |
|----------|-------|--------|
| `docs/design/iouring-benefit-model.md` | Internal: per-site predictions, falsifiers | Complete (IUM-1..4) |
| `docs/design/iouring-multi-gb-bench-design.md` | Internal: bench cell spec for data-heavy | Complete (IUB-2) |
| `docs/design/iouring-bench-high-iops-workloads.md` | Internal: bench cell spec for metadata-heavy | Complete (IUB-3) |
| `docs/user/iouring-performance.md` | External: user guide (this design's output) | Blocked on IUB-10 |
| Release notes section | External: concise payoff table | Blocked on IUB-10 |
| `docs/platform-io-fast-paths.md` | External: platform I/O dispatch overview | Exists, update when IUB-10 lands |

## 7. IUB-10 results template

When IUB-6..9 bench runs complete and IUB-10 synthesizes the payoff
matrix, populate this template. The user guide and release notes draw
directly from these numbers.

### Raw results table (filled by IUB-10)

| Cell ID | Workload | io_uring mode | Median wall-time | Stddev | Speedup vs stdlib |
|---------|----------|---------------|------------------|--------|-------------------|
| `high_iops/100k_4k/auto` | 100K files, 4 KiB | auto | TBD | TBD | TBD |
| `high_iops/100k_4k/disabled` | 100K files, 4 KiB | disabled | TBD | TBD | baseline |
| `high_iops/1m_1k/auto` | 1M files, 1 KiB | auto | TBD | TBD | TBD |
| `high_iops/1m_1k/disabled` | 1M files, 1 KiB | disabled | TBD | TBD | baseline |
| `multi_gb/2g/auto` | 2 GiB single file | auto | TBD | TBD | TBD |
| `multi_gb/2g/disabled` | 2 GiB single file | disabled | TBD | TBD | baseline |
| `multi_gb/10g/auto` | 10 GiB single file | auto | TBD | TBD | TBD |
| `multi_gb/10g/disabled` | 10 GiB single file | disabled | TBD | TBD | baseline |
| `multi_gb/50g/auto` | 50 GiB single file | auto | TBD | TBD | TBD |
| `multi_gb/50g/disabled` | 50 GiB single file | disabled | TBD | TBD | baseline |
| `mixed/50k_1g/auto` | 50K files, 1 GiB total | auto | TBD | TBD | TBD |
| `mixed/50k_1g/disabled` | 50K files, 1 GiB total | disabled | TBD | TBD | baseline |
| `mixed/100k_10g/auto` | 100K files, 10 GiB total | auto | TBD | TBD | TBD |
| `mixed/100k_10g/disabled` | 100K files, 10 GiB total | disabled | TBD | TBD | baseline |

### Hardware and kernel context (filled by IUB-6..9)

| Property | Value |
|----------|-------|
| Kernel | TBD |
| CPU | TBD |
| Storage | TBD (model, interface, queue depth) |
| Filesystem | TBD |
| io_uring ring depth | 64 (default) |
| SQPOLL | enabled / disabled |
| Registered buffers | yes / no |

### Decision matrix (derived from results)

After IUB-10 synthesis, this matrix drives the user-facing documentation:

| User's workload | Recommended `--io-uring` | Expected gain | Confidence |
|-----------------|--------------------------|---------------|------------|
| < 500 files, < 500 MiB | `auto` (no harm, no gain) | ~1.00x | High (measured) |
| 10K-100K small files | `auto` | TBD | Pending IUB-6 |
| 1M+ small files | `auto` | TBD | Pending IUB-7 |
| 2-10 GiB single file, NVMe | `auto` | TBD | Pending IUB-8 |
| 50+ GiB single file, NVMe | `auto` | TBD | Pending IUB-9 |
| Any workload, SATA/HDD | `auto` (no harm, no gain) | ~1.00x | High (IUM model) |
| Any workload, kernel < 5.6 | `disabled` (unavailable) | N/A | Definitive |

## 8. Completion criteria

IUB-11 is complete when:

1. IUB-10 results are available (bench numbers populated).
2. `docs/user/iouring-performance.md` is written using this design's
   structure, with measured numbers replacing predictions.
3. The release notes template section is filled and included in the
   next release that ships io_uring improvements.
4. `docs/platform-io-fast-paths.md` is updated to cross-reference the
   user guide.
5. The payoff table accurately reflects measured results - predictions
   that were falsified are noted, not hidden.

## 9. Open questions

- Should the user guide recommend `--io-uring-depth` values for specific
  workloads, or is "default 64 is fine for all cases" sufficient? The
  IUB-2 design includes depth-sweep cells that may inform this.
- Should we document the opt-in Cargo features (`iouring-data-writes`,
  `iouring-data-reads`, `iouring-send-zc`) in the user guide, or keep
  them internal until bench results justify promoting them?
- If the 1M-file cell shows < 1.05x, should we still recommend io_uring
  for that regime or document it as "no benefit, Auto mode is harmless"?
