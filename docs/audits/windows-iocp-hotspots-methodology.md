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
- MSYS2 `rsync` package pinned at the version under comparison (see
  "Comparison harness: oc-rsync vs MSYS2 rsync" below). MSYS2 base
  install from <https://www.msys2.org/> with `pacman -Syu` then
  `pacman -S rsync coreutils hyperfine` from an MSYS2 shell.

`_NT_SYMBOL_PATH` one-time setup (PowerShell, persisted for the user):

```powershell
[Environment]::SetEnvironmentVariable(
    'NT_SYMBOL_PATH',
    'srv*C:\Symbols*https://msdl.microsoft.com/download/symbols;' +
    'C:\path\to\oc-rsync\target\release-with-debug',
    'User')
# Re-open the PowerShell window so wpa.exe inherits the variable.
```

## Workloads

The methodology fixes a five-workload matrix. The first two map
directly to the two scenarios that
`scripts/windows_throughput_bench.sh` already generates; the
remaining three are produced by the additional fixture commands
below. All workloads live under `bench-out/drilldown/` so a single
profiler session can switch between them without regenerating data.

| ID | Description | Source layout | Fixture generator |
|----|-------------|---------------|-------------------|
| `single_huge` | One 8 GiB file. Exercises sustained large-write IOCP queue depth and `FlushFileBuffers` cost on a file that exceeds OS write-back cache. | `src/huge.bin` (8 GiB) | `dd if=/dev/urandom of=src/huge.bin bs=1M count=8192` |
| `large_1gib` | Single 1 GiB file. Default scenario from `windows_throughput_bench.sh`. Exercises the per-IO blocking drain on the IOCP file writer. | `src/large.bin` (1 GiB) | `dd if=/dev/urandom of=src/large.bin bs=1M count=1024` |
| `small_10000` | 10000 x 4 KiB files in a flat directory. Default scenario from `windows_throughput_bench.sh`. Exercises per-file fsync and CQ-depth saturation on small writes. | `src/small/file_*` (10000 entries) | Built-in to `windows_throughput_bench.sh`. |
| `many_small_nested` | 50000 files across a 5-level deep tree, file sizes 256 B .. 64 KiB. Stresses directory traversal, file-list build, IOCP per-open overhead, and the NTFS USN journal. | `src/nested/<lvl0>/<lvl1>/.../file_*` | `python tools/bench/make_nested_tree.py --root src/nested --files 50000 --depth 5 --size-min 256 --size-max 65536` (script ships under `tools/bench/`; reuse the Linux helper when available, otherwise the inline Python snippet at the end of this section). |
| `mixed` | Combination of the four workloads above plus 100 sparse files (1 MiB allocated, 256 KiB written). Represents the realistic backup workload. | `src/{huge.bin, large.bin, small/, nested/, sparse/}` | Concatenate the four generators above, then `for i in $(seq 1 100); do truncate -s 1M src/sparse/sparse_$i && dd if=/dev/urandom of=src/sparse/sparse_$i bs=4K count=64 conv=notrunc; done`. |

Inline `make_nested_tree.py` fallback (paste into MSYS2 bash if the
helper script is absent):

```sh
python - <<'PY'
import os, random, sys
root = "src/nested"
total = 50000
depth = 5
size_min, size_max = 256, 65536
random.seed(0)
for i in range(total):
    parts = [str(random.randrange(8)) for _ in range(depth)]
    d = os.path.join(root, *parts)
    os.makedirs(d, exist_ok=True)
    size = random.randint(size_min, size_max)
    with open(os.path.join(d, f"file_{i}"), "wb") as f:
        f.write(os.urandom(size))
PY
```

Each workload is profiled twice: once with `oc-rsync.exe` and once
with the MSYS2 `rsync.exe`. Both runs use the same `src/` tree and a
clean `dst/` tree (delete and recreate between runs).

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
# Start sampling: CPU profile + stack walks on every sample, written
# to a single .etl per session (-filemode disables circular buffer).
wpr.exe -start GeneralProfile -start CPU -filemode

