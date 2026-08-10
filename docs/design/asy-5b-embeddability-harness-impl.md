# ASY-5.b: Embeddability test harness implementation spec

Status: Implementation spec for #2993. Implements the harness designed
in ASY-5.a (`docs/design/asy-5-a-embeddability-test-harness.md`).
Produces the structured verdicts ASY-5.c (#2994) consumes.

Parent: ASY-5 (#2778).
Predecessor: ASY-5.a (harness specification).
Successor: ASY-5.c (#2994, user-facing gap documentation).
Adjacent: ASY-6 (`docs/design/asy-6-adopt-or-defer-decision.md`) -
the defer decision gates on ASY-5 verdicts.

## 1. Goal

Ship a self-contained test harness that embeds oc-rsync's public
client API inside a tokio runtime and produces machine-readable
verdicts for each of ASY-5.a's five scenarios. The verdicts answer
three questions the ASY-6 defer window needs:

1. Does `run_client` complete correctly inside `spawn_blocking` at
   scale?
2. At what concurrency does the tokio blocking pool saturate, and what
   happens when it does?
3. What resources leak when a transfer is cancelled mid-flight?

The harness is a measurement scaffold, not a refactor. It exercises the
existing synchronous API without changing it.

## 2. Public API surface under test

The harness calls two entry points from `oc_rsync_core::client`:

```rust
pub fn run_client(config: ClientConfig) -> Result<ClientSummary, ClientError>;
pub fn run_client_with_observer(
    config: ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError>;
```

Both are synchronous and block the calling thread for the full
transfer duration. `ClientConfig` is constructed via
`ClientConfigBuilder` with `transfer_args`, optional `recursive`,
`delete`, and other flags. `ClientSummary` reports `files_copied()`,
`bytes_transferred()`, and related counters.

The harness also observes global state that survives across calls:

- `BufferPool` (global `OnceLock`) - shared buffer allocator.
- SIMD feature probes (`OnceLock`-cached) - `is_x86_feature_detected!`
  results.
- Kernel feature probes - io_uring availability, copy_file_range
  support.

These must not corrupt or deadlock under concurrent access from
multiple `spawn_blocking` threads.

## 3. Feature gate and dependency additions

### 3.1 Cargo feature

Add to `crates/core/Cargo.toml`:

```toml
[features]
# ... existing features ...

# Embeddability test harness (ASY-5.b). Pulls tokio as a dev-dep for
# integration tests that exercise run_client inside an async runtime.
# Default off - never included in production builds.
tokio-embeddability-tests = []
```

The feature is a dev-only gate. It does not add any runtime dependency
to the library; it only controls `#[cfg(feature = "...")]` on test
files.

### 3.2 Dev-dependencies

Add to `crates/core/Cargo.toml` under `[dev-dependencies]`:

```toml
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time", "test-util"] }
axum = { version = "0.7", default-features = false, features = ["http1", "tokio"] }
reqwest = { version = "0.12", default-features = false, features = ["http1"] }
```

`tokio` is already a transitive dependency via the `async` /
`embedded-ssh` features, but the harness needs it unconditionally as a
dev-dep with `test-util` for time mocking.

`axum` and `reqwest` are S4-only. If the dev-dep footprint is
contested, S4 can substitute raw `hyper` 1.x server + client - the
scenario only needs constant-size `200 OK` responses.

`tempfile` and `filetime` are already in dev-dependencies.

### 3.3 What does NOT change

- No new runtime dependencies on the library itself.
- No changes to `run_client`, `ClientConfig`, or any production code.
- Default `cargo nextest run -p core` is unaffected (the feature is
  off by default).

## 4. Harness file layout

```
crates/core/tests/embeddability/
    mod.rs                       # #[cfg(feature = "tokio-embeddability-tests")]
    common/
        mod.rs                   # re-exports fixtures, snapshots, runtime, verdict
        fixtures.rs              # tempdir + source tree builders
        snapshots.rs             # thread / fd / temp-file snapshots (per-OS)
        runtime.rs               # tokio runtime builders with known thread caps
        verdict.rs               # structured verdict types + JSON serialization
    s1_single_tokio_task.rs
    s2_concurrent_tasks.rs
    s3_drop_cancellation.rs
    s4_interleaved_tokio_io.rs
    s5_reentrancy.rs
```

Each scenario file is a standalone integration test module. nextest
compiles each `tests/*.rs` file into a separate binary, so static
state (`OnceLock`, `BufferPool`) is isolated per scenario without
needing `harness = false` or custom runners.

The top-level `crates/core/tests/embeddability/mod.rs` is imported by
a thin `crates/core/tests/embeddability.rs` driver file:

```rust
// crates/core/tests/embeddability.rs
#![cfg(feature = "tokio-embeddability-tests")]
mod embeddability;
```

This follows the existing pattern in `crates/core/tests/` where each
`.rs` file at the top level compiles to one test binary.

## 5. Common infrastructure

### 5.1 `fixtures.rs` - test tree builder

Reuse the established `setup_test_dirs()` pattern from
`crates/engine/tests/`. Provide two fixture sizes:

- `small_fixture(n: usize)` - creates `n` files (default 200) with
  random 1-4 KB content in a `TempDir`. Used by S1, S3, S5.
- `medium_fixture(n: usize)` - creates `n` files (default 2000) with
  mixed sizes (1 KB to 256 KB). Used by S2, S4.

Each fixture returns `(TempDir, source_path, dest_path)`. The
`TempDir` handle keeps the directory alive; dropping it cleans up.

File content must vary in size to exercise the full transfer path
(delta matching, basis file reads, checksum computation). Use
deterministic pseudo-random content seeded from the file index to make
failures reproducible.

### 5.2 `snapshots.rs` - resource snapshots

Platform-abstracted resource measurement. Each snapshot captures a
triple `(thread_count, fd_count, temp_file_list)`.

```rust
pub struct ResourceSnapshot {
    pub thread_count: usize,
    pub fd_count: usize,
    pub temp_files: Vec<PathBuf>,
}

impl ResourceSnapshot {
    pub fn capture(dest_dir: &Path) -> Self;
    pub fn delta(&self, baseline: &ResourceSnapshot) -> ResourceDelta;
}

pub struct ResourceDelta {
    pub thread_delta: isize,
    pub fd_delta: isize,
    pub leaked_temp_files: Vec<PathBuf>,
}
```

Platform backends:

| Platform | Thread count | FD count | Temp files |
|----------|-------------|----------|------------|
| Linux | `/proc/self/status` `Threads:` field | `std::fs::read_dir("/proc/self/fd").count()` | glob `dest_dir` for `.oc-rsync.*`, `.~tmp~*` |
| macOS | `proc_pidinfo(PROC_PIDTASKINFO)` via `libc` | `proc_pidinfo(PROC_PIDLISTFDS)` via `libc` | same glob |
| Windows | `GetProcessHandleCount` / `NtQuerySystemInformation` | `GetProcessHandleCount` | same glob |

The macOS and Windows backends use only APIs already available through
`libc` (macOS) and `windows-sys` (Windows) - both already in the
dependency tree via `fast_io` and `metadata`. No new platform crates.

### 5.3 `runtime.rs` - tokio runtime builders

Provide factory functions that build tokio runtimes with explicit,
known thread counts so scenarios can reason about saturation points:

```rust
pub fn runtime_small() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(8)
        .enable_all()
        .build()
        .expect("tokio runtime")
}

pub fn runtime_default() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .max_blocking_threads(512)
        .enable_all()
        .build()
        .expect("tokio runtime")
}

pub fn runtime_constrained(workers: usize, blocking: usize) -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .max_blocking_threads(blocking)
        .enable_all()
        .build()
        .expect("tokio runtime")
}
```

Key: `max_blocking_threads` controls the ceiling the harness
measures against. The default tokio value is 512; making it explicit
means the harness owns the number and can vary it across scenario
runs.

### 5.4 `verdict.rs` - structured output

Each scenario produces a `Verdict`:

```rust
#[derive(Debug, Serialize)]
pub struct Verdict {
    pub scenario: String,
    pub outcome: Outcome,
    pub measurements: Vec<Measurement>,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub enum Outcome {
    Pass,
    PassWithCaveat(String),
    Fail(String),
}

#[derive(Debug, Serialize)]
pub struct Measurement {
    pub name: String,
    pub value: f64,
    pub unit: String,
}
```

Verdicts are serialized to JSON and written to
`target/embeddability-verdicts/<scenario>.json`. ASY-5.c reads these
files to generate user-facing documentation. The test runner also
asserts pass/fail so CI catches regressions.

`serde` and `serde_json` are already workspace dev-dependencies (used
by protocol golden tests). No new crate needed.

## 6. Scenario implementations

### 6.1 S1: Single transfer from a tokio task

**Purpose.** Prove that `run_client` completes when called directly
(no `spawn_blocking`) from an async context. Measure the blocking
window.

**Implementation.**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s1_single_transfer_blocking() {
    let (dir, src, dst) = small_fixture(200);

    // Spawn a canary task that increments a counter every 10ms.
    // If the runtime is blocked, the canary makes no progress.
    let canary = Arc::new(AtomicU64::new(0));
    let canary_clone = canary.clone();
    let canary_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(10)).await;
            canary_clone.fetch_add(1, Ordering::Relaxed);
        }
    });

    let canary_before = canary.load(Ordering::Relaxed);
    let start = Instant::now();

    // Direct blocking call from async context.
    let config = ClientConfig::builder()
        .transfer_args([&src, &dst])
        .recursive(true)
        .build();
    let summary = run_client(config).expect("transfer succeeds");

    let elapsed = start.elapsed();
    let canary_after = canary.load(Ordering::Relaxed);
    canary_handle.abort();

    // Verdicts:
    // - Transfer completed: pass/fail.
    // - Canary progress during transfer: if canary_after == canary_before,
    //   the runtime was fully blocked (expected with worker_threads=2
    //   minus the blocked worker = 1 worker for canary).
    // - Blocking window duration.
}
```

**Measurements emitted:**

| Name | Unit | Description |
|------|------|-------------|
| `transfer_wall_clock_ms` | ms | Total `run_client` wall time |
| `canary_ticks_during_transfer` | count | Canary increments while blocked |
| `worker_threads` | count | Runtime worker thread count (2) |
| `files_copied` | count | From `ClientSummary` |

**Pass criterion.** `run_client` returns `Ok`. Canary tick count and
blocking window recorded (may be zero ticks; that is expected, not a
failure).

**Fail criterion.** Panic, hang past 60 s, or `ClientError` not
attributable to missing fixtures.

### 6.2 S2: Concurrent transfers from N tokio tasks

**Purpose.** Find the N at which tokio's blocking pool saturates.
Prove no deadlock at or beyond saturation.

**Implementation.**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn s2_concurrent_transfers() {
    let concurrency_levels = [1, 8, 64, 256, 512, 600];

    for &n in &concurrency_levels {
        let rt = runtime_constrained(4, 512);
        // Build N independent fixture pairs.
        let fixtures: Vec<_> = (0..n)
            .map(|i| small_fixture_indexed(50, i))
            .collect();

        let start = Instant::now();
        let handles: Vec<_> = fixtures.iter().map(|(_, src, dst)| {
            let src = src.clone();
            let dst = dst.clone();
            tokio::task::spawn_blocking(move || {
                let config = ClientConfig::builder()
                    .transfer_args([&src, &dst])
                    .recursive(true)
                    .build();
                run_client(config)
            })
        }).collect();

        let results: Vec<_> = futures::future::join_all(handles).await;
        let elapsed = start.elapsed();

        // Classify: all Ok, any Err, any panic (JoinError).
        let successes = results.iter()
            .filter(|r| matches!(r, Ok(Ok(_))))
            .count();

        // Record throughput, completion time, whether queueing was
        // observed (elapsed >> n * single_transfer_time).
    }
}
```

