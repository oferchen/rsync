# Flat flist post-migration throughput measurement (RSS-A.10.b)

Task: RSS-A.10.b. Branch: `docs/flat-flist-throughput-post-migration`.
Prerequisites: RSS-A.10.a (baseline methodology, PR #5261 merged),
RSS-A.5 (flat flist implementation behind feature flag).
Downstream: RSS-A.10.d (comparison + go/no-go decision).

## Summary

This document defines the AFTER measurement for flat flist throughput
validation. It uses the identical methodology established in RSS-A.10.a
(workloads, file size distributions, statistical approach, environment
controls) but runs with the flat flist feature enabled. The flat flist
replaces `Vec<FileEntry>` direct indexing with offset-based arena
lookups - a trade-off that reduces RSS dramatically but introduces
indirection on every file-entry access.

The goal is to produce paired measurements against the RSS-A.10.a legacy
baseline and determine whether the flat flist path meets the 3%
regression threshold across all P0 configurations.

## 1. Methodology

Identical to RSS-A.10.a section 1-4, with one change: the binary is
compiled with `--features flat-flist` to activate the arena-backed file
list path.

### 1.1 Workload profiles (unchanged)

| Workload | File counts | Notes |
|----------|-------------|-------|
| Initial sync | 1K, 10K, 100K, 1M | Empty destination |
| Delta sync (10% changed) | 10K, 100K, 1M | Mtime+append modification |
| Delete sync (10% extra) | 10K, 100K | --delete flag |

### 1.2 File size distributions (unchanged)

| Distribution | Sizes |
|-------------|-------|
| all-small | 4 KiB uniform |
| all-large | 100 MiB uniform |
| mixed | 60% < 4K, 25% 4-256K, 10% 256K-2M, 5% 2-100M |

### 1.3 Iteration and statistical controls (unchanged)

- 10 iterations per configuration (20 if CV > 5%)
- Warm filesystem cache (one discarded warmup run)
- Median as primary point estimate, IQR for dispersion
- Same machine, same compiler flags, tmpfs backing store

### 1.4 Build configuration

```sh
cargo build --release --features flat-flist
```

The only difference from the baseline build is the feature flag. LTO,
codegen-units, opt-level, and target CPU remain identical to ensure the
comparison isolates data-structure overhead rather than compiler
differences.

## 2. What changed: arena indirection model

The flat flist replaces direct `Vec<FileEntry>` indexing with a layered
lookup:

```
Legacy path:
  flist.entries[ndx] -> FileEntry (inline fields)
  flist.entries[ndx].name -> PathBuf (heap pointer)

Flat flist path:
  flist.index[ndx] -> offset into header_buf
  header_buf[offset..offset+HEADER_SIZE] -> FlatHeader (fixed 48-64 B)
  FlatHeader.path_handle -> PathArena::resolve(handle) -> &[u8]
  FlatHeader.extras_offset -> ExtrasArena::decode(offset) -> extras fields
```

Every file-entry access now involves:

1. **Index dereference** - one additional array lookup vs direct Vec
   indexing (negligible: same cache line if sequential)
2. **Header decode** - read fixed-size header from a contiguous buffer
   (comparable to Vec layout if headers are packed)
3. **PathArena resolution** - resolve a 4-byte handle to a path slice
   via arena offset (new indirection vs inline PathBuf)
4. **ExtrasArena decode** - resolve optional extras data from a separate
   arena region (new indirection vs Box dereference)

The structural advantage is that headers are packed contiguously (better
prefetch characteristics for sequential iteration) and all variable-
length data shares a single allocation. The risk is that random-access
patterns (sort comparisons, filter evaluation) now chase pointers into
the path arena instead of following inline PathBuf pointers - potentially
worse cache behavior when comparisons alternate between distant entries.

## 3. Expected hotspots

### 3.1 PathArena resolution during sort

`clean_flist()` sorts by path components. Each comparison resolves two
`PathHandle` values through `PathArena` to obtain byte slices for
lexicographic comparison. With N entries, `O(N log N)` comparisons means
`O(2N log N)` arena resolutions during sort alone.

**Why this may regress:** Sort comparisons access entries in non-
sequential order. The legacy `Vec<FileEntry>` stores PathBuf inline (a
pointer to a separate heap allocation) - those heap allocations may be
scattered, but the pointer is always in the same cache line as other
FileEntry fields. In the flat flist, the PathHandle is in the header
(same cache line) but the actual bytes are in the PathArena at an
arbitrary offset. If the arena is large (1M entries x ~30 bytes avg path
= 30 MB), sort comparisons jump across 30 MB of arena memory -
exceeding typical LLC sizes on comparison-heavy configurations.

**Expected impact:** 0-5% regression on sort phase for 100K+ entries
with all-small distribution (where sort dominates wall time).

### 3.2 ExtrasArena decode during transfer

The generator and sender read metadata fields (size, mtime, mode) from
each file entry during quick-check and transfer. In the flat flist,
commonly-accessed fields (size, mtime, mode) are stored inline in the
fixed header to avoid extras-arena lookups on the hot path. However,
less-common fields (symlink target, device numbers, hardlink state,
checksums) require ExtrasArena resolution.

**Why this may regress:** Delta sync and checksum-mode transfers access
extras fields (stored checksums) per file. Each access traverses:
header -> extras_offset -> ExtrasArena -> decode variable-length record.

**Expected impact:** 0-2% regression on delta-sync workloads. Negligible
on initial sync (extras rarely populated for regular files).

### 3.3 Filter evaluation path resolution

`FilterChain::check()` calls `PathArena::resolve()` per entry to obtain
dirname and basename for rule matching. Sequential flist iteration means
sequential arena access (good locality), but filter rules may trigger
parent-directory lookups that jump backwards in the arena.

**Expected impact:** < 1% regression. Filter evaluation is not the
bottleneck at any tested scale.

### 3.4 INC_RECURSE segment iteration

INC_RECURSE appends new flist segments during transfer. Each segment's
entries are arena-allocated and sorted independently. Cross-segment NDX
lookups (sender receiving NDX from generator) must resolve through the
segment table then into the segment's header buffer.

**Expected impact:** 1-3% regression at 1M-file scale with INC_RECURSE
enabled, due to segment-boundary cache pollution.

## 4. Paired comparison analysis

### 4.1 Data pairing

Each flat flist measurement is paired with its exact counterpart from
the RSS-A.10.a baseline (same workload, size distribution, INC_RECURSE
setting, iteration count). Pairing eliminates inter-session machine
variance.

### 4.2 Ratio computation

```
ratio = median(flat_flist_wall_s) / median(legacy_wall_s)
```

Report per-configuration and per-hot-path ratios:

```
| Workload | Size | INC | Legacy (med) | Flat (med) | Ratio | Hot-path breakdown |
|----------|------|-----|--------------|------------|-------|--------------------|
| init-100K | small | on | 1.234s | ? | ? | sort: ?, gen: ?, xfer: ? |
```

### 4.3 Per-hot-path timing

The `bench-instrumentation` feature emits per-phase timings:

| Phase | Instrumentation point |
|-------|----------------------|
| sort | `clean_flist()` entry/exit |
| filter | `FilterChain::check()` total across all entries |
| generator | generator main loop start/end |
| sender | sender main loop start/end |
| delete | delete-plan construction |

If the overall ratio passes (< 1.03) but a single phase exceeds 1.05,
flag it for optimization even though it does not block migration.

### 4.4 Secondary metric comparison

| Metric | Significance |
|--------|-------------|
| L1d cache misses | Direct indicator of arena locality regression |
| LLC misses | Large-arena working-set pressure |
| Instructions retired | Detects decode overhead (extra instructions per access) |
| Peak RSS | Confirms RSS improvement (expected 3-5x reduction at 1M) |

## 5. Pass criteria

| Tier | Condition | Decision |
|------|-----------|----------|
| P0 pass | All P0 configs: ratio <= 1.03 | Flat flist becomes default |
| P0 fail | Any P0 config: ratio > 1.03 | Migration blocked - apply mitigations |
| P1 warning | Any P1 config: ratio > 1.03 (P0 passes) | Investigate, may proceed |

### 5.1 P0 configurations (gate the migration)

| # | Workload | Size dist | INC_RECURSE |
|---|----------|-----------|-------------|
| 1 | initial-100K | all-small | on |
| 2 | initial-100K | all-small | off |
| 3 | initial-1M | all-small | on |
| 4 | initial-1M | all-small | off |

These maximize per-entry overhead sensitivity: tiny files mean the flist
data-structure cost dominates wall time, not I/O.

### 5.2 P1 configurations (investigate if regression)

| # | Workload | Size dist | INC_RECURSE |
|---|----------|-----------|-------------|
| 5 | delta-100K | mixed | on |
| 6 | delta-1M | all-small | on |
| 7 | delete-100K | all-small | on |
| 8 | initial-100K | mixed | on |

## 6. Profiling plan if regression detected

If any P0 configuration exceeds the 1.03 threshold, execute this
profiling sequence to identify the exact cause:

### 6.1 Phase isolation

Run the regression configuration with `bench-instrumentation` enabled.
Identify which phase(s) account for the regression:

```sh
BENCH_INSTRUMENT=1 cargo run --release --features flat-flist,bench-instrumentation \
  -- src/ dst/ 2>timings.log
```

Parse `timings.log` for per-phase medians. If a single phase accounts
for > 80% of the regression, focus profiling there.

### 6.2 perf record on hot path

```sh
perf record -g --call-graph dwarf -F 4999 -- \
  target/release/oc-rsync src/ dst/

perf report --sort=dso,symbol --percent-limit=1
```

Look for:
- `PathArena::resolve` in top symbols (arena lookup overhead)
- `memcmp` or `slice_cmp` from sort comparisons (path comparison cost)
- `ExtrasArena::decode` (extras field deserialization)
- Cache-miss stalls in `perf stat` output

### 6.3 Cache analysis

```sh
perf stat -e L1-dcache-load-misses,LLC-load-misses,instructions \
  -- target/release/oc-rsync src/ dst/
```

Compare L1d miss count between legacy and flat flist. If flat flist
shows > 20% more L1d misses, the arena layout is the culprit.

### 6.4 DHAT heap profiling

```sh
cargo run --release --features flat-flist,dhat-heap -- src/ dst/
```

Confirm RSS reduction is achieved (validates the trade-off is worth
pursuing mitigations for throughput).

## 7. Mitigation strategies if > 3% regression

Ordered by implementation cost (cheapest first):

### 7.1 Sort-key pre-resolution (inline caching)

**Problem:** Each sort comparison resolves PathHandle through PathArena.
**Fix:** Before sorting, resolve all path handles into a temporary
`Vec<&[u8]>` or `Vec<SortKey>`. Sort the index array by comparing
pre-resolved keys. This eliminates arena lookups during the O(N log N)
comparison phase.

```rust
// Pre-resolve: O(N) arena lookups
let sort_keys: Vec<&[u8]> = index.iter()
    .map(|&offset| path_arena.resolve(headers[offset].path_handle))
    .collect();

// Sort: O(N log N) comparisons on contiguous pre-resolved data
index.sort_unstable_by(|&a, &b| sort_keys[a].cmp(&sort_keys[b]));
```

**Expected gain:** Eliminates sort-phase regression entirely.
**Cost:** O(N) temporary allocation (N pointers = 8N bytes).

### 7.2 Arena prefetch hints

**Problem:** Sequential generator iteration has good spatial locality,
but the CPU prefetcher may not follow arena offsets.
**Fix:** Emit explicit prefetch hints for the next entry's arena data
during iteration:

```rust
for i in 0..entries.len() {
    // Prefetch next entry's path data
    if i + 1 < entries.len() {
        let next_handle = headers[index[i + 1]].path_handle;
        std::arch::x86_64::_mm_prefetch(
            path_arena.ptr_for(next_handle) as *const i8,
            std::arch::x86_64::_MM_HINT_T0,
        );
    }
    process_entry(i);
}
```

**Expected gain:** 1-2% on sequential iteration paths (generator, filter).
**Cost:** Minimal code change. Platform-specific (x86_64/aarch64).

### 7.3 Header layout optimization

**Problem:** FlatHeader fields accessed together may span cache lines.
**Fix:** Reorder FlatHeader fields by access frequency. Place
{path_handle, size, mtime, mode} in the first 32 bytes (fits one cache
line). Move rarely-accessed fields (extras_offset, flags, dev) to the
second half.

```rust
#[repr(C)]
struct FlatHeader {
    // Hot fields - first cache line (32 bytes)
    path_handle: PathHandle,   // 4 B
    size: u64,                 // 8 B
    mtime: i64,                // 8 B
    mode: u32,                 // 4 B
    flags: u16,                // 2 B
    _pad0: [u8; 6],           // 6 B alignment

    // Cold fields - second cache line
    extras_offset: u32,        // 4 B
    uid: u32,                  // 4 B
    gid: u32,                  // 4 B
    // ...
}
```

**Expected gain:** Reduces cache-line fetches for quick-check (generator
reads size+mtime+mode without pulling extras_offset into L1).
**Cost:** Requires careful struct layout audit and may require
`#[repr(C)]` to prevent compiler reordering.

### 7.4 Path arena compaction by sort order

**Problem:** After sorting, entries are accessed in sorted order but
their path data is scattered across the arena in insertion order.
**Fix:** After the initial sort, compact the path arena so that paths
appear in sorted-entry order. Subsequent sequential iteration then
has perfect spatial locality in the arena.

```rust
fn compact_arena_by_sort_order(
    index: &[usize],
    headers: &mut [FlatHeader],
    path_arena: &mut PathArena,
) {
    let mut new_arena = PathArena::with_capacity(path_arena.len());
    for &idx in index {
        let old_handle = headers[idx].path_handle;
        let path_bytes = path_arena.resolve(old_handle);
        let new_handle = new_arena.intern(path_bytes);
        headers[idx].path_handle = new_handle;
    }
    *path_arena = new_arena;
}
```

**Expected gain:** 2-4% on generator iteration for 1M files (brings
arena access pattern from random to sequential).
**Cost:** O(N) copy of all path data. One-time cost after sort. Only
worthwhile if the 1M-file scale shows cache-miss regression.

### 7.5 Batch extras resolution

**Problem:** Delta sync resolves extras (checksums) per-file during
transfer.
**Fix:** Batch-decode the next K entries' extras into a local buffer
before processing them. Amortizes arena traversal and enables the CPU
to overlap decode with transfer I/O.

**Expected gain:** 0-1% (extras access is not expected to be the primary
bottleneck).
**Cost:** Low complexity. Only implement if profiling confirms extras
decode is significant.

## 8. Execution plan

### 8.1 Prerequisites

- [ ] RSS-A.10.a baseline numbers captured and stored in
  `target/bench/throughput/legacy/`
- [ ] Flat flist feature flag compiles and passes all existing tests
- [ ] `bench-instrumentation` feature emits per-phase timings
- [ ] Benchmark host available with identical configuration

### 8.2 Steps

1. Build flat flist binary:
   `cargo build --release --features flat-flist`
2. Run P0 configurations (4 configs x 10 iterations = 40 runs)
3. Compute ratios against legacy baseline
4. If all P0 pass: run P1 configurations (4 configs x 10 iterations)
5. Generate comparison report (markdown table)
6. If any regression: execute profiling plan (section 6)
7. If mitigations needed: implement cheapest effective mitigation,
   re-measure

### 8.3 Time estimate

- P0 measurement: ~2 hours (dominated by 1M-file workloads)
- P1 measurement: ~1 hour
- Profiling (if needed): ~2 hours
- Mitigation (if needed): 1-3 days depending on complexity

## 9. Output artifacts

| Artifact | Location | Purpose |
|----------|----------|---------|
| Raw measurements | `target/bench/throughput/flat-flist/` | JSON per-config |
| Comparison report | PR body | Go/no-go decision |
| Per-phase timings | `target/bench/throughput/flat-flist/phases/` | Hotspot identification |
| perf recordings | `target/bench/throughput/flat-flist/perf/` | Root-cause analysis |

## 10. Relationship to RSS-A.10.a

| Aspect | RSS-A.10.a (baseline) | RSS-A.10.b (this doc) |
|--------|----------------------|----------------------|
| Binary | Legacy Vec path | flat-flist feature flag |
| Workloads | Defined here | Identical |
| File sizes | Defined here | Identical |
| Environment | Defined here | Same machine, same session |
| Output | `target/bench/throughput/legacy/` | `target/bench/throughput/flat-flist/` |
| Decision | Capture only | Compare + gate |

The paired design ensures any throughput difference is attributable
solely to the data-structure change, not environmental drift.
