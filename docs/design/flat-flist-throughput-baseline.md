# Flat flist throughput baseline design (RSS-A.10.a)

Task: RSS-A.10.a. Branch: `docs/flat-flist-throughput-baseline`.
Prerequisites: RSS-A.9.a (RSS bench fixture design).
Downstream: RSS-A.10.b (execute baseline capture), RSS-A.10.c
(post-migration comparison), RSS-A.10.d (CI regression gate).

## Summary

This document defines the methodology for measuring transfer throughput
before and after the flat flist migration (RSS-A.5). The flat flist
replaces `Vec<FileEntry>` with offset-based arena indexing. While this
reduces RSS by eliminating per-entry heap allocations, it introduces an
indirection layer on every file-entry access - a potential latency
regression on hot paths (sort, filter, generator iteration, sender
lookup).

The baseline must be captured on the legacy `Vec<FileEntry>` path before
the flat flist becomes the default. After migration, the same workloads
run against the flat flist path. If throughput regresses beyond 3%, the
migration is blocked until the regression is resolved.

## 1. Workload profiles

### 1.1 Initial sync (empty destination)

Initial sync exercises the full flist build, sort, filter, and transfer
pipeline with no delta computation. File counts:

| Scale | File count | Purpose |
|-------|-----------|---------|
| small | 1,000 | Baseline for setup overhead |
| medium | 10,000 | Typical project |
| large | 100,000 | Monorepo / package mirror |
| extreme | 1,000,000 | Stress test for arena indexing overhead |

Each scale runs as a local transfer (`oc-rsync src/ dst/`) to isolate
flist performance from network or SSH overhead.

### 1.2 Delta sync (10% changed)

Pre-populate the destination with the full file set, then modify 10% of
files (touch mtime + append 1 byte). This exercises:

- Generator quick-check iteration over the full flist
- Selective transfer of changed files
- Sender lookup by file index

File counts: 10K, 100K, 1M.

### 1.3 Delete sync

Pre-populate the destination with 10% extra files not present in source.
Run with `--delete`. This exercises:

- Generator flist iteration for delete candidate identification
- Delete-plan construction from flist traversal
- Cohort/reorder buffer interaction with flist lookups

File counts: 10K, 100K.

## 2. File size distributions

Each workload profile runs with three file size distributions:

| Distribution | Sizes | Purpose |
|-------------|-------|---------|
| all-small | 4 KiB uniform | Maximizes files/sec - exposes per-entry overhead |
| all-large | 100 MiB uniform | Maximizes MB/s - confirms I/O bound transfers unaffected |
| mixed | 60% < 4K, 25% 4-256K, 10% 256K-2M, 5% 2-100M | Realistic distribution |

The **all-small** distribution is the critical regression detector.
When file sizes are tiny, per-entry overhead (arena lookup, offset
dereference, cache-line fetch) dominates wall-clock time. If the flat
flist adds measurable latency, it surfaces here first.

## 3. Measurement metrics

### 3.1 Primary metrics

| Metric | Unit | Capture method |
|--------|------|---------------|
| Wall-clock time | seconds (3 decimal places) | `/usr/bin/time -v` or `gtime` on macOS |
| Files/sec | count / elapsed_s | file_count / wall_time |
| MB/s | total_bytes / elapsed_s | sum(file_sizes) / wall_time |
| CPU time (user) | seconds | `getrusage` / `/usr/bin/time` |
| CPU time (system) | seconds | `getrusage` / `/usr/bin/time` |

### 3.2 Secondary metrics (profiling runs)

| Metric | Unit | Tool |
|--------|------|------|
| Instructions retired | count | `perf stat` (Linux) |
| Cache misses (L1d, LLC) | count | `perf stat` (Linux) |
| Page faults | count | `perf stat` / `gtime` |
| Peak RSS | KiB | `/usr/bin/time` maxrss |

Cache misses are the key diagnostic for flat flist regression. Arena
lookups that break spatial locality will show as elevated L1d misses
compared to contiguous `Vec` iteration.

## 4. Hot-path profiling points

These code paths are instrumented with `std::time::Instant` bracketing
in profiling builds (behind `cfg(feature = "bench-instrumentation")`):

### 4.1 Flist sort

- `clean_flist()` - sorts the file list by path components
- For legacy: `Vec::sort_unstable_by` over `FileEntry` references
- For flat flist: sort a separate index array by dereferencing through
  the arena for each comparison

Expected concern: each comparison in the flat flist dereferences a
`PathHandle` through `PathArena` to obtain the actual path bytes.
Two extra pointer chases per comparison versus inline `PathBuf`.

### 4.2 Filter evaluation

- `FilterChain::check()` called per file entry
- Reads `dirname` and `basename` from each entry
- For legacy: direct field access on `FileEntry`
- For flat flist: `PathArena::resolve(handle)` to obtain path slice

### 4.3 Generator file-list iteration

- Generator iterates the sorted flist sequentially
- Reads metadata fields (size, mtime, mode) for quick-check
- For legacy: sequential `Vec` iteration with prefetch-friendly layout
- For flat flist: index array + header array indirection

### 4.4 Sender file lookup

- Sender looks up file entries by NDX (file index)
- For legacy: direct `Vec` indexing - O(1) with zero indirection
- For flat flist: index into header array - still O(1) but through the
  flat buffer offset

### 4.5 INC_RECURSE segment append

- New flist segments appended during incremental recursion
- For legacy: `Vec::push` of complete `FileEntry`
- For flat flist: arena allocation + header push + index update

## 5. Comparison matrix

All combinations are measured:

