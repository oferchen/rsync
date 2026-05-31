# ASY-12.b: Async migration decision framework

Status: Design (capstone). Ties together the full ASY-1..12 series into
a single decision framework that maps benchmark evidence to action.

Predecessors:

- `docs/audits/asy-1-threading-model.md` - boundary inventory (12
  boundaries, 8 invariants).
- `docs/design/asy-2-tokio-runtime-feature.md` - `tokio-transfer`
  feature flag, wire-byte parity requirement, open questions.
- `docs/design/asy-3-async-boundary-spec.md` - per-boundary disposition
  (6 `.await`, 4 `spawn_blocking`, 1 dissolves, 1 unchanged).
- `docs/design/asy-5-a-embeddability-test-harness.md` - embeddability
  harness spec.
- `docs/design/asy-5b-embeddability-harness-impl.md` - harness
  implementation.
- `docs/design/asy-5c-embeddability-gap-list.md` - gap list from
  embeddability testing.
- `docs/design/asy-6-adopt-or-defer-decision.md` - defer decision with
  exit criteria.
- `docs/design/receiver-tokio-prototype.md` (ASY-7.a) - receiver tokio
  prototype.
- `docs/design/sender-tokio-prototype.md` (ASY-8.a) - sender tokio
  prototype.
- `docs/design/iouring-async-dispatch.md` (ASY-9.a) - io_uring stays
  synchronous behind `spawn_blocking`.
- `docs/design/token-loop-async-migration.md` (ASY-10.a) - token_loop
  async state machine.
- `docs/design/sync-async-wire-parity-test.md` (ASY-11.a) - wire-byte
  parity test architecture.
- `docs/design/concurrent-transfers-async-vs-threaded-bench.md`
  (ASY-12.a) - benchmark harness design with decision criteria.

## 1. Evidence summary

The ASY series produced 12 design and audit artifacts spanning boundary
analysis, prototype architecture, testing infrastructure, and
benchmarking methodology. This section summarizes what each established.

### 1.1 ASY-1: Threading model audit

Mapped all synchronous barriers in the transfer pipeline. Found 12
inter-thread boundaries and 8 safety invariants. Key findings:

- Generator, receiver, and disk-commit threads communicate through SPSC
  spin channels and crossbeam bounded channels.
- Flush-before-block and shutdown-join are the two highest-risk
  invariants under async migration.
- The sender role is not a separate runtime entity - it runs inside
  `GeneratorContext`.

### 1.2 ASY-2: Tokio runtime feature flag

Defined `tokio-transfer` as a cargo feature (default off) that gates
the async transfer path. Established constraints:

