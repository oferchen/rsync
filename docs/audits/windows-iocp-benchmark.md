# Windows IOCP benchmark plan

Tracking issue: oc-rsync task #1899. Branch: `feat/windows-iocp-benchmark-plan-1899`.
No code changes; this audit proposes a benchmark suite, harness layout, and
artifact contract for measuring the IOCP file-I/O path on Windows.

## Scope

Define a benchmark plan that compares the oc-rsync IOCP file-I/O path
(`crates/fast_io/src/iocp/`) against the synchronous fallback in the same
binary, against upstream rsync running under Cygwin/MSYS POSIX emulation, and
against a non-rsync native baseline (`robocopy`). The plan covers local copy,
SMB share copy, and `rsync://` daemon transfer scenarios at large-file,
small-file, and mixed-tree workloads. The output must match the metric
vocabulary used by `scripts/benchmark.sh`, `scripts/benchmark_hyperfine.sh`,
and `scripts/benchmark_remote.sh` so Windows numbers can sit beside the
Linux/macOS numbers in release notes without translation.

Source files inspected (all paths repository-relative):

- `crates/fast_io/src/iocp/mod.rs` (module shape, re-exports).
- `crates/fast_io/src/iocp/config.rs` (`IocpConfig`, `is_iocp_available`,
  `IOCP_MIN_FILE_SIZE`, `skip_event_optimization_available`).
- `crates/fast_io/src/iocp/completion_port.rs` (`CompletionPort` RAII wrapper
  around `CreateIoCompletionPort` / `CloseHandle`).
- `crates/fast_io/src/iocp/overlapped.rs` (`OverlappedOp` pinned `OVERLAPPED`
  + buffer container).
- `crates/fast_io/src/iocp/file_reader.rs` (`IocpReader::read_at`,
  `read_all_batched`, `GetQueuedCompletionStatus`, `FILE_FLAG_OVERLAPPED`).
- `crates/fast_io/src/iocp/file_writer.rs` (`IocpWriter::write_at`,
  `WriteFile`, `FlushFileBuffers`, `SetEndOfFile`).
- `crates/fast_io/src/iocp/file_factory.rs` (`IocpReaderFactory`,
  `IocpWriterFactory`, `reader_from_path`, `writer_from_file`).
- `crates/fast_io/src/iocp_stub.rs` (non-Windows / no-feature stub).
- `crates/fast_io/src/lib.rs` (`IocpPolicy`, `iocp_status_detail`,
  `platform_io_capabilities`).
- `crates/fast_io/Cargo.toml` (default features include `iocp`,
  `windows-sys = "0.59"` with `Win32_System_IO` / `Win32_Storage_FileSystem`).
- `scripts/benchmark.sh`, `scripts/benchmark_hyperfine.sh`,
  `scripts/benchmark_remote.sh` (existing Linux/POSIX benchmark format).

## TL;DR

