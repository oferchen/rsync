# Windows throughput benchmark

This page documents the `Benchmark (Windows throughput)` workflow
(`.github/workflows/_benchmark-windows.yml`) and its driver script
(`scripts/windows_throughput_bench.sh`). The job compares wall-clock
throughput of `oc-rsync.exe` (built with `--release --features iocp`)
against the upstream MSYS2 `rsync` package on a `windows-latest`
runner. It uses [hyperfine] for measurement.

The Windows correctness story is covered separately by the
`Interop Tests (Windows)` workflow (`_interop-windows.yml`); this
job covers performance.

[hyperfine]: https://github.com/sharkdp/hyperfine

## When does it run?

- Tag push (`v*.*.*`).
- Weekly cron (Sundays, 04:17 UTC).
- Manual `workflow_dispatch` from the Benchmark workflow.

The job is intentionally **not** in the required-checks list. Hosted
Windows runners have visibly higher wall-clock variance than Linux
runners, so blocking merges on a single hyperfine sample would create
false negatives. Treat the numbers as a regression sniff, not a gate.

## What does it measure?

Two scenarios, each driven by hyperfine with `--warmup 1 --runs 3`
(tunable via workflow inputs):

| Scenario      | Fixture                                  | What it stresses                   |
|---------------|------------------------------------------|------------------------------------|
| `large_1gib`  | One 1 GiB file of `/dev/urandom`         | Bulk-copy bandwidth, IOCP fast path|
| `small_10000` | 10000 x 4 KiB files across 100 sub-dirs  | Per-file overhead, flist walk      |

Each measured iteration starts from a wiped destination via hyperfine's
`--prepare`, so we report full-copy throughput (not no-op quick-check).

## Reading the JSON report

Hyperfine writes one JSON file per scenario into `bench-out/`:

```
bench-out/
  large_1gib.json
  small_10000.json
```

Each file contains a `results` array, one entry per `--command-name`:

```jsonc
{
  "results": [
    {
      "command": "oc-rsync",
      "mean":   12.43,        // seconds
      "stddev": 0.31,
      "median": 12.38,
      "min":    12.05,
      "max":    13.01,
      "times":  [12.05, 12.38, 13.01]
    },
    {
      "command": "upstream-rsync",
      "mean":   14.20,
      "...":    "..."
    }
  ]
}
```

Compute the throughput ratio as
`mean(upstream-rsync) / mean(oc-rsync)`. A value of `1.0` means parity;
`> 1.0` means oc-rsync is faster, `< 1.0` means it is slower.

## Acceptable performance bands

These bands are guidance for the Windows runner only. Linux
measurements live in the main `benchmark.yml` job and have their own
targets.

| Scenario      | Acceptable band (oc-rsync vs upstream)             |
|---------------|----------------------------------------------------|
| `large_1gib`  | Within **20%** of upstream wall-clock (>= 0.83x)   |
| `small_10000` | Within **50%** of upstream wall-clock (>= 0.67x)   |

Rationale: the single-large-file scenario is dominated by raw I/O, so
the IOCP fast path should hold us close to upstream. The small-files
scenario is dominated by per-file syscall overhead and metadata work
on NTFS, where MSYS2/Cygwin's path-translation layer is the variable
we cannot control. A 50% band catches regressions without flagging
runner noise.

Sustained operation outside the band on the weekly cron is a signal,
not a fault. Repeat locally before opening a regression issue:

```sh
# From an MSYS2 shell on a Windows host.
BENCH_RUNS=5 BENCH_WARMUP=2 \
  OC_RSYNC=/c/path/to/target/release/oc-rsync.exe \
  bash scripts/windows_throughput_bench.sh
```

## Drilldown mode

Set `OC_RSYNC_BENCH_DRILLDOWN=1` to append three per-hotspot
sub-scenarios to the run. They map 1:1 onto the IOCP sync points
catalogued in
[`docs/audits/iocp-sync-blocking-audit.md`](../audits/iocp-sync-blocking-audit.md)
so that a future patch targeting a specific row in that audit can be
attributed to the matching scenario without re-deriving which hotspot
moved.

| Scenario                | What it isolates                                                                 | Control                  | Audit rows |
|-------------------------|----------------------------------------------------------------------------------|--------------------------|------------|
| `write_only_iocp`       | `IocpWriter` per-IO blocking drain. `--whole-file --inplace` forces every byte through the write path with no temp-file rename. | `cp` (std::fs::copy)     | #1, #4, #13 |
| `read_only_iocp`        | `IocpReader` per-IO blocking drain. `--dry-run` walks and reads the 1 GiB fixture but writes nothing. | upstream rsync `--dry-run` | #2, #3      |
| `network_only_loopback` | `IocpSocketWriter` / `Reader` send/recv path. Pushes a 1 GiB file between two loopback rsync daemons on the same disk so disk bandwidth cancels out. | upstream rsync loopback daemon | #8 - #11    |

Invocation:

```sh
# From an MSYS2 shell on a Windows host.
OC_RSYNC_BENCH_DRILLDOWN=1 \
  BENCH_RUNS=5 BENCH_WARMUP=2 \
  OC_RSYNC=/c/path/to/target/release/oc-rsync.exe \
  bash scripts/windows_throughput_bench.sh
```

The drilldown daemons bind `127.0.0.1:$BENCH_DAEMON_PORT` and
`127.0.0.1:$((BENCH_DAEMON_PORT + 1))` (default `18730` / `18731`).
Override `BENCH_DAEMON_PORT` if those ports are in use.

### Interpretation

Read each ratio the same way as the main scenarios
(`mean(control) / mean(oc-rsync)`):

- `write_only_iocp`: regression here points at the write-path changes
  in `crates/fast_io/src/iocp/file_writer.rs` and
  `crates/fast_io/src/iocp/disk_batch.rs`. The `cp` control caps the
  upper bound at NTFS write bandwidth; oc-rsync should land within a
  small factor of it.
- `read_only_iocp`: regression here points at `file_reader.rs` or the
  generator/sender read pipeline. Because both sides run `--dry-run`,
  divergence is not explained by network or fsync work.
- `network_only_loopback`: regression here implicates `socket.rs` or
  the multiplex layer. Disk bandwidth is symmetric across both
  commands, so the delta reflects send/recv pipelining.

The drilldown sub-scenarios are **not** in the required-checks list
and have no acceptable-band thresholds; they exist to attribute
movement, not to gate merges.

## Tuning knobs

The reusable workflow exposes these inputs (all optional):

- `warmup_runs` (default 1)
- `measured_runs` (default 3)
- `large_file_mib` (default 1024)
- `small_file_count` (default 10000)
- `timeout_minutes` (default 60)

The driver script honours the matching environment variables
(`BENCH_WARMUP`, `BENCH_RUNS`, `BENCH_LARGE_MIB`, `BENCH_SMALL_COUNT`,
`BENCH_SMALL_KIB`, `BENCH_OUT_DIR`) for local invocation.

## Why MSYS2 upstream?

MSYS2 ships a current `rsync >= 3.2` on a Cygwin-style runtime. It is
the most reliable way to get a real upstream rsync on a Windows
GitHub runner; the Chocolatey `rsync` package is unmaintained. This
matches the choice made by the Windows correctness interop workflow.
