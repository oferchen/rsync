# Windows IOCP benchmark plan: IOCP vs `std::fs::File`

Tracking issues: oc-rsync #1899 (this plan), #1868 (io_uring matched
workloads parent), #1717-#1721 (IOCP module implementation, present in
`crates/fast_io/src/iocp/`). Branch: `docs/windows-iocp-bench-plan-1899`.

This audit defines a focused benchmark plan to compare the oc-rsync
Windows IOCP path against synchronous `std::fs::File` writes on the same
binary, with explicit cross-references to the Linux io_uring matched
workloads tracked under #1868. The companion audit
`docs/audits/windows-iocp-benchmark.md` covers the broader harness
(SMB, daemon TCP, robocopy, Cygwin baseline). This plan narrows scope
to the file I/O comparison required to gate "IOCP-as-default" on
Windows.

## 1. IOCP module under test

The benchmark target is the IOCP file I/O code in
`crates/fast_io/src/iocp/`, implemented under #1717-#1721:

| File | Public surface | Implements |
|------|----------------|------------|
| `mod.rs` | re-exports of `IocpReader`, `IocpWriter`, `IocpDiskBatch`, `IocpReaderFactory`, `IocpWriterFactory`, `reader_from_path`, `writer_from_file`, `IocpConfig`, `is_iocp_available`, `iocp_availability_reason`, `skip_event_optimization_available`, `IOCP_MIN_FILE_SIZE`, `CompletionPump`, `socket` submodule. | Module shape. |
| `config.rs` | `IocpConfig` (`concurrent_ops` default 4, `buffer_size` default 64 KB, `unbuffered`, `write_through`), presets `for_large_files` (8 ops, 256 KB) and `for_small_files` (4 ops, 16 KB), `IOCP_MIN_FILE_SIZE = 64 KiB`, `is_iocp_available()` cached probe, `skip_event_optimization_available()`. | Tunables, runtime detection. |
| `completion_port.rs` | `CompletionPort` RAII wrapper. | `CreateIoCompletionPort` / `CloseHandle`, `associate(handle, key)`. |
| `overlapped.rs` | `OverlappedOp` (`Pin<Box<_>>` over `OVERLAPPED` + buffer). | Pinned buffer lifetime for in-flight ops. |
| `file_reader.rs` | `IocpReader::open`, `read_at`, `read_all_batched`. | `CreateFileW` + `FILE_FLAG_OVERLAPPED`, `ReadFile` overlapped, `GetQueuedCompletionStatus`, `ERROR_IO_PENDING` (997) async path. |
| `file_writer.rs` | `IocpWriter::create`, `create_with_size`, `Write` impl, `sync`. | `CREATE_ALWAYS` + `FILE_FLAG_OVERLAPPED`, `WriteFile` overlapped, `SetFilePointerEx` + `SetEndOfFile` preallocate, `FlushFileBuffers`. |
| `disk_batch.rs` | `IocpDiskBatch::new`, `try_new`, `begin_file`, `write_data`, `flush`, `commit_file`. | Single shared `CompletionPort` reused across files; submits up to `concurrent_ops` overlapped `WriteFile` calls in flight; reaps via `GetQueuedCompletionStatusEx`. The Windows analogue of `IoUringDiskBatch` (#1086). |
| `file_factory.rs` | `IocpReaderFactory`, `IocpWriterFactory`, `IocpOrStdReader`, `IocpOrStdWriter`, `reader_from_path`, `writer_from_file`. | Threshold gating (files below `IOCP_MIN_FILE_SIZE` use `StdFileReader`/`StdFileWriter`); transparent fallback when the port construction fails. |
| `pump.rs` | `CompletionPump`, `CompletionHandler`, `IocpPumpConfig`, `oneshot_handler`, `post_completion`. | Generic completion drainer (used by socket path). |
| `socket.rs` | submodule | Out of scope for this plan; covered separately under #1928-#1932. |

Synchronous baseline under test: `StdFileReader` / `StdFileWriter` in
`crates/fast_io/src/traits.rs` and the disk-commit code in
`crates/transfer/` and `crates/engine/`, all of which on Windows today
go through `std::fs::File` via the `Std` variants because no caller
imports `IocpReaderFactory` / `IocpWriterFactory` yet (`grep -r "Iocp"`
across the workspace returns hits only inside `crates/fast_io/`). The
benchmark therefore measures the IOCP path through direct construction
(via `xtask windows-bench`, see Section 5) until #1897-#1900 wire it
into the engine.

`IocpPolicy` lives at `crates/fast_io/src/lib.rs:431` (`Auto`,
`Enabled`, `Disabled`). The benchmark drives IOCP selection by
constructing `IocpReader` / `IocpWriter` / `IocpDiskBatch` directly and
the synchronous baseline by constructing `StdFileReader` /
`StdFileWriter` directly, so the comparison does not depend on caller
wiring.

## 2. Comparison: IOCP async writes vs `std::fs::File` writes

Two participants, same `oc-rsync.exe` binary, same workload, same
destination filesystem.

| Participant | Path under test | How driven by `xtask windows-bench` |
|-------------|------------------|-----------------------------------|
| `iocp` | `IocpWriter::create_with_size` + `write_all` + `sync` for single-file, `IocpDiskBatch::begin_file` + `write_data` + `flush` + `commit_file` for multi-file. | `--writer iocp [--batch]`. |
| `std` | `std::fs::File::create` + `BufWriter::with_capacity` (256 KB to match upstream `wf_writeBufSize`) + `write_all` + `sync_data`. | `--writer std`. |

Both participants:

- write the same source bytes (deterministic PRNG seeded per workload),
- target the same filesystem (NTFS, default cluster size, drive letter
  configurable via `--dest`),
- preallocate the destination via `SetEndOfFile` for `iocp` and
  `set_len` for `std` so file-system extent allocation cost is
  comparable,
- call `FlushFileBuffers` (IOCP) or `sync_data` (`std`) once per file
  before recording wall time, so the durability boundary is identical,
- run inside a fresh per-cell destination directory that is removed via
  a typed `PathBuf` after the run (never via shell-expanded
  `Remove-Item -Recurse`; see the project's "Containers & Bind Mounts"
  pitfall).

For each (workload, participant) cell capture (per-run):

- `wall_ms` from `QueryPerformanceCounter` taken inside the runner.
- `user_seconds`, `kernel_seconds` from `GetProcessTimes` over the
  runner process delta (the runner does only the writes between start
  and stop markers, so process-level CPU is the I/O CPU).
- `peak_working_set_mib` from `GetProcessMemoryInfo`
  (`PeakWorkingSetSize`).
- `read_bytes`, `write_bytes`, `other_bytes`, `read_ops`, `write_ops`
  from `GetProcessIoCounters`.
- `iocp_wired: bool` set by the runner (always `true` for `iocp`,
  `false` for `std`); `iocp_skip_event:
  skip_event_optimization_available()`.
- `throughput_mibps = workload_bytes / (wall_ms / 1000) / 1048576`.

JSON shape mirrors `benchmark_hyperfine.sh --export-json` keys
(`mean`, `stddev`, `median`, `min`, `max`, `times`) so the existing
chart generator ingests Windows results without translation. Windows
extras land under a nested `"windows"` object
(`win_peak_working_set_mib`, `win_user_seconds`, `win_kernel_seconds`,
`win_read_bytes`, `win_write_bytes`, `win_read_ops`, `win_write_ops`)
to match the convention in
`docs/audits/windows-iocp-benchmark.md` Section 4.

## 3. Workload axes

Three axes, fully crossed for v1 to keep the cell count tractable
(3 file-size buckets x 1 file-count distribution per bucket x 4
concurrency settings = 12 cells per participant = 24 cells total):

### 3.1 Small files (4 KiB - 64 KiB)

- `small_4k`: 10 000 files x 4 KiB. Total 39 MiB.
  - IOCP behaviour: every file is below `IOCP_MIN_FILE_SIZE = 64 KiB`,
    so `IocpWriterFactory::open` (and `writer_from_file`) **falls back
    to `StdFileWriter`**. Driving this cell directly through
    `IocpWriter::create` (bypassing the factory) measures the cost of
    using IOCP below its intended threshold; the result must show that
    the factory threshold is correct (i.e. IOCP loses to `std` here).
- `small_16k`: 10 000 files x 16 KiB. Total 156 MiB. Below threshold;
  same notes as above.
- `small_64k`: 10 000 files x 64 KiB. Total 625 MiB. Exactly at
  threshold; the factory still uses `StdFileWriter` (strict `>`),
  driving directly via `IocpWriter` measures the lowest-file-size
  IOCP-positive case.

### 3.2 Large files (1 MiB - 100 MiB)

- `large_1m`: 1 000 files x 1 MiB. Total 977 MiB. Per-file IOCP setup
  cost amortizes across writes; the interesting case for
  `IocpDiskBatch` because the shared port reuses the association cost
  (vs `IocpWriter` which creates a fresh port per file).
- `large_16m`: 64 files x 16 MiB. Total 1024 MiB. Bulk-write regime;
  read-ahead and overlapping `WriteFile` should dominate.
- `large_100m`: 10 files x 100 MiB. Total 977 MiB. Sequential bulk
  throughput; sensitive to `concurrent_ops` and `buffer_size`.

### 3.3 Concurrent streams (N)

For `large_16m`, vary the number of concurrent destination files
written by independent threads:

- `streams_1` (sequential one-file-at-a-time, baseline).
- `streams_4` (4 threads, 4 destination files).
- `streams_16` (16 threads).
- `streams_64` (64 threads, deliberately above `concurrent_ops = 4`
  to surface port contention).

Concurrency tests use:

- `iocp` participant with one `IocpDiskBatch` per thread (each batch
  owns its own `CompletionPort`).
- `iocp_shared` participant variant (subset only) that uses a single
  `IocpDiskBatch` with `begin_file` called N times to keep one port
  shared across the streams. Probes whether the per-file association
  cost or the port-per-thread overhead dominates.
- `std` participant with one `BufWriter<File>` per thread.

## 4. Cross-reference to Linux io_uring matched workloads (#1868)

Each Windows workload above has a Linux io_uring counterpart driven by
the same source-bytes generator, run by the existing
`scripts/benchmark.sh` / `scripts/benchmark_hyperfine.sh` plus the
`xtask` runner under `tools/ci/`. The v1 plan does not modify those
scripts; it only adds the same workload generators to
`xtask windows-bench` so the byte-for-byte payload matches.

| Windows workload | Linux io_uring counterpart | io_uring path under test | Source |
|------------------|----------------------------|--------------------------|--------|
| `small_4k`, `small_16k`, `small_64k` | `small_files` (`benchmark_hyperfine.sh:setup_small_files`) | `IoUringWriter` for files above `IO_URING_MIN_FILE_SIZE`; standard `File` for the rest. | `crates/fast_io/src/io_uring/file_writer.rs`, factory in `file_factory.rs`. |
| `large_1m`, `large_16m`, `large_100m` | `large_file` (`benchmark_hyperfine.sh:setup_large_file`) | `IoUringWriter` with `IoUringDiskBatch` (#1086); fixed-buffer registered I/O. | `crates/fast_io/src/io_uring/disk_batch.rs`, `registered_buffers.rs`. |
| `streams_{1,4,16,64}` | `streams_{1,4,16,64}` (new sub-cell of `large_file`) | `IoUringDiskBatch` shared ring (`shared_ring.rs`), per-thread submission. | `crates/fast_io/src/io_uring/shared_ring.rs`. |

The matched comparison reports two ratios per workload:

1. `iocp_speedup = std_mean / iocp_mean` on Windows.
2. `io_uring_speedup = std_mean / io_uring_mean` on Linux.

A divergence > 25 % between the two ratios on otherwise comparable
hardware (NVMe disks, similar core count) is flagged for investigation.
The expectation, by design, is that both async backends should deliver
roughly comparable speedup over their respective synchronous baselines
on bulk writes, and roughly zero speedup (or a regression) on
sub-threshold small-file workloads. Material asymmetry indicates a
configuration bug or a missing optimization on one side.

## 5. Required runner: `windows-latest` with `--features iocp`

### 5.1 Build matrix

The benchmark binary is built on the GitHub Actions `windows-latest`
runner (Windows Server 2022 / 2025; uses NTFS) with:

```
cargo build --release \
  --target x86_64-pc-windows-msvc \
  -p xtask --bin xtask \
  --features fast_io/iocp
```

`fast_io/iocp` is in the workspace default features
(`crates/fast_io/Cargo.toml:39`) so the explicit `--features` is
defensive: a future change that drops `iocp` from defaults must not
silently disable the benchmark.

The benchmark binary is `xtask windows-bench` (new subcommand under
`xtask/src/commands/windows_bench/`, gated `#[cfg(target_os =
"windows")]` with a no-op stub elsewhere - same pattern as
`xtask/src/commands/docs/`). It depends only on `windows-sys 0.61`
(already a `fast_io` Windows dependency) for `GetProcessTimes`,
`GetProcessMemoryInfo`, `GetProcessIoCounters`,
`QueryPerformanceCounter`, plus `fast_io::iocp::*` for the IOCP
participant. No PowerShell required for v1; this plan is narrower than
the broader harness in `docs/audits/windows-iocp-benchmark.md` and
defers PowerShell driving to the v2 expansion that adds SMB and daemon
transports.

### 5.2 CI workflow

New workflow file `.github/workflows/benchmark-windows-iocp.yml` (not
created in this PR; tracked under #TBD-W1):

```
name: benchmark-windows-iocp
on:
  pull_request:
    paths:
      - "crates/fast_io/src/iocp/**"
      - "xtask/src/commands/windows_bench/**"
  schedule:
    - cron: "0 6 * * 1"  # weekly full run, Mondays 06:00 UTC
  workflow_dispatch:

jobs:
  bench:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo build --release -p xtask --features fast_io/iocp
      - run: |
          .\target\release\xtask.exe windows-bench \
            --workloads small_4k,small_16k,small_64k,large_1m,large_16m,large_100m \
            --streams 1,4,16,64 \
            --runs ${{ github.event_name == 'schedule' && 10 || 3 }} \
            --warmup 3 \
            --writers iocp,std \
            --json out/bench.json --md out/bench.md --csv out/bench.csv
      - uses: actions/upload-artifact@v4
        with:
          name: windows-iocp-bench-${{ github.run_id }}
          path: out/
```

PR runs use 3 measurements per cell (smoke); scheduled weekly runs use
10 (release-grade). The runner is GitHub-hosted to keep the comparison
reproducible across maintainers; a self-hosted runner is required only
if PR-blocking variance < 5 % becomes a goal (not a v1 requirement).

### 5.3 Run-counts and statistical contract

Inherited from `benchmark_hyperfine.sh`: 3 warmup, 10 measurement runs
per cell on the scheduled weekly job. Cells with `stddev > 10 % of
mean` after 10 runs are re-run with 20 runs; if still > 10 %, the cell
is flagged "noisy" in the markdown summary and excluded from the
release-note table. The raw `times` array is always emitted in JSON.

## 6. Expected outcomes and decision criteria for IOCP-as-default

### 6.1 Expected outcomes

The plan must validate or invalidate these hypotheses:

- **H1 (large-file write speedup):** on `large_16m` and `large_100m`,
  IOCP via `IocpDiskBatch` is at least as fast as
  `BufWriter<File>` + `sync_data` on the same host, and within 10 %
  parity of the Linux `io_uring` speedup ratio on matched hardware.
  Expected speedup: 1.2x-2.0x over `std`, similar to the io_uring
  delta in #1086.
- **H2 (small-file fallback correctness):** on `small_4k` and
  `small_16k`, IOCP via `IocpWriter::create` (bypassing the factory
  threshold) is **slower** than `std` because completion-port setup per
  file (~10-20 us) exceeds the per-file write time. The factory
  threshold `IOCP_MIN_FILE_SIZE = 64 KiB` therefore correctly routes
  these to `StdFileWriter`. Expected outcome: IOCP loses by 10 %-50 %.
- **H3 (concurrency scaling):** on `streams_{4,16,64}`, IOCP scales
  better than `std` because the thread-per-file synchronous path
  serializes inside the kernel filesystem driver while overlapped
  writes can overlap with kernel-side queueing. Expected outcome: IOCP
  speedup ratio grows from ~1.5x at `streams_4` toward >= 2.0x at
  `streams_16`.
- **H4 (per-port saturation):** on `streams_64`, the per-thread `iocp`
  variant should beat the `iocp_shared` variant because the per-port
  outstanding-op limit (`concurrent_ops = 4` by default) becomes the
  bottleneck when one port serves 64 streams. Expected outcome:
  `iocp_shared` flatlines or regresses past `streams_16`; `iocp`
  per-thread continues to scale.
- **H5 (CPU efficiency):** IOCP `kernel_seconds / wall_seconds` ratio
  is lower than `std` on bulk workloads (overlapped completion replaces
  per-write blocking syscall). Expected reduction: 10 %-30 %.

### 6.2 Decision criteria for IOCP-as-default

"IOCP-as-default" means flipping `IocpPolicy::Auto` from the current
"available but unused" state to "use IOCP whenever
`is_iocp_available()` is true and the file size threshold is met". The
benchmark gates that flip behind:

| Criterion | Required result | Source data |
|-----------|-----------------|-------------|
| C1 - Large-file no regression | `iocp_speedup >= 1.0` (within 1-stddev) on `large_1m`, `large_16m`, `large_100m` for both `streams_1` and `streams_4`. | H1, H3. |
| C2 - Small-file fallback correctness | Factory-routed `small_4k` / `small_16k` cells use `StdFileWriter` (asserted by `iocp_wired_write=false` annotation) and match the unmodified `std` baseline within 5 %. Direct-IOCP `small_4k` cells confirm IOCP loses below the threshold (justifies the threshold). | H2. |
| C3 - Concurrency scaling | `iocp_speedup` is non-decreasing from `streams_1` to `streams_16` with no cell below 1.0x. `streams_64` may regress and that is acceptable - it documents the per-port limit. | H3, H4. |
| C4 - CPU envelope | IOCP `(user + kernel) / wall` is no higher than `std` `(user + kernel) / wall + 5 %` on bulk workloads. We do not accept "faster but burns more CPU" as a default. | H5. |
| C5 - Linux parity | `iocp_speedup` is within +/-25 % of `io_uring_speedup` on matched workloads (#1868). Larger divergence requires investigation before flipping defaults. | Section 4 cross-reference. |
| C6 - Variance | All gating cells have `stddev <= 10 % of mean` over >= 10 runs on the scheduled weekly job. Noisy cells block the flip until the variance source (background task, AV scan, page-cache pressure) is identified. | Section 5.3. |
| C7 - Correctness | Every `iocp` run produces destination bytes byte-identical to the source (verified by `Get-FileHash -Algorithm SHA256` or `windows-sys` `BCryptHashData`); no run records a partial write or a write past `SetEndOfFile`. Mismatches invalidate the run. | Runner contract. |

When all seven criteria are met across two consecutive scheduled weekly
runs, `IocpPolicy::Auto` is flipped to enable IOCP by default for files
above `IOCP_MIN_FILE_SIZE` on Windows. The flip itself is a separate
PR; this benchmark plan only produces the data.

If C1, C3, or C5 fails, the failure mode is reported with a follow-up
audit and remediation issue (likely: tune `concurrent_ops` /
`buffer_size`, or move association cost outside the hot path via a
shared per-process pump). C2 failure indicates the factory threshold
is wrong and must be re-tuned. C4 failure rules out the default flip
even when wall time wins.

## 7. Out of scope

These are deferred to follow-up plans and are explicitly **not**
covered here:

- Socket IOCP (`WSARecv` / `WSASend` / `AcceptEx` / `ConnectEx`):
  tracked under #1928-#1932 and the `iocp::socket` submodule. Daemon
  TCP and SSH transports continue through `std::net::TcpStream` until
  that wiring lands.
- `FILE_FLAG_NO_BUFFERING` and `FILE_FLAG_WRITE_THROUGH`: present in
  `IocpConfig` but no caller validates sector alignment or durability
  semantics. Re-evaluate once the alignment audit lands.
- Cross-process IOCP saturation, CSV / DFS-N share targets, ReFS:
  v1 measures NTFS local writes only.
- SMB and `rsync://` daemon transports: covered separately by
  `docs/audits/windows-iocp-benchmark.md` Section 2.1 and Section 5
  (skip-with-rationale until #1928-#1932 land).
- Cygwin / MSYS upstream baseline and `robocopy`: same companion
  audit. This plan focuses solely on within-binary IOCP vs `std`.

## References

- IOCP module: `crates/fast_io/src/iocp/` (#1717-#1721).
- IOCP policy and capability detection: `crates/fast_io/src/lib.rs`
  (`IocpPolicy`, `iocp_status_detail`, `platform_io_capabilities`),
  `crates/fast_io/src/iocp/config.rs`.
- io_uring matched workloads (#1868): `crates/fast_io/src/io_uring/`
  (`disk_batch.rs`, `file_writer.rs`, `shared_ring.rs`, `file_factory.rs`).
- Companion audits:
  - `docs/audits/windows-iocp-benchmark.md` - broader harness covering
    SMB, daemon TCP, robocopy, Cygwin baseline.
  - `docs/audits/disk-commit-iouring-batching.md` (#1086) - io_uring
    counterpart to `IocpDiskBatch`.
  - `docs/audits/io-uring-fixed-buffer-audit.md` - io_uring registered
    buffer comparison.
- Existing benchmark scripts (Linux): `scripts/benchmark.sh`,
  `scripts/benchmark_hyperfine.sh`, `scripts/benchmark_remote.sh`.
- Win32 documentation: `CreateIoCompletionPort`,
  `GetQueuedCompletionStatus`, `GetQueuedCompletionStatusEx`,
  `FILE_FLAG_OVERLAPPED`, `FILE_SKIP_SET_EVENT_ON_HANDLE`,
  `GetProcessTimes`, `GetProcessMemoryInfo`, `GetProcessIoCounters`,
  `QueryPerformanceCounter` (https://learn.microsoft.com/windows/win32/api/).
