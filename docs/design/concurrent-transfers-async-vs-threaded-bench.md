# ASY-12.a: Concurrent transfers async vs threaded benchmark design

Status: Design. Final piece of the ASY-7..12 implementation series.
Tracking: ASY-12.a.

Predecessor documents:

- `docs/design/asy-6-adopt-or-defer-decision.md` - defer decision;
  names ASY-4 bench as the gate for adopt/close. This document is the
  ASY-4 successor scoped specifically to concurrent daemon transfers.
- `docs/design/sender-tokio-prototype.md` (ASY-8.a) - sender tokio
  prototype shape.
- `docs/design/token-loop-async-migration.md` (ASY-10.a) - token_loop
  async migration.
- `docs/design/iouring-async-dispatch.md` (ASY-9.a) - decision to keep
  io_uring synchronous behind `spawn_blocking`.
- `docs/design/daemon-tpc-benchmark-plan.md` - thread-per-connection
  scalability bench (measures the threaded ceiling in isolation).
- `docs/design/daemon-thread-per-conn-bench.md` - slim runnable harness
  for the W100/W1k/W10k waypoints.

Scope: benchmark harness architecture, workload profiles, metrics
collection, statistical methodology, and decision criteria that
determine whether the tokio async model justifies migration from the
current thread-per-connection daemon.

Out of scope: the actual migration implementation (ASY-7..10 own that),
io_uring interaction (ASY-9.a decided: stays synchronous), wire parity
testing (ASY-11 owns that), and the feature-flag flip (ASY-12 gate).

## 1. Problem statement

The current daemon model spawns one OS thread per client connection
(`crates/daemon/src/daemon/sections/server_runtime/connection.rs`). The
static analysis in `docs/audits/daemon-thread-per-connection-scalability.md`
establishes a ceiling at approximately 10K concurrent connections on
default Linux tunables due to thread stack reservation, fd exhaustion,
and scheduler contention.

The ASY-2..10 design series proposes a tokio-based alternative: a shared
multi-threaded runtime with one lightweight task per connection,
eliminating per-connection stack allocation and allowing the runtime to
multiplex thousands of concurrent transfers across a fixed-size thread
pool.

This benchmark answers the headline question: does the tokio model
deliver measurable throughput, latency, or resource efficiency gains
under concurrent daemon transfer load - and if so, at what concurrency
level does the crossover occur?

## 2. Bench harness architecture

### 2.1 Components

```text
+-------------------+       rsync://        +-------------------+
|  Client Driver    | ------- N conns ----> |  Daemon Under Test|
|  (rayon workers)  |                       |  (oc-rsync)       |
+-------------------+                       +-------------------+
        |                                           |
        v                                           v
+-------------------+                       +-------------------+
|  Latency Recorder |                       |  Resource Sampler |
|  (per-connection) |                       |  (sidecar thread) |
+-------------------+                       +-------------------+
        |                                           |
        +-----------------> Results <---------------+
                            (JSON + CSV)
```

1. **Daemon under test** - one `oc-rsync --daemon` process on loopback.
   Two build variants:
   - `threaded` (default) - current thread-per-connection model.
   - `tokio-transfer` (feature flag) - tokio multi-thread runtime with
     task-per-connection.
   Both variants serve an identical tmpfs-backed module with the same
   pre-generated fileset.

2. **Client driver** - `crates/daemon/benches/async_vs_threaded_driver/main.rs`.
   Uses rayon to spawn `N` concurrent pull sessions. Each session speaks
   the wire protocol directly via `crates/protocol` (no CLI overhead).
   Records per-connection timing from `connect()` through `MSG_DONE`.

3. **Resource sampler** - sidecar thread polling `/proc/$PID/status`
   (Linux) or `mach_task_basic_info` (macOS) at 50 ms cadence. Captures
   RSS, virtual memory, thread count, fd count, and CPU time.

4. **Coordinator** - orchestrates warm-up, measurement iterations,
   cool-down, and variant switching. Writes structured output to
   `target/bench/asy-12/$RUNID/`.

### 2.2 Pre-built binaries

The harness pre-builds both daemon variants before measurement begins.
No `cargo` invocation occurs during the timed phase. Build artifacts:

- `target/release/oc-rsync` (threaded baseline)
- `target/release/oc-rsync-tokio` (built with `--features tokio-transfer`)

### 2.3 Tmpfs backing store

The test module's fileset lives on tmpfs to ensure disk I/O is not the
bottleneck. The harness creates a temporary tmpfs mount (or uses
`/dev/shm` on Linux) and populates it with the workload files before
the first iteration.

