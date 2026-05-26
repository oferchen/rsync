# Parallel delete threshold decision framework (DEL-4.c)

Status: Design (task DEL-4.c; synthesizes bench results from DEL-4.a
100K and DEL-4.b 1M harnesses; decides the default-on threshold for
`parallel-delete-consumer` feature; depends on DEL-3.a/b/c wire-byte
parity passing and DEL-4.a/b bench data being collected)
Audience: engine, transfer, and release maintainers deciding when and
how to flip the `parallel-delete-consumer` feature to default-on.
Scope: result synthesis framework, decision criteria, scale-dependent
gating, wire-byte parity prerequisites, rollback criteria, and the
concrete implementation path for the default flip.

Out of scope: the benchmark harness implementations (DEL-4.a, DEL-4.b),
the wire-byte parity tests (DEL-3), the parallel consumer itself
(DEL-2.c), and filesystem-level tuning (tmpfs vs ext4 effects).

## 1. Bench result synthesis framework

### 1.1 Metrics under comparison

Four metrics determine whether the parallel consumer is ready for
default-on promotion. Each metric is collected at both the 100K
(DEL-4.a) and 1M (DEL-4.b) file scales.

| Metric | Source (100K) | Source (1M) | Why it matters |
|--------|--------------|-------------|----------------|
| **Wall-clock** | criterion `ns/iter` | criterion `seconds/iter` | Primary speedup signal |
| **CPU utilization** | derived: `seq_time / (par_time * threads)` | same | Detects lock contention and scheduling overhead |
| **Peak RSS** | not measured (negligible at 100K) | `/proc/self/status` VmHWM via monitor thread | Memory budget compliance (< 10% vs upstream target) |
| **Syscall count** | `strace -c` profiling mode | same | Validates that parallelism does not inflate per-file syscall overhead |

Wall-clock and CPU utilization are the decision-driving metrics.
Peak RSS is a veto metric - it can block promotion but cannot by itself
justify it. Syscall count is advisory - deviations signal implementation
bugs rather than architectural problems.

### 1.2 Result table format

Bench results are recorded in a structured table that covers the full
comparison matrix. One table per scale:

```
Scale: 100K files (DEL-4.a)
Date: YYYY-MM-DD
Runner: ubuntu-latest (2-core), ext4 /tmp

| Scenario         | Topology | Extras | Threads | Seq (ops/s) | Par (ops/s) | Speedup | CPU util |
|------------------|----------|--------|---------|-------------|-------------|---------|----------|
| flat_100k        | Flat     | 10%    | 4       | ...         | ...         | ...x    | ...%     |
| nested_100k      | Nested   | 10%    | 4       | ...         | ...         | ...x    | ...%     |
| mixed_types_100k | Mixed    | 10%    | 4       | ...         | ...         | ...x    | ...%     |
| flat_full_delete | Flat     | 100%   | 4       | ...         | ...         | ...x    | ...%     |
| scaling_1t       | Flat     | 100%   | 1       | ...         | ...         | ...x    | ...%     |
| scaling_2t       | Flat     | 100%   | 2       | ...         | ...         | ...x    | ...%     |
| scaling_4t       | Flat     | 100%   | 4       | ...         | ...         | ...x    | ...%     |
| scaling_8t       | Flat     | 100%   | 8       | ...         | ...         | ...x    | ...%     |
| scaling_16t      | Flat     | 100%   | 16      | ...         | ...         | ...x    | ...%     |
```

```
Scale: 1M files (DEL-4.b)
Date: YYYY-MM-DD
Runner: ubuntu-latest (2-core), tmpfs

| Scenario            | Layout    | Threads | Seq (ops/s) | Par (ops/s) | Speedup | Peak RSS (seq) | Peak RSS (par) | RSS delta |
|---------------------|-----------|---------|-------------|-------------|---------|----------------|----------------|-----------|
| flat_1m_t4          | Flat      | 4       | ...         | ...         | ...x    | ... MiB        | ... MiB        | ...%      |
| flat_1m_t16         | Flat      | 16      | ...         | ...         | ...x    | ... MiB        | ... MiB        | ...%      |
| deep_1m_t4          | Deep      | 4       | ...         | ...         | ...x    | ... MiB        | ... MiB        | ...%      |
| deep_1m_t16         | Deep      | 16      | ...         | ...         | ...x    | ... MiB        | ... MiB        | ...%      |
| realistic_1m_t4     | Realistic | 4       | ...         | ...         | ...x    | ... MiB        | ... MiB        | ...%      |
| realistic_1m_t16    | Realistic | 16      | ...         | ...         | ...x    | ... MiB        | ... MiB        | ...%      |
| realistic_1m_t64    | Realistic | 64      | ...         | ...         | ...x    | ... MiB        | ... MiB        | ...%      |
```

