# io_uring + mmap Memory Pressure Test Plan

Task: #1664. Owner: `fast_io`. Status: design — no code lands until guards
are scoped from these results.

## Goals

oc-rsync uses `memmap2::Mmap` for basis files at or above `MMAP_THRESHOLD`
(64 KiB, see `crates/fast_io/src/mmap_reader.rs`) and submits writes through
io_uring on Linux 5.6+. The two subsystems share an implicit contract:
mapped pages must remain valid for the lifetime of any in-flight SQE that
references them.

This plan exercises the boundary conditions where that contract breaks:
page cache reclaim under memory pressure with queued SQEs, basis file
truncation or unlink mid-transfer, `madvise(MADV_DONTNEED)` on an active
mapping, and SQPOLL stalls when user pages are not resident.

The pass criterion for every test is the same: oc-rsync exits with a
mapped error code and a role-tagged message; it must not abort, panic, or
deliver an unhandled SIGBUS. We accept transfer failure; we do not accept
process death without diagnostics or data corruption on the destination.

## Environment

| Attribute | Value |
|-----------|-------|
| Kernel | 5.15+ (io_uring SQPOLL stable), 6.1+ preferred |
| Filesystems | ext4, xfs, btrfs (reflink path), tmpfs (control) |
| cgroup version | v2 (unified hierarchy) |
| Privileges | `CAP_SYS_ADMIN` for cgroup writes, `CAP_IPC_LOCK` for `mlockall` |
| oc-rsync build | `--features io_uring` plus `--release` |
| Comparison binary | upstream rsync 3.4.1 from `target/interop/upstream-src` |

Tests that require cgroup `memory.high`, `mlockall`, or `madvise` against
foreign mappings cannot run in the GitHub-hosted Linux runner. They live
behind a `#[ignore]` attribute and are gated on the
`OC_RSYNC_MEMORY_PRESSURE_TESTS=1` environment variable, which the
dedicated bare-metal VM sets.

## Test Matrix

| ID | Name | Trigger | Tier | CI |
|----|------|---------|------|----|
| MP-01 | Baseline mmap transfer, no pressure | Normal load | Smoke | Yes |
| MP-02 | Page cache shrink via cgroup `memory.high` | Memory balloon | Stress | VM only |
| MP-03 | Basis truncation mid-transfer | `ftruncate(0)` | Fault injection | VM only |
| MP-04 | `madvise(MADV_DONTNEED)` on basis | Forced eviction | Fault injection | VM only |
| MP-05 | Swap-disabled OOM contention | `swapoff -a` + balloon | Stress | VM only |
| MP-06 | Same workload without io_uring | Buffered I/O fallback | Regression | Yes |
| MP-07 | Sender-side `unlink` while mapped | `unlink` then close | Fault injection | VM only |
| MP-08 | SQPOLL idle stall under reclaim | Reclaim during idle window | Stress | VM only |

Tiers: **Smoke** runs in the standard nextest job. **Stress** runs nightly
on the bare-metal VM via the `memory-pressure.yml` workflow.
**Fault injection** runs nightly and on `fast_io`-touching PRs labelled
`needs-pressure-tests`. **Regression** mirrors a stress test with io_uring
disabled to confirm the failure mode is io_uring-specific.

## Test Specifications

### MP-01: Baseline mmap transfer

- Environment: any Linux runner, ext4, no cgroup limits.
- Setup: source tree of 1 GiB across 64 files (mix of 16 MiB and 64 KiB
  payloads to span the mmap threshold). Destination empty. SQ depth 128.
- Trigger: `oc-rsync -a --inplace src/ dst/`.
- Expected outcome: transfer succeeds, exit 0, byte-identical destination.
- Pass criteria: zero SIGBUS, zero panics, mtime drift below 1 ns,
  `getrusage(RUSAGE_SELF)` shows page-cache hits dominating
  (`ru_minflt > 10 * ru_majflt`).

### MP-02: Page cache shrink via cgroup memory.high

- Environment: dedicated VM, ext4, cgroup v2 root.
- Setup: 4 GiB source file. Move oc-rsync into cgroup with
  `memory.high=256M`, `memory.max=512M`. Pre-warm page cache, then start
  transfer.
- Trigger: launch a sibling allocator inside the same cgroup that touches
  256 MiB of anonymous pages every 100 ms once oc-rsync reaches the
  delta phase.
- Expected outcome: kernel reclaims pages from the basis mapping; either
  transfer completes (kernel re-faults pages) or transfer fails with a
  graceful I/O error. `dmesg` may show `oom_reaper` activity but oc-rsync
  must not be killed by OOM if `memory.max` is not breached.
- Pass criteria: process never SIGBUS; if transfer fails, exit code is one
  of `{11, 23, 30}` with a role-tagged stderr message identifying the
  basis path.

### MP-03: Basis truncation mid-transfer

- Environment: VM, xfs (allocates aggressively, surfaces SIGBUS quickly).
- Setup: 2 GiB basis file, mapped read-only. Receiver reads via mmap
  while sender streams deltas.
- Trigger: at 25 % progress, a sidecar thread calls
  `ftruncate(basis_fd, 0)` on the basis file.
- Expected outcome: subsequent loads from the mapping past offset 0 fault
  with SIGBUS at the kernel level. oc-rsync must catch this through the
  signal handler installed by `fast_io::io_uring::install_sigbus_guard`
  (TBD; this test motivates that handler) and translate it to
  `Error::IoMappedFileTruncated`.
