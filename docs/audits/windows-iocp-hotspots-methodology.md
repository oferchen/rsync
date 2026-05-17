# Windows IOCP hotspots: profiling methodology (#2300)

Tracking issue: oc-rsync #2300 (WPG-1). Sibling audits:
[`docs/audits/iocp-sync-blocking-audit.md`](iocp-sync-blocking-audit.md)
(14 known sync points; PR #4332),
[`docs/audits/windows-iocp-benchmark.md`](windows-iocp-benchmark.md)
(broad harness),
[`docs/audits/windows-iocp-benchmark-plan.md`](windows-iocp-benchmark-plan.md)
(IOCP-vs-`std::fs::File` plan),
[`docs/benchmarks/windows-throughput.md`](../benchmarks/windows-throughput.md)
(CI throughput job).

## Purpose

The Windows throughput benchmark (`scripts/windows_throughput_bench.sh`,
PR #4327, with the drilldown extension in PR #4352) tells us **how**
fast `oc-rsync.exe` is against the MSYS2 upstream baseline. It does
not tell us **why** the IOCP path stalls when it does. This document
defines the methodology a Windows operator follows to convert that
wall-clock signal into a ranked, actionable hotspot table that maps
each top stack to either an in-flight PR or a planned WPG task.

The session author runs macOS / Linux only, so this document is the
deliverable: a future Windows operator (CI maintainer, contributor with
a Windows box, or a self-hosted runner pipeline) executes the steps
below and pastes the resulting hotspot table back into this file.

## Tooling

Pick one of the three stacks below. They produce equivalent hotspot
tables; the choice is driven by license and operator familiarity.

| Stack | License | Strengths | Weaknesses |
|-------|---------|-----------|------------|
| **Intel VTune Profiler** | Free for non-commercial use, paid for commercial use under oneAPI | Stack-sampled Hotspot + I/O analysis in one tool; symbol resolution against Rust release builds with PDBs works out of the box; per-thread timeline with overlapped-IO completion port annotations | Heavyweight install (~2 GiB); Intel-CPU bias for microarch counters; not suitable for AMD-CPU hotspot deltas |
| **ETW + Windows Performance Analyzer (WPA)** | Free (ships with ADK / Windows SDK) | Native Windows tracing, kernel-side block-IO timeline, file-object lifetime, no agent injection; same trace can be replayed offline | WPA UI has a learning curve; symbol resolution requires `_NT_SYMBOL_PATH` setup |
| **Windows Performance Recorder (WPR)** | Free (ships with ADK / Windows SDK) | Profile-driven (XML), scriptable from CI, smaller install than VTune | Less interactive than WPA; still requires WPA (or `xperf -i`) to view |

**Default choice for the first WPG-1 pass:** ETW + WPA. It is free,
ships in the Windows ADK, and the same `.etl` trace covers both CPU
hotspots and kernel block-IO. VTune is the recommended follow-up when
WPA highlights a hotspot that needs microarchitectural breakdown
(branch mispredict, L3 miss).

Required ancillary tooling (all stacks):

- Windows ADK / SDK with the Performance Toolkit feature
  (`wpr.exe`, `wpa.exe`, `xperf.exe`).
- `_NT_SYMBOL_PATH` pointing at the Microsoft public symbol server
  plus the local `target/release-with-debug/` PDB directory.
- MSYS2 bash to run `scripts/windows_throughput_bench.sh` (already a
  prerequisite of PR #4327).
- `hyperfine` on `PATH` (already a prerequisite of the bench script).
- Administrator PowerShell window for kernel-mode ETW collection
  (`wpr -start` requires `SeSystemProfilePrivilege`).

## Profiling steps

### Step 1 - Build oc-rsync with debug symbols

```sh
# From an MSYS2 bash or PowerShell at the repo root.
cargo build --profile release-with-debug --features iocp --bin oc-rsync
```

The `release-with-debug` profile (defined in `Cargo.toml`, line 360)
keeps full LLVM optimisations on but emits PDB files so VTune / WPA
can resolve Rust symbols. The `iocp` feature is on by default for the
Windows binary; the explicit flag is for clarity and to fail loudly
on a misconfigured feature matrix.

Output binary: `target/release-with-debug/oc-rsync.exe`. The
companion PDB lives in the same directory.

### Step 2 - Run the throughput bench with the drilldown flag

```sh
# In MSYS2 bash.
OC_RSYNC_BENCH_DRILLDOWN=1 \
  OC_RSYNC=/c/path/to/target/release-with-debug/oc-rsync.exe \
  BENCH_RUNS=5 BENCH_WARMUP=2 \
  bash scripts/windows_throughput_bench.sh
```

`OC_RSYNC_BENCH_DRILLDOWN=1` (PR #4352) keeps the per-iteration
intermediate directories on disk and writes a `bench-out/drilldown/`
sub-tree the profiler can re-run a single iteration against without
regenerating the 1 GiB fixture.

Confirm the bench produced `bench-out/large_1gib.json` and
`bench-out/small_10000.json` and that the drilldown directory contains
the source fixture for each scenario before starting the profiler.

### Step 3 - Capture an ETW Hotspot trace with stack sampling

In an **Administrator PowerShell** window:

```powershell
# Start sampling: CPU profile + stack walks on every sample.
wpr.exe -start CPU -start GeneralProfile -filemode

# In a separate MSYS2 shell, re-run the slowest scenario from the
# drilldown directory so we measure only the transfer phase.
OC_RSYNC=/c/path/to/target/release-with-debug/oc-rsync.exe \
  oc-rsync -a "$DRILLDOWN_LARGE_SRC/" "$DRILLDOWN_LARGE_DST/"

# Stop and write the trace.
wpr.exe -stop bench-out\drilldown\large_1gib.etl
```

Repeat for `small_10000`. Two `.etl` files are now ready for offline
analysis in `wpa.exe`.

VTune equivalent (if chosen instead of WPA):

```sh
vtune -collect hotspots -knob sampling-mode=hw \
  -result-dir bench-out/drilldown/vtune-large_1gib -- \
  "$OC_RSYNC" -a "$DRILLDOWN_LARGE_SRC/" "$DRILLDOWN_LARGE_DST/"
```

### Step 4 - Capture an I/O analysis (kernel block-IO timeline)

In **Administrator PowerShell**:

```powershell
wpr.exe -start DiskIO -start FileIO -filemode

# Re-run the transfer (drilldown source, same destination drive).
oc-rsync -a "$DRILLDOWN_LARGE_SRC/" "$DRILLDOWN_LARGE_DST/"

wpr.exe -stop bench-out\drilldown\large_1gib.io.etl
```

The DiskIO + FileIO trace gives the operator a per-handle timeline of
overlapped `WriteFile` start, kernel queue depth, completion arrival,
and `FlushFileBuffers` latency. Cross-correlating with the CPU trace
from Step 3 attributes time spent in `GetQueuedCompletionStatus`
(synchronous drain in `IocpWriter`, sync point #1) to the matching
kernel-side wait.

VTune equivalent: re-run `vtune -collect io` on the same workload;
results land under a sibling `-result-dir`.

### Step 5 - Classify hotspots and emit the report

Open `large_1gib.etl` in `wpa.exe` (or the VTune result GUI). Apply
these views in order:

1. **CPU Usage (Sampled) -> Stack** filtered to `oc-rsync.exe`. Group
   by stack; sort by inclusive sample count. Take the top 10 stacks.
2. **Generic Events -> File I/O** filtered to source/dest paths.
   Confirm overlap (or lack thereof) with the CPU sample timeline.
3. **Disk Usage -> I/O Time per File** to attribute kernel time to
   the destination handle. Note the queue depth peak.

For each of the top stacks, classify into one of three buckets defined
by [`iocp-sync-blocking-audit.md`](iocp-sync-blocking-audit.md):

| Bucket | What it looks like in WPA | Audit row | Mitigation pointer |
|--------|---------------------------|-----------|--------------------|
| **Per-IO blocking drain** | Top stack contains `IocpWriter::write` -> `GetQueuedCompletionStatus`; CPU time tracks the WriteFile sample rate 1:1 | Rows 1, 2, 4, 13 of the sync-blocking audit | Mitigation M1 in PR #4332; WPG-4 (issue #2303) wires `CompletionPump` into `IocpWriter` |
| **CQ-depth saturation** | Top stack contains `drain_completions` -> `GetQueuedCompletionStatusEx`; trace shows full 64-entry batches every drain | Row 4; also row 7 (worker thread) | PR #4358 (WPG-3) auto-sizes the completion array based on observed peak |
| **Bounce-buffer copy** | Top stack contains `IocpDiskBatch::write_data` -> `Vec::extend_from_slice` or `memcpy`; CPU time is in user mode, not kernel | Row 13 (per-buffer-fill flush) | WPG-4 (issue #2303) introduces a double-buffer / registered buffer path |

### Step 6 - Append the hotspot table to this document

The Windows operator pastes the classified table into the
"Results" section below, one row per top-10 stack. Format:

```
| Rank | Inclusive samples | % of run | Top stack (top 3 frames) | Bucket | Mitigation pointer |
|------|-------------------|----------|--------------------------|--------|--------------------|
| 1    |                   |          |                          |        |                    |
```

Stacks deeper than three frames are abbreviated with `...`; the full
stack lives in the `.etl` archive checked into the issue thread.

## Hotspot classification (reference)

Three buckets cover every realistic top-10 stack we expect from the
two scenarios in `scripts/windows_throughput_bench.sh`:

- **Per-IO blocking drain** - the single largest contributor on
  `large_1gib`. Each `WriteFile` is immediately followed by a
  synchronous `GetQueuedCompletionStatus(..., INFINITE)`. Effective
  queue depth is 1 regardless of `IocpConfig::concurrent_ops`. Audit
  rows 1, 4, 13. Mitigation: register the writer with the shared
  `CompletionPump` (WPG-4 / issue #2303) so the next `WriteFile` is
  posted before the previous completion is reaped.
- **CQ-depth saturation** - shows up on the `small_10000` scenario
  when bursts of small writes saturate the fixed
  `COMPLETION_DRAIN_BATCH = 64`. PR #4358 (WPG-3) auto-sizes the
  array; this row's mitigation is "land #4358".
- **Bounce-buffer copy** - user-mode memcpy from caller buffer into
  `IocpDiskBatch`'s owned buffer. Visible whenever the source is
  itself overlapped (e.g. socket -> disk on daemon pull). Mitigation:
  WPG-4 (issue #2303) double-buffer + registered buffer path.

A fourth bucket ("MSYS2 path translation") may appear in
upstream-rsync traces taken for comparison but does not apply to
`oc-rsync.exe`, which is a native Win32 binary.

## Results (to be filled by Windows operator)

This section is intentionally empty in WPG-1. The future Windows
operator runs Steps 1-5, then PRs an update to this section with the
classified table for both `large_1gib` and `small_10000`. The `.etl`
captures should be attached to issue #2300 and referenced here by
filename, not committed to the repo.

### `large_1gib` scenario

_Pending Windows operator run._

### `small_10000` scenario

_Pending Windows operator run._

## Implementation plan (5 steps keyed to WPG-4 .. WPG-6)

1. **WPG-1 (this audit, #2300)** - methodology landed; a Windows
   operator runs Steps 1-5 and fills the Results section. Output: a
   ranked, classified hotspot table per scenario.
2. **WPG-4 (#2303)** - based on the per-IO blocking drain bucket
   ranking from Step 1, land the `CompletionPump`-backed
   `IocpWriter` from mitigation M1 of the sync-blocking audit. Gate
   on the same throughput bench showing improved `large_1gib`
   wall-clock.
3. **WPG-5 (per-file fsync pipelining)** - based on the per-file
   bucket signal from `small_10000`, land mitigation M2 (background
   finalizer thread for `FlushFileBuffers`). Gate on `small_10000`
   wall-clock improvement plus a regression-free `large_1gib`.
4. **WPG-6 (socket-side pipelining)** - mitigation M3 from the
   sync-blocking audit (>=2 sends and recvs in flight in
   `IocpSocketWriter`/`Reader`). Profile the daemon-push path with
   the same methodology before and after; report the resulting
   throughput delta.
5. **WPG-1 closure** - rerun Steps 1-5 against `oc-rsync.exe` built
   from the WPG-4..WPG-6 branches; confirm each bucket from the
   original ranking has either moved down the table or dropped out.
   Close #2300 once the three top-bucket mitigations have a
   corresponding "before vs after" trace pair attached to the issue.

## Constraints and gotchas

- ETW traces can grow quickly (~50 MiB/min at default buffer sizes).
  Always run with `-filemode` and stop the trace as soon as the
  measured iteration finishes.
- Run the profiler against the **drilldown** directory rather than
  the live bench output: hyperfine wipes the destination on every
  iteration via `--prepare`, which would skew the trace toward setup
  cost.
- The `release-with-debug` profile is the only Windows-supported way
  to get Rust symbol resolution; the default `release` profile strips
  debug info and produces "unknown" frames in WPA.
- Never run the profiler under a Windows defender real-time scan; AV
  hooks add IRP-level latency to every `WriteFile` and bias the
  per-IO bucket upward. Add the destination directory and the binary
  to the AV exclusion list before profiling.
- Reuse a typed `PathBuf` for the destination cleanup. Never invoke
  `Remove-Item -Recurse` against a path assembled from environment
  variables; see the "Containers and Bind Mounts" pitfall in the
  project handbook.