### 1.3 Cross-scale comparison

A third table links 100K and 1M results to detect non-linear scaling:

```
| Metric                          | 100K (flat, 4t) | 1M (flat, 4t) | 1M/100K ratio | Expected (linear) |
|---------------------------------|-----------------|---------------|---------------|--------------------|
| Sequential throughput (ops/s)   | ...             | ...           | ...x          | 1.0x               |
| Parallel throughput (ops/s)     | ...             | ...           | ...x          | 1.0x               |
| Parallel speedup factor         | ...             | ...           | ...x          | ~1.0x              |
| Peak RSS (parallel)             | n/a             | ... MiB       | n/a           | ~10x of 100K est.  |
```

If sequential throughput at 1M is less than 0.8x the 100K rate, the
pipeline has a super-linear scaling problem (likely `DeletePlanMap`
lookup cost growing with entry count). If the parallel speedup factor
at 1M is materially higher than at 100K, that strengthens the case for
a scale-dependent default (section 3).

### 1.4 Upstream rsync comparison (informational)

When DEL-4.b's profiling mode (`DEL_BENCH_UPSTREAM=1`) is active, a
fourth row captures upstream rsync 3.4.1's delete throughput and RSS
for the same fixture via hyperfine. This comparison is never a gate -
it provides context for roadmap prioritization:

```
| Metric           | oc-rsync seq | oc-rsync par (4t) | upstream 3.4.1 | oc-rsync vs upstream |
|------------------|-------------|-------------------|----------------|----------------------|
| Throughput       | ... ops/s   | ... ops/s         | ... ops/s      | ...x                 |
| Peak RSS         | ... MiB     | ... MiB           | ... MiB        | ...x                 |
```

## 2. Decision criteria for default-on

### 2.1 Required gates (all must pass)

The feature flips to default-on only when every gate in this table is
satisfied:

| # | Gate | Threshold | Rationale |
|---|------|-----------|-----------|
| G1 | **Wire-byte parity** | DEL-3.a/b/c pass on Linux, macOS, Windows at thread widths 1, 2, 4, host-natural | Non-negotiable correctness prerequisite |
| G2 | **Parallel speedup at 100K (flat, 4 threads)** | >= 1.5x | Minimum payoff to justify the parallel machinery at moderate scale |
| G3 | **Parallel speedup at 1M (realistic, 4 threads)** | >= 2.0x | Must demonstrate material improvement at the scale where users feel delete latency |
| G4 | **No regression at 1 thread** | parallel (1t) within 5% of sequential | The parallel code path with a single worker must not be measurably slower than sequential; this is the "no overhead" gate |
| G5 | **RSS overhead (1M, realistic, 4 threads)** | <= 15% above sequential RSS | The `ReorderBuffer` + `CohortBatcher` + per-thread batch allocation must not blow the memory budget |
| G6 | **No over-subscription regression** | parallel (64t) >= 0.9x of parallel (16t) throughput | Over-subscription must degrade gracefully, not cliff |
| G7 | **Sequential throughput stability** | sequential path at 100K within 5% of `delete_end_to_end` baseline | The feature flag must not regress the sequential path even when compiled off |
| G8 | **Interop matrix clean** | upstream rsync 3.0.9, 3.1.3, 3.4.1, 3.4.2 interop tests pass with feature on | Wire compatibility across supported upstream versions |

### 2.2 Gate evaluation order

Gates are evaluated in dependency order:

1. G1 (wire-byte parity) - blocks all other gates; if the parallel
   consumer produces different wire bytes, no performance discussion
   is relevant.
2. G8 (interop matrix) - validates G1 against real upstream binaries,
   not just internal golden captures.
3. G7 (sequential stability) - ensures the feature addition has not
   perturbed the default code path.
4. G4 (single-thread overhead) - ensures the parallel machinery's
   constant-factor cost is acceptable.
5. G2, G3 (speedup) - the positive case for promotion.
6. G5 (RSS) - the memory veto check.
7. G6 (over-subscription) - robustness under non-ideal threading.

If any gate fails, promotion is deferred and the failing gate becomes
a tracked issue with a remediation plan.

### 2.3 Confidence level