The test uses `runtime_constrained` to set `max_blocking_threads`
explicitly. At N=512 the pool is fully occupied; at N=600 the excess
48 tasks queue. The harness records:

| Name | Unit | Description |
|------|------|-------------|
| `concurrency_n` | count | Number of parallel transfers |
| `wall_clock_ms` | ms | Total elapsed for all N |
| `successes` | count | Transfers returning `Ok` |
| `failures` | count | Transfers returning `Err` |
| `panics` | count | `JoinError::is_panic()` results |
| `mean_per_transfer_ms` | ms | `wall_clock / n` |
| `blocking_pool_ceiling` | count | `max_blocking_threads` setting |

**Pass criterion.** All N transfers return `Ok` at every concurrency
level. No deadlock. The harness records the smallest N at which
wall-clock time exceeds `single_transfer_time * 1.5` (indicating
queueing). Results reproducible across 3 runs.

**Fail criterion.** Any transfer hangs (120 s timeout per level).
Cross-task data corruption (one `ClientSummary` reflects another's
bytes). Blocking-pool exhaustion that prevents `tokio::task::yield_now`
from progressing.

**Shared-pool competition note.** The russh SSH transport already
consumes blocking-pool threads via its own `spawn_blocking` calls
(see `project_russh_spawn_blocking_ceiling`). S2 uses local transfers
to isolate the measurement from russh's budget. A follow-up variant
(S2b, out of scope for initial implementation) would add SSH
transfers to measure the combined ceiling.