- Pass criteria: exit code 23, stderr contains the basis path and
  `[receiver]`, no core file produced, destination temp file is removed.

### MP-04: `madvise(MADV_DONTNEED)` on basis

- Environment: VM, ext4. Requires that oc-rsync expose a debug-only
  syscall hook (cfg=`oc_rsync_test_hooks`) so the test can call
  `madvise` against the mapping it owns.
- Setup: 1 GiB basis, mmap'd, transfer in progress.
- Trigger: hook fires `madvise(MADV_DONTNEED, addr, len)` once per
  100 ms during the delta phase.
- Expected outcome: pages are dropped, subsequent reads page-fault and
  re-read from disk; transfer succeeds but slows. SQEs that referenced
  evicted pages are either re-issued by the kernel or completed with
  `-EFAULT`, which oc-rsync must surface as a retry, not a fatal error.
- Pass criteria: transfer succeeds; `iostat` shows elevated read traffic;
  at most one retry per SQE; no data corruption on destination
  (`sha256sum` parity with source).

### MP-05: Swap-disabled OOM contention

- Environment: VM with `swapoff -a` executed before the run, ext4, no
  cgroup limits (use raw RAM).
- Setup: 8 GiB basis on a 4 GiB-RAM VM. Run a `stress-ng --vm 2
  --vm-bytes 2G --vm-keep` adversary in parallel.
- Trigger: adversary launches 5 s after oc-rsync starts.
- Expected outcome: kernel evicts file-backed pages (no swap target for
  anonymous pages, so file cache is the only victim). oc-rsync experiences
  page faults, possibly SIGBUS if a backing block was deallocated.
- Pass criteria: oc-rsync either completes the transfer or exits with a
  mapped error; it does not segfault, abort, or hang. The VM workflow
  enforces a 10-minute wall-clock kill.

### MP-06: Same workload without io_uring (regression)

- Environment: same VM as MP-02.
- Setup: identical to MP-02, but oc-rsync built without the `io_uring`
  feature (`--no-default-features --features metadata,zstd`). All other
  variables held constant.
- Trigger: same balloon as MP-02.
- Expected outcome: transfer completes or fails with the same error
  classes as MP-02. Pure-buffered path is the control: if MP-02 fails
  catastrophically and MP-06 succeeds, the failure is io_uring-specific
  and the io_uring guard is the work item.
- Pass criteria: documented baseline numbers (throughput, RSS) recorded
  to `target/test-pressure/MP-06.json` for diffing against MP-02.

### MP-07: Sender-side unlink while mapped

- Environment: VM, ext4 (semantics: unlink keeps inode alive while open).
- Setup: 512 MiB basis open and mapped. Transfer in progress.
- Trigger: `unlink(basis_path)` from sidecar thread.
- Expected outcome: mapping remains valid (Linux unlink-on-close
  semantics), transfer succeeds. Confirms our assumption that filename
  removal does not invalidate pages.
- Pass criteria: transfer succeeds, exit 0. If this fails, the assumption
  in `mmap_reader.rs` is wrong and we must hold a path-stable reference.

### MP-08: SQPOLL idle stall under reclaim

- Environment: VM, kernel 6.1+, io_uring with `IORING_SETUP_SQPOLL`
  enabled and a 200 ms idle timeout.
- Setup: many small files (10 000 x 4 KiB) so SQPOLL sleeps frequently.
- Trigger: trigger reclaim via `echo 3 > /proc/sys/vm/drop_caches`
  every 250 ms.
- Expected outcome: SQPOLL wakeups continue, transfer completes. If
  SQPOLL stalls on user-page pin, the transfer hangs and the test
  fails the 5-minute timeout.
- Pass criteria: completion within 5 minutes, exit 0, byte-identical
  destination.

## Known Limitations

- Cannot reproduce on macOS or Windows. mmap semantics and io_uring are
  Linux-specific; macOS uses `clonefile`, Windows uses IOCP and
  `CopyFileExW`.
- Filesystem variance: btrfs and xfs differ from ext4 on truncation
  semantics. We test ext4 and xfs explicitly; btrfs is observational only.
- No userspace SIGBUS recovery on stable Rust. If the SIGBUS guard
  motivated by MP-03 cannot be implemented safely without nightly, the
  alternative is to disable mmap when io_uring is active on filesystems
  that permit external truncation.
- cgroup v1 not covered. RHEL 7 and SLES 12 are out of support.

## CI Integration

- `MP-01` and `MP-06` join the standard nextest matrix as
  `mp_smoke_baseline` and `mp_smoke_buffered_regression` with 64 MiB
  fixtures so the GitHub runner finishes them in under 60 s.
- `MP-02` through `MP-08` run via `.github/workflows/memory-pressure.yml`
  on a self-hosted bare-metal runner nightly at 02:00 UTC. The workflow
  sets `OC_RSYNC_MEMORY_PRESSURE_TESTS=1` and passes
  `--include-ignored` to nextest.
- Results land in `target/test-pressure/<id>.json`, uploaded as workflow
  artefacts. Regressions in pass rate or throughput open a GitHub issue
  tagged `memory-pressure`.
- Failures of MP-03 or MP-04 block PRs touching `crates/fast_io/src/`,
  enforced via a CODEOWNERS-driven required check so unrelated PRs are
  not gated by VM availability.