```
Backing store:   [legacy Vec<FileEntry>] | [flat flist (feature flag)]
Workload:        [initial-1K] [initial-10K] [initial-100K] [initial-1M]
                 [delta-10K]  [delta-100K]  [delta-1M]
                 [delete-10K] [delete-100K]
Size dist:       [all-small] [all-large] [mixed]
INC_RECURSE:     [on] [off]
```

Total: 2 stores x 9 workloads x 3 distributions x 2 INC_RECURSE = 108
configurations. Each configuration runs 10 iterations.

Priority tiers for CI budget:

| Tier | Configurations | Purpose |
|------|---------------|---------|
| P0 (must-run) | initial-100K + all-small + INC_RECURSE on/off | Maximum per-entry sensitivity |
| P1 (should-run) | initial-1M + all-small, delta-100K + mixed | Scale and delta path |
| P2 (nice-to-have) | remaining combinations | Full coverage |

## 6. Statistical rigor

### 6.1 Iteration count

Minimum **10 iterations** per configuration. If the coefficient of
variation (CV = stddev / mean) exceeds 5%, increase to 20 iterations.

### 6.2 Summary statistics

Report **median** as the primary point estimate. Report **IQR**
(interquartile range, P25-P75) as the dispersion measure. Do not use
mean - transfer times are right-skewed due to occasional GC or I/O
scheduling jitter.

### 6.3 Warm cache

All measurements run with warm filesystem cache:

1. Run the transfer once (warmup, discarded)
2. Run 10 measured iterations
3. Filesystem caches are NOT dropped between iterations

This isolates flist data-structure overhead from cold-cache I/O
variance. A separate cold-cache run (single iteration, `sync && echo 3 >
/proc/sys/vm/drop_caches` on Linux) captures first-run behavior but is
not used for regression decisions.

### 6.4 Environment controls

- CPU frequency pinned (`cpupower frequency-set -g performance`)
- No other user workloads during measurement
- tmpfs source and destination (eliminates disk I/O variance)
- Same machine for legacy and flat flist runs (paired comparison)
- Rust compiler version and flags identical (release profile, LTO=thin)

### 6.5 Comparison test

For the regression decision, compute the ratio:

```
ratio = median(flat_flist_time) / median(legacy_time)
```

If `ratio > 1.03` on any P0 configuration, the flat flist migration is
blocked. The 3% threshold accounts for measurement noise while catching
meaningful regressions.

Additionally, report per-hot-path timing ratios from the instrumented
profiling builds to identify which specific path regressed.

## 7. Regression threshold

| Outcome | Condition | Action |
|---------|-----------|--------|
| Pass | All P0 configs: ratio <= 1.03 | Proceed with flat flist default |
| Warning | Any P1 config: ratio > 1.03 but P0 passes | Investigate, may proceed |
| Block | Any P0 config: ratio > 1.03 | Fix regression before proceeding |

### 7.1 Known mitigation strategies if regression detected

- **Sort**: pre-resolve paths into a temporary sort-key buffer to avoid
  repeated arena lookups during comparison
- **Filter**: batch-resolve paths before filter chain evaluation
- **Generator iteration**: prefetch next N headers during sequential scan
- **Sender lookup**: no expected regression (still O(1) direct index)

## 8. Execution environment

### 8.1 Primary benchmark host

The `localhost/oc-rsync-bench:latest` container (Arch Linux, 9 GB):

- Rust toolchain matching `rust-toolchain.toml`
- Source and destination on tmpfs
- `run_benchmark.py` adapted with the throughput-specific workloads
- `BENCH_RUNS=10` (or 20 if CV > 5%)

### 8.2 Supplementary platforms

| Platform | Purpose |
|----------|---------|
| macOS (Apple Silicon) | NEON code paths, unified memory |
| Linux x86_64 (bare metal) | AVX2 paths, `perf stat` for cache analysis |

macOS results are informational - the regression gate uses only the
Linux container results for reproducibility.

## 9. Output format

Results are stored in `target/bench/throughput/` (gitignored) as JSON:

```json
{
  "run_id": "rss-a10-baseline-20260601",
  "backing_store": "legacy",
  "workload": "initial-100K",
  "size_dist": "all-small",
  "inc_recurse": true,
  "iterations": [
    {
      "wall_s": 1.234,
      "user_cpu_s": 1.100,
      "sys_cpu_s": 0.120,
      "files_per_sec": 81037,
      "mb_per_sec": 316.5,
      "peak_rss_kib": 45200
    }
  ],
  "summary": {
    "wall_s_median": 1.234,
    "wall_s_p25": 1.210,
    "wall_s_p75": 1.258,
    "files_per_sec_median": 81037
  }
}
```

A comparison report is generated as a markdown table for PR review:

```
| Workload | Size | Legacy (med) | Flat (med) | Ratio | Verdict |
|----------|------|--------------|------------|-------|---------|
| initial-100K | small | 1.234s | 1.251s | 1.014 | PASS |
```

## 10. Timeline and dependencies

```
RSS-A.9.a (fixture design)        [DONE]
RSS-A.10.a (this design)          [current]
RSS-A.10.b (capture legacy baseline)
   - requires: production build on legacy path
   - blocks: RSS-A.5 becoming default
RSS-A.5 (flat flist implementation)
RSS-A.10.c (capture flat flist measurements)
   - requires: flat flist behind feature flag
RSS-A.10.d (comparison + go/no-go decision)
   - gate: ratio <= 1.03 on all P0 configs
```

The legacy baseline (RSS-A.10.b) can run immediately on the current
codebase. The flat flist measurement (RSS-A.10.c) runs after RSS-A.5
lands behind a feature flag. Both use identical workloads, fixture
generation, and environment.