### 6.3 S3: Transfer cancellation via Drop

**Purpose.** Verify that dropping a `spawn_blocking` transfer
mid-flight does not leak threads, file descriptors, or temp files.

**Implementation.**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s3_drop_cancellation() {
    let (dir, src, dst) = medium_fixture(2000);
    let baseline = ResourceSnapshot::capture(&dst);

    // Start a transfer, then cancel it after 500ms.
    let src_clone = src.clone();
    let dst_clone = dst.clone();
    let handle = tokio::task::spawn_blocking(move || {
        let config = ClientConfig::builder()
            .transfer_args([&src_clone, &dst_clone])
            .recursive(true)
            .build();
        run_client(config)
    });

    // Race: transfer vs timeout.
    tokio::select! {
        result = handle => {
            // Transfer completed before timeout - still valid,
            // just means the fixture was too small. Record as
            // PassWithCaveat("transfer completed before cancel window").
        }
        _ = tokio::time::sleep(Duration::from_millis(500)) => {
            // handle is dropped here, which drops the JoinHandle.
            // Note: dropping a JoinHandle does NOT cancel
            // spawn_blocking - the blocking task runs to completion.
            // This is a known tokio limitation. The harness must
            // record this as a finding.
        }
    }

    // Wait for the settle window (background threads to join).
    tokio::time::sleep(Duration::from_secs(2)).await;

    let after = ResourceSnapshot::capture(&dst);
    let delta = after.delta(&baseline);

    // Verdicts.
    assert!(delta.thread_delta <= 0, "thread leak: {}", delta.thread_delta);
    assert_eq!(delta.fd_delta, 0, "fd leak: {}", delta.fd_delta);
    assert!(delta.leaked_temp_files.is_empty(),
        "temp files leaked: {:?}", delta.leaked_temp_files);
}
```

**Critical implementation detail.** Dropping a `JoinHandle` from
`spawn_blocking` does not cancel the task - tokio's blocking pool runs
tasks to completion regardless. True cancellation would require a
cooperative cancellation token threaded through `run_client`, which
does not exist today. The harness must:

1. Record this as a finding in the verdict: "spawn_blocking tasks
   are not cancellable via Drop; the blocking thread runs to
   completion even after the JoinHandle is dropped."
2. Verify that once the transfer does complete (naturally, after the
   handle is dropped), resources are cleaned up.
3. For the `ApplierStillReferenced` case: if the transfer errors with
   this variant, classify it as a documented-error outcome per
   ASY-5.a section 2.3, not a leak.

**Measurements emitted:**

| Name | Unit | Description |
|------|------|-------------|
| `cancel_requested_at_ms` | ms | When the select! chose the timeout arm |
| `transfer_completed_at_ms` | ms | When the blocking task actually finished |
| `thread_delta_after_settle` | count | Threads above baseline after 2 s |
| `fd_delta_after_settle` | count | FDs above baseline after 2 s |
| `leaked_temp_file_count` | count | Orphaned `.oc-rsync.*` / `.~tmp~*` files |
| `spawn_blocking_cancellable` | bool | Always `false` with current tokio |

**Pass criterion.** Thread delta back to baseline within 2 s. FD delta
zero. No temp files. `ApplierStillReferenced` (if returned) classified
as documented error.

**Fail criterion.** Persistent thread or FD delta. Orphaned temp
files. Panic or undefined behaviour.

### 6.4 S4: Transfer interleaved with tokio I/O

**Purpose.** Measure whether a concurrent `run_client` transfer
degrades latency of other tokio tasks (an HTTP server on the same
runtime).

**Implementation.**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn s4_interleaved_tokio_io() {
    // 1. Stand up a minimal axum server on the runtime.
    let app = axum::Router::new()
        .route("/health", axum::routing::get(|| async { "ok" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let url = format!("http://{}/health", addr);

    // 2. Baseline: measure P50/P95/P99 over 1000 requests, no transfer.
    let baseline_latencies = measure_latencies(&client, &url, 1000).await;

    // 3. Start a transfer via spawn_blocking.
    let (dir, src, dst) = medium_fixture(2000);
    let transfer = tokio::task::spawn_blocking(move || {
        let config = ClientConfig::builder()
            .transfer_args([&src, &dst])
            .recursive(true)
            .build();
        run_client(config)
    });

    // 4. During-transfer: measure latencies while transfer is in flight.
    let during_latencies = measure_latencies(&client, &url, 1000).await;

    // 5. Wait for transfer, then measure post-transfer.
    let _ = transfer.await;
    let after_latencies = measure_latencies(&client, &url, 1000).await;
```