### 2.4 Isolation

- CPU pinning via `taskset` to prevent cross-socket migration.
- Daemon and driver pinned to separate CPU sets (e.g., daemon on cores
  0-7, driver on cores 8-15 on a 16-core host).
- Network namespace isolation (optional, for W1k+ to avoid ephemeral
  port exhaustion).
- `SCHED_BATCH` for the driver to avoid preempting daemon threads.

## 3. Concurrency levels

| Level | Concurrent connections | Purpose |
|-------|----------------------|---------|
| C1 | 1 | Single-connection baseline; measures per-connection overhead |
| C4 | 4 | Typical small-server load |
| C16 | 16 | Moderate load; threaded model expected comfortable |
| C64 | 64 | Production operating point for mid-range daemons |
| C256 | 256 | Heavy load; thread stack pressure begins |
| C1024 | 1024 | Stress; approaches default fd/thread limits |

All levels use all-at-once arrival: the driver fires all `connect()`
calls within a 10 ms window to stress the accept path and maximize
concurrent active transfers. Steady-state arrival (replacement rate
maintaining N active) is a secondary run for C64 and C1024 only.

Ulimits are raised to 65536 for fds and nproc to ensure the harness
measures daemon efficiency rather than kernel rejection.

## 4. Workload profiles

### 4.1 Small-files

- 1000 files, 1 KiB each (1 MiB total per connection).
- High file-list and generator overhead relative to data.
- Stresses per-file setup cost: file-list exchange, iflags, checksum
  negotiation, stat calls.
- Expected bottleneck: thread spawn latency (threaded) vs task spawn
  latency (tokio).

### 4.2 Large-files

- 10 files, 100 MiB each (1 GiB total per connection).
- Data throughput dominated; file-list overhead negligible.
- Stresses sustained I/O throughput and buffer management.
- Expected bottleneck: wire write contention, buffer pool pressure,
  memory bandwidth.

### 4.3 Mixed

- 900 files at 1 KiB + 10 files at 10 MiB (approximately 101 MiB
  total per connection).
- Represents realistic daemon workloads (many small config files plus
  a few large binaries).
- Exercises both fast-path (whole-file for small) and delta pipeline
  (signatures + tokens for large).

### 4.4 Delta-update

- Same fileset as mixed, but destination pre-seeded with slightly
  different content (10% of bytes modified per file).
- Exercises the full delta pipeline: signature generation, block
  matching, token emission.
- Distinguishes models under compute-heavy workloads where async I/O
  overlap matters most.

## 5. Metrics collection

### 5.1 Per-connection metrics

| Metric | Measurement point | Unit |
|--------|------------------|------|
| Time-to-first-byte (TTFB) | `connect()` return to first data byte after greeting | ms |
| Transfer completion latency | `connect()` return to `MSG_DONE` receipt | ms |
| Per-connection throughput | Total bytes received / completion latency | MB/s |
| Success/failure | Full fileset received vs partial/error | bool |

### 5.2 Aggregate metrics

| Metric | Derivation | Unit |
|--------|-----------|------|
| Aggregate throughput | Sum of all bytes transferred / wall-clock time of the run | GB/s |
| Goodput | Successful connections * bytes-per-connection / wall-clock | GB/s |
| Connection success rate | Successful / attempted | % |
| TTFB p50, p95, p99, max | Percentile distribution across all connections | ms |
| Completion latency p50, p95, p99, max | Percentile distribution | ms |

### 5.3 Resource metrics (sampled at 50 ms cadence)

| Metric | Source | Unit |
|--------|--------|------|
| Peak RSS | `/proc/$PID/status` VmRSS | MiB |
| Peak virtual memory | `/proc/$PID/status` VmSize | MiB |
| Peak thread count | `/proc/$PID/status` Threads | count |
| Peak fd count | `ls /proc/$PID/fd \| wc -l` | count |
| CPU user time | `/proc/$PID/stat` field 14 | seconds |
| CPU system time | `/proc/$PID/stat` field 15 | seconds |
| CPU utilization | (user + sys) / (wall-clock * cores allocated) | % |
| Context switches (voluntary) | `/proc/$PID/status` | count |
| Context switches (involuntary) | `/proc/$PID/status` | count |

### 5.4 Derived efficiency metrics

| Metric | Formula | Purpose |
|--------|---------|---------|
| RSS per connection | Peak RSS / N | Memory efficiency |
| Throughput per core | Aggregate throughput / CPU cores used | CPU efficiency |
| Latency * throughput product | p99 latency * aggregate throughput | Queuing theory indicator |
| Context switches per transfer | Total ctx switches / (N * files) | Scheduling overhead |