# In a separate MSYS2 shell, re-run the slowest scenario from the
# drilldown directory so we measure only the transfer phase.
OC_RSYNC=/c/path/to/target/release-with-debug/oc-rsync.exe \
  oc-rsync -a "$DRILLDOWN_LARGE_SRC/" "$DRILLDOWN_LARGE_DST/"

# Stop and write the trace.
wpr.exe -stop bench-out\drilldown\large_1gib.etl
```

Repeat for every workload row in the Workloads table. Each produces
one `.etl` ready for offline analysis in `wpa.exe`.

`GeneralProfile` provides scheduler, process, and DLL load events;
`CPU` adds the 1 kHz sampling profile with stack walks. Both are
built-in WPR profiles. For deeper inspection (per-syscall events,
context switches, ready-thread chain) use the explicit profile XML
in [Appendix A](#appendix-a-wpr-profile-xml-iocp-deep-trace) and
start with `wpr.exe -start path\to\iocp-deep.wprp -filemode`.

VTune equivalent (if chosen instead of WPA):

```sh
vtune -collect hotspots -knob sampling-mode=hw \
  -result-dir bench-out/drilldown/vtune-large_1gib -- \
  "$OC_RSYNC" -a "$DRILLDOWN_LARGE_SRC/" "$DRILLDOWN_LARGE_DST/"
```

### Step 4 - Capture an I/O analysis (kernel block-IO timeline)

In **Administrator PowerShell**:

```powershell
# DiskIO + FileIO cover kernel block-IO start/complete and per-handle
# FileObject lifetime. Add SystemServices for the IOCP-relevant
# context-switch and ready-thread events.
wpr.exe -start DiskIO -start FileIO -start SystemServices -filemode

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

To filter the I/O trace to a single provider (useful when the trace
file would otherwise exceed a few hundred MiB), use `xperf.exe`
directly with the `Microsoft-Windows-Kernel-IO` provider:

```powershell
xperf.exe -on Microsoft-Windows-Kernel-IO+Microsoft-Windows-Kernel-File `
          -stackwalk FileCreate+FileRead+FileWrite+FileFlush `
          -BufferSize 1024 -MinBuffers 64 -MaxBuffers 512 `
          -f bench-out\drilldown\large_1gib.kernelio.etl

# Re-run the transfer here (separate MSYS2 shell).

xperf.exe -stop -d bench-out\drilldown\large_1gib.kernelio.etl
```

This isolates `IRP_MJ_WRITE` and `IRP_MJ_FLUSH_BUFFERS` events plus
their call stacks, which is the minimum signal needed to attribute
wall-clock to the IOCP file writer hot path. The resulting `.etl`
opens in `wpa.exe` under **System Activity -> Generic Events** with
the provider GUID filter pre-applied.

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

## Comparison harness: oc-rsync vs MSYS2 rsync

Hotspot profiling alone is not enough; the same workload must be run
through the MSYS2 build of upstream rsync so the operator can pin
each oc-rsync hotspot to a real wall-clock delta against the
reference implementation.

### Install MSYS2 rsync

Pin the comparison to the MSYS2 `rsync` package version that ships
upstream rsync 3.4.2 (the version `oc-rsync` targets for wire
compatibility). At the time of writing, the MSYS2 package is
`rsync-3.4.2-1`.

```powershell
# In an MSYS2 UCRT64 shell (Start menu -> MSYS2 UCRT64).
pacman -Sy
pacman -S --needed rsync=3.4.2-1 coreutils hyperfine
rsync --version | head -n 1
# Expected first line:
#   rsync  version 3.4.2  protocol version 32
```

If `pacman` resolves a newer `rsync`, record the actual version
string in the Results section; do not silently move the baseline.
The methodology only requires that both binaries report the same
`protocol version`.

### Build oc-rsync with the IOCP feature

```sh
# From the repo root in MSYS2 UCRT64.
cargo build --release --features iocp --bin oc-rsync
file target/release/oc-rsync.exe
# Expect: PE32+ executable (console) x86-64, for MS Windows
```

For profiling runs that need PDBs, also build the
`release-with-debug` profile per Step 1. The `--release` binary is
the one used for the head-to-head wall-clock comparison; PDBs are
not needed for hyperfine measurements and slow the build.

### Drive both binaries against the same source tree

`scripts/windows_throughput_bench.sh` already orchestrates this for
the `large_1gib` and `small_10000` workloads. For the remaining
three workloads (`single_huge`, `many_small_nested`, `mixed`), drive
hyperfine directly:

```sh
# From MSYS2 bash. SRC is the fixture root; DST is a per-binary
# scratch directory on the same NTFS volume.
SRC=bench-out/drilldown/many_small_nested/src
DST_OC=bench-out/drilldown/many_small_nested/dst_oc
DST_UP=bench-out/drilldown/many_small_nested/dst_upstream

