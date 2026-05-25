# ASY-5.a: Embeddability test harness specification

Status: Specification. ASY-5.a defines the harness; ASY-5.b implements it;
ASY-5.c writes the user-facing gap-list from the verdicts ASY-5.b produces.
This document does not commit any implementation work and is independent
of the ASY-4 benchmark outcome and the ASY-6 adopt/defer decision: it is
a measurement scaffold, not a refactor.

Cross-links:

- `project_no_async_threaded_only` - oc-rsync currently uses blocking
  channels and OS threads end to end. Embedding inside a tokio runtime
  forces every call site through `tokio::task::spawn_blocking`, which
  defeats the purpose of running on an async runtime. The harness must
  surface that cost in numbers, not adjectives.
- `project_finish_file_arc_unwrap_ergonomics` - cancellation paths today
  return `ApplierStillReferenced` as a typed error rather than a barrier
  or drain primitive. Scenario S3 exercises exactly that path.
- `project_russh_spawn_blocking_ceiling` - the russh transport already
  bridges to tokio through `spawn_blocking`. Scenario S2 needs to be aware
  that the blocking pool is a shared resource, not an oc-rsync-private
  one.

## 1. Scope

ASY-5.a specifies the harness that proves - or disproves - the claim
that `oc-rsync` runs cleanly as a library inside an external tokio
application. The harness is a **capability** test, not a performance
test: it asks "does it work, and what does it leak / block / starve
when it does?", not "how fast is it?". Performance characterisation
belongs to ASY-12.

In scope:

- Driving the existing public entry points (`core::client::run_client`,
  `core::client::run_client_with_observer`) from within a tokio runtime.
- Observing side effects the embedder cares about: blocking-pool
  saturation, file-descriptor leaks, background-thread leaks, reactor
  starvation, re-entrancy safety.

Out of scope:

- Any redesign of the public API surface. The harness only exercises
  what `oc_rsync_core` already exposes.
- Async-native transports. Whether to add them is an ASY-6 decision,
  not an ASY-5 one.
- Performance regressions vs the threaded baseline. ASY-4 owns that.
- Library-API documentation for end users. ASY-5.c owns that.

Public entry points the harness will call (confirmed in
`crates/core/src/client/run/mod.rs`):

```rust
pub fn run_client(config: ClientConfig) -> Result<ClientSummary, ClientError>;
pub fn run_client_with_observer(
    config: ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError>;
```

Both are synchronous. The harness must therefore wrap every call site
either in a dedicated OS thread or in `tokio::task::spawn_blocking`,
and must record which strategy each scenario uses.

## 2. Test scenarios

Five concrete scenarios. Each is independent, lives in its own test
file, and produces a per-scenario verdict that ASY-5.c can quote.

### S1: Single transfer from a tokio task

Spin up `#[tokio::main(flavor = "multi_thread", worker_threads = 2)]`,
call `run_client` directly from inside an `async fn`, transfer a small
fixture tree (a few hundred files), and observe what happens.

- Mechanism: synchronous call from an async context, no
  `spawn_blocking`.
- Expected: completes, but the calling worker thread is blocked for
  the entire transfer duration. With `worker_threads = 2` the runtime
  has one thread left; with `worker_threads = 1` the reactor is
  fully blocked.
- Record: wall-clock duration, whether other spawned tasks made any
  progress concurrently, and whether the runtime dropped any wakeups.

### S2: Concurrent transfers from N tokio tasks

Run N parallel `tokio::task::spawn_blocking(|| run_client(cfg.clone()))`
calls against N independent source/dest pairs. Vary N across
`{1, 8, 64, 256, 512, 600}`.

- Validate: every transfer completes; completion order is consistent
  with task scheduling; each call observes its own working state
  (no cross-pollution of `ClientSummary`, no shared mutable surprise
  from `OnceLock` pools).
- Measure: the N at which the tokio blocking pool starts queueing.
  The default `tokio::runtime::Builder::max_blocking_threads` is 512;
  the harness must detect both the pre-saturation regime
  (`N < 512`, all run concurrently) and the saturated regime
  (`N > 512`, FIFO queueing kicks in). Crossing the ceiling without
  deadlock is a pass; deadlock is a fail.
- Capture: `russh` already consumes blocking-pool budget, so an
  embedder running both SSH transports and other blocking work
  competes with itself. The harness writes this competition into
  the verdict even though it is not the harness's job to fix.

### S3: Transfer cancellation via Drop

Wrap the call in a `tokio::select!` against a short
`tokio::time::sleep`, so the transfer future is dropped mid-flight.

- Verify after the cancellation completes:
  - **Thread count.** Sample `/proc/self/status:Threads` (Linux),
    `task_info()` / `proc_pidinfo` (macOS), and the
    `GetProcessHandleCount` family on Windows before and after.
    Delta must return to baseline within a bounded settle window
    (target: 2 s).
  - **File descriptors.** Snapshot `/proc/self/fd` on Linux,
    `lsof -p $$` on macOS, handle counts on Windows. Delta must be
    zero.
  - **Partial files.** No `.oc-rsync.*` or `.~tmp~*` temp files left
    in the destination tree.