Each speedup and overhead measurement must achieve criterion's 99%
confidence interval (matching the `significance_level = 0.01`
configured in both DEL-4.a and DEL-4.b). Results with overlapping
confidence intervals between sequential and parallel are treated as
"no measurable difference" and fail the speedup gates (G2, G3).

## 3. Scale-dependent default decision

### 3.1 Options

Three default-on strategies are evaluated against the bench results:

| Strategy | Description | When to choose |
|----------|-------------|----------------|
| **Unconditional default-on** | Feature compiled into every build; no runtime file-count check | G2 (100K speedup) and G4 (1t overhead) both pass cleanly - parallel is always better or neutral |
| **Threshold-gated default-on** | Feature compiled in by default but the receiver dispatches to sequential for transfers below N files | G3 (1M speedup) passes but G2 (100K speedup) fails or is marginal - parallel only pays off at scale |
| **Opt-in only** | Feature stays off-by-default; users enable via `--features` | Any of G1-G8 fails; the parallel consumer is not ready |

### 3.2 Threshold-gated design

If bench results indicate a crossover point where parallel overtakes
sequential, the receiver's delete dispatch gains a runtime check:

```rust
const PARALLEL_DELETE_THRESHOLD: usize = <N>;

let use_parallel = extras_count >= PARALLEL_DELETE_THRESHOLD
    && rayon::current_num_threads() >= 2;
```

The threshold N is derived from the bench results by finding the
smallest extras count where the parallel path is >= 1.2x faster than
sequential with 95% confidence. Candidate values based on theoretical
analysis:

| Threshold | Rationale |
|-----------|-----------|
| 1,000 | Conservative; parallel overhead (~50 us for Condvar + slot transitions) is amortized over 1K unlinks at ~3 us each |
| 10,000 | Moderate; matches the 10% extras fraction of DEL-4.a's 100K fixture |
| 50,000 | Aggressive; only kicks in at large-scale deletes where the speedup is unambiguous |

The final threshold is determined by the bench data, not by
theoretical analysis. If the speedup curve has a clean crossover at
a specific N, that N becomes the threshold. If the curve is noisy or
the crossover is gradual, the conservative bound (highest N where
parallel is still >= 1.2x faster) is used.

### 3.3 Thread-count minimum

The runtime check also gates on `rayon::current_num_threads() >= 2`.
On single-core machines or when `RAYON_NUM_THREADS=1`, the parallel
code path adds overhead (Condvar, slot transitions, consumer thread
spawn) with zero throughput benefit. The sequential path is always
preferred when only one worker is available.

### 3.4 Decision tree

```
G1 (wire parity) fails?
  -> Opt-in only. Stop.

G4 (1t overhead) fails?
  -> Opt-in only. Fix overhead first.

G2 (100K speedup >= 1.5x) passes AND G4 passes?
  -> Unconditional default-on (no threshold needed).

G3 (1M speedup >= 2.0x) passes but G2 fails?
  -> Threshold-gated default-on.
     Set threshold = crossover point from scaling bench.

G3 also fails?
  -> Opt-in only. Re-evaluate after DEL-1.c batching tuning.
```

## 4. Wire-byte parity prerequisite

### 4.1 Hard gate

The DEL-3 test suite is a non-negotiable prerequisite for default-on
promotion. Specifically:

- **DEL-3.a**: Sequential wire-byte capture harness produces
  deterministic golden captures for all fixture shapes.
- **DEL-3.b**: Parallel vs sequential parity test passes for every
  fixture in the DEL-3.a catalog, at rayon thread widths 1, 2, 4, and
  host-natural, on Linux, macOS, and Windows.
- **DEL-3.c**: Cohort-ordering stress tests pass at 64 concurrent
  workers, 100K cohort counts, and all adversarial input patterns
  (reverse, interleaved, burst, starvation).

### 4.2 Parity definition

From DEL-3.b section 1:

1. **NDX channel bytes identical.** The `NDX_DEL_STATS` sentinel plus
   five varints and the closing `NDX_DONE` are byte-for-byte equal.
2. **MSG channel set-equivalent.** `MSG_DELETED` path notifications
   within a goodbye cohort are compared as an unordered set (DEL-1.a
   section 5.1 establishes intra-cohort reorderability). Byte content
   of each notification is identical.
3. **Stats counters equal.** The `DeleteStats` struct from both
   emitters has identical per-kind counts.

### 4.3 Parity across interop matrix

