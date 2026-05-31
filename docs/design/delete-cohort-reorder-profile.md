# Delete cohort/reorder-buffer overhead profile design (DML-2)

Status: Design (task DML-2, #3307)
Audience: engine and performance maintainers
Scope: identify which pipeline stages dominate overhead for small-directory
deletes (< 100 files) by instrumenting each stage independently.

Depends on: DML-1 (latency bench, merged PR #5258), DML-6 (architecture
doc, merged).
Feeds into: DML-3 (fast-path design, complete) - validates the threshold
and confirms which stages the fast path must bypass.

Out of scope: large-scale delete throughput (covered by DEL-4.a/b/c),
fast-path implementation (DML-4), and the parallel consumer path
(gated behind `parallel-delete-consumer` feature flag).

## 1. Motivation

DML-1 measures end-to-end delete-phase latency against upstream rsync at
various scales. It confirms the overhead exists but does not reveal where
in the pipeline time is spent. For small directories (10-100 files), the
unlink syscalls complete in microseconds - any constant-factor pipeline
setup that exceeds the sum of unlinks represents pure overhead.

DML-2 instruments each pipeline stage individually to produce a breakdown
showing:

1. Which stage dominates at small scale (setup vs per-item).
2. Whether overhead is proportional to file count or constant per
   directory.
3. Whether the DML-3 fast-path threshold (64 extras) is well-calibrated
   to the crossover point where pipeline setup cost < sequential unlink
   cost.

## 2. Pipeline stages to instrument

The parallel-deterministic-delete (PDD) pipeline has five distinct stages
for each directory processed via `--delete-before`:

### 2.1 Stage A: compute_extras (plan build)

- **Location:** `crates/engine/src/delete/extras.rs::compute_extras`
- **Work performed:**
  - `read_dir` on destination directory (1 syscall + N `DirEntry` reads)
  - `HashSet` build from segment basenames (N inserts)
  - `symlink_metadata` per survivor (1 syscall each)
  - Classification into `DeleteEntryKind` variants
  - `Vec<DeleteEntry>` allocation and population
- **Expected cost at 10 files:** 10 stat syscalls (~30us) + HashSet
  build (~1us) + Vec alloc (~0.1us) = ~31us
- **Expected cost at 100 files:** 100 stat syscalls (~300us) + HashSet
  build (~5us) = ~305us
- **Nature:** per-item dominated (linear in file count)

### 2.2 Stage B: plan sort and publish (cohort indexing)

- **Location:** `DeletePlan::sort_by_name` + `DeletePlanMap::publish`
- **Work performed:**
  - `sort_unstable_by` on extras Vec (reverse `f_name_cmp` order)
  - `PathBuf` clone for the plan map key
  - `Mutex::lock` + `HashMap::insert` on `DeletePlanMap`
- **Expected cost at 10 files:** sort 10 items (~0.5us) + PathBuf
  clone (~0.1us) + map insert (~0.2us) = ~0.8us
- **Expected cost at 100 files:** sort 100 items (~3us) + PathBuf
  clone + map insert = ~3.5us
- **Nature:** per-item (sort) + constant (publish). Sort dominates.

### 2.3 Stage C: cursor registration and observation

- **Location:** `DeleteContext::observe_segment_for_delete` ->
  `CursorObservation` channel send + cursor build in `into_emitter`
- **Work performed:**
  - `PathBuf` + `Vec<FileEntry>` clone for the observation message
  - `crossbeam_channel::send` (unbounded, allocation on grow)
  - At drain time: `DirTraversalCursor` construction from all
    observations (one `sort_unstable_by` of all directories)
- **Expected cost at 10 files (1 dir):** channel send (~0.1us) +
  cursor build from 1 observation (~0.2us) = ~0.3us
- **Expected cost at 100 files (5 dirs):** 5 sends + cursor build
  sorting 5 entries = ~1.5us
- **Nature:** constant per directory, linear in directory count

### 2.4 Stage D: reorder buffer insert/seal/drain

- **Location:** `CohortBatcher::enqueue_cohort` +
  `CohortBatcher::drain_batch` (wrapping `ReorderBuffer`)
- **Work performed:**
  - `BTreeMap::insert` for the cohort slot (keyed by rank)
  - `DeleteCohortKey` construction (PathBuf clone)
  - Per-operation `DeleteOperation` struct construction and Vec push
  - `seal` call (marks slot ready)
  - `try_drain_ready`: BTreeMap iteration from lowest rank, remove up
    to `DRAIN_BATCH_CAP` = 8 sealed entries
  - `CohortBatch` allocation (Vec of `CohortBatchEntry`)
- **Expected cost at 10 files (1 cohort):** BTreeMap insert/remove
  (~0.3us) + PathBuf clone (~0.1us) + 10 DeleteOperation structs
  (~0.5us) + drain alloc (~0.2us) = ~1.1us
- **Expected cost at 100 files (5 cohorts):** 5 BTreeMap ops + 5
  PathBuf clones + 100 DeleteOperation structs + drain = ~7us
- **Nature:** constant per cohort (BTreeMap ops) + linear per item
  (DeleteOperation construction). At small scale, the per-cohort
  constant dominates.

### 2.5 Stage E: emitter dispatch (unlink execution)

- **Location:** `DeleteEmitter::run_plan` -> `dispatch_entry` ->
  `DeleteFs::unlink` / `DeleteFs::rmdir`
- **Work performed:**
  - Path join (`plan.directory.join(&entry.name)`)
  - `unlink(2)` or `rmdir(2)` syscall
  - Error classification
  - `DeleteStats` counter increment
- **Expected cost at 10 files:** 10 unlinks (~30us on ext4) + 10 path
  joins (~1us) = ~31us
- **Expected cost at 100 files:** 100 unlinks (~300us) + path joins
  (~10us) = ~310us
- **Nature:** per-item dominated (syscall cost is the floor)

### 2.6 Stage F: pipeline setup/teardown (constant overhead)

- **Location:** `DeleteContext::into_emitter` + channel drain +
  `DeleteEmitter` construction
- **Work performed:**
  - Drop the channel sender (signals EOF)
  - Drain all `CursorObservation` messages from the channel
  - Build `DirTraversalCursor` from observations
  - Construct `DeleteEmitter` struct (moves owned state)
  - Final `NDX_DEL_STATS` frame emission
- **Expected cost:** ~2-5us regardless of file count
- **Nature:** pure constant (one-time per transfer)

## 3. Measurement approach

### 3.1 Instrumentation via manual Instant pairs

Each stage boundary gets a pair of `std::time::Instant::now()` calls.
The instrumentation is compiled unconditionally but only activated by an
environment variable gate to avoid release-build overhead:

```rust
struct StageTimings {
    compute_extras_ns: u64,
    sort_and_publish_ns: u64,
    cursor_registration_ns: u64,
    reorder_buffer_ns: u64,
    emitter_dispatch_ns: u64,
    pipeline_setup_ns: u64,
}
```

Activation: `OC_RSYNC_PROFILE_DELETE=1` environment variable.

When active, each `DeleteContext` accumulates per-directory timings in a
`Vec<StageTimings>` and emits a summary to stderr at drain completion:

```
DML2_PROFILE: dirs=5 files=100
  compute_extras: total=305us mean=61us
  sort_publish:   total=3.5us mean=0.7us
  cursor_reg:     total=1.5us mean=0.3us
  reorder_buffer: total=7us   mean=1.4us
  emitter_unlink: total=310us mean=62us
  pipeline_setup: total=4us   (once)
  TOTAL:          total=631us
  overhead:       total=16us  (= total - compute_extras - emitter_unlink)
```

The "overhead" line isolates pipeline machinery cost from inherent work
(reading the directory and executing unlinks).

### 3.2 Why not tracing spans

Tracing spans (`tracing::info_span!`) add:
- Per-span allocation (~100-200ns with the default subscriber)
- Global subscriber dispatch overhead
- String formatting for span metadata

For microsecond-scale measurements (stages B-D at small scale), the
tracing overhead would perturb results. Manual `Instant::now()` pairs
add ~20ns per call on modern hardware - below the noise floor for any
stage measured here.

A tracing-based variant can be added later for production observability,
but the profiling instrument must use raw timestamps.

### 3.3 Accumulation strategy

Per-directory timings are accumulated in a pre-allocated
`Vec<StageTimings>` with capacity set to the expected directory count
(from the flist segment). This avoids reallocation during measurement.
The vec is local to the `DeleteContext` - no synchronization needed
since `compute_extras` results are published before the single-threaded
drain begins.

For stages that span the worker/consumer boundary (C and D), the worker
records its portion (channel send latency) and the consumer records its
portion (cursor build, BTreeMap ops) separately. The sum is reported
under the respective stage.

### 3.4 Clock source

`std::time::Instant` is monotonic and has nanosecond resolution on all
target platforms:
- Linux: `clock_gettime(CLOCK_MONOTONIC)` - ~20ns overhead
- macOS: `mach_absolute_time()` - ~30ns overhead
- Windows: `QueryPerformanceCounter` - ~15ns overhead

No platform-specific clock source is needed.

## 4. Small-directory scenarios

### 4.1 Scenario matrix

| ID | Files | Dirs | Layout | Description |
|----|-------|------|--------|-------------|
| S1 | 10    | 1    | flat   | Single directory, 10 files |
| S2 | 10    | 5    | nested | 5 directories, 2 files each |
| S3 | 50    | 1    | flat   | Single directory, 50 files |
| S4 | 50    | 10   | nested | 10 directories, 5 files each |
| S5 | 100   | 1    | flat   | Single directory, 100 files |
| S6 | 100   | 10   | nested | 10 directories, 10 files each |
| S7 | 100   | 50   | deep   | 50 directories, 2 files each |

### 4.2 Why nested layouts matter

The pipeline's per-directory constant overhead multiplies with directory
count. Scenario S2 (10 files / 5 dirs) exercises the per-cohort BTreeMap
and PathBuf-clone overhead 5x vs S1 (10 files / 1 dir) for the same
total file count. If the reorder buffer stage shows 5x cost in S2 vs S1,
it confirms the overhead is per-cohort rather than per-item.

Scenario S7 (100 files / 50 dirs) is the stress case: 50 cohorts with
only 2 items each means Stage D's per-cohort constant dominates over
its per-item cost. This is the exact pattern where the PDD pipeline is
most expensive relative to upstream's flat loop.

### 4.3 Fixture creation

Each scenario uses zero-byte files on tmpfs. The profiling harness
(`tools/bench/delete_profile.sh`) creates and tears down fixtures between
every measurement iteration:

```sh
# S4: 50 files across 10 directories
for d in $(seq 0 9); do
    mkdir -p "$ROOT/dst/dir_$d"
    for f in $(seq 0 4); do
        : > "$ROOT/dst/dir_$d/f_$f.dat"
    done
done
mkdir -p "$ROOT/src"  # empty source triggers full delete
```

### 4.4 Iteration count

Per-scenario: 5 warm-up + 51 measured runs. The high iteration count is
feasible because each run completes in under 5ms at these scales.
Reporting uses median and P10/P90 for each stage independently.

## 5. Expected overhead distribution

### 5.1 Predicted breakdown by scenario

Based on syscall costs (ext4, warm dentry cache) and data structure
operation costs:

| Scenario | compute_extras | sort_publish | cursor_reg | reorder_buf | emitter | setup | overhead% |
|----------|---------------|--------------|------------|-------------|---------|-------|-----------|
| S1 (10/1) | 31us | 0.5us | 0.3us | 1.1us | 31us | 4us | 9% |
| S2 (10/5) | 31us | 2.5us | 1.5us | 5.5us | 31us | 4us | 18% |
| S3 (50/1) | 155us | 2us | 0.3us | 3us | 155us | 4us | 3% |
| S4 (50/10) | 155us | 7us | 3us | 14us | 155us | 4us | 8% |
| S5 (100/1) | 310us | 3.5us | 0.3us | 6us | 310us | 4us | 2% |
| S6 (100/10) | 310us | 10us | 3us | 14us | 310us | 4us | 5% |
| S7 (100/50) | 310us | 25us | 15us | 55us | 310us | 4us | 15% |

Overhead% = (sort_publish + cursor_reg + reorder_buf + setup) /
(compute_extras + emitter_dispatch)

### 5.2 Key predictions

1. **compute_extras and emitter_dispatch dominate at all scales** - they
   contain the actual syscalls (`readdir`, `stat`, `unlink`). Pipeline
   overhead is always a minority fraction.

2. **Reorder buffer is the largest overhead component** - especially in
   many-directory scenarios (S2, S7) where per-cohort BTreeMap ops and
   PathBuf clones multiply with directory count.

3. **Pipeline setup is constant and small** - the ~4us one-time cost
   (channel drain + cursor build) is negligible even at 10-file scale.

4. **The crossover happens at the per-cohort level, not per-file** -
   overhead is primarily proportional to directory count, not file
   count. A directory with 2 files pays nearly the same pipeline
   overhead as a directory with 50 files.

5. **S7 is the pathological case** - 50 directories with 2 files each
   represents 50 BTreeMap insert/seal/drain cycles for work that
   upstream handles with 50 inline loops. The ~15% overhead here
   validates DML-3's per-directory threshold rather than a per-transfer
   threshold.

### 5.3 Overhead taxonomy

Two categories emerge:

- **One-time setup (O(1) per transfer):** pipeline_setup (channel drain,
  cursor build, emitter construction). Fixed ~4us.
- **Per-directory setup (O(D) where D = directory count):** BTreeMap
  insert + seal + drain, PathBuf clone for cohort key, Vec allocation
  for DeleteOperation batch. ~1-2us per directory.
- **Per-item processing (O(N) where N = file count):** DeleteOperation
  struct construction, Vec push. ~50ns per item.

At small scale, the per-directory cost dominates overhead because D is
a significant fraction of N.

## 6. Upstream comparison (strace-based)

### 6.1 Upstream rsync's delete path

Upstream rsync (`generator.c:272-347`) performs deletes in a tight loop:

```c
// Simplified upstream delete_in_dir:
for (i = dirlist->used; i > 0; ) {
    struct file_struct *fp = dirlist->files[--i];
    if (flist_find(cur_flist, fp) >= 0) continue;  // exists in source
    delete_item(fbuf, fp->mode);  // unlink/rmdir
}
```

No buffering, no reordering, no allocation beyond the stack-local
`fbuf`. The per-directory cost is: flist_find (binary search, O(log N))
+ delete_item (unlink syscall).

### 6.2 Strace measurement protocol

```sh
# Capture upstream rsync syscall trace for delete phase
strace -e trace=unlink,unlinkat,rmdir,openat,getdents64 \
    -T -o /tmp/upstream_strace.log \
    rsync -a --delete-before src/ dst/

# Extract per-syscall wall-clock from -T output
grep -E '^(unlink|rmdir)' /tmp/upstream_strace.log | \
    awk -F'<|>' '{sum += $2} END {printf "total_unlink_us=%.0f\n", sum*1e6}'
```

The strace captures:
- `openat` + `getdents64`: directory scan (equivalent to our
  compute_extras Stage A)
- `unlinkat` / `rmdir`: actual deletes (equivalent to our emitter
  Stage E)

There are no intervening syscalls between directory scan and unlinks in
upstream. Any syscalls that oc-rsync adds between these two phases
represent measurable pipeline overhead.

### 6.3 Expected strace differential

For scenario S4 (50 files / 10 dirs), upstream's strace shows:

```
10x openat (one per directory)
10x getdents64 (one per directory)
50x unlinkat (one per file)
```

oc-rsync's strace for the same scenario adds:
- No extra syscalls from Stages B-D (pure userspace computation)
- The same 10x openat + 10x getdents64 + 50x unlinkat

The overhead is entirely userspace CPU time (data structure operations),
not additional syscalls. This means `strace -T` captures identical
syscall profiles - the difference is only visible via wall-clock timing
or CPU profiling (perf stat / DHAT).

### 6.4 CPU cycle measurement

For sub-microsecond stage resolution, `perf stat` provides cycle counts:

```sh
perf stat -e cycles,instructions,cache-misses \
    env OC_RSYNC_PROFILE_DELETE=1 \
    ./target/release/oc-rsync -a --delete-before src/ dst/
```

Additionally, DHAT profiling (via `tools/dhat-profile/`) can attribute
heap allocations to specific stages, confirming that PathBuf clones and
Vec allocations in Stages B-D are the allocation hotspots.

## 7. Actionable output

### 7.1 Setup vs per-item classification

The profiling output classifies each stage as:

- **Setup cost (one-time per directory):** amortized across all files in
  that directory. Includes BTreeMap insert, seal, PathBuf clone, Vec
  allocation.
- **Per-item cost (scales with file count):** includes DeleteOperation
  construction, Vec push, sort comparison.

The ratio `setup_cost / per_item_cost` determines whether overhead is
constant-factor or scaling. If setup >> per_item at small scale, the
DML-3 fast path (which eliminates setup entirely) captures nearly all
the overhead.

### 7.2 Decision matrix

| Measured result | Action |
|----------------|--------|
| Stage D (reorder) > 50% of overhead | Fast path justified; threshold = point where Stage D cost < 1 unlink |
| Stage B (sort) > 50% of overhead | Consider lazy sorting (sort only when emitter drains, not at publish) |
| Stage C (cursor) > 50% of overhead | Consider pre-built cursor for small transfers |
| Stage F (setup) > 50% of overhead | Consider lazy emitter construction |
| Overhead < 5% at all scenarios | Fast path unnecessary; close DML-3 as wontfix |

### 7.3 Threshold calibration

The profiling data feeds directly into DML-3's `FAST_PATH_EXTRAS_THRESHOLD`:

```
threshold = argmin_{T} [
    for all scenarios where extras <= T:
        sum(sort_publish + cursor_reg + reorder_buffer) > unlink_cost * 0.1
]
```

In words: the threshold is the largest extras count where pipeline
overhead exceeds 10% of the inherent unlink cost. Below this threshold,
the fast path eliminates > 10% overhead - above it, the pipeline's
amortization makes the overhead negligible.

Current prediction: threshold = 64 (matching DML-3's design). Profiling
will confirm or revise this value.

### 7.4 Regression detection

Once profiling establishes baseline per-stage costs, future changes to
the delete pipeline can be validated against these baselines:

- Stage D (reorder buffer): any change to `ReorderBuffer` or
  `CohortBatcher` should not increase per-cohort cost beyond the
  measured baseline + 20%.
- Stage B (sort): changes to `DeletePlan::sort_by_name` or
  `DeletePlanMap::publish` should not regress.
- New stages: any new pipeline stage must be profiled and its overhead
  justified against the measured baseline.

## 8. Connection to DML-3 fast-path

### 8.1 What DML-2 validates

DML-3's fast-path design makes specific predictions about overhead
distribution:

1. "BTreeMap insert/lookup per operation is non-trivial for a handful of
   entries" - DML-2 Stage D measurements confirm or refute this.
2. "PathBuf cloning for DeleteCohortKey construction" adds cost - DML-2
   Stage B/D measurements quantify this.
3. "Two-map bookkeeping (by_key + cohorts)" adds cost - DML-2 Stage D
   measurements capture this.

### 8.2 Threshold justification evidence

The fast path is justified when DML-2 shows:

```
For extras <= 64:
    overhead_total > 0.10 * inherent_cost

where:
    overhead_total = sort_publish + cursor_reg + reorder_buffer + setup
    inherent_cost = compute_extras + emitter_dispatch
```

If this inequality holds for measured data, the fast path eliminates
meaningful overhead. If the inequality does not hold (overhead < 10%),
the fast path is premature optimization and DML-3 should be deprioritized.

### 8.3 Threshold revision protocol

If profiling reveals the crossover point is not at 64:

- If crossover < 64: tighten threshold (e.g., to 32). Fewer directories
  take the fast path, but the fast path is only applied where its benefit
  exceeds measurement noise.
- If crossover > 64: loosen threshold (e.g., to 128). More directories
  benefit from the fast path, but the DML-3 implementation must handle
  larger sequential loops.
- If no clear crossover exists (overhead is always < 5%): close DML-3
  as unnecessary.

## 9. Profiling script

### 9.1 Location

`tools/bench/delete_profile.sh` - self-contained script that:

1. Builds oc-rsync in release mode with `OC_RSYNC_PROFILE_DELETE`
   instrumentation compiled in.
2. Iterates through scenarios S1-S7.
3. For each scenario: creates fixture, runs warm-up (5 iterations),
   runs measured iterations (51), captures stderr profile output.
4. Parses `DML2_PROFILE` lines and computes per-stage median/P10/P90.
5. Outputs a summary table and CSV.

### 9.2 Output format

```
=== DML-2: Delete pipeline stage profile ===

Scenario S1 (10 files / 1 dir / flat):
  Stage              Median    P10      P90
  compute_extras     32us      28us     38us
  sort_publish       0.5us     0.3us    0.8us
  cursor_reg         0.3us     0.2us    0.5us
  reorder_buffer     1.2us     0.9us    1.6us
  emitter_dispatch   30us      26us     35us
  pipeline_setup     4us       3us      5us
  ---
  overhead_total     6us (9.7% of inherent work)

Scenario S7 (100 files / 50 dirs / deep):
  Stage              Median    P10      P90
  compute_extras     315us     290us    340us
  sort_publish       26us      22us     31us
  cursor_reg         16us      13us     19us
  reorder_buffer     58us      50us     68us
  emitter_dispatch   305us     280us    330us
  pipeline_setup     4us       3us      6us
  ---
  overhead_total     104us (16.8% of inherent work)

Summary:
  Dominant overhead stage: reorder_buffer (55% of total overhead)
  Overhead type: per-directory (correlates with dir count, not file count)
  DML-3 threshold validation: 64 extras is CONFIRMED
    (overhead > 10% at all scenarios with >10 dirs)
```

### 9.3 CSV output

```csv
scenario,files,dirs,layout,compute_extras_us,sort_publish_us,cursor_reg_us,reorder_buffer_us,emitter_dispatch_us,pipeline_setup_us,overhead_pct
S1,10,1,flat,32,0.5,0.3,1.2,30,4,9.7
S2,10,5,nested,32,2.5,1.5,5.5,30,4,21.8
S3,50,1,flat,155,2,0.3,3,155,4,3.0
S4,50,10,nested,155,7,3,14,155,4,8.8
S5,100,1,flat,310,3.5,0.3,6,310,4,2.2
S6,100,10,nested,310,10,3,14,310,4,5.0
S7,100,50,deep,315,26,16,58,305,4,16.8
```

## 10. Implementation plan

| Task | Description | Effort |
|------|-------------|--------|
| DML-2.a | Add `StageTimings` struct and `OC_RSYNC_PROFILE_DELETE` gate to `DeleteContext` | S |
| DML-2.b | Instrument Stage A (compute_extras entry/exit) | S |
| DML-2.c | Instrument Stage B (sort_by_name + publish) | S |
| DML-2.d | Instrument Stage C (cursor observation send + build) | S |
| DML-2.e | Instrument Stage D (enqueue_cohort + drain_batch) | M |
| DML-2.f | Instrument Stage E (emitter dispatch loop) | S |
| DML-2.g | Instrument Stage F (into_emitter setup) | S |
| DML-2.h | Summary emission to stderr with CSV format | S |
| DML-2.i | `tools/bench/delete_profile.sh` script | M |
| DML-2.j | Run profiling, document results, confirm/revise DML-3 threshold | M |

## 11. Acceptance criteria

DML-2 is complete when:

1. Per-stage timings are captured for all seven scenarios with < 5%
   measurement perturbation (verified by comparing instrumented vs
   non-instrumented total wall-clock: difference < 5%).
2. The dominant overhead stage is identified with > 80% confidence
   (median contribution exceeds the second-largest by 2x+).
3. The DML-3 threshold (64 extras) is confirmed or revised with measured
   evidence.
4. Results are reproducible: re-running the profiling script on the same
   hardware produces per-stage medians within +/- 15% of the initial run.
5. Profiling instrumentation has zero overhead when
   `OC_RSYNC_PROFILE_DELETE` is unset (no Instant::now calls, no Vec
   allocation).

## 12. Limitations

- **Userspace-only profiling.** Kernel time within `unlink(2)` is not
  decomposed. If filesystem journal commit or inode deallocation varies
  between runs, it shows as noise in Stages A and E, not as a separate
  measured cost.

- **Single-threaded measurement.** The profiling measures the sequential
  emitter path. The parallel-delete-consumer path (Stage D with Condvar
  wake-ups) has additional synchronization overhead not captured here.

- **Hardware sensitivity.** BTreeMap and HashMap performance depends on
  CPU cache hierarchy. Results from a CI runner (shared L3, NUMA effects)
  may differ from developer workstations (dedicated L3, uniform memory).
  The profiling script records CPU model for reproducibility context.

- **No macOS/Windows coverage.** Unlink latency characteristics differ
  across platforms (APFS metadata journaling, NTFS MFT updates). The
  overhead ratios may shift on non-Linux platforms even though the
  userspace computation cost is identical.

## 13. Cross-references

- DML-1 (latency bench): `docs/design/delete-before-latency-bench.md`
- DML-3 (fast-path design): `docs/design/delete-small-dir-fast-path.md`
- DML-3 (fast-path impl): `docs/design/delete-small-dir-fast-path-impl.md`
- PDD architecture: `docs/design/parallel-deterministic-delete.md`
- DEL-1.b reorder buffer: `docs/design/del-1b-reordering-buffer.md`
- DEL-1.c cohort batching: `docs/design/del-1c-cohort-batching-strategy.md`
- ReorderBuffer source: `crates/engine/src/delete/reorder_buffer.rs`
- CohortBatcher source: `crates/engine/src/delete/cohort_batcher.rs`
- DeletePlanMap source: `crates/engine/src/delete/plan_map.rs`
- DeleteEmitter source: `crates/engine/src/delete/emitter/mod.rs`
- compute_extras source: `crates/engine/src/delete/extras.rs`
- DeleteContext source: `crates/engine/src/delete/context/core.rs`