Where `measure_latencies` is a helper that fires N sequential HTTP
requests, records each round-trip duration, and computes percentiles.

**Measurements emitted:**

| Name | Unit | Description |
|------|------|-------------|
| `baseline_p50_us` | us | Baseline P50 latency |
| `baseline_p95_us` | us | Baseline P95 latency |
| `baseline_p99_us` | us | Baseline P99 latency |
| `during_transfer_p50_us` | us | P50 during transfer |
| `during_transfer_p95_us` | us | P95 during transfer |
| `during_transfer_p99_us` | us | P99 during transfer |
| `after_transfer_p50_us` | us | P50 after transfer |
| `after_transfer_p95_us` | us | P95 after transfer |
| `after_transfer_p99_us` | us | P99 after transfer |
| `p99_degradation_ratio` | ratio | `during_p99 / baseline_p99` |

**Pass criterion.** P99 during transfer < 2x baseline P99. Server
accepts every request. No connection drops.

**Fail criterion.** P99 >= 2x baseline. Any request returns 5xx.
Server stops accepting connections.

**Why `spawn_blocking` should pass S4.** Unlike S1 (which calls
`run_client` directly from an async context, blocking a worker
thread), S4 uses `spawn_blocking` correctly. The transfer runs on the
blocking pool, not a worker thread, so the 4 worker threads remain
available for HTTP serving. The scenario validates this expectation.
Failure would indicate that `run_client` is doing something that
contends with the worker threads beyond just CPU time (e.g., holding
a lock that a worker thread needs).