Wire-byte parity is necessary but not sufficient. Gate G8 requires that
the full interop test matrix (`tools/ci/run_interop.sh`) passes with
the `parallel-delete-consumer` feature enabled. The interop matrix
covers upstream rsync versions 3.0.9, 3.1.3, 3.4.1, and 3.4.2 in both
daemon push and pull modes with `--delete` variants.

### 4.4 Regression CI integration

After promotion, the DEL-3.b parity test becomes a required CI check
(not advisory). Any future change to the delete pipeline that breaks
parity blocks the PR until fixed. The parity test runs in the existing
`nextest` CI cell with `--features parallel-delete-consumer` added to
the feature matrix.

## 5. Rollback criteria

### 5.1 Conditions triggering rollback

If any of the following conditions occur after the feature is promoted
to default-on, it is reverted to opt-in within one patch release:

| # | Condition | Detection mechanism | Severity |
|---|-----------|---------------------|----------|
| R1 | Wire-byte divergence in interop | DEL-3.b / interop CI failure | Critical - immediate revert |
| R2 | RSS regression > 25% vs sequential at 1M | DEL-4.b nightly bench regression | High - revert within 48 hours |
| R3 | Throughput regression > 10% vs sequential at any scale | DEL-4.a/b nightly bench regression | High - revert within 48 hours |
| R4 | Deadlock or hang in `ReorderBuffer` drain | User bug report or CI timeout | Critical - immediate revert |
| R5 | `delete_end_to_end` baseline regression > 10% | Existing bench CI | Medium - investigate, revert if not fixable in 1 week |
| R6 | Platform-specific failure (macOS, Windows) | CI matrix failure | High - revert on that platform, keep on passing platforms |

### 5.2 Rollback mechanism

Reverting the default is a single-line change in
`crates/engine/Cargo.toml`:

```toml
# Before (default-on):
default = ["zstd", "lz4", "xattr", "lazy-metadata", "parallel-delete-consumer"]

# After (reverted):
default = ["zstd", "lz4", "xattr", "lazy-metadata"]
```

The sequential `DeleteEmitter::emit_all` path remains compiled in all
builds regardless of the feature flag. The
`#[cfg(not(feature = "parallel-delete-consumer"))]` guards in
`crates/engine/src/delete/context/core.rs` ensure the sequential path
is always available as a fallback.

### 5.3 Deprecation of sequential path

The sequential emitter is retained for at least one full minor release
cycle after the parallel consumer becomes default-on. This gives the
nightly bench and interop regression detection one full release cycle
to surface problems. After that cycle, if no rollback conditions have
triggered, the sequential path can be removed and the feature flag
deleted. Removal is tracked as a separate task (not part of DEL-4.c).

## 6. Implementation path

### 6.1 Cargo.toml feature flag change

When all gates (G1-G8) pass, the promotion is a single commit:

**File:** `crates/engine/Cargo.toml`

```toml
# Current (off by default):
default = ["zstd", "lz4", "xattr", "lazy-metadata"]

# Promoted (on by default):
default = ["zstd", "lz4", "xattr", "lazy-metadata", "parallel-delete-consumer"]
```

No other Cargo.toml files need changes. The feature is local to the
`engine` crate and does not propagate to dependent crates (`core`,
`cli`, `daemon`, `transfer`).

### 6.2 Runtime threshold (if threshold-gated)

If the decision tree (section 3.4) selects threshold-gated default-on,
an additional code change is needed in the receiver-side delete
dispatch:

**File:** `crates/engine/src/delete/context/core.rs`

Add a runtime check before the `#[cfg(feature)]` branch:

```rust
#[cfg(feature = "parallel-delete-consumer")]
{
    if extras_count >= PARALLEL_DELETE_THRESHOLD
        && rayon::current_num_threads() >= 2
    {
        // parallel path
    } else {
        // sequential fallback
    }
}
```

The threshold constant lives in
`crates/engine/src/delete/context/core.rs` alongside the existing
dispatch logic, not in a separate configuration module.

### 6.3 CI workflow promotion

After the default flip:

1. **DEL-4.a bench** (`bench-delete-throughput.yml`): promoted from
   advisory to required. Regression beyond 10% blocks PRs touching
   `crates/engine/src/delete/**`.
2. **DEL-4.b bench** (`bench-delete-1m.yml`): remains advisory (1M-file
   benches are too expensive for every PR). Nightly runs continue for
   trend detection.
3. **DEL-3.b parity test**: added to the required nextest CI cell's
   feature matrix. No separate workflow - it runs as part of the
   standard test suite.

