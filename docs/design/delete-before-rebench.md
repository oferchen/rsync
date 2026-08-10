# --delete-before rebench design (DML-5)

Status: Design (task DML-5, #3310)
Audience: engine, CI, and performance maintainers
Scope: re-run the DML-1 latency benchmark after DML-4's small-directory
fast path lands, validate latency improvements at small scale, and
confirm no regressions at large scale.

Depends on: DML-1 (latency bench, merged PR #5258), DML-4 (fast-path
implementation, merged PR #5249).
Feeds into: DML-6 architecture doc (close-out section), release notes.

Out of scope: transfer throughput, --delete-during timing, parallel
consumer tuning, or threshold value changes (those follow if this bench
shows the fast path is insufficient).

## 1. Relationship to DML-1 baseline

DML-1 established the pre-fast-path baseline: wall-clock latency of
--delete-before at five scales (10, 100, 1K, 10K, 100K files) comparing
oc-rsync against upstream rsync 3.4.1. DML-5 reruns the identical
benchmark under the same conditions to produce a direct comparison.

### 1.1 Shared infrastructure

DML-5 reuses DML-1 without modification:

- **Same fixture layout.** `tools/bench/delete_before_latency_fixture.sh`
  with identical directory fan-out (ceil(sqrt(N)) dirs, ceil(N/dirs)
  files per dir).
- **Same measurement script.** `tools/bench/delete_before_latency.sh`
  with identical warm-up/iteration counts, startup subtraction, and
  environment controls.
- **Same instrumentation.** `OC_RSYNC_BENCH_DELETE=1` internal timestamps
  plus external wall-clock.
- **Same upstream binary.** rsync 3.4.1 built from source via
  `tools/ci/build_upstream_rsync.sh`.
- **Same environment.** `RAYON_NUM_THREADS=4`, `LANG=C`, taskset where
  available.

### 1.2 Comparison against DML-1 numbers

DML-5 produces a three-column comparison:

| Metric | Source |
|--------|--------|
| Upstream rsync 3.4.1 | Re-measured (validates runner consistency) |
| oc-rsync pre-DML-4 | DML-1 historical CSV (artifact from PR #5258 CI) |
| oc-rsync post-DML-4 | Fresh measurement (this bench) |

The pre-DML-4 column uses stored DML-1 results rather than rebuilding
old code. If the upstream column deviates > 10% from DML-1's upstream
numbers, the runner environment has changed and historical comparison is
invalid - the script emits a warning and uses only the current-run
upstream numbers as the reference.

## 2. What changed: DML-4 fast path

DML-4 added a runtime branch in `DeleteContext::emit_all` (and
`emit_one`) that bypasses the full cohort/reorder pipeline when the
total extras count across all directories falls below
`SMALL_DIR_FAST_PATH_THRESHOLD` (64):

```
if total_extras < 64 || rayon::current_num_threads() < 2 {
    // Sequential DeleteEmitter directly - no CohortBatcher,
    // no ReorderBuffer, no Condvar wake-up, no slot management.
    emitter.emit_all()
} else {
    // Full parallel pipeline via CohortBatcher + ReorderBuffer.
    emit_parallel_from_parts(...)
}
```

### 2.1 Overhead eliminated for small directories

At scales below 64 total extras, the fast path avoids:

- `CohortBatcher` allocation and teardown (~15 us)
- `ReorderBuffer` BTreeMap insert/seal/drain cycle (~20 us)
- `Condvar` + slot allocation in `ParallelDeleteEmitter` (~15 us)
- Per-cohort `Vec<DeleteOperation>` allocation (~5 us per directory)
- Rank-monotonicity validation bookkeeping (~2 us)

Total pipeline overhead eliminated: ~50-60 us constant + ~5 us per
directory with plans.

### 2.2 Preserved at large scale

Directories with >= 64 total extras continue through the full parallel
pipeline. The fast path has zero cost for these transfers (a single
integer comparison after plan extraction).

## 3. Expected improvements by scale

### 3.1 Scale: 10 files

- **DML-1 baseline ratio:** 1.5-2.5x upstream (pipeline overhead
  dominates unlink cost of ~30 us total)
- **Expected post-DML-4 ratio:** 0.95-1.10x upstream
- **Improvement magnitude:** 40-60% latency reduction
- **Fast-path active:** Yes (10 < 64 threshold)
- **Rationale:** With pipeline overhead (~50 us) removed, the remaining
  work is 10 unlinks + fixture scan - functionally identical to
  upstream's inline `delete_in_dir`. Process startup noise (~1-3 ms)
  becomes the dominant measurement uncertainty, making the ratio
  approach 1.0.

### 3.2 Scale: 100 files

- **DML-1 baseline ratio:** 1.2-1.8x upstream
- **Expected post-DML-4 ratio:** 1.0-1.15x upstream
- **Improvement magnitude:** 15-35% latency reduction
- **Fast-path active:** Depends on layout. With the DML-1 fixture
  (10 dirs, 10 files/dir), each directory has exactly 10 extras. The
  total is 100 >= 64, so the parallel path runs. However, the per-dir
  cost is already low and the rayon thread pool amortizes across 10
  directories.
- **Note:** If the fixture's total extras (100) exceeds the threshold,
  the improvement at this scale comes from general pipeline maturation
  rather than the fast path. See section 4 for the A/B comparison that
  isolates the fast-path contribution.

### 3.3 Scale: 1K files

- **DML-1 baseline ratio:** 1.05-1.20x upstream
- **Expected post-DML-4 ratio:** 1.05-1.20x upstream (unchanged)
- **Improvement magnitude:** None expected
- **Fast-path active:** No (1,024 >> 64)
- **Rationale:** At 1K+ files, unlink syscall time dominates and the
  parallel pipeline's overhead is amortized. No change expected.

### 3.4 Scale: 10K files

- **DML-1 baseline ratio:** 0.95-1.10x upstream
- **Expected post-DML-4 ratio:** 0.95-1.10x upstream (unchanged)
- **Fast-path active:** No
- **Rationale:** Parallel plan compute overlaps with emitter drain.
  No regression expected from the fast-path branch (single integer
  comparison cost is negligible).

### 3.5 Scale: 100K files

- **DML-1 baseline ratio:** 0.80-1.05x upstream
- **Expected post-DML-4 ratio:** 0.80-1.05x upstream (unchanged)
- **Fast-path active:** No
- **Rationale:** Rayon parallelism pays off at scale. The fast-path
  check adds one `usize` comparison - unmeasurable overhead.

## 4. A/B comparison: fast-path enabled vs disabled

To isolate the fast-path contribution from other changes between DML-1
and DML-5 runs (compiler upgrades, dependency updates, unrelated
optimizations), DML-5 includes a controlled A/B comparison.

### 4.1 Method

Run the benchmark twice per scale:

1. **Arm A (default):** Standard release build with
   `SMALL_DIR_FAST_PATH_THRESHOLD = 64` (the production value).
2. **Arm B (disabled):** Same release build with the fast-path threshold
   forced to 0 via environment variable override:
   `OC_RSYNC_DELETE_FAST_PATH_THRESHOLD=0`

The environment-variable override is implemented in DML-4 as a
development/bench knob:

```rust
let threshold = std::env::var("OC_RSYNC_DELETE_FAST_PATH_THRESHOLD")
    .ok()
    .and_then(|v| v.parse::<usize>().ok())
    .unwrap_or(SMALL_DIR_FAST_PATH_THRESHOLD);
```

Setting threshold to 0 guarantees `total_extras < 0` is never true,
forcing all transfers through the full parallel pipeline regardless of
size.

### 4.2 Output format

The bench script adds columns for the A/B comparison:

```
scale,oc_default_ms,oc_nofastpath_ms,upstream_ms,ratio_default,ratio_nofastpath,improvement_pct
10,1.2,2.4,1.1,1.09,2.18,50.0
100,3.8,4.5,3.2,1.19,1.41,15.6
1000,18.0,18.1,15.4,1.17,1.18,0.6
10000,141.0,141.5,128.5,1.10,1.10,0.4
100000,1375.0,1378.0,1290.0,1.07,1.07,0.2
```

The `improvement_pct` column shows the latency reduction attributable
solely to the fast path: `(nofastpath - default) / nofastpath * 100`.

### 4.3 Validation

The A/B comparison validates that:

1. **Fast-path helps at small scale.** `improvement_pct > 20%` at
   scale 10 confirms the pipeline bypass provides real benefit.
2. **Fast-path is neutral at large scale.** `improvement_pct < 2%` at
   scales 1K+ confirms no interference with the parallel pipeline.
3. **No regression from the branch.** Arm B's ratio against upstream
   should match DML-1's baseline within measurement noise (< 5%
   deviation).

## 5. Pass criteria

### 5.1 Primary pass criteria

| Scale | Maximum ratio (oc-rsync / upstream) | Rationale |
|-------|-------------------------------------|-----------|
| 10    | <= 1.10                             | Fast-path eliminates pipeline overhead; remaining gap is startup noise |
| 100   | <= 1.15                             | Fast path may not fire (100 >= 64); moderate improvement from pipeline maturation |
| 1K    | <= 1.20                             | Unchanged from DML-1 acceptance threshold |
| 10K   | <= 1.15                             | Unchanged from DML-1 |
| 100K  | <= 1.10                             | Unchanged from DML-1 |

### 5.2 Target criteria (stretch goals)

| Scale | Target ratio | Justification |
|-------|--------------|---------------|
| 10    | <= 1.05      | Fast path + minimal startup overhead |
| 100   | <= 1.05      | Fast path fires or pipeline is well-amortized |

### 5.3 A/B improvement criteria

| Scale | Minimum improvement_pct |
|-------|------------------------|
| 10    | >= 30%                 |
| 100   | >= 10% (if fast-path fires) or >= 0% (if threshold exceeded) |
| 1K+   | No requirement (fast path inactive) |

### 5.4 Non-regression criteria

No scale's ratio may increase more than 5% relative to DML-1 baseline.
If any scale regresses beyond 5%, the result is FAIL regardless of
whether it meets the absolute threshold.

## 6. Failure investigation targets

If the primary pass criteria are not met, investigate in this order:

### 6.1 Scale 10 fails (ratio > 1.10)

1. **Threshold too low.** Check whether the fixture's total extras
   (10 files across 4 directories = 10 total) correctly triggers the
   fast path. If the threshold check has a bug (e.g., counts directories
   instead of files), the parallel path runs unnecessarily.

2. **Startup overhead.** Compare internal timestamp
   (`DML1_DELETE_NS`) against wall-clock. If internal time is < 0.5 ms
   but wall-clock shows > 1.5 ms, the gap is process initialization
   (arg parsing, config validation, flist load) not delete-phase
   overhead. Mitigation: use internal timestamps as primary metric
   rather than wall-clock.

3. **Per-item emitter overhead.** Profile `DeleteEmitter::emit_entry`
   at scale 10. If per-item cost exceeds 10 us (vs upstream's ~3 us
   per unlink), there is unnecessary work per entry (e.g., PathBuf
   allocation, itemize formatting, or MSG_DELETED frame construction).

4. **Fixture scan overhead.** The fast path still calls
   `compute_extras` which performs `read_dir` + stat per entry. If this
   phase alone takes > 50% of total time, the bottleneck is pre-delete
   computation, not the pipeline.

### 6.2 Scale 100 fails (ratio > 1.15)

1. **Threshold boundary.** The 100-file fixture has total extras = 100
   which exceeds the 64 threshold. If improvement is needed at this
   scale, consider raising `SMALL_DIR_FAST_PATH_THRESHOLD` to 128 or
   switching to a per-directory threshold (each dir has 10 extras,
   well below 64).

2. **Per-directory threshold variant.** DML-3 specified both per-total
   and per-directory threshold modes. If the per-total threshold at 64
   misses the 100-file case, a per-directory check (each dir's extras
   < threshold) would cover it because individual directories are small.

3. **Cohort batch size.** If the parallel path runs and its overhead is
   the bottleneck, check whether the `CohortBatcher` batch size can be
   tuned for the 10-dir case (fewer cohort seal cycles).

### 6.3 Large-scale regression (1K+ ratio increases > 5%)

1. **Fast-path branch cost.** Unlikely (single comparison), but verify
   that `into_drain_parts` is not called twice or that the plan map
   iteration for `total_extras_count()` is not inadvertently expensive.

2. **Compiler regression.** Compare with the same commit built on the
   DML-1 compiler version. If the regression vanishes, it is a compiler
   issue unrelated to DML-4.

3. **Runner variance.** Compare upstream column against DML-1 upstream
   numbers. If upstream also regressed proportionally, the runner
   environment changed (different hardware, higher co-tenant load).

## 7. CI integration

### 7.1 Workflow extension

The existing `bench-delete-latency.yml` workflow (created for DML-1) is
extended with a DML-5 comparison job:

```yaml
jobs:
  delete-latency-rebench:
    runs-on: ubuntu-latest
    timeout-minutes: 20
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo build --release

      - run: bash tools/ci/build_upstream_rsync.sh 3.4.1

      # Arm A: default (fast-path enabled)
      - run: |
          bash tools/bench/delete_before_latency.sh \
              --oc-rsync ./target/release/oc-rsync \
              --upstream ./target/interop/upstream-bin/rsync-3.4.1 \
              --root /tmp/dml5-a \
              --output /tmp/dml5-arm-a.csv
        env:
          OC_RSYNC_BENCH_DELETE: "1"
          RAYON_NUM_THREADS: "4"

      # Arm B: fast-path disabled (threshold=0)
      - run: |
          bash tools/bench/delete_before_latency.sh \
              --oc-rsync ./target/release/oc-rsync \
              --upstream ./target/interop/upstream-bin/rsync-3.4.1 \
              --root /tmp/dml5-b \
              --output /tmp/dml5-arm-b.csv
        env:
          OC_RSYNC_BENCH_DELETE: "1"
          OC_RSYNC_DELETE_FAST_PATH_THRESHOLD: "0"
          RAYON_NUM_THREADS: "4"

      # Compare against DML-1 baseline
      - run: |
          python3 tools/bench/dml5_compare.py \
              --arm-a /tmp/dml5-arm-a.csv \
              --arm-b /tmp/dml5-arm-b.csv \
              --baseline /tmp/dml1-baseline.csv \
              --output /tmp/dml5-report.md
        continue-on-error: true

      - uses: actions/upload-artifact@v4
        with:
          name: dml5-results
          path: |
            /tmp/dml5-arm-a.csv
            /tmp/dml5-arm-b.csv
            /tmp/dml5-report.md
```

### 7.2 Baseline comparison script

`tools/bench/dml5_compare.py` performs:

1. Load Arm A (fast-path enabled) and Arm B (fast-path disabled) CSVs.
2. Load DML-1 baseline CSV (downloaded from artifacts or checked into
   `tools/bench/baselines/dml1-baseline.csv`).
3. Compute per-scale:
   - `ratio_default = arm_a_median / upstream_median`
   - `ratio_nofastpath = arm_b_median / upstream_median`
   - `improvement_pct = (arm_b_median - arm_a_median) / arm_b_median * 100`
   - `regression_vs_baseline = (arm_a_ratio - dml1_ratio) / dml1_ratio * 100`
4. Apply pass/fail criteria from section 5.
5. Output a markdown report suitable for PR comment posting.

### 7.3 Advisory PR comments

When triggered on a pull request, the workflow posts a summary comment
using `actions/github-script`:

```
## DML-5: --delete-before rebench results

| Scale | Ratio (default) | Ratio (no fast-path) | Improvement | vs DML-1 | Verdict |
|-------|-----------------|---------------------|-------------|----------|---------|
| 10    | 1.05            | 2.15                | 51%         | -48%     | PASS    |
| 100   | 1.12            | 1.42                | 21%         | -23%     | PASS    |
| 1K    | 1.17            | 1.18                | 0.8%        | -1%      | PASS    |
| 10K   | 1.10            | 1.10                | 0.0%        | +0.5%   | PASS    |
| 100K  | 1.07            | 1.07                | 0.0%        | +0.2%   | PASS    |

Fast-path target met: 10-file scale <= 1.10x upstream.
```

### 7.4 Failure policy

The workflow remains advisory (non-required check). Rationale:

- CI runner filesystem performance is variable.
- The benchmark measures microsecond-scale improvements that are within
  noise margins on shared infrastructure.
- A persistent failure (3+ consecutive runs exceeding thresholds)
  warrants investigation; a single failure does not block merge.

### 7.5 Trigger paths

The workflow triggers on changes to:

- `crates/engine/src/delete/**` (delete module changes)
- `crates/engine/src/local_copy/deletion/**` (deletion executor)
- `tools/bench/delete_before_latency*` (bench infrastructure)
- `tools/bench/dml5_compare.py` (comparison script)

## 8. Execution plan

### 8.1 Prerequisites

1. DML-4 implementation is merged and passing CI.
2. DML-1 baseline CSV exists as a CI artifact or checked-in baseline.
3. Upstream rsync 3.4.1 binary is buildable via existing CI infra.

### 8.2 Steps

1. Run DML-1 bench script unchanged with the DML-4 build (Arm A).
2. Run DML-1 bench script with `OC_RSYNC_DELETE_FAST_PATH_THRESHOLD=0`
   (Arm B).
3. Parse both CSVs and compare against DML-1 historical numbers.
4. Apply pass criteria. Document results.
5. If pass: close DML-5, update DML-6 architecture doc with final
   performance numbers.
6. If fail: open follow-up investigation per section 6 priorities.

### 8.3 Timeline

DML-5 is a measurement task with no code changes (beyond optional CI
workflow additions). Expected effort: 1-2 hours of bench execution and
result analysis once DML-4 is merged.

## 9. Cross-references

| Document | Relevance |
|----------|-----------|
| `docs/design/delete-before-latency-bench.md` (DML-1) | Baseline bench design, fixture layout, methodology |
| `docs/design/delete-cohort-reorder-profile.md` (DML-2) | Pipeline stage breakdown, overhead quantification |
| `docs/design/delete-small-dir-fast-path.md` (DML-3) | Threshold justification, fast-path design |
| `docs/design/delete-small-dir-fast-path-impl.md` (DML-4) | Implementation plan, bypass logic, wire parity |
| `docs/design/del-4c-delete-threshold-decision.md` | Threshold value rationale and crossover analysis |
| `crates/engine/src/delete/context/core.rs` | Fast-path dispatch logic |
| `tools/bench/delete_before_latency.sh` | Bench script (shared with DML-1) |