### 6.5 S5: Re-entrancy

**Purpose.** Determine whether `run_client` can be called from inside
a callback or completion handler of another `run_client` invocation.

**Implementation.**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn s5_reentrancy() {
    let (dir_outer, src_outer, dst_outer) = small_fixture(50);
    let (dir_inner, src_inner, dst_inner) = small_fixture(10);

    let result = tokio::task::spawn_blocking(move || {
        let config = ClientConfig::builder()
            .transfer_args([&src_outer, &dst_outer])
            .recursive(true)
            .build();
        let summary = run_client(config)?;

        // Immediately re-enter from the same thread after completion.
        let inner_config = ClientConfig::builder()
            .transfer_args([&src_inner, &dst_inner])
            .recursive(true)
            .build();
        let inner_summary = run_client(inner_config)?;

        Ok::<_, ClientError>((summary, inner_summary))
    }).await.unwrap();

    match result {
        Ok((outer, inner)) => {
            // Pass A: both transfers completed.
        }
        Err(e) => {
            // Pass B: typed error indicating re-entrancy not supported.
            // Fail: panic would be caught by JoinHandle as JoinError.
        }
    }
}
```

**Note on scope.** ASY-5.a specifies re-entrancy from inside a
callback (e.g., `Drop` impl on a value in `ClientSummary`). The
current public API does not expose mid-transfer callbacks - only the
`ClientProgressObserver` trait, which receives `&mut self` references
and cannot easily call `run_client` (which takes ownership of
`ClientConfig`). The harness therefore tests the simpler case:
sequential re-entrancy on the same thread (outer completes, then inner
starts). This exercises `OnceLock`, `BufferPool`, and any
thread-local state that might not be re-entrant.

A stronger re-entrancy test (calling from inside
`ClientProgressObserver::on_progress`) can be added if the observer
API is extended to support it.

**Measurements emitted:**

| Name | Unit | Description |
|------|------|-------------|
| `outer_transfer_ok` | bool | Outer transfer succeeded |
| `inner_transfer_ok` | bool | Inner transfer succeeded |
| `reentrancy_supported` | bool | Both succeeded without error |
| `error_if_any` | string | Error message if either failed |

**Pass criterion.** Either both transfers complete (Pass A) or the
inner transfer returns a typed `ClientError` (Pass B).

**Fail criterion.** Panic, deadlock, undefined behaviour, or silent
data loss.

## 7. Comparison baseline: standalone vs embedded

Each scenario measures embedded-in-tokio performance. To contextualize
the numbers, S1 and S2 include a standalone baseline run:

```rust
fn standalone_baseline(src: &Path, dst: &Path) -> (Duration, ClientSummary) {
    let start = Instant::now();
    let config = ClientConfig::builder()
        .transfer_args([src, dst])
        .recursive(true)
        .build();
    let summary = run_client(config).expect("standalone succeeds");
    (start.elapsed(), summary)
}
```

This runs `run_client` on a plain OS thread (no tokio runtime). The
comparison is:

| Metric | Standalone | Embedded (S1) | Embedded (S2, N=1) |
|--------|-----------|---------------|-------------------|
| Wall clock | baseline | expected ~same | expected ~same |
| Thread overhead | 0 extra | tokio worker + blocking threads | same + N blocking threads |

The comparison is informational, not pass/fail. It quantifies the
overhead of the tokio bridge so ASY-6 can reference concrete numbers.

## 8. Verdict aggregation

After all five scenarios complete, the harness writes a summary
verdict file:

```
target/embeddability-verdicts/
    s1_single_tokio_task.json
    s2_concurrent_tasks.json
    s3_drop_cancellation.json
    s4_interleaved_tokio_io.json
    s5_reentrancy.json
    summary.json
