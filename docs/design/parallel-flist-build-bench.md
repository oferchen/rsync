# Parallel vs sequential FlatFileList construction benchmark

Task: RSS-A.11.d. Branch: `docs/parallel-flist-build-bench`.
Prerequisites: RSS-A.11.c (parallel FlatFileList builder with rayon).
Downstream: RSS-A.11.e (execute benchmark, decide on default activation).

## Summary

RSS-A.11.c shipped a `ParallelFlatFileListBuilder` behind the
`flat-flist-rayon` feature flag. The builder uses per-thread arenas
merged into a single `FlatFileList` at completion. This document defines
the benchmark that determines whether the parallel path delivers
sufficient speedup over sequential `FlatFileList::push_with_extras` to
justify its complexity at 100K+ file counts.

The core question: does parallelizing arena construction (interning paths
and appending extras across multiple workers) outweigh the O(n) merge
cost, and by how much?

## 1. Bench architecture

### 1.1 Isolation from stat/readdir

The benchmark measures flist construction time only - not filesystem
operations. Both paths receive pre-computed inputs:

```rust
struct SyntheticEntry {
    dirname: &'static str,
    basename: &'static str,
    metadata: SyntheticMetadata,
    extras: Option<ExtrasPayload>,
}
```

Entries are generated once during setup and pinned in memory. This
isolates the arena allocation/interning cost from I/O variance.

### 1.2 Measurement boundary

```
[setup: generate entries] -> [TIMED: build flist] -> [teardown: drop]
```

For the parallel path, the timed region includes both the accumulation
phase (per-worker arena building) and the merge phase. Section 6
describes how to isolate these sub-phases.

### 1.3 Harness location

`crates/flist/benches/parallel_build.rs` (new file, criterion).

Gated behind `OC_RSYNC_RUN_FLIST_BENCH=1` to keep CI nightly time
bounded. Runner: `cargo bench -p flist --bench parallel_build --features flat-flist-rayon`.

## 2. File counts and entry distributions

### 2.1 Scale tiers

| Tier | Entry count | Purpose |
|------|-------------|---------|
| small | 10,000 | Baseline - parallel overhead visible |
| medium | 100,000 | Decision threshold |
| large | 500,000 | Scaling validation |
| extreme | 1,000,000 | Peak RSS and merge cost dominance |

### 2.2 Path length distributions

Matching the RSS-A.9.a fixture specification:

- Dirname components: 3-15 characters, 2-8 components deep.
- Average dirname length: ~40 bytes.
- Basename length: 12-50 characters (60% short, 25% medium, 15% long).
- Average basename length: ~18 bytes.
- Total average path: ~58 bytes per entry.

### 2.3 Directory sharing ratio

Three distributions exercise different interning cache-hit rates:

| Distribution | Unique dirnames | Sharing ratio | Interning pressure |
|-------------|----------------|---------------|-------------------|
| monorepo | entries / 1000 | 1000:1 | Low (high dedup) |
| project | entries / 20 | 20:1 | Medium |
| wide | entries / 5 | 5:1 | High (minimal dedup) |

The **monorepo** distribution is the primary case - most real transfers
have high dirname sharing. The **wide** distribution is the worst case
for parallel interning because each worker sees mostly unique dirnames,
causing the merge dedup to do substantial work.

### 2.4 Extras distribution

Per RSS-A.9.a section 1.4:

- 85% entries: no extras (`ExtrasRef::NO_EXTRAS`).
- 5% entries: 16-byte checksum tail.
- 5% entries: symlink target (20-60 bytes).
- 3% entries: ACL/xattr index (8 bytes).
- 2% entries: device rdev or hardlink index.

## 3. Metrics

Each benchmark arm reports:

| Metric | Unit | Collection method |
|--------|------|-------------------|
| Wall-clock construction time | ns (criterion) | `Instant::now()` around build |
| Per-entry amortized cost | ns/entry | total_time / entry_count |
| Arena merge overhead | ns | separate timer around `merge()` call |
| Accumulation phase time | ns | total - merge |
| Peak RSS during build | bytes | `/proc/self/statm` or `mach_task_info` sampled at merge start and end |
| Speedup vs sequential | ratio | sequential_time / parallel_time |

