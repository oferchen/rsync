# Flat flist RSS measurement instrumentation (RSS-A.9.b)

Task: RSS-A.9.b. Branch: `docs/flat-flist-rss-measurement`.
Prerequisites: RSS-A.9.a (bench fixture design, PR #5256).
Downstream: RSS-A.9.c (CI regression gate), RSS-A.9.d (validate ratio).

## Summary

This document specifies the instrumentation binary that builds a
1M-entry flat file list, captures peak RSS before and after construction,
and emits structured JSON suitable for CI trend tracking. The binary
exercises four configurations (flat vs legacy, INC_RECURSE vs not) and
computes comparison ratios against upstream rsync 3.4.1 baselines.

RSS-A.9.a defined the fixture shape and measurement methodology. This
document defines the implementation - the actual code that captures RSS,
the multi-configuration matrix, output schema, and regression detection
logic.

## 1. Instrumentation approach

### 1.1 Standalone binary

A dedicated binary at `tools/flat-flist-rss-bench/src/main.rs` (separate
Cargo package in the workspace) that:

1. Parses CLI flags selecting configuration and entry count.
2. Captures baseline RSS (process overhead before any flist work).
3. Builds the file list for the selected configuration.
4. Optionally freezes the PathArena (drops dedup HashMap).
5. Captures loaded RSS.
6. Computes and emits the differential measurement.
7. Holds the flist live via `black_box` until after measurement.

This is not a criterion bench - criterion measures throughput, not peak
memory. A standalone binary provides direct RSS capture with no framework
overhead contaminating the measurement.

### 1.2 Binary interface

```
flat-flist-rss-bench [OPTIONS]

Options:
  --entries <N>        Number of entries to generate (default: 1000000)
  --config <NAME>      Configuration: flat-no-inc, flat-inc, legacy-no-inc, legacy-inc
  --all                Run all configurations sequentially
  --distribution <D>   Path distribution: shared, deep, wide (default: shared)
  --freeze             Drop PathArena dedup HashMap before measurement
  --json               Emit JSON output (default: human-readable table)
  --runs <N>           Number of iterations for median (default: 5)
```

### 1.3 Build and run

```bash
cargo build --release -p flat-flist-rss-bench --features flat-flist
./target/release/flat-flist-rss-bench --all --json --freeze
```

The binary links against the `protocol` crate to access `FlatFileList`,
`FileEntryHeader`, `PathArena`, `ExtrasArena`, and the legacy
`Vec<FileEntry>` path.

## 2. Platform-specific RSS capture

### 2.1 Linux: /proc/self/status VmHWM

The primary measurement platform. VmHWM (high-water mark) is the kernel-
tracked peak RSS - it captures the true maximum even if the process later
frees memory.

```rust
#[cfg(target_os = "linux")]
fn peak_rss_bytes() -> u64 {
    let status = match std::fs::read_to_string("/proc/self/status") {
        Ok(s) => s,
        Err(_) => return 0,
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let trimmed = rest.trim().trim_end_matches(" kB").trim();
            return trimmed.parse::<u64>().unwrap_or(0) * 1024;
        }
    }
    0
}
```

VmHWM is monotonically increasing within a process lifetime - it never
decreases. This means:

- Baseline measurement captures process startup overhead.
- After flist construction, VmHWM reflects the true peak including any
  transient allocations during build (e.g., PathArena dedup HashMap).
- After `freeze()` drops the HashMap, VmHWM still reflects the pre-freeze
  peak. To measure steady-state RSS, use VmRSS (current RSS) instead.

The binary captures both:
- `VmHWM` - peak during build (includes HashMap).
- `VmRSS` - current RSS after freeze (steady-state, HashMap freed).

```rust
#[cfg(target_os = "linux")]
fn current_rss_bytes() -> u64 {
    let status = match std::fs::read_to_string("/proc/self/status") {
        Ok(s) => s,
        Err(_) => return 0,
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let trimmed = rest.trim().trim_end_matches(" kB").trim();
            return trimmed.parse::<u64>().unwrap_or(0) * 1024;
        }
    }
    0
}
```

### 2.2 macOS: mach_task_info

macOS does not expose VmHWM via procfs. Use the Mach kernel API to query
task memory info. The `resident_size_max` field in `MACH_TASK_BASIC_INFO`
is the peak RSS equivalent.

```rust
#[cfg(target_os = "macos")]
fn peak_rss_bytes() -> u64 {
    use std::mem;

    extern "C" {
        fn mach_task_self() -> u32;
        fn task_info(
            target_task: u32,
            flavor: u32,
            task_info_out: *mut i32,
            task_info_out_cnt: *mut u32,
        ) -> i32;
    }

    const MACH_TASK_BASIC_INFO: u32 = 20;

    #[repr(C)]
    struct MachTaskBasicInfo {
        virtual_size: u64,
        resident_size: u64,
        resident_size_max: u64,
        user_time: [u32; 2],
        system_time: [u32; 2],
        policy: i32,
        suspend_count: i32,
    }

    let mut info: MachTaskBasicInfo = unsafe { mem::zeroed() };
    let mut count = (mem::size_of::<MachTaskBasicInfo>() / mem::size_of::<i32>()) as u32;

    let kr = unsafe {
        task_info(
            mach_task_self(),
            MACH_TASK_BASIC_INFO,
            &mut info as *mut _ as *mut i32,
            &mut count,
        )
    };

    if kr == 0 {
        info.resident_size_max
    } else {
        0
    }
}

#[cfg(target_os = "macos")]
fn current_rss_bytes() -> u64 {
    // Same as above but read resident_size (current, not peak)
    use std::mem;

    extern "C" {
        fn mach_task_self() -> u32;
        fn task_info(
            target_task: u32,
            flavor: u32,
            task_info_out: *mut i32,
            task_info_out_cnt: *mut u32,
        ) -> i32;
    }

    const MACH_TASK_BASIC_INFO: u32 = 20;

    #[repr(C)]
    struct MachTaskBasicInfo {
        virtual_size: u64,
        resident_size: u64,
        resident_size_max: u64,
        user_time: [u32; 2],
        system_time: [u32; 2],
        policy: i32,
        suspend_count: i32,
    }

    let mut info: MachTaskBasicInfo = unsafe { mem::zeroed() };
    let mut count = (mem::size_of::<MachTaskBasicInfo>() / mem::size_of::<i32>()) as u32;

    let kr = unsafe {
        task_info(
            mach_task_self(),
            MACH_TASK_BASIC_INFO,
            &mut info as *mut _ as *mut i32,
            &mut count,
        )
    };

    if kr == 0 {
        info.resident_size
    } else {
        0
    }
}
```

### 2.3 Fallback: getrusage

For platforms without `/proc` or Mach APIs (e.g., FreeBSD, Windows WSL),
fall back to `getrusage(RUSAGE_SELF)`. Note the unit difference:
Linux `ru_maxrss` is in kilobytes, macOS in bytes.

```rust
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peak_rss_bytes() -> u64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    if ret != 0 {
        return 0;
    }
    // getrusage ru_maxrss units vary by platform:
    // Linux: kilobytes, macOS: bytes, FreeBSD: kilobytes
    (usage.ru_maxrss as u64) * 1024
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn current_rss_bytes() -> u64 {
    // getrusage only provides peak, not current
    // Return peak as a conservative estimate
    peak_rss_bytes()
}
```

### 2.4 Windows

Windows does not provide `getrusage`. Use `GetProcessMemoryInfo` from
`kernel32`:

```rust
#[cfg(target_os = "windows")]
fn peak_rss_bytes() -> u64 {
    use std::mem;

    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(
            process: isize,
            pmc: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }

    let mut pmc: ProcessMemoryCounters = unsafe { mem::zeroed() };
    pmc.cb = mem::size_of::<ProcessMemoryCounters>() as u32;

    let ret = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut pmc,
            pmc.cb,
        )
    };

    if ret != 0 {
        pmc.peak_working_set_size as u64
    } else {
        0
    }
}

#[cfg(target_os = "windows")]
fn current_rss_bytes() -> u64 {
    // Same structure, read working_set_size instead of peak
    use std::mem;

    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(
            process: isize,
            pmc: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }

    let mut pmc: ProcessMemoryCounters = unsafe { mem::zeroed() };
    pmc.cb = mem::size_of::<ProcessMemoryCounters>() as u32;

    let ret = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut pmc,
            pmc.cb,
        )
    };

    if ret != 0 {
        pmc.working_set_size as u64
    } else {
        0
    }
}
```

### 2.5 Unified interface

All platform-specific functions export through a unified module:

```rust
mod rss {
    /// Returns peak RSS in bytes (high-water mark since process start).
    pub fn peak() -> u64 { peak_rss_bytes() }

    /// Returns current RSS in bytes (live resident pages now).
    pub fn current() -> u64 { current_rss_bytes() }
}
```

## 3. Measurement protocol

### 3.1 Phase sequence

Each configuration run follows this exact sequence:

```
Phase 0: Process startup
  - Binary loads, runtime initializes
  - No explicit measurement (captured implicitly in baseline)

Phase 1: Baseline capture
  - Force a minor allocation + deallocation to stabilize the allocator
  - Read peak RSS -> baseline_peak
  - Read current RSS -> baseline_current

Phase 2: Flist construction
  - Build the file list for the selected configuration
  - All entries constructed and held live
  - PathArena dedup HashMap is active (build-phase peak)
  - Read peak RSS -> build_peak (captures transient HashMap overhead)
  - Read current RSS -> build_current

Phase 3: Freeze (optional, --freeze flag)
  - Drop PathArena dedup HashMap
  - Drop any transient builder state
  - Force allocator to return pages (libc::malloc_trim on Linux)
  - Read current RSS -> steady_current
  - Peak RSS unchanged (VmHWM is monotonic)

Phase 4: Measurement complete
  - Emit results
  - black_box(&flist) to prevent optimizer from dropping flist early
```

### 3.2 Baseline capture details

The baseline must be captured after the binary is fully initialized but
before any flist-related allocation. This means:

- After `main()` entry and argument parsing.
- After any logging or output framework initialization.
- Before creating `FlatFileList`, `PathArena`, `ExtrasArena`, or
  `Vec<FileEntry>`.

To stabilize the allocator state, perform a dummy allocation/deallocation
cycle before baseline capture:

```rust
fn stabilize_allocator() {
    // Allocate and immediately free 1 MB to exercise allocator metadata
    let v: Vec<u8> = vec![0u8; 1_048_576];
    drop(v);
    // Small sleep to allow kernel to update RSS counters
    std::thread::sleep(std::time::Duration::from_millis(10));
}
```

### 3.3 Differential computation

The primary metric is the **delta** - the RSS attributable to the flist:

```
build_delta  = build_peak - baseline_peak      (peak during construction)
steady_delta = steady_current - baseline_current (after freeze)
```

The `build_delta` captures the worst-case peak (including HashMap).
The `steady_delta` captures the production steady-state (HashMap freed).

### 3.4 Page reclamation after freeze

On Linux, `glibc` does not eagerly return freed pages to the OS.
The `malloc_trim(0)` call hints that freed memory should be returned:

```rust
#[cfg(target_os = "linux")]
fn reclaim_pages() {
    extern "C" {
        fn malloc_trim(pad: usize) -> i32;
    }
    unsafe { malloc_trim(0); }
    // Allow kernel time to update RSS counters
    std::thread::sleep(std::time::Duration::from_millis(50));
}
```

Without this, `VmRSS` may not reflect the freed HashMap memory, making
`steady_current` appear higher than actual. This is critical for
measuring the benefit of the freeze optimization.

### 3.5 Multi-run median

Each configuration runs `N` iterations (default 5). The first run is
discarded as warm-up (allocator state, filesystem cache). The median of
the remaining runs is reported. This eliminates outliers from GC pauses,
kernel scheduling jitter, or background processes competing for pages.

```rust
fn median(values: &mut [u64]) -> u64 {
    values.sort_unstable();
    values[values.len() / 2]
}
```

## 4. Multi-configuration matrix

### 4.1 Configuration definitions

| Config | Backing store | INC_RECURSE | Description |
|--------|--------------|-------------|-------------|
| `flat-no-inc` | FlatFileList | No | Primary target: all 1M entries in flat arena |
| `flat-inc` | FlatFileList | Yes | Append segments, no drop (current behavior) |
| `legacy-no-inc` | Vec<FileEntry> | No | Reference: legacy representation |
| `legacy-inc` | Vec<FileEntry> | Yes | Reference: legacy INC_RECURSE |

### 4.2 Flat configuration construction

```rust
fn build_flat(entries: usize, distribution: Distribution, inc_recurse: bool) {
    let mut flist = FlatFileList::with_capacity(entries);

    if inc_recurse {
        // Append in 1000-entry segments (matching upstream segment size)
        let segment_size = 1000;
        for segment_start in (0..entries).step_by(segment_size) {
            let segment_end = (segment_start + segment_size).min(entries);
            for i in segment_start..segment_end {
                let path = distribution.generate(i);
                let header = build_header(i, &path);
                flist.push(header);
            }
            flist.finish_segment();
        }
    } else {
        for i in 0..entries {
            let path = distribution.generate(i);
            let header = build_header(i, &path);
            flist.push(header);
        }
    }
}
```

### 4.3 Legacy configuration construction

```rust
fn build_legacy(entries: usize, distribution: Distribution, inc_recurse: bool) {
    let interner = PathInterner::new();
    let mut file_list: Vec<FileEntry> = Vec::with_capacity(entries);

    for i in 0..entries {
        let path = distribution.generate(i);
        let entry = FileEntry::new(
            path.file_name().unwrap().to_owned(),
            interner.intern(path.parent().unwrap()),
            synthetic_size(i),
            synthetic_mtime(i),
            synthetic_mode(i),
            (i % 1000) as u32, // uid
            (i % 100) as u32,  // gid
        );
        file_list.push(entry);
    }
}
```

### 4.4 Sequential execution

When `--all` is specified, configurations run sequentially in separate
child processes (via `std::process::Command` re-invoking the same binary
with `--config <name>`). This ensures each configuration gets a clean
address space without cross-contamination from prior allocations.

Running in-process would pollute VmHWM with the previous configuration's
peak, making differential measurement impossible.

## 5. Output format

### 5.1 JSON schema

```json
{
  "version": 1,
  "timestamp": "2026-06-01T12:00:00Z",
  "platform": "linux-x86_64",
  "entries": 1000000,
  "distribution": "shared",
  "freeze": true,
  "runs": 5,
  "configurations": [
    {
      "name": "flat-no-inc",
      "baseline_peak_bytes": 8912896,
      "build_peak_bytes": 143654912,
      "steady_current_bytes": 82837504,
      "build_delta_bytes": 134742016,
      "steady_delta_bytes": 73924608,
      "build_delta_mb": 128.5,
      "steady_delta_mb": 70.5,
      "per_entry_bytes_build": 134,
      "per_entry_bytes_steady": 73
    },
    {
      "name": "flat-inc",
      "baseline_peak_bytes": 8912896,
      "build_peak_bytes": 145752064,
      "steady_current_bytes": 84935680,
      "build_delta_bytes": 136839168,
      "steady_delta_bytes": 76022784,
      "build_delta_mb": 130.5,
      "steady_delta_mb": 72.5,
      "per_entry_bytes_build": 136,
      "per_entry_bytes_steady": 76
    },
    {
      "name": "legacy-no-inc",
      "baseline_peak_bytes": 8912896,
      "build_peak_bytes": 206569472,
      "steady_current_bytes": 206569472,
      "build_delta_bytes": 197656576,
      "steady_delta_bytes": 197656576,
      "build_delta_mb": 188.5,
      "steady_delta_mb": 188.5,
      "per_entry_bytes_build": 197,
      "per_entry_bytes_steady": 197
    },
    {
      "name": "legacy-inc",
      "baseline_peak_bytes": 8912896,
      "build_peak_bytes": 206569472,
      "steady_current_bytes": 206569472,
      "build_delta_bytes": 197656576,
      "steady_delta_bytes": 197656576,
      "build_delta_mb": 188.5,
      "steady_delta_mb": 188.5,
      "per_entry_bytes_build": 197,
      "per_entry_bytes_steady": 197
    }
  ],
  "comparisons": {
    "upstream_no_inc_mb": 76.8,
    "upstream_inc_mb": 7.6,
    "flat_vs_upstream_no_inc": 0.92,
    "flat_vs_upstream_inc": 9.54,
    "flat_vs_legacy_no_inc": 0.37,
    "flat_vs_legacy_inc": 0.37,
    "legacy_vs_upstream_no_inc": 2.45,
    "legacy_vs_upstream_inc": 24.7
  },
  "regression": {
    "target_steady_mb": 85,
    "measured_steady_mb": 70.5,
    "headroom_mb": 14.5,
    "status": "pass"
  }
}
```

### 5.2 Field definitions

| Field | Type | Description |
|-------|------|-------------|
| `version` | u32 | Schema version for forward compatibility |
| `timestamp` | string | ISO 8601 UTC measurement time |
| `platform` | string | `{os}-{arch}` identifier |
| `entries` | u64 | Number of synthetic entries |
| `distribution` | string | Path distribution model used |
| `freeze` | bool | Whether PathArena was frozen |
| `runs` | u32 | Number of measurement iterations |
| `configurations[].name` | string | Configuration identifier |
| `configurations[].baseline_peak_bytes` | u64 | RSS before flist work |
| `configurations[].build_peak_bytes` | u64 | Peak RSS during construction |
| `configurations[].steady_current_bytes` | u64 | Current RSS after freeze |
| `configurations[].build_delta_bytes` | u64 | build_peak - baseline_peak |
| `configurations[].steady_delta_bytes` | u64 | steady_current - baseline_current |
| `configurations[].build_delta_mb` | f64 | Delta in MiB (1048576 divisor) |
| `configurations[].steady_delta_mb` | f64 | Delta in MiB |
| `configurations[].per_entry_bytes_build` | u64 | build_delta / entries |
| `configurations[].per_entry_bytes_steady` | u64 | steady_delta / entries |
| `comparisons.*` | f64 | Ratios (measured / reference) |
| `regression.target_steady_mb` | f64 | Pass/fail ceiling |
| `regression.measured_steady_mb` | f64 | Actual flat-no-inc steady delta |
| `regression.headroom_mb` | f64 | target - measured (positive = margin) |
| `regression.status` | string | `pass` or `fail` |

### 5.3 Human-readable output (default)

When `--json` is not specified, emit a formatted table:

```
flat-flist-rss-bench: 1,000,000 entries (shared distribution)

Configuration      Build Peak    Steady State    Per-Entry
--------------     ----------    ------------    ---------
flat-no-inc         128.5 MB       70.5 MB        73 B
flat-inc            130.5 MB       72.5 MB        76 B
legacy-no-inc       188.5 MB      188.5 MB       197 B
legacy-inc          188.5 MB      188.5 MB       197 B

Comparisons (steady state):
  flat-no-inc vs upstream (76.8 MB):  0.92x  [PASS < 1.10x]
  flat-no-inc vs legacy (188.5 MB):   0.37x  [2.67x reduction]

Regression gate: 70.5 MB < 85.0 MB target  [PASS]
```

## 6. Comparison computation

### 6.1 Upstream baselines (constants)

These values come from RSS-1.b/1.c measurements against upstream rsync
3.4.1 at 1M files and are encoded as constants in the binary:

```rust
const UPSTREAM_NO_INC_MB: f64 = 76.8;
const UPSTREAM_INC_MB: f64 = 7.6;
```

### 6.2 Ratio calculations

```rust
fn compute_comparisons(configs: &[ConfigResult]) -> Comparisons {
    let flat_no_inc = configs.iter()
        .find(|c| c.name == "flat-no-inc")
        .expect("flat-no-inc required");

    let legacy_no_inc = configs.iter()
        .find(|c| c.name == "legacy-no-inc")
        .expect("legacy-no-inc required");

    Comparisons {
        // Ratio vs upstream (< 1.0 means better than upstream)
        flat_vs_upstream_no_inc: flat_no_inc.steady_delta_mb / UPSTREAM_NO_INC_MB,
        flat_vs_upstream_inc: flat_no_inc.steady_delta_mb / UPSTREAM_INC_MB,
        // Ratio vs legacy (< 1.0 means improvement)
        flat_vs_legacy_no_inc: flat_no_inc.steady_delta_mb / legacy_no_inc.steady_delta_mb,
        flat_vs_legacy_inc: flat_no_inc.steady_delta_mb / legacy_no_inc.steady_delta_mb,
        // Legacy vs upstream (for context)
        legacy_vs_upstream_no_inc: legacy_no_inc.steady_delta_mb / UPSTREAM_NO_INC_MB,
        legacy_vs_upstream_inc: legacy_no_inc.steady_delta_mb / UPSTREAM_INC_MB,
    }
}
```

### 6.3 Per-entry byte cost

The per-entry cost is the most directly comparable metric across
implementations:

```rust
fn per_entry_cost(delta_bytes: u64, entries: u64) -> u64 {
    delta_bytes / entries
}
```

| Implementation | Per-entry (B) | Notes |
|---------------|---------------|-------|
| Upstream pool_alloc | ~70 | Contiguous 32 KB extents |
| Flat flist (steady) | 63-73 | Target: match upstream |
| Legacy Vec<FileEntry> | ~182 | Heap-per-entry overhead |

## 7. Regression detection

### 7.1 Threshold logic

The binary performs pass/fail evaluation against a configured target:

```rust
const TARGET_STEADY_MB: f64 = 85.0;  // RSS-A.9.a success criterion
const TARGET_BUILD_MB: f64 = 140.0;  // Acceptable build-phase peak

fn evaluate_regression(result: &ConfigResult) -> RegressionStatus {
    if result.steady_delta_mb > TARGET_STEADY_MB {
        RegressionStatus::Fail {
            measured: result.steady_delta_mb,
            target: TARGET_STEADY_MB,
            excess: result.steady_delta_mb - TARGET_STEADY_MB,
        }
    } else {
        RegressionStatus::Pass {
            measured: result.steady_delta_mb,
            target: TARGET_STEADY_MB,
            headroom: TARGET_STEADY_MB - result.steady_delta_mb,
        }
    }
}
```

### 7.2 CI baseline file

A checked-in baseline at `.github/baselines/rss-flat-flist-1m.json`
stores the last-known-good values for CI comparison:

```json
{
  "fixture": "flat-flist-1m-shared",
  "entries": 1000000,
  "target_steady_mb": 85.0,
  "target_build_mb": 140.0,
  "threshold_percent": 10,
  "updated": "2026-06-01",
  "known_good": {
    "flat-no-inc": { "steady_mb": 70.5, "build_mb": 128.5 },
    "flat-inc": { "steady_mb": 72.5, "build_mb": 130.5 }
  }
}
```

### 7.3 Alerting logic in CI

The CI workflow (RSS-A.9.c) uses the binary's exit code for gating:

| Exit code | Meaning |
|-----------|---------|
| 0 | All configurations within target |
| 1 | One or more configurations exceed target |
| 2 | Measurement failure (RSS read returned 0, unsupported platform) |

Additionally, the JSON output `regression.status` field enables
workflow-level decisions (e.g., post a comment on the PR with the
regression details).

### 7.4 Regression triage workflow

When CI reports a regression:

1. Check `per_entry_bytes_steady` - did per-entry cost increase?
2. Check the component breakdown (if available from FlatFileList
   introspection APIs) - which arena grew?
3. Compare the git diff for changes to `FileEntryHeader` size,
   `PathArena` interning logic, or `ExtrasArena` record format.
4. If the regression is from a transitive dependency update (allocator
   behavior change), update the baseline rather than reverting.

### 7.5 Baseline updates

The baseline is updated via:

```bash
./target/release/flat-flist-rss-bench --all --json --freeze \
    > .github/baselines/rss-flat-flist-1m.json
```

Baseline updates require a separate commit with a clear explanation of
why the ceiling changed (e.g., "added 4-byte field to FileEntryHeader,
new per-entry cost is 77 B").

## 8. Implementation plan

| Step | Deliverable | Notes |
|------|-------------|-------|
| 8.1 | `tools/flat-flist-rss-bench/Cargo.toml` | Workspace member, depends on `protocol` |
| 8.2 | `tools/flat-flist-rss-bench/src/rss.rs` | Platform-specific RSS capture module |
| 8.3 | `tools/flat-flist-rss-bench/src/fixture.rs` | Synthetic entry generation (from RSS-A.9.a) |
| 8.4 | `tools/flat-flist-rss-bench/src/main.rs` | CLI parsing, measurement loop, output |
| 8.5 | `.github/baselines/rss-flat-flist-1m.json` | Initial baseline (placeholder until first run) |

## 9. Success criteria

| Metric | Target | Validated by |
|--------|--------|--------------|
| flat-no-inc steady RSS | < 85 MB | Binary exit code 0 |
| flat-no-inc per-entry | < 80 B | JSON per_entry_bytes_steady |
| flat/upstream ratio | < 1.10x | JSON flat_vs_upstream_no_inc |
| flat/legacy ratio | < 0.45x | JSON flat_vs_legacy_no_inc |
| Measurement variance | < 3% across 5 runs | Median vs min/max spread |
| CI execution time | < 3 min (build + measure) | Workflow duration |

## 10. Cross-references

- RSS-A.9.a fixture design: `docs/design/flat-flist-rss-bench-fixture.md`
- RSS-12.a CI workflow spec: `docs/design/rss-12a-ci-rss-regression-workflow.md`
- Flat flist representation: `docs/design/flat-flist-representation.md`
- Existing criterion bench: `crates/protocol/benches/flat_flist_rss.rs`
- RSS-1.b/1.c results: `docs/benchmarks/rss-1b-1c-peak-rss-2026-05-29.md`
- Arena growth strategy: `docs/design/rss-a8b-arena-growth-strategy.md`