```

`summary.json` aggregates the per-scenario outcomes:

```json
{
  "harness_version": "0.1.0",
  "platform": "x86_64-unknown-linux-gnu",
  "scenarios": {
    "s1": "Pass",
    "s2": "PassWithCaveat: blocking pool saturates at N=512",
    "s3": "PassWithCaveat: spawn_blocking not cancellable via Drop",
    "s4": "Pass",
    "s5": "Pass"
  },
  "blocking_pool_saturation_n": 512,
  "cancellation_supported": false,
  "reentrancy_supported": true
}
```

ASY-5.c consumes this file to generate the user-facing
`docs/user/embeddability.md`.

## 9. CI integration

### 9.1 Workflow

Add a new workflow `.github/workflows/embeddability.yml`:

```yaml
name: Embeddability (ASY-5)
on:
  push:
    branches: [master]
  pull_request:
    labels: [embeddability]
  workflow_dispatch:

jobs:
  embeddability:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: taiki-e/install-action@nextest
      - name: Run embeddability harness
        run: |
          cargo nextest run -p core \
            --features tokio-embeddability-tests \
            -E 'test(embeddability::)' \
            --color never
      - name: Upload verdicts
        uses: actions/upload-artifact@v4
        with:
          name: embeddability-verdicts-${{ matrix.os }}
          path: target/embeddability-verdicts/