## 6. Comparison methodology

### 6.1 A/B structure

Each benchmark run produces a paired comparison:

1. Run the threaded daemon variant through all concurrency levels and
   workloads.
2. Restart with the tokio-transfer variant on the same port, same
   tmpfs fileset, same client driver configuration.
3. Repeat for the configured number of iterations.

Variant order alternates between iterations (ABAB pattern) to cancel
systematic drift (thermal throttling, memory fragmentation).

### 6.2 Upstream comparison (optional tier)

A third variant uses upstream rsync 3.4.1 daemon (fork-per-connection)
to establish the C-implementation baseline. This is an optional tier
because upstream's fork model has fundamentally different resource
characteristics (COW address spaces vs shared heap) and serves as
context rather than a decision input.

## 7. Statistical methodology

### 7.1 Warm-up

- 3 warm-up iterations discarded before measurement begins.
- Warm-up serves: JIT-equivalent (branch predictor training), page
  cache population, buffer pool pre-allocation, tokio runtime thread
  ramp-up.

### 7.2 Measurement iterations

- Minimum 10 iterations per (variant, concurrency, workload) triple.
- Additional iterations until the coefficient of variation (CV) of
  aggregate throughput drops below 5% or 30 iterations are reached,
  whichever comes first.
- Each iteration is a complete cycle: daemon start, transfer, daemon
  shutdown. No state carries between iterations (cold daemon per
  iteration ensures measurement of startup + steady-state).

### 7.3 Statistical analysis

- **Central tendency:** Geometric mean of per-iteration aggregate
  throughput (geometric mean is appropriate for ratio-scale performance
  data).
- **Dispersion:** 95% confidence interval via bootstrap resampling
  (10,000 resamples) for each metric.
- **Significance test:** Two-sided Welch's t-test on log-transformed
  throughput values. Significance threshold alpha = 0.05 with
  Bonferroni correction for multiple comparisons (6 concurrency levels
  * 4 workloads = 24 comparisons, corrected alpha = 0.002).
- **Effect size:** Report Cohen's d alongside p-values. Practically
  significant only if d >= 0.5 (medium effect).
- **Outlier handling:** Flag iterations where any metric exceeds 3
  standard deviations from the mean. Report with and without outliers;
  do not silently discard.

### 7.4 Cool-down

- 2-second pause between iterations to allow TCP TIME_WAIT sockets to
  drain and kernel memory reclamation.
- 5-second pause between variant switches.

## 8. Decision criteria

The benchmark output gates the ASY-12 feature-flag flip (async becomes
default) and the broader adopt/defer/close decision from ASY-6. The
criteria are structured as a tiered decision tree:

### 8.1 Adopt (flip tokio-transfer to default-on)

All of the following must hold:

1. **Throughput floor:** Tokio variant achieves >= 95% of threaded
   throughput at C1 and C4 (no regression at low concurrency).
2. **Throughput crossover:** Tokio variant achieves >= 10% higher
   aggregate throughput than threaded at one or more of C64, C256,
   C1024.
3. **Latency bound:** Tokio p99 completion latency does not exceed
   threaded p99 by more than 20% at any concurrency level.
4. **RSS efficiency:** Tokio peak RSS is <= 80% of threaded peak RSS at
   C256 and C1024 (at least 20% memory savings at scale).
5. **No correctness regression:** 100% connection success rate for both
   variants at C1 through C256. C1024 may show failures from resource
   limits but tokio must not fail at a lower concurrency than threaded.
6. **Statistical confidence:** All threshold comparisons pass the
   significance test (Section 7.3) with effect size d >= 0.5.

### 8.2 Defer (keep threaded, revisit later)

If the tokio variant shows improvement trends but does not clear the
adopt thresholds:

- Throughput crossover exists but is < 10%.
- RSS savings exist but are < 20%.
- Latency regression is borderline (within 20-30%).

Action: document results, identify specific bottlenecks in the tokio
prototype, open targeted optimization tickets, re-bench after fixes.

### 8.3 Close (abandon async migration)

If any of the following hold:

1. Tokio variant regresses throughput by > 5% at C1 or C4 and shows
   no compensating gain at higher concurrency levels.
2. Tokio p99 latency exceeds threaded by > 50% at C64 or below.
3. Tokio RSS exceeds threaded RSS at any concurrency level (async
   overhead dominates the per-connection stack savings).
4. After two rounds of targeted optimization (defer cycle), the adopt
   thresholds remain unmet.