hyperfine \
  --warmup 1 --runs 5 \
  --prepare "rm -rf $DST_OC && mkdir -p $DST_OC" \
  --prepare "rm -rf $DST_UP && mkdir -p $DST_UP" \
  --export-json bench-out/many_small_nested.json \
  "/c/path/to/target/release/oc-rsync.exe -a $SRC/ $DST_OC/" \
  "rsync -a $SRC/ $DST_UP/"
```

`hyperfine` emits a `bench-out/many_small_nested.json` per workload
with `mean`, `stddev`, `median`, `min`, `max` for each binary. The
JSON shape matches the existing Linux benchmark output so the
Windows numbers feed the release-notes table without translation.

### Capture CPU% and peak RSS

Hyperfine measures wall-clock only. To capture CPU and memory in the
same pass, wrap each invocation in a PowerShell sidecar:

```powershell
# Save as tools\bench\measure-process.ps1 (or paste inline).
param([string]$Binary, [string[]]$Args, [string]$OutJson)

$proc = Start-Process -FilePath $Binary -ArgumentList $Args `
                      -NoNewWindow -PassThru
$samples = @()
while (-not $proc.HasExited) {
    $samples += [pscustomobject]@{
        ts_ms = [int]((Get-Date) - $proc.StartTime).TotalMilliseconds
        cpu_pct = (Get-Counter "\Process($($proc.ProcessName))\% Processor Time" `
                   -ErrorAction SilentlyContinue).CounterSamples.CookedValue
        rss_mib = [math]::Round((Get-Process -Id $proc.Id).WorkingSet64 / 1MB, 2)
    }
    Start-Sleep -Milliseconds 100
}
$proc.WaitForExit()
$summary = [pscustomobject]@{
    exit_code = $proc.ExitCode
    wall_s    = ($proc.ExitTime - $proc.StartTime).TotalSeconds
    cpu_pct_peak = ($samples | Measure-Object cpu_pct -Maximum).Maximum
    cpu_pct_mean = ($samples | Measure-Object cpu_pct -Average).Average
    rss_mib_peak = ($samples | Measure-Object rss_mib -Maximum).Maximum
    samples = $samples
}
$summary | ConvertTo-Json -Depth 4 | Set-Content -Path $OutJson
```

Drive it once per workload, per binary:

```powershell
.\tools\bench\measure-process.ps1 `
    -Binary "C:\path\to\oc-rsync.exe" `
    -Args @("-a", "$SRC\", "$DST_OC\") `
    -OutJson "bench-out\many_small_nested.oc.process.json"

.\tools\bench\measure-process.ps1 `
    -Binary "C:\msys64\usr\bin\rsync.exe" `
    -Args @("-a", "$SRC\", "$DST_UP\") `
    -OutJson "bench-out\many_small_nested.upstream.process.json"
```

The JSON pair plus the hyperfine `bench-out/<workload>.json` form
the per-workload artifact set; attach all three to the issue when
posting results.

## Metrics to report

For each workload the Windows operator records the following metrics
for both binaries side by side. The metric vocabulary matches
`scripts/benchmark.sh` so Windows numbers slot into the existing
release-notes table.

| Metric | Source | Unit | How to derive |
|--------|--------|------|---------------|
| Wall-clock | hyperfine `mean` | seconds | `bench-out/<workload>.json -> results[*].mean` |
| Throughput | computed | MiB/s | `total_bytes_transferred / wall_clock_s / 1024^2`. `total_bytes_transferred` is the on-disk size of `src/` (`du -sb src/` in MSYS2). |
| CPU% peak | `measure-process.ps1` | % | `process.json -> cpu_pct_peak`. On a 4-core box this is bounded at 400. |
| CPU% mean | `measure-process.ps1` | % | `process.json -> cpu_pct_mean`. Sustained CPU utilisation across the run. |
| Peak RSS | `measure-process.ps1` | MiB | `process.json -> rss_mib_peak`. Reflects the highest working-set value sampled at 10 Hz. |
| Syscall count | ETW counter | count | `xperf -i bench-out\drilldown\<workload>.kernelio.etl -a syscall -o syscall.csv` then `awk -F, 'NR>1{sum++} END{print sum}' syscall.csv`. The Kernel-File provider records every `IRP_MJ_*`; counting rows attributed to the process gives an apples-to-apples syscall-equivalent. |
| Completion port wakes | ETW (kernel scheduler) | count | From the SystemServices trace: count of `ReadyThread` events targeted at the IOCP completion-port wait thread inside `oc-rsync.exe`. Use the WPA **CPU Usage (Precise) -> Ready Thread Stacks** view filtered to `GetQueuedCompletionStatus` frames. |
| Errors | hyperfine `exit_codes` + process.json `exit_code` | int | Non-zero is a methodology failure; rerun. |

Reporting format (paste under the workload heading in the Results
section):

```
| Binary | Wall s | MiB/s | CPU% peak | CPU% mean | RSS MiB | Syscalls | CQ wakes |
|--------|--------|-------|-----------|-----------|---------|----------|----------|
| oc-rsync.exe |    |       |           |           |         |          |          |
| MSYS2 rsync  |    |       |           |           |         |          |          |
| delta        |    |       |           |           |         |          |          |
```

`delta` is `(oc-rsync - upstream) / upstream * 100%` for each cell,
signed so positive numbers always mean oc-rsync is worse.

## Expected baselines

The methodology author runs macOS / Linux only, so no Windows baseline
numbers exist in this repository yet. The cells below are populated
**by the first Windows operator who runs the harness end-to-end**;
this section then becomes the regression gate for WPG-4..WPG-6.

Until the first run lands, the cells read `tbd by first run`. Once
populated, each subsequent WPG PR must either improve the
oc-rsync numbers without regressing the upstream baseline (which
should be invariant across runs on the same hardware), or include a
note in the PR body justifying any regression.

### `single_huge` (8 GiB single file)

| Binary | Wall s | MiB/s | CPU% peak | CPU% mean | RSS MiB | Syscalls | CQ wakes |
|--------|--------|-------|-----------|-----------|---------|----------|----------|
| oc-rsync.exe | tbd by first run | tbd | tbd | tbd | tbd | tbd | tbd |
| MSYS2 rsync 3.4.2 | tbd by first run | tbd | tbd | tbd | tbd | tbd | n/a |

### `large_1gib` (1 GiB single file)

| Binary | Wall s | MiB/s | CPU% peak | CPU% mean | RSS MiB | Syscalls | CQ wakes |
|--------|--------|-------|-----------|-----------|---------|----------|----------|
| oc-rsync.exe | tbd by first run | tbd | tbd | tbd | tbd | tbd | tbd |
| MSYS2 rsync 3.4.2 | tbd by first run | tbd | tbd | tbd | tbd | tbd | n/a |

### `small_10000` (10000 x 4 KiB files, flat)

| Binary | Wall s | MiB/s | CPU% peak | CPU% mean | RSS MiB | Syscalls | CQ wakes |
|--------|--------|-------|-----------|-----------|---------|----------|----------|
| oc-rsync.exe | tbd by first run | tbd | tbd | tbd | tbd | tbd | tbd |
| MSYS2 rsync 3.4.2 | tbd by first run | tbd | tbd | tbd | tbd | tbd | n/a |

### `many_small_nested` (50000 files, depth 5, 256 B .. 64 KiB)

| Binary | Wall s | MiB/s | CPU% peak | CPU% mean | RSS MiB | Syscalls | CQ wakes |
|--------|--------|-------|-----------|-----------|---------|----------|----------|
| oc-rsync.exe | tbd by first run | tbd | tbd | tbd | tbd | tbd | tbd |
| MSYS2 rsync 3.4.2 | tbd by first run | tbd | tbd | tbd | tbd | tbd | n/a |

### `mixed` (single_huge + large_1gib + small_10000 + many_small_nested + 100 sparse)

| Binary | Wall s | MiB/s | CPU% peak | CPU% mean | RSS MiB | Syscalls | CQ wakes |
|--------|--------|-------|-----------|-----------|---------|----------|----------|
| oc-rsync.exe | tbd by first run | tbd | tbd | tbd | tbd | tbd | tbd |
| MSYS2 rsync 3.4.2 | tbd by first run | tbd | tbd | tbd | tbd | tbd | n/a |

Hardware envelope to capture alongside the first run (paste at the
top of the Results section): CPU model + core count, RAM GiB, NTFS
volume type (SATA SSD / NVMe SSD / spinning disk) and free space,
Windows build number (`ver`), MSYS2 `pacman -Q rsync` output,
`oc-rsync --version` output. Without those fields the numbers are
not reproducible.

## Hotspot classification (reference)

Three buckets cover every realistic top-10 stack we expect from the
five workloads in the Workloads table:

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
operator runs Steps 1-5 across all five workloads in the Workloads
table, then PRs an update to this section with the per-workload
metrics table (from "Metrics to report") and the classified hotspot
table for each scenario. The `.etl` captures should be attached to
issue #2300 and referenced here by filename, not committed to the
repo.

### `single_huge` scenario

_Pending Windows operator run._

### `large_1gib` scenario

_Pending Windows operator run._

### `small_10000` scenario

_Pending Windows operator run._

### `many_small_nested` scenario

_Pending Windows operator run._

### `mixed` scenario

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

## Appendix A: WPR profile XML (IOCP deep trace)

Save the following as `tools\bench\iocp-deep.wprp` and start it with
`wpr.exe -start tools\bench\iocp-deep.wprp -filemode`. The profile
combines CPU sampling, kernel file/disk IO, scheduler events
(context-switch + ready-thread for completion-port wake analysis),
and the Microsoft-Windows-Kernel-IO ETW provider with stack walks on
every `IRP_MJ_WRITE` and `IRP_MJ_FLUSH_BUFFERS`.

```xml
<?xml version="1.0" encoding="utf-8"?>
<WindowsPerformanceRecorder Version="1.0" Author="oc-rsync" Comments="WPG-1 IOCP deep trace">
  <Profiles>
    <SystemCollector Id="SystemCollector" Name="oc-rsync-iocp">
      <BufferSize Value="1024"/>
      <Buffers Value="512"/>
    </SystemCollector>
    <EventCollector Id="EventCollector" Name="oc-rsync-iocp-events">
      <BufferSize Value="1024"/>
      <Buffers Value="64"/>
    </EventCollector>
    <SystemProvider Id="SystemProvider">
      <Keywords>
        <Keyword Value="ProcessThread"/>
        <Keyword Value="Loader"/>
        <Keyword Value="CpuConfig"/>
        <Keyword Value="DiskIO"/>
        <Keyword Value="FileIO"/>
        <Keyword Value="FileIOInit"/>
        <Keyword Value="ContextSwitch"/>
        <Keyword Value="ReadyThread"/>
        <Keyword Value="SampledProfile"/>
        <Keyword Value="DPC"/>
        <Keyword Value="Interrupt"/>
      </Keywords>
      <Stacks>
        <Stack Value="SampledProfile"/>
        <Stack Value="FileIoCreate"/>
        <Stack Value="FileIoRead"/>
        <Stack Value="FileIoWrite"/>
        <Stack Value="FileIoCleanup"/>
        <Stack Value="ReadyThread"/>
      </Stacks>
    </SystemProvider>
    <EventProvider Id="KernelIo"
                   Name="Microsoft-Windows-Kernel-IO"
                   Level="5">
      <Keywords>
        <Keyword Value="0xFFFFFFFFFFFFFFFF"/>
      </Keywords>
    </EventProvider>
    <EventProvider Id="KernelFile"
                   Name="Microsoft-Windows-Kernel-File"
                   Level="5">
      <Keywords>
        <Keyword Value="0xFFFFFFFFFFFFFFFF"/>
      </Keywords>
    </EventProvider>
    <Profile Id="IocpDeep.Verbose.File"
             Name="IocpDeep"
             Description="oc-rsync IOCP deep trace (CPU + file/disk IO + scheduler + Kernel-IO/File providers)"
             LoggingMode="File"
             DetailLevel="Verbose">
      <Collectors>
        <SystemCollectorId Value="SystemCollector">
          <SystemProviderId Value="SystemProvider"/>
        </SystemCollectorId>
        <EventCollectorId Value="EventCollector">
          <EventProviders>
            <EventProviderId Value="KernelIo"/>
            <EventProviderId Value="KernelFile"/>
          </EventProviders>
        </EventCollectorId>
      </Collectors>
    </Profile>
  </Profiles>
</WindowsPerformanceRecorder>
```

Trace footprint at the default buffer sizes is roughly 80 MiB per
minute for `large_1gib` and 250 MiB per minute for
`many_small_nested`. Always stop the trace immediately after the
measured iteration finishes.

## Appendix B: End-to-end runbook checklist

A condensed checklist for the Windows operator. Each item maps to a
section above.

- [ ] MSYS2 installed; `pacman -Syu` complete; `rsync 3.4.2` and
      `hyperfine` on `PATH` (see Comparison harness).
- [ ] Windows ADK Performance Toolkit installed; `wpr.exe`,
      `wpa.exe`, `xperf.exe` on `PATH`.
- [ ] `_NT_SYMBOL_PATH` set; Microsoft public symbol server reachable.
- [ ] Repo cloned; `cargo build --release --features iocp --bin oc-rsync`
      successful; `oc-rsync.exe` on `PATH` or referenced by absolute
      path.
- [ ] Optional: `cargo build --profile release-with-debug --features iocp
      --bin oc-rsync` for PDB-backed profiler runs.
- [ ] Fixtures generated for all five workloads under
      `bench-out/drilldown/<workload>/src/`.
- [ ] AV exclusion list contains `bench-out/`, the
      `target/release*/oc-rsync.exe` paths, and the MSYS2
      `rsync.exe` path.
- [ ] Hyperfine + `measure-process.ps1` JSON pair captured for each
      workload, each binary.
- [ ] ETW traces (`GeneralProfile + CPU` and `DiskIO + FileIO +
      SystemServices`) captured for each workload against
      `oc-rsync.exe`.
- [ ] Optional: `xperf -on Microsoft-Windows-Kernel-IO` trace for
      syscall-count attribution.
- [ ] Results tables (per-workload metrics + hotspot bucket
      classification) pasted into the Results section.
- [ ] `.etl`, `.json`, and `.wprp` artifacts attached to issue
      #2300; not committed to the repo.