```

### 9.2 CI policy

- The workflow does not gate every PR. It runs on master pushes,
  on PRs labeled `embeddability`, and on manual dispatch.
- Allowed to be slower than the default test suite (S2 at N=600
  creates 600 temp directories).
- Timeout: 30 minutes per OS.

## 10. Implementation sequence

Deliver in 3 PRs to isolate failures and keep reviews focused:

### PR 1: Common infrastructure + S1 + S5

- Add `tokio-embeddability-tests` feature to `crates/core/Cargo.toml`.
- Add `tokio`, `axum`, `reqwest` dev-dependencies.
- Implement `common/` (fixtures, snapshots, runtime, verdict).
- Implement S1 (single transfer) and S5 (re-entrancy).
- S1 and S5 are the simplest scenarios and validate that the
  infrastructure works end to end.

### PR 2: S2 + S3

- Implement S2 (concurrent transfers) - the primary measurement
  scenario.
- Implement S3 (cancellation) - the primary resource-safety scenario.
- These depend on fixtures and snapshots from PR 1.

### PR 3: S4 + CI workflow + verdict aggregation

- Implement S4 (interleaved I/O) - requires axum/reqwest.
- Add `.github/workflows/embeddability.yml`.
- Add `summary.json` aggregation logic.
- This is the final PR that completes the harness.

## 11. Expected findings

Based on the current architecture (`project_no_async_threaded_only`)
and known constraints, the harness is expected to produce:

1. **S1: Pass.** `run_client` blocks the calling thread but completes.
   The canary shows reduced ticks (1 of 2 workers blocked).

2. **S2: PassWithCaveat.** All transfers complete at every concurrency
   level, but wall-clock time inflects at N=512 (the default
   `max_blocking_threads`). The caveat: each concurrent transfer
   consumes one blocking-pool thread for its full duration. An
   embedder running 500 transfers plus russh SSH connections is at risk
   of pool exhaustion.

3. **S3: PassWithCaveat.** Resources clean up after the transfer
   completes naturally, but `spawn_blocking` tasks are not cancellable
   via `JoinHandle` drop. The transfer runs to completion even when the
   caller has moved on. This is a tokio limitation, not an oc-rsync
   bug, but the embedder must be aware.

4. **S4: Pass.** HTTP P99 stays within 2x baseline because
   `spawn_blocking` correctly offloads the transfer to the blocking
   pool, leaving worker threads free for async I/O.

5. **S5: Pass.** Sequential re-entrancy works because `run_client` has
   no re-entrancy guards. `BufferPool` and `OnceLock` probes are
   thread-safe and re-entrant. The global buffer pool is shared across
   calls, which is correct behaviour.

## 12. Success criteria for ASY-5.b

ASY-5.b is complete when:

1. All five scenarios compile and run on Linux, macOS, and Windows.
2. Each scenario produces a JSON verdict in
   `target/embeddability-verdicts/`.
3. CI workflow runs on all three platforms without timeout.
4. The verdicts are consistent across 3 consecutive runs (no flaky
   outcomes).
5. The summary verdict is sufficient for ASY-5.c to write
   `docs/user/embeddability.md` without re-running the harness.

ASY-5.b does not need to fix any finding. It only needs to measure and
classify. Fixes belong to ASY-7+ or standalone tickets.

## 13. Risks and mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| S2 at N=600 is slow in CI | 30+ min CI job | Reduce fixture size at high N (50 files per pair, not 200). Total file count caps at 30k across all pairs. |
| S4 latency measurement is noisy on shared CI runners | False fail | Use P99 ratio (during/baseline) not absolute values. Run 3 iterations, take median. Allow 3x ratio on CI (stricter 2x locally). |
| `ResourceSnapshot` platform backends are fragile | S3 false positives on unfamiliar OS versions | Fall back to `PassWithCaveat("snapshot unavailable on this platform")` rather than hard-failing. |
| tokio version bump changes blocking pool semantics | Verdicts become stale | Pin tokio `~1.x` in dev-deps. Re-run harness on tokio upgrades. |
| `BufferPool` `OnceLock` leaks across scenarios | Cross-test pollution | nextest compiles each top-level test file as a separate binary - no shared process state. Verified by the layout in section 4. |

## 14. Non-goals

- **Async-native API.** The harness does not propose or prototype an
  async `run_client`. That belongs to ASY-7+.
- **Performance optimization.** The harness measures overhead, not
  optimizes it. Performance work belongs to ASY-4 and post-v0.5.9.
- **russh interaction.** S2 uses local transfers to isolate the
  measurement. SSH-transport interaction with the blocking pool is a
  known issue (`project_russh_spawn_blocking_ceiling`) but out of scope
  for initial implementation.
- **Daemon embeddability.** The harness tests client-side embedding
  only. Daemon embedding (running `oc-rsyncd` inside a tokio service)
  is a separate concern for a future ASY ticket.

## 15. Cross-references

- `docs/design/asy-5-a-embeddability-test-harness.md` - the harness
  specification this document implements.
- `docs/design/asy-6-adopt-or-defer-decision.md` - the defer decision
  whose exit criteria depend on ASY-5 verdicts.
- `docs/design/asy-2-tokio-runtime-feature.md` - the `tokio-transfer`
  feature gate that a future async API would live behind.
- `docs/design/asy-3-async-boundary-spec.md` - per-boundary contracts
  showing 4 `spawn_blocking` islands and 6 `.await` conversions.
- `project_no_async_threaded_only` - the standing constraint that
  motivates the harness.
- `project_russh_spawn_blocking_ceiling` - shared blocking-pool
  competition, documented in S2 caveat.
- `project_finish_file_arc_unwrap_ergonomics` - the
  `ApplierStillReferenced` error S3 must classify.