### 6.4 Release notes

The release that includes the default flip adds a section:

```markdown
### Performance

- **Parallel delete consumer enabled by default.** The `--delete-*`
  family of options now uses a parallel consumer pipeline for
  destination-side deletions. At 100K+ files, delete throughput
  improves by <N>x on multi-core systems. The sequential fallback
  is available by building with
  `--no-default-features --features "zstd,lz4,xattr,lazy-metadata"`.
  Wire compatibility with upstream rsync is preserved - the parallel
  consumer produces byte-identical protocol output.
```

### 6.5 Commit sequence

The promotion lands as a single PR with the following commits:

1. `feat(engine): enable parallel-delete-consumer by default` -
   Cargo.toml default change, optional runtime threshold constant.
2. `test(engine): add parallel-delete-consumer to CI feature matrix` -
   nextest configuration update.
3. `ci: promote delete throughput bench to required` - workflow
   `required: true` flip for DEL-4.a.

Each commit is independently revertable for surgical rollback.

## 7. Timeline and dependencies

```
DEL-3.a/b/c (wire-byte parity)  ─────┐
DEL-4.a (100K bench harness)    ──────┤
DEL-4.b (1M bench harness)     ──────┤
                                      v
                        DEL-4.c data collection
                              |
                              v
                    Gate evaluation (G1-G8)
                              |
                     ┌────────┴────────┐
                     v                 v
              All gates pass     Any gate fails
                     |                 |
                     v                 v
              Promotion PR      Remediation plan
                     |           (tracked issue)
                     v
              Release with
              default-on
                     |
                     v
              1 minor release
              bake-in period
                     |
                     v
              Sequential path
              removal (optional)
```

## 8. Open questions

1. **macOS unlink performance.** macOS's APFS has different dentry-cache
   characteristics than Linux ext4/tmpfs. If the parallel speedup on
   macOS is significantly lower than on Linux, should the threshold be
   platform-specific? Decision: start with a single threshold; add
   platform-specific logic only if macOS bench data shows a materially
   different crossover point.

2. **Windows delete performance.** Windows `DeleteFileW` performance
   characteristics differ from POSIX `unlink(2)`. The parallel consumer
   uses `std::fs::remove_file` which maps to the platform-native call.
   If Windows shows no speedup or regression, the feature can be
   gated with `#[cfg(unix)]` while keeping it default-on for Unix
   platforms.

3. **Interaction with `--delete-delay`.** The `--delete-delay` mode
   replays deletions at end-of-flist rather than during the transfer
   phase. The parallel consumer handles this via the same
   `DirTraversalCursor` replay mechanism, but the bench harnesses
   (DEL-4.a/b) only measure `--delete-during` and `--delete-before`
   topologies. If `--delete-delay` shows different scaling
   characteristics, a follow-up bench variant is needed.

4. **io_uring `IORING_OP_UNLINKAT` future.** If the `fast_io` crate
   gains `unlinkat` support via io_uring, the parallel consumer
   becomes the natural batching layer for ring submission. The
   threshold and default-on decision would need revisiting with a
   three-way comparison (sequential / parallel-rayon / io_uring). This
   is out of scope for the current decision but noted for the roadmap.

## 9. Cross-references

- DEL-1.a ordering audit: `docs/design/del-1a-upstream-ordering-audit.md`
- DEL-1.b reorder buffer: `docs/design/del-1b-reordering-buffer.md`
- DEL-1.c cohort batching: `docs/design/del-1c-cohort-batching-strategy.md`
- DEL-3.a capture harness: `docs/design/del-3a-wire-byte-capture-harness.md`
- DEL-3.b parity test: `docs/design/del-3b-wire-byte-parity-test.md`
- DEL-3.c stress test: `docs/design/del-3c-cohort-ordering-stress-test.md`
- DEL-4.a 100K bench: `docs/design/del-4a-100k-file-delete-bench.md`
- DEL-4.b 1M bench: `docs/design/del-4b-1m-file-delete-bench.md`
- Feature flag: `crates/engine/Cargo.toml:110`
- Sequential dispatch: `crates/engine/src/delete/context/core.rs`
- Parallel consumer: `crates/engine/src/delete/parallel_consumer.rs`
- Parallel interop parity gap (prior art for flag promotion policy):
  `project_parallel_interop_parity_gap.md`
- Delete consumer bottleneck note:
  `project_delete_consumer_single_threaded.md`