- Known fragility: `finish_file` can return
  `ApplierStillReferenced` if outstanding work is still in flight at
  the moment of cancellation. The harness must classify that as a
  documented-error outcome, not a leak, and ASY-5.c folds it into the
  user-facing gap list. See `project_finish_file_arc_unwrap_ergonomics`.

### S4: Transfer interleaved with tokio I/O

Stand up a minimal HTTP server on the same tokio runtime (the harness
should pick `axum` for ergonomics; `hyper` directly is acceptable if
`axum` is rejected as a dev-dep). The server serves a fixed handler
that returns `200 OK` in O(1).

- Drive load against the server with `reqwest` or raw `hyper::Client`
  - a tight loop measuring per-request latency.
- Concurrently, run a moderate transfer via `spawn_blocking`.
- Record HTTP P50 / P95 / P99 latency during three windows:
  baseline (no transfer), during transfer, after transfer.
- Pass: P99 during transfer < 2x baseline P99. Fail: any latency
  window stalls past 100 ms or the server stops accepting connections.

### S5: Re-entrancy

Register a post-transfer callback (using whatever observer or hook the
public API exposes; if none exists today, the harness will call
`run_client` from inside a `Drop` impl placed on a value owned by the
first transfer's `ClientSummary`).

- Inner call: a tiny transfer against a different source/dest pair.
- Acceptable outcomes:
  - **Pass A:** inner call completes cleanly. Document that
    re-entrancy is supported.
  - **Pass B:** inner call returns a typed `ClientError` variant
    that says "already inside a transfer". Document the error.
- Failure: panic, deadlock, undefined behaviour, or silent data loss.

## 3. Harness file layout

```
crates/core/tests/embeddability/
    mod.rs                       # gates all tests behind the cargo feature
    common/
        mod.rs                   # re-exports helpers
        fixtures.rs              # tempdir + source-tree builder
        snapshots.rs             # thread / fd / file snapshots, per OS
        runtime.rs               # tokio runtime builders with known caps
    s1_single_tokio_task.rs
    s2_concurrent_tasks.rs
    s3_drop_cancellation.rs
    s4_interleaved_tokio_io.rs
    s5_reentrancy.rs
```

- One file per scenario so a failure isolates to that scenario and
  does not poison the others through shared static state. This matches
  the project rule "modularity: each module does one thing well".
- The `common/` module owns shared scaffolding. `fixtures.rs` uses
  `tempfile::TempDir` and the established `setup_test_dirs()` pattern
  already used across `crates/engine/tests/` and `crates/core/src/client/tests/`.
- `snapshots.rs` is the only file that needs `#[cfg(...)]` per OS;
  each scenario file stays portable.

Test fixtures must follow the in-repo convention: small synthetic
trees built per test, never relying on workspace state, and always
inside a `TempDir` that is dropped at the end of the test.

## 4. Pass / fail criteria

| Scenario | Pass criterion | Fail criterion |
|----------|----------------|----------------|
| S1 | `run_client` returns `Ok(summary)`; no panic; harness records the blocking-window duration and the number of other tasks that ran during it (may be zero). | Panic, hang past a generous timeout (60 s for the fixture tree), or `ClientError` other than ones already documented for missing fixtures. |
| S2 | All N transfers return `Ok`; no deadlock; harness records the smallest N at which tokio's blocking pool starts queueing; results are reproducible across runs. | Any transfer hangs; cross-task data corruption (one `ClientSummary` reflects another's bytes); blocking-pool exhaustion that prevents `tokio::task::yield_now` from progressing. |
| S3 | Thread-count delta back to baseline within 2 s of drop; fd delta zero; no temp files; `ApplierStillReferenced` (if returned) classified as documented-error not as leak. | Persistent thread or fd delta; orphaned temp files; UB; panic. |
| S4 | HTTP P99 latency during transfer < 2x baseline P99; server accepts every request; no connection drops. | P99 >= 2x baseline; any request returns 5xx; server stops accepting. |
| S5 | Inner `run_client` returns `Ok` (Pass A) or returns a documented typed `ClientError` (Pass B). | Panic, deadlock, UB, or silent data loss (e.g., inner transfer reports success but destination is empty). |

All scenarios must additionally satisfy:

- No `unsafe` introduced in the harness; the `unsafe_code` deny stays.
- No reliance on `sleep` to mask a race; settle windows are bounded
  and explicit.
- Each test is hermetic: tempdirs only, no `/tmp` collisions, no
  env-var mutation outside `EnvGuard`.

## 5. Test dependencies

Dev-dependencies the harness adds to `crates/core/Cargo.toml`, gated
behind a new `tokio-embeddability-tests` feature so default builds
remain unaffected:

