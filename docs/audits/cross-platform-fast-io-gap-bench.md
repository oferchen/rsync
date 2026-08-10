# Cross-platform fast-I/O gap benchmark plan (#1386)

Tracking issue: oc-rsync task #1386. This is a benchmark plan, not a
results report. It defines the workloads, metrics, and platform matrix
needed to quantify the performance gap between the three fast paths
oc-rsync wires for bulk file transfer:

- Linux io_uring writes via `fast_io::io_uring`.
- macOS reflink and copy-engine via `fast_io::platform_copy`
  (`clonefile(2)`, `fcopyfile(3)`).
- Windows `CopyFileExW` plus IOCP-pumped overlapped writes via
  `fast_io::iocp` and `fast_io::copy_file_ex`.

Companion documents:

- `docs/audits/fast-io-fallback-macos-vs-linux.md` (#1652) - source-
  grounded audit of the macOS gap.
- `docs/design/iocp-transfer-pipeline-wiring.md` (#1868) - Windows
  IOCP wiring that closed the receiver-side gap on NTFS.
- `docs/design/macos-kqueue-fast-io.md` (#1385) - the kqueue backend
  proposed to close most of the macOS write-side gap.

## 1. Three fast paths under test

### 1.1 Linux: io_uring writes (`fast_io::io_uring`)

- Public surface: `IoUringFileFactory`, `IoUringFileWriter`,
  `IoUringFileReader`, `SharedRing`, `DiskBatch`. Probed at startup
  (`io_uring/mod.rs`); falls back to `pwrite`/`std::fs` if the kernel
  lacks the required ops or `IORING_REGISTER_*` quotas.
- Strengths to measure:
  - Submission and completion in shared rings; no per-syscall trap on
    the steady-state path once SQEs are batched.
  - Registered buffers (`IORING_REGISTER_BUFFERS`) avoid per-write
    page-pinning. Buffer-ring (`IORING_REGISTER_PBUF_RING`) supplies
    landing pages without copy.
  - `IORING_OP_LINKAT` and `IORING_OP_RENAMEAT2` keep temp-file commit
    inside the same ring (no `linkat(2)` syscall round-trip).
  - SQPOLL with kernel-thread polling can drop submission syscalls to
    near zero on sustained workloads.
- Expected wins: large sustained writes, 10K-file batches, and
  modify-in-place delta apply where the writer can pipeline sub-block
  writes.

### 1.2 macOS: clonefile and fcopyfile (`fast_io::platform_copy`)

- Public surface: `DefaultPlatformCopy::copy_file`, `try_clonefile`,
  `try_fcopyfile` (`platform_copy/mod.rs`, `platform_copy/dispatch.rs`).
- Strengths to measure:
  - `clonefile(2)` is an O(1) APFS reflink: a metadata-only operation
    that materialises a full copy-on-write twin regardless of source
    size. No data copy happens until the next write.
  - `fcopyfile(3)` invokes the macOS copy-engine, which uses the VM
    subsystem to move pages without bouncing through user space when
    the source is mmap-friendly.
- Expected wins: any single-file copy on the same APFS volume - the
  cost is independent of file size. fcopyfile is the fallback for
  cross-volume or non-APFS targets.
- Known weak spot: the receiver-side delta-apply path does not benefit
  from clonefile, because the destination is rebuilt from a network
  byte stream. There is no async write surface today, so 1 MB of
  network input becomes ~16 `pwrite` calls (#1652).

### 1.3 Windows: CopyFileExW plus IOCP overlapped writes

- Public surface: `copy_file_ex.rs` (`CopyFileExW` wrapper),
  `iocp/file_writer.rs`, `iocp/disk_batch.rs`, `iocp/pump.rs`,
  `iocp/completion_port.rs`.
- Strengths to measure:
  - `CopyFileExW` for whole-file copies leverages the kernel copy
    engine; on ReFS it can fall through to
    `FSCTL_DUPLICATE_EXTENTS_TO_FILE` for an O(1) block-clone.
  - IOCP pump dispatches up to N overlapped `WriteFileEx` operations
    against a single completion port; the kernel handles the wakeup.
    The transfer disk-commit writer routes to `Writer::Iocp` on
    Windows when present (#1868).
  - Buffer reuse keeps each outstanding `OVERLAPPED` entry on a
    pre-pinned page, similar in spirit to io_uring registered
    buffers.
- Expected wins: receiver-side delta apply (the case io_uring also
  wins, where macOS still trails), large sustained writes on NTFS,
  whole-file copies on ReFS.

## 2. Bench methodology

All numbers below are targets for the harness, not measurements. The
harness must be code-driven and reproducible from a clean checkout.

### 2.1 Workloads

| ID | Name | Shape | What it stresses |
|----|------|-------|------------------|
| W1 | Single 1 MB copy | One file, fits in one buffer | Per-op overhead and syscall floor |
| W2 | Single 100 MB copy | One file, sustained writeout | Buffer pipeline depth, registered-buffer benefit |
| W3 | Single 1 GB copy | One file, beyond cache | Steady-state throughput, fdatasync cost, copy-engine path |
| W4 | 10 K small files | 4 KiB each, flat directory | Per-file fixed cost: open, write, link, fsync, rename |
| W5 | Modify-in-place delta apply | 100 MB basis, 5% block churn | Sub-block writes, sparse-region preservation, delta-script `apply_delta` |

W1 isolates fixed overhead. W3 isolates throughput. W4 is where each
platform's directory-entry and metadata-commit batching shows up. W5
is the one workload where all three fast paths must do roughly the
same amount of useful work, so the gap between them is purest.

### 2.2 Metrics

Captured per workload, per platform, per backend:

- Wall time (`hyperfine --warmup 3 --runs 10`).
- Syscall count (`strace -c` on Linux, `dtruss -c` on macOS, ETW or
  `Process Monitor` on Windows).
- Peak RSS (`/usr/bin/time -v`, `time -l` on macOS, `Get-Process` on
  Windows).
- Bytes written to disk vs bytes ingressed (clonefile should show
  near-zero disk bytes on W1 and W2).

### 2.3 Backends per platform

Each workload runs against every backend the platform supports, plus
the portable fallback, so the gap is measured both end-to-end and per
hop in the fallback chain:

- Linux: `io_uring (registered)`, `io_uring (unregistered)`,
  `copy_file_range`, `std::fs::copy`.
- macOS: `clonefile`, `fcopyfile`, `std::fs::copy`.
- Windows: `CopyFileExW`, `IOCP overlapped + WriteFileEx`,
  `std::fs::copy`.

`std::fs::copy` is the shared baseline. The fallback chain is wired
in `crates/fast_io/src/platform_copy/dispatch.rs`; the harness must
exercise each link by forcing the policy enums (`IoUringPolicy`,
`IocpPolicy`) rather than relying on auto-detection.

### 2.4 Hardware matrix

- Linux: Arch container `localhost/oc-rsync-bench:latest` on the
  podman host; kernel 6.x with io_uring enabled. Filesystem: ext4
  with `data=ordered`, plus a btrfs partition for FICLONE checks.
- macOS: Mac mini M2 baseline (recorded under #1659). Filesystem:
  APFS on the internal SSD; an external HFS+ volume to force the
  fcopyfile path.
- Windows: Windows 11 runner from the CI matrix. Filesystem: NTFS;
  add a ReFS volume for the `FSCTL_DUPLICATE_EXTENTS_TO_FILE` path.

### 2.5 Driver

- Reuse `crates/fast_io/benches/platform_copy.rs` and
  `crates/fast_io/benches/io_optimizations.rs` as the criterion
  driver. Add a `delta_apply` group that wires through
  `engine::delta::script::apply_delta` against a synthetic basis.
- W4 uses a generator that writes 10 K files of fixed size into a
  fresh `tempfile::TempDir`, reproduced from the small-file test
  fixtures already in `crates/fast_io/tests/`.

## 3. Expected outcomes per platform

Predictions only; the bench will accept or reject them.

### 3.1 W1 - 1 MB single-file copy

- macOS clonefile: dominates by 10x or more. The cost is constant in
  file size, so even at 1 MB the metadata-only path beats any read-
  write loop.
- Windows CopyFileExW on ReFS: parity with macOS once the FSCTL path
  fires. On NTFS, parity with the Linux `copy_file_range` path.
- Linux io_uring: parity with `copy_file_range`; SQE batching doesn't
  pay back its setup cost at this size.
- Gap exposed: macOS leads. Linux and Windows-NTFS within noise of
  each other.

### 3.2 W2 - 100 MB single-file copy

- macOS clonefile: still dominates for the copy itself. fcopyfile on
  cross-volume targets falls in line with Linux/Windows.
- Linux io_uring with registered buffers: starts to pull ahead of
  `copy_file_range` because the buffer-ring keeps the destination
  page-cache hot without bounce.
- Windows CopyFileExW: parity with macOS fcopyfile. IOCP-pumped
  raw writes only win when the source is a network stream, not a
  local file - irrelevant for W2.
- Gap exposed: APFS reflink dwarfs both, but among the read-write
  paths Linux io_uring beats Windows by the cost of SetFilePointer
  per request.

### 3.3 W3 - 1 GB single-file copy

- macOS clonefile: still constant-time. The first write after the
  copy will charge real I/O, but the bench measures the copy itself.
- Linux io_uring (registered, SQPOLL): peak. Submission syscalls
  approach zero; the dominant cost is fdatasync at end of file.
- Windows IOCP overlapped writes: parity with Linux io_uring on NTFS
  if the queue depth is tuned (16-32 outstanding `WriteFileEx`).
  CopyFileExW on ReFS is again O(1).
- Gap exposed: macOS leads on APFS, Windows leads on ReFS, Linux
  leads on ext4. The cross-FS comparison is the answer to "where is
  oc-rsync paying for the wrong abstraction."

### 3.4 W4 - 10 K small-file batch

- Linux io_uring with `IORING_OP_LINKAT` and `IORING_OP_RENAMEAT2`:
  big win. The temp-file commit step collapses to one CQE per file.
- Windows IOCP plus `MoveFileExW` post-write: middle of the pack.
  The completion port absorbs commit syscalls, but rename remains
  blocking.
- macOS: trails. There is no async surface; every file is a
  synchronous `open`, `write`, `fsync`, `rename` chain. This is
  exactly the workload `docs/design/macos-kqueue-fast-io.md`
  (#1385) targets.
- Gap exposed: macOS slowest by 2-3x against Linux io_uring. This is
  the highest-priority workload for closing the platform gap.

### 3.5 W5 - modify-in-place delta apply

- Linux io_uring: dominant. The receiver pipelines sub-block writes
  against a single shared ring; `apply_delta` already caches basis
  offsets for sequential `COPY` tokens.
- Windows IOCP: close behind. Each overlapped write hits the
  completion port without blocking the apply loop.
- macOS: trails. `apply_delta` falls back to the synchronous
  `pwrite` chain because there is no async surface. clonefile does
  not help: the destination is rebuilt block by block from a network
  stream, so the fast path never fires.
- Gap exposed: this is the workload that motivates the kqueue backend
  (#1385). Until it lands, macOS pays the same syscall tax as Linux
  did before io_uring.

## 4. Workloads that expose the gap

Ranked by how much they discriminate between platforms, highest first:

1. W4 (10 K small files) - macOS lacks any batched commit path;
   Linux io_uring linkat-in-ring and Windows IOCP both keep up.
2. W5 (modify-in-place delta apply) - macOS lacks an async write
   surface; the others pipeline through their completion engines.
3. W3 (1 GB copy) - separates the per-FS reflink paths (APFS, ReFS)
   from the read-write paths (ext4, NTFS).
4. W1 (1 MB copy) - mostly a clonefile demo on macOS; uninteresting
   on the other two platforms.
5. W2 (100 MB copy) - same shape as W1 plus a small advantage to
   io_uring's registered buffers.

## 5. Out of scope

- Cross-host benchmarks. SSH and daemon transfers are covered by
  `scripts/benchmark_remote.sh` and the benchmark workflow; this plan
  is local-disk only.
- `--inplace --append` and `--whole-file` interaction. Tracked in
  `project_post_v059_perf.md` and not part of this gap analysis.
- Buffer-pool tuning. Each backend uses its default policy; tuning is
  a follow-up once the gap is quantified.