RSS measurement uses platform-specific APIs:
- Linux: read `/proc/self/statm` field 1 (resident pages) * page_size.
- macOS: `mach_task_basic_info` via `task_info()`.

## 4. Worker count variation

The parallel path is benchmarked at these rayon thread pool sizes:

| Workers | Rationale |
|---------|-----------|
| 1 | Overhead floor - parallel machinery with no parallelism |
| 2 | Minimum useful parallelism |
| 4 | Typical laptop core count |
| 8 | Typical workstation / CI runner |
| 16 | High-core server, scaling ceiling test |

Each worker count is a separate criterion benchmark group. The thread
pool is constructed explicitly via `rayon::ThreadPoolBuilder` to control
the worker count precisely:

```rust
let pool = rayon::ThreadPoolBuilder::new()
    .num_threads(workers)
    .build()
    .unwrap();

pool.install(|| {
    // parallel build
});
```

## 5. Sequential vs parallel comparison

### 5.1 Sequential baseline

The sequential path calls `FlatFileList::push_with_extras` in a tight
loop over the same synthetic entries:

```rust
let mut flist = FlatFileList::with_capacity(entries.len());
for entry in &entries {
    let dirname = flist.paths_mut().intern(entry.dirname);
    let name = flist.paths_mut().intern(entry.basename);
    let header = FileEntryHeader { name, dirname, ..from_metadata(&entry.metadata) };
    flist.push_with_extras(header, entry.extras.as_ref());
}
flist.sort();
```

### 5.2 Parallel path

The parallel path uses `ParallelFlatFileListBuilder`:

```rust
let mut builder = ParallelFlatFileListBuilder::with_capacity(workers, per_worker);
let chunks: Vec<&[SyntheticEntry]> = entries.chunks(per_worker).collect();

pool.install(|| {
    chunks.par_iter().enumerate().for_each(|(i, chunk)| {
        let list = builder.worker_list_mut(i);
        for entry in *chunk {
            let dirname = list.paths_mut().intern(entry.dirname);
            let name = list.paths_mut().intern(entry.basename);
            let header = FileEntryHeader { name, dirname, ..from_metadata(&entry.metadata) };
            list.push_with_extras(header, entry.extras.as_ref());
        }
    });
});
let flist = builder.merge();
```

### 5.3 Comparison matrix

Full matrix: 4 tiers * 3 distributions * 6 thread counts (1 seq + 5
parallel) = 72 data points. Criterion groups:

```
parallel_build/seq/{monorepo,project,wide}/{10k,100k,500k,1m}
parallel_build/par_{1,2,4,8,16}/{monorepo,project,wide}/{10k,100k,500k,1m}
```

## 6. Arena merge cost isolation

The merge phase is measured separately to determine its share of total
parallel build time:

```rust
// Inside the parallel path timing:
let t_start = Instant::now();
pool.install(|| { /* accumulation */ });
let t_accum = t_start.elapsed();

let t_merge_start = Instant::now();
let flist = builder.merge();
let t_merge = t_merge_start.elapsed();
```

### 6.1 Merge sub-phases

Within `merge()`, instrument these sub-operations:

| Sub-phase | Operation | Expected cost |
|-----------|-----------|---------------|
| Path re-interning | Resolve handles from worker arenas, intern into final | O(n) with HashMap lookup per entry |
| Extras re-encoding | Decode from worker extras arena, encode into final | O(n * extras_size), affects only 15% of entries |
| Header fixup | Update PathHandle/ExtrasRef in each header | O(n), trivial per-entry |
| Final sort | `sort_unstable_by` on merged headers | O(n log n), dirname-then-name comparator |

### 6.2 Merge scaling expectation

Merge is sequential O(n). As worker count increases, accumulation time
decreases (ideally linearly) while merge time stays constant. The
crossover point - where merge dominates total parallel time - determines
the maximum useful worker count.

