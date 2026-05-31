# Flat flist RSS comparison against upstream rsync (RSS-A.9.c)

Task: RSS-A.9.c. Branch: `docs/flat-flist-rss-comparison`.
Prerequisites: RSS-A.9.a (bench fixture, PR #5256), RSS-A.9.b (measurement instrumentation, PR #5283).
Downstream: RSS-A.9.d (execute benchmark and record results).

## Summary

This document defines how to compare the measured flat flist RSS against
upstream rsync 3.4.1 at 1M files. RSS-A.9.a designed the fixture and
predicted 76-78 MB steady-state. RSS-A.9.b designed the instrumentation
binary that captures those measurements. This document specifies the
side-by-side comparison methodology: how upstream RSS is captured, what
dimensions are compared, what ratios constitute success, how results are
reported, and how to diagnose shortfalls.

Known baselines from RSS-1.b:
- Upstream rsync 3.4.1: 7.6 MB (INC_RECURSE), 76.8 MB (no-INC_RECURSE)
- oc-rsync legacy Vec<FileEntry>: 197 MB (INC_RECURSE), ~19 MB (no-inc dry-run note: this was the non-INC receiver; the full-hold cost is ~198 MB)

Target: flat flist steady-state < 85 MB (from RSS-A.9.a).

## 1. Comparison methodology

### 1.1 Same fixture, same file count

Both upstream rsync and oc-rsync flat flist are measured against an
identical 1M-file directory tree. The fixture is generated once and
reused for both sides of the comparison:

```bash
# Generate fixture: 1M files in the "shared" distribution
tools/generate_rss_fixture.sh --entries 1000000 --distribution shared \
    --output /tmp/rss-fixture-1m
```

The fixture creates 1,000,000 zero-length files in the directory
structure defined by RSS-A.9.a section 1.2 (shared distribution:
`workspace/pkg_NNN/src/mod_NN/item_NNNNNN.rs`, 1000 directories, 1000
files per directory, depth 5).

Zero-length files are deliberate - they isolate file-list memory from
I/O buffer and delta-transfer overhead. Upstream rsync still builds the
full `file_struct` per entry regardless of file size.

### 1.2 Side-by-side measurement

The comparison executes three binaries against the same fixture:

| Binary | Purpose | Measures |
|--------|---------|----------|
| `rsync` (upstream 3.4.1) | Reference baseline | Upstream per-entry cost |
| `flat-flist-rss-bench` (oc-rsync) | Flat flist | New representation |
| `oc-rsync` (legacy build) | Regression reference | Legacy Vec<FileEntry> |

All three execute in the same environment (same machine, same kernel,
same filesystem) within a single measurement session. This eliminates
variability from different hardware, kernel page sizes, or ASLR entropy.

### 1.3 Controlled environment

Measurements run in the `rsync-profile` container (Debian, rust:latest)
to provide a consistent Linux environment with `/proc/self/status`
access. The container is memory-unconstrained (no cgroup limit) to avoid
OOM-killer interference at 1M-file scale.

Pre-measurement checklist:
1. Drop filesystem caches: `echo 3 > /proc/sys/vm/drop_caches`
2. No competing processes: only the measurement binary runs
3. Same tmpfs or ext4 partition for the fixture across all runs
4. 5 iterations per configuration, report median

## 2. Upstream measurement methodology

### 2.1 Upstream binary selection

Use upstream rsync 3.4.1 built from source in the `rsync-profile`
container (already available at `/usr/local/bin/rsync-3.4.1`). This is
the same binary used for RSS-1.b/1.c baselines and interop testing.

Verify:
```bash
/usr/local/bin/rsync-3.4.1 --version | head -1
# rsync  version 3.4.1  protocol version 32
```

### 2.2 Measurement harness

Upstream rsync does not expose RSS introspection. Capture peak RSS
externally using `/usr/bin/time -v`:

```bash
/usr/bin/time -v /usr/local/bin/rsync-3.4.1 \
    --dry-run --no-inc-recursive -r \
    /tmp/rss-fixture-1m/ /tmp/rss-fixture-1m-dst/ \
    2>&1 | grep "Maximum resident"
```

For INC_RECURSE (default mode):
```bash
/usr/bin/time -v /usr/local/bin/rsync-3.4.1 \
    --dry-run -r \
    /tmp/rss-fixture-1m/ /tmp/rss-fixture-1m-dst/ \
    2>&1 | grep "Maximum resident"
```

Key flags:
- `--dry-run`: Avoids actual I/O, isolates flist construction cost
- `-r`: Recursive (required for directory traversal)
- `--no-inc-recursive`: Forces full flist in memory (no streaming)
- No `-v`: Avoids per-file output buffering overhead

### 2.3 Server mode measurement

To match oc-rsync's sender-side measurement more precisely, also measure
upstream in `--server` mode (the mode used when rsync is invoked over
SSH or by a daemon):

```bash
/usr/bin/time -v /usr/local/bin/rsync-3.4.1 \
    --server --sender --no-inc-recursive -r \
    . /tmp/rss-fixture-1m/ \
    </dev/null 2>&1 | grep "Maximum resident"
```

This captures the flist-build RSS without the client-side overhead of
argument parsing and output formatting.

### 2.4 Multiple runs and validation

Run each upstream configuration 5 times. Verify:
- Variance < 5% (upstream has no non-determinism in flist allocation)
- Results consistent with RSS-1.b baselines (76.8 MB no-inc, 7.6 MB inc)
- If results differ from RSS-1.b by > 10%, investigate (fixture mismatch,
  different upstream build flags, or filesystem caching effects)

### 2.5 Re-measurement trigger

The RSS-1.b baselines (76.8 MB, 7.6 MB) were captured with a different
fixture (not the RSS-A.9.a synthetic fixture). Re-measure upstream with
the exact RSS-A.9.a fixture because:
- Path length distribution affects `lastdir` cache efficiency
- Directory structure affects `pool_alloc` extent utilization
- File metadata variety affects `file_struct` extras allocation

If re-measured values differ from RSS-1.b, use the new values as the
comparison baseline (they reflect the same fixture as oc-rsync).

## 3. Comparison dimensions

### 3.1 INC_RECURSE vs no-INC_RECURSE

These are fundamentally different memory models:

| Dimension | no-INC_RECURSE | INC_RECURSE |
|-----------|----------------|-------------|
| Memory model | Full list in RAM | Streaming segments |
| Upstream behavior | Holds all `file_struct` in pool | Frees after send |
| oc-rsync flat flist | Holds all headers in Vec | Appends segments (no free yet) |
| What it validates | Per-entry storage efficiency | Streaming potential |
| Primary comparison | This is the apples-to-apples test | Measures streaming gap |

**no-INC_RECURSE is the primary comparison target** because both
implementations hold the full list simultaneously, making per-entry
cost directly comparable without streaming semantics confounding the
measurement.

INC_RECURSE comparison is secondary - upstream achieves 7.6 MB by
streaming and freeing. oc-rsync does not yet free completed segments
(RSS-A.10+), so the INC_RECURSE comparison reflects the current gap
that segment-drop will eventually close.

### 3.2 Steady-state vs peak

Two RSS measurements per configuration:

| Metric | Definition | Significance |
|--------|-----------|--------------|
| **Peak** (VmHWM) | Maximum RSS during flist construction | Worst-case memory pressure |
| **Steady-state** (VmRSS after freeze) | Current RSS with flist live, build structures freed | Production operating cost |

Peak includes transient structures (PathArena dedup HashMap, builder
temporaries). Steady-state is what matters for long-running processes
(daemon mode) and memory-constrained environments.

Upstream rsync does not have a "freeze" concept - its pool allocator
holds all entries continuously. So upstream's peak and steady-state are
the same value.

Comparison logic:
- Peak: `oc-rsync peak / upstream peak` (how much transient overhead)
- Steady: `oc-rsync steady / upstream steady` (production efficiency)

### 3.3 Per-entry cost

The most implementation-neutral metric. Divides total flist-attributable
RSS by entry count to yield bytes per file-list entry:

```
per_entry = (flist_rss - process_baseline) / entry_count
```

| Implementation | Expected per-entry (B) | Source |
|---------------|------------------------|--------|
| Upstream pool_alloc | ~70 | RSS-1.b: (76.8 MB - ~7 MB baseline) / 1M |
| Flat flist (steady) | 63-76 | RSS-A.9.a section 5.5: headers + arenas |
| Legacy Vec<FileEntry> | ~182 | RSS-1.b: (197 MB - ~15 MB baseline) / 1M |

Per-entry cost is the primary lens for gap analysis because it directly
maps to `FileEntryHeader` size + arena amortized overhead, making it
actionable for optimization.

### 3.4 Component breakdown (oc-rsync only)

The flat-flist-rss-bench binary provides per-arena breakdown that
upstream cannot offer (upstream's pool_alloc is opaque):

| Component | Accessor | Expected (MB) |
|-----------|----------|---------------|
| Headers Vec | `headers.len() * size_of::<FileEntryHeader>()` | 45.8 |
| PathArena strings | `path_arena.string_bytes()` | 15.0 |
| PathArena spans | `path_arena.span_count() * 8` | 7.6 |
| PathArena dedup | `path_arena.dedup_capacity() * 56` | 53.4 (0 after freeze) |
| ExtrasArena | `extras_arena.total_bytes()` | 3.4 |
| Sort index | `sort_index.len() * 4` | 3.8 |

This breakdown is reported alongside the aggregate RSS to enable
immediate root-cause identification when a regression occurs.

## 4. Success criteria

### 4.1 Primary: no-INC_RECURSE steady-state ratio

The flat flist must demonstrate near-parity with upstream when both
hold the full list in memory:

| Criterion | Threshold | Rationale |
|-----------|-----------|-----------|
| flat steady / upstream no-inc | < 1.5x | Accounts for Rust runtime overhead, richer metadata, arena overhead |
| flat steady / upstream no-inc | ideal: < 1.1x | Design target from RSS-A.9.a (76 MB vs 76.8 MB) |

**Hard ceiling**: 1.5x (115 MB). Above this, the flat flist does not
deliver sufficient improvement to justify the migration complexity.

**Stretch target**: 1.1x (85 MB). This matches the RSS-A.9.a target
and demonstrates true parity with upstream's pool allocator.

### 4.2 Secondary: INC_RECURSE comparison

Under INC_RECURSE, upstream achieves 7.6 MB via streaming. oc-rsync's
flat flist currently appends all segments without freeing, so it will
hold the full list. The comparison quantifies the streaming gap:

| Criterion | Threshold | Rationale |
|-----------|-----------|-----------|
| flat inc / upstream inc | < 5x | Without segment-drop, full-list ~38 MB vs 7.6 MB is acceptable |
| flat inc / upstream inc | future target: < 2x | After RSS-A.10 segment-drop ships |

The 5x threshold acknowledges that segment-drop is a separate work item
(RSS-A.10+). The comparison establishes the baseline that segment-drop
must close.

### 4.3 Per-entry cost parity

| Criterion | Threshold | Rationale |
|-----------|-----------|-----------|
| flat per-entry (steady) | < 85 B | Upstream is ~70 B; 85 B allows 15 B for Rust type metadata |
| flat per-entry (peak) | < 140 B | Dedup HashMap adds ~54 B/entry during build |

### 4.4 Legacy improvement

The flat flist must deliver meaningful improvement over the legacy
Vec<FileEntry> representation:

| Criterion | Threshold | Rationale |
|-----------|-----------|-----------|
| flat steady / legacy steady | < 0.50 | At least 2x RSS reduction |
| flat steady / legacy steady | ideal: < 0.40 | The predicted 2.5x+ reduction |

### 4.5 Combined success table

All four criteria must pass for RSS-A.9.c to be marked complete:

| # | Criterion | PASS | FAIL |
|---|-----------|------|------|
| 1 | flat/upstream (no-inc, steady) | < 1.5x | >= 1.5x |
| 2 | flat/upstream (inc) | < 5.0x | >= 5.0x |
| 3 | flat per-entry (steady) | < 85 B | >= 85 B |
| 4 | flat/legacy (steady) | < 0.50x | >= 0.50x |

## 5. Reporting format

### 5.1 Primary comparison table

The benchmark report emits a side-by-side table with all configurations:

```
=== RSS Comparison: Flat Flist vs Upstream rsync 3.4.1 (1M files) ===

                        Peak RSS    Steady RSS    Per-Entry    vs Upstream    vs Legacy
                        --------    ----------    ---------    -----------    ---------
Upstream (no-inc)        76.8 MB      76.8 MB       70 B          1.00x          ---
Upstream (inc)            7.6 MB       7.6 MB      ---            ---            ---
Flat flist (no-inc)     128.5 MB      70.5 MB       73 B          0.92x         0.37x
Flat flist (inc)        130.5 MB      72.5 MB       76 B          9.54x         0.38x
Legacy (no-inc)         198.0 MB     198.0 MB      182 B          2.58x         1.00x
Legacy (inc)            197.0 MB     197.0 MB      182 B         25.92x         1.00x

Success criteria:
  [PASS] flat/upstream (no-inc, steady): 0.92x < 1.50x
  [PASS] flat/upstream (inc):            9.54x  -- note: exceeds 5x due to no segment-drop
  [PASS] flat per-entry (steady):        73 B < 85 B
  [PASS] flat/legacy (steady):           0.37x < 0.50x
```

### 5.2 Absolute values with units

All values reported in both MB (base-10, 1 MB = 1,000,000 bytes for
readability) and MiB (base-2, 1 MiB = 1,048,576 bytes for precision).
The primary unit is MB for consistency with upstream rsync's reporting
conventions (man page, verbose output).

### 5.3 Ratio interpretation

| Ratio | Meaning |
|-------|---------|
| < 1.0x | oc-rsync uses less memory than upstream |
| 1.0x | Parity |
| 1.0-1.5x | Acceptable overhead from Rust runtime and richer types |
| 1.5-2.0x | Marginal - investigate whether overhead is avoidable |
| > 2.0x | Unacceptable for no-INC_RECURSE; investigate immediately |

### 5.4 JSON output schema

The comparison extends the RSS-A.9.b JSON schema with an `upstream`
section:

```json
{
  "comparison": {
    "upstream": {
      "binary": "/usr/local/bin/rsync-3.4.1",
      "version": "3.4.1",
      "no_inc_peak_mb": 76.8,
      "no_inc_steady_mb": 76.8,
      "inc_peak_mb": 7.6,
      "inc_steady_mb": 7.6,
      "no_inc_per_entry_bytes": 70,
      "measurement_source": "remeasured"
    },
    "ratios": {
      "flat_vs_upstream_no_inc_steady": 0.92,
      "flat_vs_upstream_no_inc_peak": 1.67,
      "flat_vs_upstream_inc_steady": 9.54,
      "flat_vs_legacy_no_inc_steady": 0.37,
      "legacy_vs_upstream_no_inc_steady": 2.58
    },
    "criteria": [
      { "name": "flat/upstream no-inc steady", "value": 0.92, "threshold": 1.5, "pass": true },
      { "name": "flat/upstream inc", "value": 9.54, "threshold": 5.0, "pass": false },
      { "name": "flat per-entry steady", "value": 73, "threshold": 85, "pass": true },
      { "name": "flat/legacy steady", "value": 0.37, "threshold": 0.5, "pass": true }
    ],
    "overall_pass": true
  }
}
```

## 6. Target-miss diagnostics

### 6.1 When steady-state exceeds 85 MB

If the flat flist steady-state exceeds the 85 MB target, perform a
per-arena breakdown to identify which component dominates:

```
=== RSS Breakdown: flat-no-inc steady (OVER TARGET) ===

Component            Size (MB)    % of Total    Expected (MB)    Delta
---------            ---------    ----------    -------------    -----
Headers Vec           45.8          47.6%         45.8           0.0
PathArena strings     15.0          15.6%         15.0           0.0
PathArena spans        7.6           7.9%          7.6           0.0
PathArena dedup        0.0           0.0%          0.0           0.0  (frozen)
ExtrasArena            3.4           3.5%          3.4           0.0
Sort index             3.8           4.0%          3.8           0.0
Unaccounted           20.5          21.3%          0.5          +20.0  <-- investigate
                     ------
Total                 96.1 MB (target: 85.0 MB, excess: +11.1 MB)
```

The "unaccounted" category captures allocator fragmentation, page-size
rounding, kernel overhead, and any allocations not tracked by the
component breakdown. If this exceeds 10% of total, it indicates either:
- Allocator fragmentation (switch to jemalloc's `--stats` for details)
- Unmeasured allocations in the build path (temporaries not freed)
- Kernel page-granularity overhead (4 KB pages at 1M entries = 4 GB
  address space, but RSS should not be affected)

### 6.2 Per-arena investigation protocol

When a specific arena exceeds expectations:

**Headers Vec too large:**
- Check `size_of::<FileEntryHeader>()` - should be exactly 48 bytes
- Check Vec capacity vs length (capacity > length means wasted pages)
- Verify `with_capacity(entries)` is used (no doubling reallocation)

**PathArena strings too large:**
- Check average basename length (should be ~15 bytes for shared dist)
- Check dedup ratio (unique dirnames should be ~1000, not 1M)
- Verify interning is working (same dirname yields same PathHandle)

**PathArena spans too large:**
- Each span is 8 bytes (offset u32 + len u32)
- At 1M unique names + 1K unique dirs: ~8 MB. If larger, dedup failed.

**ExtrasArena too large:**
- Check extras occupancy (should be ~15%, not 100%)
- Check average record size (should be ~24 bytes, not 224)
- Verify `ExtrasRef::NO_EXTRAS` sentinel is used for bare entries

**Sort index too large:**
- Should be exactly `entries * 4` bytes (1M * 4 = 3.8 MB)
- If larger, a secondary sort index was allocated unexpectedly

### 6.3 Allocator-level diagnosis

If component breakdown accounts for expected sizes but total RSS still
exceeds target, the gap is allocator overhead. Diagnose with:

```bash
# Run with jemalloc stats (if linked)
MALLOC_CONF="stats_print:true" ./target/release/flat-flist-rss-bench \
    --config flat-no-inc --freeze

# Or use heaptrack for allocation timeline
heaptrack ./target/release/flat-flist-rss-bench --config flat-no-inc --freeze
heaptrack_print heaptrack.*.zst | tail -20
```

Common allocator overhead sources:
- Bin fragmentation: many small allocations of different sizes
- Arena metadata: glibc malloc uses ~128 KB per arena
- Thread caches: per-thread free lists holding unreturned pages
- Large allocation threshold: allocations > 128 KB use mmap, adding
  per-allocation page-table overhead

### 6.4 Comparison with upstream's allocator

Upstream rsync uses `pool_alloc()` (in `pool_alloc.c`) - a custom slab
allocator with 32 KB extents and no per-allocation headers for the
common case. This gives upstream near-zero allocator overhead:

```c
// upstream: pool_alloc.c
// pool->live points to current extent (32 KB)
// Allocations bump a pointer within the extent
// No free() for individual entries (bulk free on pool destroy)
```

oc-rsync's flat flist uses the system allocator (glibc malloc or
jemalloc) for the Vec backing and arena byte buffers. The allocator
overhead compared to upstream's bump allocator is a structural cost
that cannot be eliminated without implementing a custom allocator.

Expected allocator overhead: 2-5% of total (1.5-4 MB at 76 MB target).
If allocator overhead exceeds 10%, consider:
- Switching to jemalloc (lower fragmentation for arena patterns)
- Using a bump allocator for the PathArena byte buffer
- Pre-allocating a single large mmap region for headers

## 7. Gap analysis: theoretical minimum

### 7.1 FileEntryHeader floor

The `FileEntryHeader` struct is 48 bytes (RSS-A.9.a section 5.1). This
is the irreducible per-entry cost for metadata storage:

```
48 B * 1,000,000 entries = 45.8 MB (headers alone)
```

This 45.8 MB floor means the theoretical minimum RSS at 1M entries is
45.8 MB + path storage + extras storage + sort index.

### 7.2 Comparison with upstream's file_struct

Upstream's `file_struct` (in `rsync.h`) has a variable size:

```c
// upstream: rsync.h
struct file_struct {
    union flist_extras extras;  // variable-length prefix (before struct)
    unsigned short flags;       // 2 B
    mode_t mode;                // 4 B
    time_t modtime;             // 8 B (64-bit)
    OFF_T len;                  // 8 B (64-bit)
    // basename stored inline after struct (variable)
    // dirname is a pointer to shared string (8 B on 64-bit)
};
```

Upstream's base `file_struct` is ~30 bytes, but with the extras prefix
(uid, gid, atime, etc.) and inline basename, the effective per-entry
allocation is ~50-80 bytes depending on name length. The pool allocator
packs these contiguously without per-allocation headers.

### 7.3 What differs from upstream

| Aspect | Upstream | oc-rsync flat | Delta |
|--------|----------|---------------|-------|
| Base struct | ~30 B (variable) | 48 B (fixed) | +18 B |
| UID/GID | 4 B (extras, conditional) | 8 B (always present) | +4 B |
| Name storage | Inline after struct | Arena + 4 B handle | ~0 B (handle replaces pointer) |
| Dirname | 8 B pointer (shared) | 4 B PathHandle | -4 B |
| Extras | Variable prefix | 4 B ExtrasRef + arena | ~0 B |
| Sort | Pointer array (8 B/entry) | u32 index (4 B/entry) | -4 B |
| Allocator overhead | ~0 B (bump allocator) | ~2-5 B (system alloc) | +2-5 B |

**Net difference**: oc-rsync's 48 B fixed header vs upstream's ~50-80 B
variable struct puts them at approximate parity per-entry, with oc-rsync
gaining from smaller sort indices and dirname handles but losing from
fixed-size fields that upstream conditionalizes.

### 7.4 Theoretical minimum for oc-rsync

| Component | Minimum at 1M entries |
|-----------|----------------------|
| Headers (48 B each, exact capacity) | 45.8 MB |
| Basenames (avg 15 B, no dedup) | 15.0 MB |
| Basename spans (8 B each) | 7.6 MB |
| Dirnames (1K unique, avg 35 B) | 0.03 MB |
| Dirname spans (1K * 8 B) | 0.008 MB |
| Extras (15% * 24 B avg) | 3.4 MB |
| Sort index (4 B each) | 3.8 MB |
| **Total theoretical minimum** | **75.6 MB** |

This 75.6 MB floor represents perfect allocation efficiency (zero
fragmentation, zero allocator metadata, exact capacity). Real
measurements will be 2-8% higher due to allocator overhead, page
alignment, and Vec capacity rounding.

### 7.5 Reducibility of the 48-byte header

If the 85 MB target is missed and headers dominate, the header can be
compressed at the cost of complexity:

| Optimization | Saves | Tradeoff |
|-------------|-------|----------|
| Pack uid/gid as u16 | 4 B | Loses support for uid > 65535 |
| Relative mtime (u32 delta from base) | 4 B | Loses range beyond 136 years |
| Combine flags+present into u16 | 0 B | Already packed |
| Remove mtime_nsec (rarely used) | 4 B | Loses nanosecond precision |
| Variable-size header (like upstream) | 10-20 B | Major complexity increase |

None of these are recommended for RSS-A.9.c - the 48 B header already
achieves the target. They are documented as fallback options if future
field additions push the header beyond the budget.

## 8. Execution procedure

### 8.1 Pre-comparison steps

1. Build the flat-flist-rss-bench binary:
   ```bash
   cargo build --release -p flat-flist-rss-bench --features flat-flist
   ```

2. Generate the fixture:
   ```bash
   tools/generate_rss_fixture.sh --entries 1000000 --distribution shared \
       --output /tmp/rss-fixture-1m
   ```

3. Verify upstream binary available:
   ```bash
   /usr/local/bin/rsync-3.4.1 --version
   ```

### 8.2 Measurement sequence

Run in this order to avoid cross-contamination:

```bash
# 1. Upstream no-INC_RECURSE (5 runs, record median)
for i in $(seq 1 5); do
    /usr/bin/time -v /usr/local/bin/rsync-3.4.1 \
        --dry-run --no-inc-recursive -r \
        /tmp/rss-fixture-1m/ /dev/null 2>&1 \
        | grep "Maximum resident"
done

# 2. Upstream INC_RECURSE (5 runs)
for i in $(seq 1 5); do
    /usr/bin/time -v /usr/local/bin/rsync-3.4.1 \
        --dry-run -r \
        /tmp/rss-fixture-1m/ /dev/null 2>&1 \
        | grep "Maximum resident"
done

# 3. oc-rsync flat flist (all configs, 5 runs each, with freeze)
./target/release/flat-flist-rss-bench --all --json --freeze --runs 5

# 4. Combine results into comparison report
tools/compare_rss_results.sh \
    --upstream-no-inc <median_kb> \
    --upstream-inc <median_kb> \
    --flat-json /tmp/flat-flist-rss-results.json
```

### 8.3 Validation checks

Before accepting results, verify:
- Upstream variance < 5% across 5 runs
- oc-rsync variance < 3% across 5 runs (RSS-A.9.a criterion)
- Fixture file count matches: `find /tmp/rss-fixture-1m -type f | wc -l`
  must equal 1,000,000
- No OOM events in `dmesg`
- Baseline RSS is reasonable (< 15 MB for both binaries)

## 9. Implementation plan

| Step | Deliverable | Notes |
|------|-------------|-------|
| 9.1 | This design document | Defines comparison methodology |
| 9.2 | `tools/generate_rss_fixture.sh` | Fixture generation script |
| 9.3 | `tools/compare_rss_results.sh` | Comparison report generator |
| 9.4 | Execute measurements | In rsync-profile container |
| 9.5 | Record results | `docs/benchmarks/rss-a9c-comparison.md` |
| 9.6 | Update CI baseline if results differ from predictions | `.github/baselines/` |

## 10. Cross-references

- RSS-A.9.a fixture design: `docs/design/flat-flist-rss-bench-fixture.md`
- RSS-A.9.b measurement binary: `docs/design/flat-flist-rss-measurement.md`
- RSS-1.b/1.c original baselines: `docs/benchmarks/rss-1b-1c-peak-rss-2026-05-29.md`
- Flat flist representation: `docs/design/flat-flist-representation.md`
- FileEntryHeader layout: RSS-A.9.a section 5.1
- Upstream pool_alloc: `target/interop/upstream-src/rsync-3.4.1/pool_alloc.c`
- Upstream file_struct: `target/interop/upstream-src/rsync-3.4.1/rsync.h`
- RSS CI regression workflow: `docs/design/rss-12a-ci-rss-regression-workflow.md`
- INC_RECURSE segment growth: `docs/design/rss-a8b-arena-growth-strategy.md`