- Wire-byte parity is non-negotiable (section 7).
- 5% end-to-end uplift is the minimum floor for justifying the
  migration (section 10 #5).
- Tokio CVE exposure must be bounded (section 9).
- IOCP interaction on Windows is an open question.

### 1.3 ASY-3: Per-boundary disposition

Classified all 12 boundaries into migration categories:

- 6 boundaries become `.await` (wire I/O hot path).
- 4 boundaries become `spawn_blocking` (disk I/O, compute).
- 1 boundary dissolves (SPSC channel replaced by in-task buffer).
- 1 boundary unchanged (already async via russh).

Cross-cutting concerns: error helper crate, cancellation token
propagation, logging instrumentation (3 additional PRs).

### 1.4 ASY-5: Embeddability testing

Three-part investigation (5.a spec, 5.b impl, 5.c gap list):

- Confirmed `core::session()` cannot be called from within a tokio
  runtime without `spawn_blocking` wrappers today.
- Identified blocking points: SPSC spin-wait, thread-local buffer pool,
  synchronous stat batches.
- Gap list quantifies what the migration must fix for embedding.

### 1.5 ASY-6: Adopt/defer/close decision

Selected Option B (defer) pending ASY-4 benchmark data and ASY-5
embeddability confirmation. Exit criteria:

- Both ASY-4 bench and ASY-5 test must land before re-evaluation.
- >= 5% uplift + embedding gap confirmed -> adopt (Option A).
- < 5% uplift + embedding satisfied by `spawn_blocking` -> close
  (Option C).
- Mixed results -> re-open with new evidence.

### 1.6 ASY-7.a: Receiver tokio prototype

Sketched migration of `run_pipeline_loop_decoupled` from SPSC spin
channels + disk-commit OS thread to a tokio task graph. Key decisions:

- Delta verification remains CPU-bound, stays in `spawn_blocking`.
- Wire reads become `AsyncRead` with `BufReader` wrapping.
- Disk writes dispatched via `spawn_blocking` (io_uring path per
  ASY-9.a stays synchronous inside its own blocking task).

### 1.7 ASY-8.a: Sender tokio prototype

Sketched migration of `transfer_loop.rs` (sender role). Key decisions:

- File reads (`open_and_generate_delta`) become async with tokio
  `File`.
- Whole-file streaming uses `tokio::io::copy` with buffered transport.
- Hash compute (`rolling_checksum + strong_checksum`) dispatched to
  `spawn_blocking` pool.

### 1.8 ASY-9.a: io_uring async dispatch

Decided io_uring stays synchronous behind `spawn_blocking`. Rationale:

- `tokio-uring` requires a `!Send` task model incompatible with the
  multi-threaded runtime used for wire I/O.
- The per-thread ring layout (IUR-2) already eliminates the
  `Arc<Mutex>` bottleneck without tokio-uring.
- No measured benefit to native async io_uring at the disk-commit
  granularity (one SQE per chunk is already optimal).

### 1.9 ASY-10.a: token_loop async migration

Designed async state machine for the receiver hot path. Key decisions:

- State machine enum (`TokenLoopState`) replaces the tight loop with
  explicit yields between states.
- SPSC spin channel dissolves - token data flows directly into the
  disk-commit task via `mpsc::Sender`.
- Drain-on-error semantics preserved through a dedicated
  `drain_remaining_tokens` async fn.

### 1.10 ASY-11.a: Wire-byte parity testing

Defined the test architecture that gates ASY-12:

- `WireCapture` adapter records every wire byte (cfg(test) only).
- Dual-path harness runs sync and async through identical scenarios.
- In-process loopback (no TCP, no tcpdump) eliminates flakiness.
- Logical equivalence for framing boundaries where physical byte
  identity is impossible (MSG_INFO coalescing precedent).

### 1.11 ASY-12.a: Concurrent transfers benchmark

Specified the benchmark harness that produces the decision data:

- 6 concurrency levels (C1 through C1024).
- 4 workload profiles (small-files, large-files, mixed, delta-update).
- Per-connection metrics (TTFB, completion latency, throughput).
- Resource metrics (RSS, threads, fds, context switches, CPU time).
- Statistical methodology (geometric mean, bootstrap CI, Welch's t-test
  with Bonferroni correction, Cohen's d >= 0.5).
- Decision criteria codified in section 8.

## 2. Decision criteria (consolidated from ASY-12.a Section 8)

### 2.1 Adopt thresholds

All must hold simultaneously:

| # | Criterion | Threshold | Measurement |
|---|-----------|-----------|-------------|
| A1 | Low-concurrency floor | Tokio >= 95% of threaded throughput | C1, C4 aggregate throughput |
| A2 | Scale crossover | Tokio >= 10% higher throughput | Any of C64, C256, C1024 |
| A3 | Latency bound | Tokio p99 <= 120% of threaded p99 | All concurrency levels |
| A4 | RSS efficiency | Tokio peak RSS <= 80% of threaded | C256, C1024 |
| A5 | Correctness | 100% success rate (both variants) | C1 through C256 |
| A6 | Statistical validity | All comparisons pass significance test | Welch's t-test, d >= 0.5 |

### 2.2 Defer thresholds

Any improvement trends present but adopt thresholds unmet:

| Condition | Range |
|-----------|-------|
| Throughput crossover exists | > 0% but < 10% |
| RSS savings exist | > 0% but < 20% |
| Latency regression borderline | 20-30% range |

### 2.3 Close triggers

Any single trigger is sufficient:

| # | Trigger | Threshold |
|---|---------|-----------|
| C1 | Low-concurrency regression | Tokio < 95% throughput at C1 or C4 with no gain at scale |
| C2 | Severe latency regression | Tokio p99 > 150% of threaded at C64 or below |
| C3 | RSS regression | Tokio RSS > threaded at any level |
| C4 | Persistent failure | Adopt thresholds unmet after 2 optimization cycles |

## 3. Results format specification

### 3.1 File layout

Each benchmark run produces artifacts under a deterministic path:

```
target/bench/asy-12/$RUNID/
  metadata.json
  threaded/
    c{1,4,16,64,256,1024}_{small,large,mixed,delta}.json
    c{1,4,16,64,256,1024}_{small,large,mixed,delta}_resources.csv
  tokio/
    c{1,4,16,64,256,1024}_{small,large,mixed,delta}.json
    c{1,4,16,64,256,1024}_{small,large,mixed,delta}_resources.csv
  comparison.json
  decision.json
```

### 3.2 metadata.json

```json
{
  "run_id": "20260601-143022-abc123",
  "git_commit": "17d725853...",
  "rust_version": "1.88.0",
  "kernel": "6.8.0-45-generic",
  "cpu_model": "AMD EPYC 7763 64-Core",
  "cores_allocated": 16,
  "ram_total_gib": 64,
  "ulimit_nofile": 65536,
  "ulimit_nproc": 65536,
  "tmpfs_size_gib": 8,
  "iterations_per_triple": 10,
  "warmup_iterations": 3,
  "variant_order": "ABAB",
  "timestamp_utc": "2026-06-01T14:30:22Z"
}
```

### 3.3 Per-triple JSON (e.g., `c64_mixed.json`)

```json
{
  "concurrency": 64,
  "workload": "mixed",
  "variant": "threaded",
  "iterations": [
    {
      "iteration": 1,
      "wall_clock_ms": 2340,
      "aggregate_throughput_gbps": 2.78,
      "goodput_gbps": 2.78,
      "connection_success_rate": 1.0,
      "connections": [
        {
          "id": 0,
          "ttfb_ms": 1.2,
          "completion_ms": 2280,
          "throughput_mbps": 44.3,
          "success": true,
          "bytes_transferred": 105906176
        }
      ]
    }
  ],
  "summary": {
    "throughput_geomean_gbps": 2.75,
    "throughput_ci95_low": 2.68,
    "throughput_ci95_high": 2.82,
    "throughput_cv_pct": 3.2,
    "ttfb_p50_ms": 1.1,
    "ttfb_p95_ms": 2.4,
    "ttfb_p99_ms": 3.8,
    "ttfb_max_ms": 5.1,
    "latency_p50_ms": 2250,
    "latency_p95_ms": 2380,
    "latency_p99_ms": 2420,
    "latency_max_ms": 2510,
    "outliers_flagged": 0
  }
}
```

### 3.4 Resource CSV format (50 ms cadence)

```csv
timestamp_ms,rss_mib,vmsize_mib,threads,fds,cpu_user_s,cpu_sys_s,ctx_voluntary,ctx_involuntary
0,12.4,48.2,5,24,0.00,0.00,0,0
50,45.8,112.6,68,88,0.12,0.04,142,3
100,62.1,112.6,68,88,0.28,0.09,310,7
...
```

### 3.5 comparison.json (decision input)

```json
{
  "comparisons": [
    {
      "concurrency": 64,
      "workload": "mixed",
      "throughput_ratio": 1.12,
      "throughput_delta_pct": 12.3,
      "throughput_p_value": 0.00018,
      "throughput_cohens_d": 1.84,
      "throughput_significant": true,
      "latency_p99_ratio": 1.05,
      "latency_p99_delta_pct": 5.2,
      "rss_peak_ratio": 0.72,
      "rss_peak_delta_pct": -28.1,
      "success_rate_threaded": 1.0,
      "success_rate_tokio": 1.0
    }
  ],
  "criteria_evaluation": {
    "A1_low_concurrency_floor": true,
    "A2_scale_crossover": true,
    "A3_latency_bound": true,
    "A4_rss_efficiency": true,
    "A5_correctness": true,
    "A6_statistical_validity": true,
    "decision": "adopt"
  }
}
```

### 3.6 decision.json (machine-readable verdict)

```json
{
  "decision": "adopt",
  "confidence": "high",
  "criteria_met": ["A1", "A2", "A3", "A4", "A5", "A6"],
  "criteria_failed": [],
  "best_crossover_level": "C256",
  "best_crossover_gain_pct": 18.4,
  "rss_savings_at_c1024_pct": 34.2,
  "worst_latency_regression_pct": 5.2,
  "recommendation": "Proceed with ASY-12 feature gate flip.",
  "next_steps": [
    "Merge ASY-7..10 implementation PRs in dependency order",
    "Run ASY-11.a wire parity tests against merged code",
    "Flip tokio-transfer to default-on behind ASY-12 gate"
  ]
}
```

### 3.7 Summary tables (human-readable)

The harness auto-generates a Markdown summary with three tables:

**Table 1: Throughput comparison**

```
| Concurrency | Workload | Threaded (GB/s) | Tokio (GB/s) | Delta (%) | Significant | Cohen's d |
|-------------|----------|-----------------|--------------|-----------|-------------|-----------|
| C1          | small    | 0.82 +/- 0.03   | 0.80 +/- 0.02| -2.4      | no          | 0.31      |
| C1          | large    | 3.41 +/- 0.08   | 3.38 +/- 0.07| -0.9      | no          | 0.12      |
| ...         | ...      | ...             | ...          | ...       | ...         | ...       |
| C256        | mixed    | 2.14 +/- 0.12   | 2.52 +/- 0.09| +17.8     | yes         | 2.14      |
| C1024       | mixed    | 1.87 +/- 0.21   | 2.41 +/- 0.14| +28.9     | yes         | 3.02      |
```

**Table 2: Latency comparison (p99 ms)**

```
| Concurrency | Workload | Threaded p99 | Tokio p99 | Delta (%) | Within bound |
|-------------|----------|--------------|-----------|-----------|--------------|
| C1          | small    | 12.4         | 12.8      | +3.2      | yes          |
| ...         | ...      | ...          | ...       | ...       | ...          |
| C1024       | mixed    | 8420         | 6510      | -22.7     | yes          |
```

**Table 3: Resource efficiency**

```
| Concurrency | Threaded RSS (MiB) | Tokio RSS (MiB) | Ratio | Threaded threads | Tokio threads |
|-------------|-------------------|-----------------|-------|------------------|---------------|
| C1          | 28                | 32              | 1.14  | 8                | 12            |
| C16         | 84                | 48              | 0.57  | 24               | 12            |
| ...         | ...               | ...             | ...   | ...              | ...           |
| C1024       | 2048              | 196             | 0.10  | 1032             | 16            |
```

### 3.8 Visualization (CI artifact)

The nightly and release tiers produce PNG charts:

1. **Throughput scaling curve** - X: concurrency (log scale), Y:
   aggregate throughput (GB/s). Two lines (threaded, tokio) with 95% CI
   shading. Crossover point annotated.
2. **RSS scaling curve** - X: concurrency (log scale), Y: peak RSS
   (MiB, log scale). Highlights the divergence point where tokio's
   fixed thread pool wins over per-connection stacks.
3. **Latency distribution** - Box plots at each concurrency level,
   side-by-side threaded vs tokio. Shows p50, p95, p99, max.
4. **Decision matrix heatmap** - 6x4 grid (concurrency x workload),
   color-coded: green (tokio wins >= 10%), yellow (< 10% either way),
   red (tokio loses >= 5%).

Charts are generated by the harness using the `plotters` crate and
uploaded as CI artifacts alongside the JSON/CSV data.

## 4. Decision flow

### 4.1 Evidence-to-action mapping

```text
Run ASY-12.a benchmark harness
            |
            v
   Collect comparison.json
            |
            v
   Evaluate criteria A1..A6
            |
     +------+------+
     |      |      |
     v      v      v
   ADOPT  DEFER  CLOSE
```

**ADOPT** - All A1..A6 pass. Proceed to Section 5.

**DEFER** - Improvement trends visible but thresholds unmet. Proceed to
Section 6.

**CLOSE** - Any close trigger (C1..C4) fires. Proceed to Section 7.

### 4.2 Ambiguity resolution

If the four workload profiles produce conflicting signals at a given
concurrency level (e.g., tokio wins on small-files but loses on
large-files), evaluate criteria per-workload and apply the following
precedence:

1. Mixed workload is the primary decision input (closest to production).
2. Delta-update is secondary (exercises the full pipeline).
3. Small-files and large-files are supporting context only.

A criterion passes if it passes on the mixed workload. A criterion
fails if it fails on both mixed and delta-update. If mixed passes but
delta-update fails, the criterion is marked "conditional" and the adopt
path requires targeted investigation of the delta-update regression
before the feature gate flip.

### 4.3 Iteration protocol

If the first benchmark run has coefficient of variation > 10% on any
primary metric, the run is considered unreliable. The harness
automatically extends to 30 iterations. If CV remains > 10% after 30
iterations, the run environment is deemed too noisy and must be
re-executed on a quieter host or with stronger isolation (CPU pinning,
network namespace, dedicated bare-metal runner).

## 5. Adopt path

### 5.1 PR merge order

The adopt path lands PRs in strict dependency order:

| Phase | PRs | Gate |
|-------|-----|------|
| 1 - Foundation | Error helper crate, cancellation token, logging instrumentation | Unit tests pass, clippy clean |
| 2 - Wire boundaries | ASY-7 receiver + ASY-8 sender wire `.await` conversion | Golden byte tests pass, ASY-11 wire parity green |
| 3 - Channel swap | SPSC channel dissolution, mpsc replacement | Transfer integration tests pass |
| 4 - Compute dispatch | `spawn_blocking` wrappers for hash/stat/delta | Performance parity at C1 (no regression) |
| 5 - Token loop | ASY-10 state machine conversion | Full transfer test suite green |
| 6 - Gate flip | `tokio-transfer` feature default-on | All below validation gates pass |

### 5.2 Validation gates per phase

Each phase must pass before the next begins:

- **Phase 1:** `cargo nextest run --workspace --all-features` green.
  No new `unsafe`. No public API changes to non-test crates.
- **Phase 2:** ASY-11.a wire parity test suite passes (byte-identical
  output for all golden scenarios). Interop suite green against
  upstream 3.0.9, 3.1.3, 3.4.1, 3.4.2.
- **Phase 3:** End-to-end daemon transfer tests pass with both
  threaded and tokio variants. No hang detection (60s timeout on all
  transfer tests).
- **Phase 4:** ASY-12.a benchmark at C1 shows tokio >= 95% of
  threaded throughput (criterion A1 in isolation).
- **Phase 5:** Full test suite green. Wire parity confirmed for
  token-loop-specific scenarios (partial transfers, abort mid-stream,
  interrupted delta).
- **Phase 6:** Full ASY-12.a benchmark re-run confirms all A1..A6
  criteria still hold after the complete merge sequence.

### 5.3 Rollback criteria

After the gate flip, the following trigger revert to threaded-default:

- Any interop failure against upstream rsync in CI.
- Wire parity test regression (ASY-11.a).
- User-reported transfer corruption.
- Performance regression > 10% reported by the nightly benchmark.

Rollback is a single commit: flip `default = ["tokio-transfer"]` back
to `default = []` in the workspace `Cargo.toml`. The entire async
pipeline remains compiled and testable behind the feature flag.

### 5.4 Post-adopt cleanup

After 30 days of default-on without rollback:

1. Remove the dual-path test infrastructure (sync tests become
   redundant - the sync path is no longer the default).
2. Mark the threaded pipeline as deprecated with a 2-release removal
   horizon.
3. Update `project_no_async_threaded_only.md` to record the transition.
4. Close ASY-1..12 tracking issues.

## 6. Defer path

### 6.1 Entry conditions

The defer path activates when:

- Improvement trends exist (tokio is not worse overall).
- At least one adopt criterion fails.
- No close trigger fires.

### 6.2 Optimization targets

Based on the comparison data, identify the specific bottleneck
preventing threshold attainment:

| Failed criterion | Investigation target | Likely root cause |
|-----------------|---------------------|-------------------|
| A1 (low-concurrency floor) | Task spawn overhead | Tokio runtime initialization cost per transfer |
| A2 (scale crossover) | Accept path contention | Shared state in task scheduling |
| A3 (latency regression) | Wake-up latency | Tokio work-stealing adding hops |
| A4 (RSS efficiency) | Runtime overhead | Tokio's own allocations dominate at tested scale |
| A6 (statistical validity) | Measurement noise | Environment isolation insufficient |

### 6.3 Optimization cycle

Each defer cycle has a fixed scope:

1. **Diagnose** (1 week) - profile the tokio variant under the failing
   workload with `perf record`, `flamegraph`, and `tokio-console`.
   Produce a bottleneck report identifying the top 3 contributors.
2. **Fix** (2 weeks) - implement targeted optimizations addressing the
   diagnosed bottlenecks. Each fix is a separate PR with its own
   micro-benchmark proving improvement.
3. **Re-bench** (3 days) - re-run the full ASY-12.a harness. Compare
   against the previous defer-cycle baseline and the original threaded
   baseline.
4. **Re-evaluate** (1 day) - apply Section 2 criteria to new results.
   Exit to adopt, continue deferring, or exit to close.

### 6.4 Maximum defer cycles

Two optimization cycles maximum (approximately 2 months total). If
adopt thresholds remain unmet after the second cycle, criterion C4
fires and the close path activates. This prevents indefinite investment
in diminishing returns.

### 6.5 Re-trigger conditions

If the async series is deferred and later developments change the
equation, re-evaluation is triggered by:

- New tokio release with documented scheduling improvements.
- New workload profile (e.g., 10K+ concurrent connections from a real
  deployment) that the threaded model cannot serve.
- Embedding use case materializes (external consumer blocked on
  `spawn_blocking` wrappers, per ASY-5.c gap list).
- `fast_io` crate gains native async I/O support that the sync path
  cannot leverage.

Each re-trigger resets the defer cycle counter to zero and begins a
fresh benchmark run.

## 7. Close path

### 7.1 Entry conditions

The close path activates when any close trigger fires:

- C1: Low-concurrency regression with no gain at scale.
- C2: Severe latency regression at moderate concurrency.
- C3: RSS regression (async overhead exceeds per-connection savings).
- C4: Two defer cycles exhausted without meeting adopt thresholds.

### 7.2 Documentation deliverables

| Document | Content | Location |
|----------|---------|----------|
| Close-out summary | Final benchmark numbers, criteria evaluation, rationale | `docs/design/asy-series-close-out.md` |
| Decision record | One-paragraph statement of permanent threaded model | `docs/decisions/adr-async-close.md` |
| Memory update | Update `project_no_async_threaded_only.md` to "permanent - closed with evidence" | Project memory |

### 7.3 Code cleanup

In the close path, the following code is removed:

1. `#[cfg(feature = "async")]` skeleton in
   `crates/transfer/src/pipeline/async_pipeline.rs`.
2. `tokio-transfer` feature definition from workspace `Cargo.toml`.
3. `tokio` dev-dependency from `crates/transfer/Cargo.toml` (if not
   used by other features).
4. ASY-7..10 prototype code (if any was merged behind the feature
   flag).

Cleanup is a single PR titled "chore: remove async transfer skeleton
after ASY series close."

### 7.4 Preserving value

Even on close, the following artifacts retain value and are NOT deleted:

- ASY-1 threading model audit (documents the existing architecture).
- ASY-5 embeddability gap list (informs future API design).
- ASY-11 wire parity test infrastructure (reusable for any protocol
  refactor).
- ASY-12 benchmark harness (reusable for thread-per-connection
  scalability tracking).

These are reclassified from "active design" to "historical reference"
by prepending a status line: `Status: Closed (historical). Series
concluded YYYY-MM-DD.`

### 7.5 Re-opening conditions

A closed series can be re-opened only if:

- A fundamentally new async runtime emerges that invalidates the
  benchmark assumptions (e.g., kernel-scheduled green threads, native
  io_uring async without the `!Send` constraint).
- A concrete production deployment requires > 10K concurrent daemon
  connections, exceeding the thread-per-connection ceiling documented
  in ASY-1.
- The re-opener produces a fresh prototype demonstrating the previous
  bottleneck is eliminated (not just theoretically possible).

Re-opening creates a new series (ASY-II-1..N) rather than reopening
closed ASY-1..12 issues.

## 8. Timeline

Estimated calendar from benchmark execution to final decision:

| Week | Activity | Deliverable |
|------|----------|-------------|
| W1 | Harness validation (threaded-only dry run) | Baseline variance report |
| W2 | ASY-7..10 PRs land behind feature flag | Tokio-transfer variant buildable |
| W3 | Full benchmark run (10+ iterations, all workloads) | Raw data in `target/bench/asy-12/` |
| W4 | Statistical analysis and chart generation | `comparison.json`, PNG charts |
| W4 | Decision evaluation against Section 2 criteria | `decision.json` |
| W5 | Decision review and ratification | Status update to this document |

**If adopt:** W6-W10 execute Section 5 (phased merge with gates).
W11 feature gate flip. W12-W16 bake period.

**If defer:** W6-W8 first optimization cycle. W9 re-bench. W10
re-evaluate. If still deferred: W11-W13 second cycle. W14 final
re-bench. W15 adopt or close.

**If close:** W6 produce documentation deliverables. W7 cleanup PR.
W8 series marked done.

Total time budget: 5 weeks to first decision, up to 15 weeks if two
defer cycles are needed, 8 weeks for post-adopt bake through to
permanent default.

## 9. Success metrics for this framework

This document succeeds if:

- The benchmark harness team can produce results without ambiguity
  about format or statistical methodology.
- The decision is mechanically derivable from `comparison.json` without
  subjective interpretation.
- Each outcome path has a concrete, actionable checklist with no
  unspecified steps.
- Future readers can trace the full chain from ASY-1 boundary audit
  through to the final adopt/defer/close action.

## 10. Cross-references

- `docs/audits/asy-1-threading-model.md` - boundary inventory.
- `docs/design/asy-2-tokio-runtime-feature.md` - feature flag design.
- `docs/design/asy-3-async-boundary-spec.md` - per-boundary contracts.
- `docs/design/asy-5-a-embeddability-test-harness.md` - harness spec.
- `docs/design/asy-5c-embeddability-gap-list.md` - gap list.
- `docs/design/asy-6-adopt-or-defer-decision.md` - defer decision.
- `docs/design/receiver-tokio-prototype.md` (ASY-7.a).
- `docs/design/sender-tokio-prototype.md` (ASY-8.a).
- `docs/design/iouring-async-dispatch.md` (ASY-9.a).
- `docs/design/token-loop-async-migration.md` (ASY-10.a).
- `docs/design/sync-async-wire-parity-test.md` (ASY-11.a).
- `docs/design/concurrent-transfers-async-vs-threaded-bench.md` (ASY-12.a).
- `docs/design/daemon-tpc-benchmark-plan.md` - thread-per-connection
  scalability bench (baseline for this work).