The IOCP code in `fast_io` is real, feature-gated, and exposes a complete
`Reader`/`Writer` factory pair, but **it is not currently wired into any
caller**. A grep for `Iocp` outside `crates/fast_io/` returns zero matches
in `engine`, `transfer`, `core`, or `cli`, so today every Windows file
read/write goes through `StdFileReader` / `StdFileWriter`. Sockets are not
addressed at all - the IOCP module only ships `file_reader` / `file_writer`,
no socket reader/writer, no `WSARecv`/`WSASend`/`AcceptEx`/`ConnectEx`
plumbing. Issues #1897-#1900 and #1928-#1932 track wiring IOCP into the
file and socket paths. The benchmark plan must therefore cover three
distinct states - "today" (synchronous fallback only), "file-IOCP wired"
(post-#1897/#1898/#1899), and "socket-IOCP wired" (post-#1928-#1932) - and
must declare what is measurable at each stage. The harness lives in
`scripts/benchmark_windows.ps1` driving `Measure-Command` loops and an
`xtask windows-bench` Rust runner that emits the same JSON shape as
`benchmark_hyperfine.sh --export-json`. Hyperfine is not used: hyperfine's
PowerShell support has rough edges around quoting and exit-code propagation,
and we get cleaner CPU/RSS numbers from `Get-Process`/`Get-Counter` than
from hyperfine's per-platform shims.

## Upstream evidence

Upstream rsync 3.4.1 under `target/interop/upstream-src/rsync-3.4.1/` has no
IOCP path - it runs on Windows only via Cygwin or MSYS, where the POSIX
emulation layer maps `read(2)` / `write(2)` onto `ReadFile` / `WriteFile`
synchronously. The relevant entry points in upstream are
`fileio.c:write_file` (256 KB static `wf_writeBuf`, plain `write(2)`) and
`receiver.c:recv_files` (per-file open + write loop). There is no upstream
parity obligation for IOCP itself; the obligation is on the visible result
(final on-disk content, mtime, perms, exit code, wire bytes) being
indistinguishable from upstream regardless of which I/O backend ran.

## 1. Current IOCP state

### 1.1 What is implemented

`crates/fast_io/src/iocp/`:

- `IocpReader` (`file_reader.rs:28`): opens the file with
  `CreateFileW(... FILE_FLAG_OVERLAPPED ...)`, associates the handle with a
  per-reader `CompletionPort` via `CreateIoCompletionPort`, and submits
  reads through `ReadFile` against pinned `OVERLAPPED` structures.
  `read_at` (`file_reader.rs:89`) handles the synchronous-completion
  fast-path (no completion queued) and the `ERROR_IO_PENDING` (997) async
  path waiting on `GetQueuedCompletionStatus`. `read_all_batched`
  (`file_reader.rs:160`) submits up to `concurrent_ops` reads at a time.
- `IocpWriter` (`file_writer.rs:27`): opens with `CREATE_ALWAYS` +
  `FILE_FLAG_OVERLAPPED`, owns a buffer that flushes via overlapped
  `WriteFile`, supports `create_with_size` (preallocate via
  `SetFilePointerEx` + `SetEndOfFile`), and `sync` via `FlushFileBuffers`.
- `CompletionPort` (`completion_port.rs:17`): RAII wrapper holding the port
  handle and `associate(file_handle, key)`. Standalone port created with
  `INVALID_HANDLE_VALUE`; cleaned up via `CloseHandle` in `Drop`.
- `OverlappedOp` (`overlapped.rs:16`): `Pin<Box<_>>` co-locates the
  `OVERLAPPED` and the I/O buffer so the kernel sees stable pointers for
  the operation lifetime.
- `IocpConfig` (`config.rs:33`): `concurrent_ops` (default 4),
  `buffer_size` (default 64 KB), `unbuffered` (`FILE_FLAG_NO_BUFFERING`,
  default off), `write_through` (`FILE_FLAG_WRITE_THROUGH`, default off).
  Presets `for_large_files` (8 ops, 256 KB) and `for_small_files`
  (4 ops, 16 KB).
- `IocpReaderFactory` / `IocpWriterFactory` (`file_factory.rs:144`,
  `file_factory.rs:202`): respect `IOCP_MIN_FILE_SIZE = 64 KB`
  (`config.rs:23`) for reads (smaller files use `StdFileReader`), and fall
  back transparently when ring construction fails.
- `IocpPolicy` (`lib.rs:431`): `Auto` (default), `Enabled` (errors if
  unavailable), `Disabled`. `is_iocp_available` (`config.rs:91`) caches
  the probe result in a process-wide atomic; `skip_event_optimization_available`
  (`config.rs:105`) reports whether `FILE_SKIP_SET_EVENT_ON_HANDLE` is
  active. The probe in `probe_iocp` (`config.rs:127`) creates a standalone
  port and immediately closes it.

### 1.2 What is NOT implemented

- **Socket IOCP**: `crates/fast_io/src/iocp/` contains no `socket_reader`,
  no `socket_writer`, no `WSARecv` / `WSASend` / `AcceptEx` / `ConnectEx`
  call sites. `platform_io_capabilities` (`lib.rs:327`) lists `IOCP` as a
  Windows capability but the Linux io_uring path exposes
  `IoUringSocketReader` / `IoUringSocketWriter` while the Windows side has
  no equivalent. Daemon and SSH transports on Windows still go through
  `std::net::TcpStream` (synchronous read/write).
- **Caller wiring**: `Grep -r "Iocp"` across the workspace returns hits
  only inside `crates/fast_io/` itself plus `Cargo.toml` (feature
  declaration). Neither `engine` (delta pipeline, local-copy executor) nor
  `transfer` (disk commit thread, network thread) nor `core` (session
  facade) nor `cli` (no `--iocp` / `--no-iocp` flags) imports any IOCP
  symbol. `IocpPolicy` is a public type with no production caller today.
- **`writer_from_file` cannot use IOCP**: `file_factory.rs:268` documents
  that an existing `std::fs::File` was opened without `FILE_FLAG_OVERLAPPED`,
  so it cannot be associated with a completion port. Even with
  `IocpPolicy::Enabled` the function falls through to `StdFileWriter`. Any
  caller that opens with `std::fs::File::create` and then asks for IOCP
  silently gets buffered I/O.
- **Per-file completion port**: each `IocpReader` / `IocpWriter` allocates
  its own `CompletionPort` (`file_reader.rs:60`, `file_writer.rs:60`).
  There is no shared port, no thread-pool drainer, no per-process
  proactor. This is fine for a single-file read but does not scale to a
  many-files workload because port creation costs ~10-20 us and the OS
  enforces a per-process port budget.
- **Read-ahead reordering**: `read_all_batched` waits for completions in
  submission order (`file_reader.rs:213-242` comment "completions are
  processed in submission order"). True out-of-order completion handling
  would require keying off `OVERLAPPED` pointers and is not implemented.
- **Direct I/O alignment**: `IocpConfig::unbuffered` exists but no caller
  validates sector alignment of buffers / offsets, which
  `FILE_FLAG_NO_BUFFERING` requires. The flag is present for future use
  only.

### 1.3 Issue tracker context

The benchmark plan must explicitly enumerate which paths are measurable
today and which are blocked behind specific issues:

- **#1897, #1898**: file-read IOCP integration into the receiver hot path
  (basis read + verify). Until they land, "oc-rsync IOCP read" benchmarks
  are forced fallback - that is a documented data point, not a missing
  measurement.
- **#1899**: this task. The benchmark plan itself.
- **#1900**: file-write IOCP integration into the disk commit thread.
  Mirrors the io_uring `IoUringDiskBatch` audit (#1086) but for Windows.
- **#1928, #1929**: socket-read IOCP via `WSARecv`. Required for the
  `rsync://` daemon to pull and the SSH transport to receive without
  thread-per-connection blocking.
- **#1930, #1931**: socket-write IOCP via `WSASend`. Required for the
  daemon push path.
- **#1932**: `AcceptEx` / `ConnectEx` integration for the daemon
  listener. Listener path; affects daemon `accept(2)` latency under load.

The benchmark plan covers all listed paths with explicit "skip with
rationale" entries when the code path is not yet wired.

## 2. Proposed benchmark suite

### 2.1 Workloads

Each workload runs locally on Windows, over an SMB share to a peer host,
and over `rsync://` daemon TCP. SMB and daemon TCP are configured to point
at the same peer (a Linux box running smbd and oc-rsyncd) so that network
RTT does not vary between protocols.

| Workload | Source layout | Total bytes | Why |
|----------|--------------|-------------|-----|
| `large_1g` | 1 file x 1 GiB random data | 1 GiB | Tests bulk write throughput, IOCP read-ahead, write coalescing. Dominated by per-buffer cost; matches `benchmark_hyperfine.sh:setup_large_file` shape (scaled 10x). |
| `small_10k` | 10000 files x 1 KiB | 9.77 MiB | Tests per-file open/close/commit overhead. Most files are below `IOCP_MIN_FILE_SIZE`, so `IocpReaderFactory::open` falls back to `StdFileReader` per file. The data point here is exactly that the IOCP factory threshold is correct. |
| `small_10k_64k` | 10000 files x 64 KiB | 625 MiB | Same file count above the IOCP minimum, so reads go through `IocpReader::read_all_batched`. Isolates per-file IOCP setup cost from synchronous I/O. |
| `mixed_tree` | 100 dirs x (1 x 4 MiB + 100 x 4 KiB) | ~440 MiB | Realistic source-tree shape, mirrors `benchmark_hyperfine.sh:setup_mixed_tree`. |
| `delta_update` | `large_1g` with 1 % random byte mutations on the destination | 1 GiB read, ~10 MiB sent | Exercises the rolling+strong checksum scheduler over IOCP reads on the basis file plus IOCP writes on the temp file. The interesting case for the receiver hot path. |

### 2.2 Comparison points

For every workload x transport combination, run all of these participants
that are buildable for the current participant matrix. Skip-with-rationale
applies when a binary is unavailable on the host.

| Participant | Binary / invocation | Notes |
|-------------|---------------------|-------|
| `oc-rsync-iocp` | `oc-rsync.exe --iocp=enabled ...` | Forces IOCP for file reads/writes once #1897-#1900 land. Until then, behaves like `oc-rsync-sync` and the run is recorded with the `wired=false` annotation. |
| `oc-rsync-sync` | `oc-rsync.exe --iocp=disabled ...` | Synchronous fallback. The within-binary control. |
| `oc-rsync-iocp-socket` | `oc-rsync.exe --iocp=enabled --iocp-socket=enabled ...` | Adds socket IOCP. Skipped (rationale logged) until #1928-#1932 land and a `--iocp-socket` flag exists. |
| `upstream-rsync-cygwin` | `C:\cygwin64\bin\rsync.exe ...` | Upstream 3.4.1 built under Cygwin. Translates `read`/`write` to `ReadFile`/`WriteFile` synchronously via newlib. The closest "what users have today" baseline. |
| `upstream-rsync-msys` | `C:\msys64\usr\bin\rsync.exe ...` | Optional second POSIX baseline. Some users ship MSYS rather than Cygwin. Skip if not installed. |
| `robocopy` | `robocopy.exe SRC DEST /E /COPY:DAT /NJH /NJS` | Native Windows non-rsync baseline. Local copy and SMB only - no rsync wire protocol. Measures the device + filesystem ceiling. |

`oc-rsync` does not currently expose `--iocp` or `--iocp-socket`. The
benchmark plan assumes those flags will be added as part of #1897-#1932 in
parity with the existing `--io-uring` / `--no-io-uring` flags. Until they
exist, the harness sets the policy via the `OC_RSYNC_IOCP=enabled` /
`OC_RSYNC_IOCP=disabled` environment variable that callers can introduce
in `cli` without changing the public CLI surface.

### 2.3 Metrics

For every (workload, transport, participant, run) tuple capture:

- **Wall time** (`Measure-Command { ... }` `TotalMilliseconds`).
- **Throughput** (`MiB/s = workload_bytes / wall_time_seconds / 1048576`).
- **CPU time** (`Get-Process -Id $pid` after wait: `CPU` property =
  `TotalProcessorTime`). Captured as `user_seconds`, `kernel_seconds` via
  the `Get-Process | Select-Object UserProcessorTime,
  PrivilegedProcessorTime` columns.
- **CPU%** = `(user + kernel) / wall * 100 / num_cpus`.
- **Peak working set** (`Get-Process -Id $pid` `PeakWorkingSet64`,
  expressed in MiB).
- **Peak private bytes** (`PeakPagedMemorySize64` + `PeakNonpagedSystemMemorySize64`).
- **Bytes transferred on the wire** (for daemon and SMB scenarios:
  `Get-NetTCPConnection` byte counters before/after, or via the
  `oc-rsync --stats` JSON output for oc-rsync runs).
- **Per-file syscall counts** (optional, debug builds only): hook
  `CreateFile` / `ReadFile` / `WriteFile` via Detours-style instrumentation
  exposed under a Cargo `bench-syscalls` feature. Out of scope for v1.
- **Tagged annotations** logged with each run:
  - `iocp_wired: bool` (whether the engine actually invoked the IOCP
    factory; read by emitting a single `tracing::info!` line at startup
    when `IocpReader` / `IocpWriter` is constructed).
  - `iocp_skip_event: bool` (`skip_event_optimization_available()`).
  - `kernel_build`, `os_version`, `cpu_model`, `disk_model`,
    `filesystem_type`, `cluster_size`, `participant_version`.

The metric names match the JSON keys emitted by
`benchmark_hyperfine.sh --export-json`: `mean`, `stddev`, `median`, `min`,
`max`, `times`. Add Windows-specific keys with the `win_` prefix
(`win_peak_working_set_mib`, `win_peak_private_mib`, `win_user_seconds`,
`win_kernel_seconds`).

### 2.4 Run counts and statistical contract

Inherit from `benchmark_hyperfine.sh`: 3 warmup, 10 measurement per
(workload, transport, participant) cell. Total cell count is
`5 workloads * 3 transports * up to 6 participants = 90` cells, run cost
~3-6 hours on a quiet host. Allow `--scenario` filter mirroring
`benchmark_hyperfine.sh -s small_files` to run a single workload, and a
`--participants` filter to drop unavailable binaries cleanly.

Reject cells where `stddev > 10 % of mean` after 10 runs and re-run with
20 runs. If still > 10 %, log the cell as "noisy" and exclude it from
release-note tables; downstream consumers must surface the original
observations.

## 3. Harness

### 3.1 Layout (proposed; not created in this PR)

- `scripts/benchmark_windows.ps1` - top-level driver, mirrors
  `scripts/benchmark.sh` shape (option parsing, prereq checks, workload
  setup teardown, JSON export). PowerShell rather than bash because (a)
  the surface APIs we want (`Measure-Command`, `Get-Process`, `Get-Counter`,
  `Get-NetTCPConnection`) are PowerShell-native, (b) Cygwin bash on
  Windows reports POSIX-emulated time, not native NT performance counters,
  and (c) PowerShell ships in-box on Windows 10+ without extra installs.
- `xtask/src/commands/windows_bench/` - Rust runner invoked as
  `cargo xtask windows-bench --workload large_1g --transport local
  --participants oc-rsync-iocp,oc-rsync-sync`. Spawns the participant
  binary with `std::process::Command`, attaches a job object so the child
  cannot escape, captures `GetProcessTimes`, `GetProcessMemoryInfo`, and
  `QueryProcessIoCounters` after wait. Emits one JSON line per run plus a
  summary JSON document at end. The `xtask` exists today as a workspace
  member (`xtask/`) so this is a new subcommand, not a new crate.
- `scripts/benchmark_windows.ps1` invokes the xtask binary for each cell
  rather than re-implementing process measurement in PowerShell. The
  PowerShell layer drives workload setup/teardown, the Rust layer does
  the per-run measurement, the JSON shape is owned by the Rust layer.

### 3.2 Why not hyperfine

`benchmark_hyperfine.sh` uses hyperfine because hyperfine handles
warmup/measurement and exports JSON/markdown. On Windows hyperfine has
known issues:

- `hyperfine --setup` / `--cleanup` invokes commands through a shell that
  is not bash on Windows. Quoting rules differ between PowerShell, cmd.exe,
  and (Cygwin/MSYS) bash, and the rsync command line is sensitive to
  quoting (filter rules, paths with spaces).
- hyperfine reports wall time via `QueryPerformanceCounter` but does not
  expose CPU time, working set, or per-process I/O counters. We need
  CPU% and RSS for a meaningful comparison.
- hyperfine does not capture child-process exit codes cleanly when the
  child is launched through a wrapper shell, and rsync exit codes are
  load-bearing for the comparison.

`Measure-Command` plus `Get-Process` covers the wall+CPU+RSS triple
natively. The xtask runner gets us cross-validation: every metric is
collected twice (once by the PowerShell script, once by the Rust child
using Win32 APIs) and the harness asserts they agree within 1 %.

### 3.3 Artifacts

For each invocation of `scripts/benchmark_windows.ps1`:

- `benchmark_windows_<timestamp>.json` - one JSON document with the
  schema:
  ```
  {
    "host": { "os": "Windows 11 23H2", "cpu": "...", "disk": "...", ... },
    "binaries": { "oc-rsync": "0.5.9", "rsync_cygwin": "3.4.1", "robocopy": "10.0.22621.1" },
    "iocp": { "available": true, "skip_event": true, "wired_file": false, "wired_socket": false },
    "scenarios": [
      { "workload": "large_1g", "transport": "local", "participant": "oc-rsync-iocp",
        "runs": 10, "mean_ms": ..., "stddev_ms": ..., "throughput_mibps": ...,
        "win_peak_working_set_mib": ..., "win_user_seconds": ..., ... }
    ]
  }
  ```
- `benchmark_windows_<timestamp>.md` - markdown table per workload, one
  row per participant, suitable for direct inclusion in release notes.
- `benchmark_windows_<timestamp>.csv` - flat row-per-(scenario,run) export
  for downstream charting.
- A diff summary appended to `benchmark_windows_<timestamp>.md` showing
  the IOCP-vs-sync delta for each workload, computed as
  `(sync.mean - iocp.mean) / sync.mean * 100`. Negative means IOCP is
  slower than the synchronous fallback - a real and possible result that
  must be visible.

### 3.4 Harness invariants

- The harness must be runnable on a stock Windows 11 host with no
  developer tooling beyond Rust + Cygwin/MSYS (for upstream rsync).
  Specifically: no `make`, no `bash`, no Python.
- The harness must not modify global system state. No registry edits, no
  service installs, no firewall rule changes outside its lifetime. SMB
  and rsyncd peers run on a separate Linux host that is configured by a
  separate, out-of-band setup script (out of scope here).
- The harness must verify destination integrity before recording timing.
  Each run is followed by a `Get-FileHash -Algorithm SHA256` over source
  and destination; mismatched hashes invalidate the run and the cell is
  re-run up to 3 times before being flagged.
- Each measurement run runs in its own destination directory and deletes
  the directory afterward. **Never use `Remove-Item -Recurse -Force` on a
  variable-expanded path that could be empty - it has bitten this project
  before (per the project's "Containers & Bind Mounts" pitfall).** The
  xtask runner uses a typed `PathBuf` and refuses to delete the workspace
  root.

## 4. Parity with existing scripts

The Windows harness must produce JSON keys that overlap with the Linux
scripts so a chart generator can ingest both:

| Key | Source on Linux | Source on Windows |
|-----|-----------------|-------------------|
| `mean` (s) | hyperfine `--export-json` | xtask `wall_seconds` averaged across runs. |
| `stddev` (s) | hyperfine | xtask sample-stddev across runs. |
| `times` ([s]) | hyperfine | xtask per-run `wall_seconds` array. |
| `command` | hyperfine | xtask records the participant invocation including all args. |
| `throughput_mibps` | derived in `benchmark_hyperfine.sh` | derived identically. |

Windows-only additions (`win_*` prefix) are written under a nested
`"windows"` object in the JSON so the chart generator can ignore them
without conflict. The key naming convention matches the `iocp_*` log
fields already emitted by `iocp_status_detail` (`lib.rs:177`) and
`iocp_availability_reason` (`config.rs:113`).

## 5. Coverage of known IOCP gaps

The benchmark plan covers each IOCP gap surfaced by the issue tracker
either as a measured cell or as an explicit "skip with rationale" entry.

### 5.1 Cells covered by the v1 benchmark

- **#1897, #1898 file read**: covered by all five workloads under the
  `oc-rsync-iocp` participant once the wiring lands. Pre-wiring the cell
  records `iocp_wired_file=false` and the harness logs the run as
  measuring the synchronous fallback only.
- **#1900 file write (disk commit)**: covered by `large_1g`, `small_10k_64k`,
  and `delta_update`. `small_10k` is below `IOCP_MIN_FILE_SIZE` and uses
  the synchronous path even with IOCP enabled - that is the correct
  outcome and the cell records it with `iocp_wired_write=false`.
- **#1928, #1929 socket read**: covered by all daemon-TCP workloads when
  the participant is `oc-rsync-iocp-socket`. Until the wiring lands the
  participant resolves to `oc-rsync-iocp` and the cell skips with
  rationale `"socket IOCP not yet wired (#1928, #1929)"`.
- **#1930, #1931 socket write**: same shape; covered when daemon push
  workloads run under the socket-IOCP participant.
- **#1932 AcceptEx/ConnectEx**: covered indirectly by daemon connection
  cost. The harness measures `time-to-first-byte` on the daemon control
  channel: connect + greeting + module list. That isolates the listener
  cost from the transfer cost. Skipped with rationale until the wiring
  lands.

### 5.2 Cells explicitly skipped

- **`FILE_FLAG_NO_BUFFERING` / unbuffered**: `IocpConfig::unbuffered`
  exists but no caller validates sector alignment. The benchmark does
  not exercise unbuffered mode in v1 because correctness is unverified
  at the alignment boundary. Track separately.
- **`FILE_FLAG_WRITE_THROUGH`**: equivalent to `--fsync` per file, which
  is already covered by the io_uring-side disk-commit audit. Out of scope
  here; rerun the benchmark with `oc-rsync --fsync` to capture write-through
  numbers when needed.
- **Multi-process IOCP saturation**: a single benchmark process consumes
  one completion port per file. Multi-tenant workloads (many concurrent
  oc-rsync invocations on the same host) are not measured; the per-process
  port budget on Windows is high enough that this is unlikely to bite,
  but no measurement is taken.
- **Cluster-shared-volume / DFS-N share targets**: standard SMB shares
  only in v1. CSV / DFS-N have different coherency semantics and need a
  separate plan.

## 6. Recommendation

**Build the harness in two phases.** Phase A is the PowerShell driver
plus xtask runner with the synchronous-fallback participant only. That
produces a reproducible Windows baseline number for the upcoming v0.6.0
release and validates the JSON contract. Phase B adds the IOCP
participants once #1897-#1932 begin landing; the harness already supports
the participants list, only the binaries and the `--iocp=enabled` flag
need to come online.

Concrete next steps (none of these are implemented in this PR):

1. **#TBD-A:** Add a `--iocp` / `--no-iocp` CLI flag to the `oc-rsync`
   binary in parity with `--io-uring` / `--no-io-uring`, threaded into a
   new `IocpPolicy` field on `CoreConfig`. Until #1897-#1900 land the
   flag is observable only through `iocp_status_detail` in
   `oc-rsync --version`. (Findings on `IocpPolicy` having no caller
   today.)
2. **#TBD-B:** Implement `xtask windows-bench` in a new
   `xtask/src/commands/windows_bench/` module, gated `#[cfg(target_os
   = "windows")]`, with a no-op stub on other platforms. Use
   `windows-sys = "0.59"` (already in `fast_io`) for `GetProcessTimes`,
   `GetProcessMemoryInfo`, `QueryProcessIoCounters`. Reuse the JSON
   emission pattern from `xtask/src/commands/docs/`.
3. **#TBD-C:** Author `scripts/benchmark_windows.ps1` calling the xtask
   subcommand, with workload setup helpers paralleling
   `benchmark_hyperfine.sh:setup_small_files` /
   `setup_large_file` / `setup_mixed_tree`.
4. **#TBD-D:** Add a CI workflow `.github/workflows/benchmark-windows.yml`
   that runs phase A on the `windows-latest` runner with `runs=3` (CI
   smoke test only) on every PR touching `crates/fast_io/src/iocp/` and
   on a weekly schedule for full `runs=10` measurements pushed to a
   benchmark artifact bucket.
5. **Deferred to follow-up audits:** unbuffered / write-through cells;
   multi-process IOCP saturation; CSV / DFS-N share targets; integration
   with the `IoUringStats` / `BufferPoolStats` reporting hooks once the
   IOCP path emits comparable telemetry.

## References

- Upstream rsync 3.4.1 source: `target/interop/upstream-src/rsync-3.4.1/`
  (`fileio.c:write_file`, `receiver.c:recv_files`).
- Existing oc-rsync IOCP infrastructure: `crates/fast_io/src/iocp/`
  (modules `mod`, `config`, `completion_port`, `overlapped`,
  `file_reader`, `file_writer`, `file_factory`).
- Existing benchmark scripts:
  - `scripts/benchmark.sh` (POSIX, oc-rsync vs upstream local copy).
  - `scripts/benchmark_hyperfine.sh` (POSIX, statistically rigorous, JSON
    + markdown export).
  - `scripts/benchmark_remote.sh` (POSIX, `rsync://` daemon comparison
    across versions).
- Companion audits:
  - `docs/audits/disk-commit-iouring-batching.md` (#1086) - the io_uring
    counterpart to #1900 file-write integration.
  - `docs/audits/iouring-socket-sqpoll-defer-taskrun.md` - the io_uring
    counterpart to #1928-#1932 socket integration.
  - `docs/audits/macos-dispatch-io.md` - the macOS counterpart in the
    "third async I/O backend" decision space.
- Win32 documentation: `CreateIoCompletionPort`,
  `GetQueuedCompletionStatus`, `FILE_FLAG_OVERLAPPED`,
  `FILE_SKIP_SET_EVENT_ON_HANDLE`, `WSARecv`, `WSASend`, `AcceptEx`,
  `ConnectEx` (all under https://learn.microsoft.com/windows/win32/api/).
