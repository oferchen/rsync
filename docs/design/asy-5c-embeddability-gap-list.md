# ASY-5.c: Embeddability gap-list

Status: Design spec (#2994). Documents the gaps that prevent oc-rsync
from being cleanly embeddable as a library inside a tokio application.
Consumes the findings from ASY-5.a (harness spec) and ASY-5.b (harness
implementation). Feeds into ASY-6
(`docs/design/asy-6-adopt-or-defer-decision.md`) as evidence for the
adopt/defer/close decision.

Parent: ASY-5 (#2778).
Predecessor: ASY-5.b (#2993, harness implementation spec).
Successor: ASY-6 (re-evaluation with ASY-5 evidence on file).

## 1. Goal

Produce a priority-ordered inventory of every architectural property
that makes oc-rsync difficult to embed inside a host tokio application.
Each gap states what the problem is, why it matters to an embedder,
what the fix would cost, and whether the fix is gated on the ASY-6
adopt decision or can be pursued independently.

The audience is a developer who wants to call `run_client` from inside
their own async service and needs to know what will break, what will
degrade, and what workarounds exist today.

## 2. Embedding model

The expected embedding pattern is:

```rust
// Host application owns the tokio runtime.
let rt = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(8)
    .max_blocking_threads(512)
    .build()?;

rt.block_on(async {
    // Each transfer is offloaded to the blocking pool.
    let handle = tokio::task::spawn_blocking(move || {
        let config = ClientConfig::builder()
            .transfer_args(["source/", "dest/"])
            .recursive(true)
            .build();
        run_client(config)
    });
    let summary = handle.await??;
});
```

Every gap below is evaluated against this pattern. "Embedder" means
the host application that owns the runtime.

## 3. Gap inventory

### G1: Synchronous-only public API

**Severity:** High.
**Crate:** `core`.
**Entry points:** `run_client`, `run_client_with_observer`.

Both public entry points are synchronous and block the calling thread
for the full transfer duration. There is no `async fn run_client`
alternative. An embedder must wrap every call in
`tokio::task::spawn_blocking`, which moves the work to the tokio
blocking pool.

**Impact.** Each concurrent transfer consumes one blocking-pool thread
for its entire lifetime - typically seconds to minutes. Tokio's default
`max_blocking_threads` is 512. An embedder running N concurrent
transfers plus other blocking work (database queries, russh SSH
sessions) competes for the same pool. At saturation, new
`spawn_blocking` calls queue in FIFO order with no backpressure signal
to the caller.

**Workaround.** Use `spawn_blocking` and size `max_blocking_threads`
to accommodate the expected peak concurrency. Monitor the tokio
blocking-pool queue depth if the runtime exposes it (currently it does
not; tokio #4862).

**Fix.** Provide an `async fn run_client_async` that yields at wire
I/O boundaries (ASY-3 boundaries 1, 2, 4, 5) and offloads disk I/O
to `spawn_blocking` internally. This is the ASY-7+ conversion path
and is gated on the ASY-6 adopt decision.

**Effort:** Large (6+ PRs, ASY-7..12 series).
**Independent of ASY-6:** No. The async API is the core deliverable
of the adopt path.

---

### G2: No cooperative cancellation

**Severity:** High.
**Crate:** `core`.

`run_client` accepts no cancellation token, shutdown flag, or progress
callback that can request early termination. Once a transfer starts,
it runs to completion or fails on its own.

Dropping the `JoinHandle` returned by `spawn_blocking` does not cancel
the underlying task - tokio runs blocking tasks to completion
regardless of handle lifetime. This is a tokio design constraint, not
an oc-rsync bug, but the combination means an embedder cannot cancel a
transfer without killing the process.

**Impact.** An HTTP server that needs to cancel a transfer on client
disconnect has no mechanism to do so. The transfer continues consuming
CPU, I/O bandwidth, and a blocking-pool thread until it finishes
naturally. In the worst case (large recursive transfer), this can take
minutes after the embedder has moved on.

ASY-5.b scenario S3 confirmed this finding: the `tokio::select!` arm
that drops the handle does not stop the transfer. The blocking thread
runs to natural completion. Resources (threads, FDs, temp files) are
cleaned up only after the transfer finishes, not at the point of
cancellation.

**Workaround.** None that is clean. The embedder can set aggressive
timeouts on the underlying transport (SSH, daemon TCP) to bound the
worst-case tail, but this affects all transfers, not just cancelled
ones.

**Fix.** Thread a `CancellationToken` (or `AtomicBool` shutdown flag)
through the transfer pipeline. Check it at natural yield points: after
each file in the generator loop, after each delta-token batch in the
receiver, and before each `disk_commit`. Return a typed
`ClientError::Cancelled` when the flag is set.

**Effort:** Medium (2-3 PRs). The plumbing is mechanical - add a
field to `ClientConfig` or `TransferContext`, check it in 4-5 loops.
The complexity is in ensuring partial-transfer cleanup (temp files,
buffered writes) is correct on the cancellation path.

**Independent of ASY-6:** Yes. A cancellation token works with the
synchronous API and does not require async conversion.

---

### G3: Global `BufferPool` singleton

**Severity:** Medium.
**Crate:** `engine`.

`BufferPool` is initialized as a process-wide `OnceLock<Arc<BufferPool>>`
singleton (`GLOBAL_BUFFER_POOL` in
`crates/engine/src/local_copy/buffer_pool/global.rs`). The first call
to `init_global_buffer_pool` or `global_buffer_pool` wins; subsequent
calls with different parameters are silently ignored.

**Impact.** An embedder cannot configure per-transfer buffer pools.
All concurrent transfers share the same pool with the same byte budget
and buffer count cap. If the embedder's workload profile differs from
the CLI default (e.g., many small transfers vs. few large ones), there
is no way to tune per-transfer. The `OnceLock` also means the
embedder's call to `init_global_buffer_pool` races with oc-rsync's
own initialization inside `run_client_internal` - whoever calls first
wins.

The singleton also makes testing fragile: tests that exercise
buffer-pool capacity must serialize via `EnvGuard` to avoid corrupting
shared state across parallel test runs.

**Workaround.** Call `init_global_buffer_pool` before the first
`run_client` call to lock in the embedder's preferred configuration.
Accept that all transfers share the pool.

**Fix.** Accept an optional `Arc<BufferPool>` in `ClientConfig`. When
provided, use it instead of the global singleton. Fall back to
`global_buffer_pool()` when `None`. This preserves backward
compatibility while giving embedders per-transfer control.

**Effort:** Small (1 PR). Add the field to `ClientConfigBuilder`,
thread it through `run_client_internal` to the engine, replace
`global_buffer_pool()` calls with a local-or-global accessor.

**Independent of ASY-6:** Yes.

---

### G4: Rayon global thread pool

**Severity:** Medium.
**Crate:** `flist`, `signature`, `transfer`, `engine`.

Rayon's global thread pool is used for parallel `stat` batches
(`flist::parallel`), parallel signature generation
(`signature::parallel`), parallel directory metadata application
(`transfer::receiver`), and parallel delta verification
(`engine::concurrent_delta`). The pool is initialized once per process
via `rayon::ThreadPoolBuilder::build_global` (called from
`cli::frontend::execution::drive::thread_tunables`). The pool size
defaults to the number of logical CPUs.

**Impact.** The embedder cannot control how many OS threads oc-rsync's
rayon pool uses, nor can it share its own rayon pool with oc-rsync. If
the host application also uses rayon, there will be two global pools
competing for CPU time (rayon's `build_global` is once-per-process;
whichever library initializes first wins). If the host application
does not use rayon, oc-rsync silently spawns N threads (one per CPU)
that persist for the process lifetime.

**Workaround.** Call `rayon::ThreadPoolBuilder::new().num_threads(N)
.build_global()` before loading oc-rsync to control the pool size.
Accept that all rayon users in the process share one pool.

**Fix.** Replace global-pool `par_iter` calls with explicit
`ThreadPool::install(|| ...)` on a pool owned by `ClientConfig` or
`TransferContext`. This lets the embedder pass in a pool sized for
their workload, or share their existing pool.

**Effort:** Medium (2 PRs). One PR to add pool ownership to
`ClientConfig`, one to thread it through all `par_iter` call sites
(~15 call sites across 4 crates). Each call site changes from
`entries.par_iter()...` to `pool.install(|| entries.par_iter()...)`.

**Independent of ASY-6:** Yes.

---

### G5: Thread-per-connection daemon model

**Severity:** Medium (daemon-only).
**Crate:** `daemon`.

The daemon spawns one OS thread per accepted TCP connection
(`spawn_connection_worker` in
`crates/daemon/src/daemon/sections/server_runtime/connection.rs`).
Each thread runs a full `handle_session` to completion. The design
mirrors upstream rsync's fork-per-connection model.

**Impact.** An embedder that wants to run an rsync daemon as a service
inside their tokio application hits a ~10K concurrent connection
ceiling due to thread stack overhead and scheduler pressure. The
thread-per-connection model also conflicts with the embedder's async
accept loop - the daemon owns its own `TcpListener` and cannot share
the embedder's listener or runtime.

**Workaround.** Set `--max-connections` well below the ceiling.
Accept the thread-per-connection model for daemon workloads, which
typically have lower concurrency than HTTP services.

**Fix.** Convert the daemon accept loop to async (the `async-daemon`
feature already provides a partial prototype in
`crates/daemon/src/async_listener.rs`) and bridge each session to a
`spawn_blocking` task. Long-term: convert session handling itself to
async (ASY-7+ path).

**Effort:** Large (3-4 PRs for accept-loop conversion, more for
session conversion). The accept loop is straightforward; session
conversion touches the full protocol pipeline.

**Independent of ASY-6:** Partially. The async accept loop can ship
independently. Full session conversion is gated on ASY-6 adopt.

---

### G6: `spawn_blocking` ceiling from russh bridge

**Severity:** Medium (SSH transfers only).
**Crate:** `rsync_io`, `core`.

The russh SSH transport uses `tokio::spawn_blocking` to bridge between
russh's async API and oc-rsync's synchronous transfer pipeline
(`crates/rsync_io/src/ssh/embedded/sync_bridge.rs`). Each SSH
connection consumes one blocking-pool thread for the transfer duration.

**Impact.** When the embedder uses oc-rsync for SSH transfers, the
blocking-pool budget is consumed by both the oc-rsync transfer (G1)
and the russh bridge simultaneously. An embedder running 200 SSH
transfers concurrently uses 200 blocking-pool threads just for russh,
plus 200 for the transfer pipeline (if they run on separate threads),
approaching the default 512-thread ceiling. The embedder's own
`spawn_blocking` calls compete for the remaining budget.

This is distinct from G1 because even an async `run_client_async`
(the G1 fix) would still need `spawn_blocking` for the russh bridge
unless the transfer pipeline itself becomes async-native.

**Workaround.** Increase `max_blocking_threads` proportionally to
expected SSH concurrency. Size it as `2 * max_ssh_transfers +
headroom_for_other_blocking_work`.

**Fix.** Convert the transfer pipeline to async-native so the russh
bridge dissolves (ASY-3 boundary 12). This is part of the ASY-7+
conversion path.

**Effort:** Large (gated on full async pipeline, ASY-7..12).

**Independent of ASY-6:** No. Dissolving the bridge requires the
async pipeline.

---

### G7: Signal handler global state

**Severity:** Low.
**Crate:** `core`.

oc-rsync installs process-wide signal handlers
(`crates/core/src/signal/unix.rs`) and maintains a global cleanup
manager (`CLEANUP_MANAGER` `OnceLock` in
`crates/core/src/signal/cleanup.rs`). Signal handlers use
`AtomicBool` flags (`SIGNAL_COUNT`) to coordinate shutdown.

**Impact.** If the embedder has its own signal handling (common in
long-running services), oc-rsync's handlers overwrite them. The
cleanup manager registers temp-file paths globally; a cancelled
transfer that does not deregister its paths leaves stale entries.

**Workaround.** Call `run_client` before installing the embedder's
signal handlers, or accept that oc-rsync's handlers take precedence
during transfers.

**Fix.** Make signal handler installation opt-in via a
`ClientConfig` flag (e.g., `install_signal_handlers: bool`, default
`true` for CLI, `false` for library embedding). Move cleanup
registration to per-transfer scope instead of global scope.

**Effort:** Small (1 PR). The signal module already has clean
boundaries; the change is adding a gate.

**Independent of ASY-6:** Yes.

---

### G8: SIMD and kernel feature probe singletons

**Severity:** Low.
**Crate:** `checksums`, `fast_io`.

SIMD capability probes (AVX2, SSE2, NEON) and kernel feature probes
(io_uring availability, copy_file_range support) are cached in
`OnceLock` singletons. These are initialized on first use and persist
for the process lifetime.

**Impact.** Minimal for most embedders. The probes are read-only after
initialization, thread-safe, and correct for the process lifetime.
The only concern is that an embedder cannot override the detected
capabilities (e.g., to force scalar fallback for debugging). This is
a niche use case.

**Workaround.** None needed for normal use. For debugging, set
environment variables that the probes check (e.g., `SIMDE_NO_NATIVE`).

**Fix.** Accept optional capability overrides in `ClientConfig`.
Low priority - the singletons are benign for embedding.

**Effort:** Small (1 PR).

**Independent of ASY-6:** Yes.

---

### G9: No `run_daemon` library API

**Severity:** Low (daemon embedding only).
**Crate:** `daemon`.

The daemon entry point (`run_daemon`) is designed for standalone
process use. There is no library API for starting a daemon listener
on an embedder-provided `TcpListener` or within an embedder-managed
async runtime. The daemon owns its own accept loop, signal handling,
and process lifecycle.

**Impact.** An embedder that wants to serve rsync protocol as part of
a larger service cannot embed the daemon without forking the daemon
crate or wrapping it in a child process.

**Workaround.** Run the daemon as a separate process and communicate
via the rsync protocol over TCP.

**Fix.** Expose a `DaemonBuilder` API that accepts an external
listener, runtime handle, and configuration. This is a larger API
design effort that builds on G5 (async accept loop) and G7 (signal
handler opt-out).

**Effort:** Large (3+ PRs). Depends on G5 and G7.

**Independent of ASY-6:** Partially. The API design is independent;
the async accept loop dependency links to G5.

## 4. Priority matrix

Gaps ordered by impact-to-effort ratio, from highest to lowest:

| Rank | Gap | Severity | Effort | ASY-6 gated | Recommended phase |
|------|-----|----------|--------|-------------|-------------------|
| 1 | G7: Signal handler opt-out | Low | Small | No | Immediate |
| 2 | G3: BufferPool per-transfer | Medium | Small | No | Immediate |
| 3 | G2: Cancellation token | High | Medium | No | Near-term |
| 4 | G4: Rayon pool ownership | Medium | Medium | No | Near-term |
| 5 | G8: Feature probe overrides | Low | Small | No | Opportunistic |
| 6 | G1: Async public API | High | Large | Yes | Post ASY-6 |
| 7 | G5: Async daemon accept | Medium | Large | Partial | Post ASY-6 |
| 8 | G6: russh bridge dissolution | Medium | Large | Yes | Post ASY-6 |
| 9 | G9: Daemon library API | Low | Large | Partial | Post ASY-6 |

**Key observation.** Gaps G2, G3, G4, G7, and G8 (ranks 1-5) are
independent of the ASY-6 adopt/defer decision. They improve
embeddability under the current synchronous architecture and remain
valuable regardless of whether the async pipeline is adopted. These
should be pursued as standalone tickets.

Gaps G1, G5, G6, and G9 (ranks 6-9) require partial or full async
conversion and are gated on ASY-6.

## 5. Relationship to ASY-6

The ASY-6 decision doc (`docs/design/asy-6-adopt-or-defer-decision.md`)
chose Option B (defer) pending ASY-4 benchmark data and ASY-5
embeddability evidence. This gap-list is the ASY-5 evidence.

The findings inform ASY-6 re-evaluation as follows:

- **If the embedder's primary need is cancellation and resource
  control (G2, G3, G4, G7):** These gaps are fixable without async
  conversion. The defer path (Option B) or even the close path
  (Option C) can deliver them. This weakens the case for Option A
  (adopt) unless the async API (G1) is independently demanded.

- **If the embedder needs high-concurrency SSH transfers (G1 + G6):**
  The blocking-pool ceiling is a hard constraint that only the async
  pipeline can remove. This strengthens the case for Option A.

- **If the embedder needs daemon embedding (G5 + G9):** The
  thread-per-connection ceiling is real but the concurrency threshold
  (~10K) is higher than most rsync daemon deployments. The partial
  async accept loop (already prototyped) may suffice without full
  pipeline conversion.

ASY-6 should re-evaluate when:
1. ASY-4 benchmark data quantifies the overhead of `spawn_blocking`
   bridging at scale.
2. At least G2 and G3 have shipped, establishing whether the
   "synchronous with escape hatches" model is sufficient for known
   embedding use cases.

## 6. Current embedding guidance

Until gaps are addressed, embedders should follow this pattern:

### 6.1 Do

- **Always use `spawn_blocking`.** Never call `run_client` directly
  from an async context. It blocks the calling thread for the full
  transfer duration.

- **Size the blocking pool explicitly.** Set `max_blocking_threads`
  to at least `max_concurrent_transfers + headroom`. For SSH
  transfers, double the count (G6).

  ```rust
  tokio::runtime::Builder::new_multi_thread()
      .max_blocking_threads(1024)
      .build()
  ```

- **Initialize `BufferPool` early.** Call `init_global_buffer_pool`
  before the first `run_client` to lock in preferred parameters (G3).

- **Initialize rayon early.** Call
  `rayon::ThreadPoolBuilder::new().num_threads(N).build_global()`
  before loading oc-rsync if you need to control the pool size (G4).

- **Accept that transfers run to completion.** There is no
  cancellation mechanism today (G2). Design your application to
  tolerate in-flight transfers that outlive the request that started
  them.

### 6.2 Do not

- **Do not call `run_client` from a tokio worker thread.** This
  starves the runtime. Use `spawn_blocking` exclusively.

- **Do not assume signal handlers are yours.** oc-rsync installs
  process-wide signal handlers (G7). If you need custom handlers,
  install them after the first `run_client` call returns.

- **Do not expect per-transfer isolation.** All transfers in the
  process share the global `BufferPool` (G3), rayon pool (G4), SIMD
  probes (G8), and signal handlers (G7).

- **Do not embed the daemon listener.** There is no library API for
  running an rsync daemon inside your application (G9). Use a
  separate process.

## 7. Verdict summary from ASY-5.b scenarios

The following table maps ASY-5.b scenario outcomes to gaps:

| Scenario | Expected outcome | Primary gap(s) |
|----------|-----------------|----------------|
| S1: Single transfer | Pass (blocks worker thread) | G1 |
| S2: Concurrent transfers | PassWithCaveat (pool saturates at N=512) | G1, G6 |
| S3: Drop cancellation | PassWithCaveat (not cancellable) | G2 |
| S4: Interleaved I/O | Pass (spawn_blocking isolates) | G1 (workaround works) |
| S5: Re-entrancy | Pass (sequential re-entry works) | G3 (shared pool, benign) |

S3's finding is the most actionable: the absence of cooperative
cancellation (G2) is the gap with the highest severity-to-effort
ratio that does not require async conversion.

## 8. Cross-references

- `docs/design/asy-5-a-embeddability-test-harness.md` - harness spec.
- `docs/design/asy-5b-embeddability-harness-impl.md` - harness
  implementation spec with scenario details.
- `docs/design/asy-6-adopt-or-defer-decision.md` - the decision this
  gap-list feeds into.
- `docs/design/asy-2-tokio-runtime-feature.md` - `tokio-transfer`
  feature gate.
- `docs/design/asy-3-async-boundary-spec.md` - per-boundary async
  disposition contracts.