Action: mark ASY-7..12 as closed/rejected, update
`project_no_async_threaded_only.md` as permanent, remove the
`tokio-transfer` feature flag skeleton.

## 9. Output format

### 9.1 Per-run artifacts

```
target/bench/asy-12/$RUNID/
  metadata.json          # git commit, build flags, kernel, ulimits
  threaded/
    c1_small.json        # raw per-connection timings
    c1_small_resources.csv  # 50ms-cadence resource samples
    ...
  tokio/
    c1_small.json
    c1_small_resources.csv
    ...
  summary.md             # auto-generated comparison table
  summary.json           # machine-readable comparison
```

### 9.2 Summary table format

```
| Concurrency | Workload | Variant | Throughput (GB/s) | p99 Latency (ms) | Peak RSS (MiB) | Threads |
|-------------|----------|---------|-------------------|-------------------|----------------|---------|
| C1          | small    | thread  | ...               | ...               | ...            | ...     |
| C1          | small    | tokio   | ...               | ...               | ...            | ...     |
| ...         | ...      | ...     | ...               | ...               | ...            | ...     |
```

### 9.3 CI integration

- **PR tier (fast):** C1 + C16 with small-files workload only. Runs in
  ~60 seconds. Gates PRs touching `crates/daemon/` or
  `crates/transfer/`.
- **Nightly tier (slow):** Full matrix (all concurrency levels, all
  workloads, 10+ iterations). Runs in ~20 minutes. Results archived as
  CI artifacts.
- **Release tier:** Full matrix with 30 iterations and upstream
  comparison. Runs in ~60 minutes. Results included in release notes.

## 10. Implementation plan

| Step | Description | Depends on |
|------|-------------|-----------|
| 1 | Pre-generate workload filesets (deterministic seed) | None |
| 2 | Implement client driver (`crates/daemon/benches/async_vs_threaded_driver/`) | Step 1 |
| 3 | Implement resource sampler (reuse from daemon-tpc harness) | None |
| 4 | Implement coordinator with warm-up/iteration/cool-down logic | Steps 2, 3 |
| 5 | Build tokio-transfer daemon variant (depends on ASY-7/8/10 PRs) | ASY-7..10 |
| 6 | Run initial comparison and calibrate iteration count | Steps 4, 5 |
| 7 | Add statistical analysis (bootstrap CI, Welch's t-test) | Step 6 |
| 8 | CI integration (PR tier + nightly tier) | Step 7 |
| 9 | Publish decision based on Section 8 criteria | Step 8 |

Steps 1-4 can proceed immediately against the threaded baseline alone
(useful for validating the harness and establishing baseline variance).
Step 5 is blocked on the tokio prototype landing behind its feature
flag.

## 11. Risks and mitigations

| Risk | Impact | Mitigation |
|------|--------|-----------|
| Loopback masks network latency effects | Bench irrelevant to WAN daemons | Add optional 1ms/10ms artificial latency via `tc netem` in a secondary run |
| Tmpfs eliminates disk bottleneck | Bench irrelevant to spinning-disk hosts | Document as intentional (isolates runtime model, not storage) |
| Tokio prototype incomplete at bench time | Cannot run comparison | Harness validates against threaded-only first; tokio tier enabled when ASY-7..10 land |
| Ephemeral port exhaustion at C1024 | False failures | Use network namespace with expanded port range or SO_REUSEADDR + TIME_WAIT override |
| Thermal throttling across iterations | Variance inflation | ABAB ordering, CPU frequency pinning (`cpupower frequency-set -g performance`) |
| Noisy neighbor in CI | Unreproducible results | Dedicated runner, `SCHED_BATCH`, resource sampler detects interference via involuntary context switch spikes |

## 12. Relationship to ASY-6 decision

ASY-6 deferred the adopt/close decision pending benchmark data. This
document specifies what that benchmark data looks like. The decision
criteria in Section 8 are the concrete exit conditions for the ASY-6
defer window:

- If Section 8.1 (adopt) is satisfied, ASY-6 exits to Option A.
- If Section 8.3 (close) is satisfied, ASY-6 exits to Option C.
- If neither is satisfied, ASY-6 remains in Option B (defer) with a
  targeted optimization cycle.

The 5% end-to-end uplift floor named in ASY-6 Section 1 maps to
Section 8.1 criterion #2 here (10% at scale), with the additional
constraint that low-concurrency must not regress (criterion #1). The
higher bar (10% vs 5%) reflects that the migration cost has become
better understood through ASY-7..10 design work: 10-14 PRs of churn
warrant a proportionally larger measured benefit.