Expected merge cost at 1M entries (from RSS-A.11.b estimates):
- Path re-interning: ~50 ms (HashMap lookup + possible allocation).
- Extras re-encoding: ~5 ms (15% of entries, small payloads).
- Header fixup: ~3 ms (pointer-width writes).
- Final sort: ~200 ms (n log n string comparisons).

The sort dominates merge. Both paths (sequential and parallel) pay the
same sort cost, so the speedup comparison should also be reported
excluding sort time.

## 7. Decision criteria

### 7.1 Go / no-go thresholds

The parallel builder becomes the default (feature flag removed) only if
ALL conditions hold:

| Condition | Threshold | Rationale |
|-----------|-----------|-----------|
| Speedup at 100K entries, 4 workers | >= 1.5x | Justifies complexity for typical workloads |
| Speedup at 1M entries, 8 workers | >= 2.0x | Must scale with entry count |
| 1-worker overhead vs sequential | <= 10% | Parallel machinery cost must be bounded |
| Peak RSS overhead at 1M, 8 workers | <= 50% over sequential | Memory cost must be tolerable |
| Merge phase <= 30% of total parallel time | At 4+ workers, 100K+ entries | Merge must not dominate |

### 7.2 Conditional outcomes

| Result | Action |
|--------|--------|
| All thresholds met | Remove feature gate, enable parallel builder as default above `PARALLEL_STAT_THRESHOLD` |
| Speedup meets 1.5x but RSS exceeds 50% | Keep feature gate, document memory trade-off, enable only with `--parallel-flist` flag |
| 1-worker overhead exceeds 10% | Investigate builder overhead, optimize before re-benchmarking |
| Speedup below 1.5x at 100K | Reject parallel builder for arena construction - keep sequential path, parallel stat only |

### 7.3 Reporting format

Results go into `docs/design/parallel-flist-build-bench-results.md` with:

- Raw criterion output (JSON export).
- Summary table: entry count * worker count * distribution -> speedup.
- Merge fraction chart: merge_time / total_parallel_time vs worker count.
- RSS comparison: sequential vs parallel peak at each tier.
- Recommendation paragraph citing which thresholds passed/failed.

## 8. Implementation notes

### 8.1 Criterion configuration

```toml
[[bench]]
name = "parallel_build"
harness = false
required-features = ["flat-flist-rayon"]
```

Criterion settings: 100 samples, 5 s warm-up, 10 s measurement for the
1M tier (smaller tiers use defaults). Noise threshold set to 5% to
reject unstable measurements.

### 8.2 Synthetic entry generation

Use a seeded PRNG (`SmallRng::seed_from_u64(0xRSSA11D)`) for
reproducible entry generation. The generator lives in a shared
`bench_fixtures` module reusable by downstream benchmarks (RSS-A.12+).

### 8.3 Platform considerations

- Run on Linux (CI runner, 8 cores) as the primary platform.
- macOS results reported separately - different memory allocator behavior
  (jemalloc vs system malloc) may shift the crossover point.
- Windows excluded from this benchmark (rayon works identically, the
  platform-specific aspect is RSS measurement only).

## 9. Risks

1. **NUMA effects at 16 workers.** Cross-socket memory access during
   merge may inflate merge time on multi-socket machines. Mitigation:
   pin the benchmark to a single NUMA node via `numactl --cpunodebind=0`
   on CI.

2. **Allocator warm-up.** The first benchmark arm pays jemalloc metadata
   costs that subsequent arms avoid. Mitigation: criterion's warm-up
   phase handles this; additionally, run a throwaway 10K build before
   measurement begins.

3. **Sort dominance masking construction speedup.** If sort is 80% of
   total time, a 4x speedup in construction yields only 1.25x end-to-end.
   Mitigation: report speedup both including and excluding sort time.
   The decision criteria in section 7 apply to construction-only time
   (excluding sort) since both paths share the same sort implementation.

4. **Dedup HashMap resizing during merge.** If the final `PathArena`'s
   dedup map resizes during merge, a single resize at ~500K entries adds
   ~10 ms of rehashing. Mitigation: pre-size the dedup map in
   `FlatFileList::with_capacity()` based on an estimated unique dirname
   count (entry_count / expected_sharing_ratio).