```toml
[dev-dependencies]
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time", "test-util"] }
tempfile = { workspace = true }                    # already present
axum = { version = "0.7", default-features = false, features = ["http1", "tokio"] }
reqwest = { version = "0.12", default-features = false, features = ["http1"] }

[features]
tokio-embeddability-tests = []
```

S3 needs no extra dependency: thread and fd inspection uses `std::fs`
on `/proc/self/{status,fd}` (Linux), `libproc` already in the tree via
`fast_io` (macOS), and the `windows-sys` re-export from `fast_io`
(Windows). The harness must not pull a new platform crate just for
snapshots.

S4 may substitute `hyper` 1.x for `axum` if the dev-dep footprint
needs to shrink; the scenario only needs a server that returns
constant-size `200 OK`.

## 6. Failure modes the harness must catch

The harness exists because the current implementation has several
known sharp edges. Each scenario is wired to surface one or more
of them:

1. **Tokio worker-thread starvation from blocking calls** (S1, S4).
   `run_client` blocks for the full transfer duration. On a
   single-worker runtime that is total starvation; on a multi-worker
   runtime that is N-1/N degradation per concurrent transfer.
2. **Tokio blocking-pool exhaustion** (S2). Default 512 threads, shared
   with every other `spawn_blocking` in the embedder's process,
   including russh's own bridge
   (`project_russh_spawn_blocking_ceiling`).
3. **File-descriptor leaks on Drop / panic paths** (S3). Cancellation
   today does not always join every background thread before
   returning; the harness must catch any fd that escapes.
4. **Background thread leaks** (S3). The generator / reorder / spill
   threads must all terminate when the orchestrator drops; any
   `JoinHandle` we forget to await is a leak.
5. **Static state poisoning between tests** (all scenarios). The
   global `BufferPool` and the `OnceLock`-cached SIMD / kernel-feature
   probes survive across tests in the same binary. The harness must
   either run each scenario in a fresh process (`harness = false` +
   custom runner) or assert that static state is benign across
   reuse. Default plan: separate test files give nextest separate
   binaries.
6. **Re-entrancy unsafety** (S5). Calling `run_client` from inside a
   callback fired by another `run_client` invocation is undefined
   today; the harness must produce a verdict either way.

## 7. ASY-5.b implementation outline

ASY-5.b is the implementation ticket (#2993). Recommended sequence:

1. Build `crates/core/tests/embeddability/common/` first - fixtures,
   snapshots, runtime builders. Land it as a no-op PR that only adds
   the helpers and a smoke test.
2. Add the cargo feature `tokio-embeddability-tests` and gate the
   entire `tests/embeddability/` tree behind it. Default `cargo
   nextest run -p core` must be unaffected.
3. Implement scenarios in order S1 -> S2 -> S3 -> S4 -> S5. Each in
   its own PR so a failing scenario does not block the next one.
4. CI: add a job that runs

   ```sh
   cargo nextest run -p core --features tokio-embeddability-tests \
       -E 'test(embeddability::)'
   ```

   on Linux, macOS, and Windows. The job is allowed to be slower
   than the default suite; it should not gate every PR. Wire it as a
   nightly or label-gated workflow.
5. Each scenario writes its verdict (pass / pass-with-caveat / fail)
   to a structured output the ASY-5.c writer can consume.

## 8. ASY-5.c documentation outline

ASY-5.c (#2994) consumes the verdicts ASY-5.b produces and writes
`docs/user/embeddability.md`. Expected content:

- A short "can I embed oc-rsync in my tokio app?" summary table -
  one row per scenario, one column for each supported OS.
- The known-good usage pattern (always `spawn_blocking`, always one
  call per task, never re-enter from a callback) with example code.
- The known sharp edges, framed in user terms ("each concurrent
  transfer occupies one tokio blocking-pool thread for its full
  duration").
- Pointers back to ASY-5.b's machine-readable verdict file for
  anyone who wants to re-run the suite locally.

ASY-5.c is not in scope for this spec; the pointer here only exists
so reviewers can see the full chain.

## 9. Cross-references

Memory notes this spec depends on, listed once with their reasons:

- `project_no_async_threaded_only` - the underlying constraint that
  motivates the entire ASY-5 series.
- `project_finish_file_arc_unwrap_ergonomics` - the typed error S3
  must classify, not treat as a leak.
- `project_russh_spawn_blocking_ceiling` - the shared blocking pool
  that S2 must measure against, not in isolation.

Parent and follow-ups:

- Parent ticket: ASY-5 (#2778).
- Implementation follow-up: ASY-5.b (#2993).
- Documentation follow-up: ASY-5.c (#2994).
- Adjacent: ASY-4 (#2776, async vs threaded benchmark), ASY-6
  (`docs/design/asy-6-adopt-or-defer-decision.md`, adopt / defer).
