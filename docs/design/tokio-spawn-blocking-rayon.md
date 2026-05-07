# tokio::task::spawn_blocking and rayon Composition (#1751)

Tracking issue: oc-rsync task #1751.
Related design notes:

- `docs/design/io-uring-rayon-composition.md` (#1283/#1284) - the
  rayon + io_uring composition this note layers on top of. Section 7
  there sets the policy: any daemon-side blocking call that cannot go
  through io_uring uses `spawn_blocking`; CPU-bound work stays on
  rayon.
- `docs/design/async-migration-plan.md` (#1594) - the broader
  sync/async migration sequencing.
- `docs/audits/tokio-dependency-boundary-2026.md` (PR #3706) - the
  tokio-feature-gated crate boundary this note honours.
- `docs/audits/daemon-thread-per-connection-scalability.md`
  (PR #3705) - the scaling problem the async daemon (#1934) is
  intended to solve.

## 1. Summary

oc-rsync mixes two pools today and is on track to mix three when the
async daemon (#1934) lands. This note locks in how they compose.

- **rayon** runs CPU work: parallel `stat`/file-list build, parallel
  signature generation, parallel match candidate verification,
  delta-pipeline scheduling. Workers are sized by
  `rayon::current_num_threads()` and meant to be saturated by
  arithmetic, not by syscalls.
- **tokio** runs async I/O for the daemon listener (#1934) and the
  feature-gated async pipeline. The runtime owns a pool of N worker
  threads (one per CPU by default) that drive `Future`s and a separate
  blocking pool for `spawn_blocking` (default cap 512).
- **`tokio::task::spawn_blocking`** offloads a synchronous closure to
  the tokio blocking pool so that the worker thread that called it
  stays free to drive other futures. The blocking pool is sized for
  I/O wait, not CPU work.

The risk is two-way deadlock when a tokio worker blocks on rayon work
that itself wants to drive a future, or when rayon work calls
`spawn_blocking` and waits for the result on the same runtime that is
fully occupied. Section 5 details the patterns; section 6 gives the
recommended explicit handoff.

This note is design-only. Implementation lands under the async daemon
work in #1934 and the follow-up wiring in #1284 (io_uring + rayon)
and the receiver-side work in #1796/#1797 (the directory creation/
deletion sites currently noted as future `spawn_blocking` users).

## 2. Current State

### 2.1 rayon's CPU surface

Confirmed call sites that put CPU-bound closures on rayon:

- `crates/transfer/src/parallel_io.rs:107-125` -
  `map_blocking::<T, R, F>` is the threshold-gated helper. Below
  `min_parallel`, sequential; above, `into_par_iter().map(f).collect()`.
  This is the canonical entry point for parallel CPU work.
- `crates/transfer/src/receiver/transfer/pipeline.rs:182-189` -
  parallel basis-file lookup during pipeline fill. The closure does
  filesystem `stat` calls; today these are synchronous and CPU-cheap
  but I/O-blocking.
- `crates/transfer/src/delta_pipeline.rs:324` -
  `rayon::current_num_threads()` sizes the parallel delta pipeline.
- `crates/match/src/index/mod.rs:191,293` - parallel candidate
  verification when a rolling-hash bucket has more candidates than
  `PARALLEL_THRESHOLD`. Pure CPU.
- `crates/flist/src/parallel.rs:81-388` and
  `crates/flist/src/batched_stat/{cache,dir_stat}.rs` - parallel
  file-list construction and batched stat. Mix of CPU sort/encode
  plus directory-walk syscalls.
- `crates/fast_io/src/cached_sort.rs:118-120` - parallel key
  extraction for the cached sort fast path.

The thread count is rayon's global default - one worker per logical
CPU. There is no `rayon::ThreadPoolBuilder` override anywhere in the
workspace.

### 2.2 tokio's async surface

Today's tokio surface is feature-gated and concentrated in a small
set of crates (per `docs/audits/tokio-dependency-boundary-2026.md`):

- `crates/daemon/src/daemon/async_session/listener.rs:128` -
  `AsyncDaemonListener::bind` (the #1934 work).
- `crates/daemon/src/daemon/async_session/session.rs:68,119` -
  `AsyncSession::handle` and the protocol handler.
- `crates/transfer/src/pipeline/async_pipeline.rs:137` - async
  transfer pipeline behind the `async` feature.
- `crates/protocol/src/negotiation/sniffer/async_read.rs:53` - async
  prologue sniffer.
- `crates/bandwidth/src/async_limiter.rs:77,107` - async rate limiter.

Today's `spawn_blocking` calls:

- `crates/engine/src/async_io/copier.rs:184` - exactly one site.
  Wraps a sync copy under an async caller.
- `crates/transfer/src/receiver/directory/creation.rs:26` and
  `deletion.rs:26` - documented as "future `spawn_blocking` users";
  currently sync, planned to use the blocking pool when the receiver
  side gains an async surface.

### 2.3 The collision point

The async daemon (#1934) accepts on a tokio runtime. When a connection
arrives, the session needs the same engine that the CLI uses: file
list build, signature generation, basis lookup, delta apply. Those
paths today run on rayon. The collision is:

```text
tokio worker
    -> async session
        -> needs file list
            -> rayon::par_iter on flist build
                -> rayon worker blocks on directory `stat`
                    -> rayon worker also wants `spawn_blocking` for
                       a syscall the engine cannot route through
                       io_uring
                        -> tokio blocking pool full -> deadlock
```

The deadlock has two failure modes:

1. **Tokio worker starvation.** A tokio worker calls into rayon
   synchronously and sits on the future for the duration of the
   parallel job. Rayon is fast in aggregate but the *first* worker
   blocks the future. The runtime loses one of N worker slots until
   the rayon job completes.
2. **Blocking-pool back-pressure.** Rayon workers call
   `spawn_blocking` and wait on the join handle. If enough rayon
   workers do this concurrently and the blocking pool is at capacity
   (default 512, but smaller in custom runtimes), every rayon worker
   waits on a tokio task that cannot start because no rayon worker
   is free to do CPU work for it. The pools deadlock each other.

Both modes are real today on the *daemon* side once #1934 lands. The
CLI side stays sync and is unaffected.

## 3. Composition Options

We considered four shapes. The recommendation is option D.

### 3.1 Option A: tokio everywhere, drop rayon

Convert every parallel CPU loop to `tokio::spawn` + a dedicated
`Runtime::new_multi_thread()` worker count.

- Pro: one pool, no composition problem.
- Con: tokio's scheduler is tuned for I/O wait, not work-stealing
  across CPU-bound tasks. Existing benchmarks
  (`crates/transfer/benches/`) show rayon's work-stealing wins on
  signature/match/checksum loops.
- Con: requires a runtime in the CLI binary, which today has no
  runtime. Either we pay the runtime cost on every CLI invocation
  (a non-trivial startup hit) or we keep two parallel implementations.
- Con: rejected in `docs/design/io-uring-rayon-composition.md`
  section 16, citing the same trade-off.

### 3.2 Option B: rayon everywhere, drop tokio for the daemon

Run the async daemon on a rayon-backed `block_on` shim.

- Pro: one pool.
- Con: rayon does not have async primitives. Connection accept,
  timer wheels, and async sockets all need a runtime. Bolting them
  on top of rayon recreates a worse tokio.
- Con: defeats the entire #1934 design.

Rejected.

### 3.3 Option C: spawn rayon tasks from inside `spawn_blocking`

The tokio worker calls `spawn_blocking` to enter the blocking pool;
that closure then calls `rayon::scope` or `par_iter` and waits for
the rayon job; when rayon finishes, the closure returns and the
join handle resolves on the tokio side.

```rust
let signatures = tokio::task::spawn_blocking(move || {
    files.par_iter()
        .map(generate_signature)
        .collect::<Vec<_>>()
}).await?;
```

- Pro: simple. One line of glue per call site.
- Pro: keeps rayon's CPU pool and tokio's blocking pool distinct.
  The tokio worker is freed (via `await`) for the duration of the
  blocking job.
- Con: still risks the blocking-pool back-pressure mode if rayon
  workers themselves call `spawn_blocking` re-entrantly. This is
  why option D adds the explicit channel.
- Con: every blocking-pool slot is held for the full rayon job
  duration, even if only the first 1% of the job is on the critical
  path. The blocking pool is a coarse rate limiter for rayon
  parallelism in this shape.

This is the *minimum viable* pattern. Option D refines it.

### 3.4 Option D: explicit handoff via crossbeam channel (recommended)

The async caller posts a `Job` to a bounded `crossbeam_channel` whose
consumer is a long-lived worker that runs rayon jobs. The consumer
drives rayon, computes the result, and sends it back through a
`tokio::sync::oneshot` to the awaiting future. The async caller
never blocks a tokio worker on rayon directly.

```rust
struct RayonJob<T: Send + 'static> {
    work: Box<dyn FnOnce() -> T + Send + 'static>,
    reply: tokio::sync::oneshot::Sender<T>,
}

// in tokio context:
let (tx, rx) = tokio::sync::oneshot::channel();
rayon_dispatcher.send(RayonJob {
    work: Box::new(move || files.par_iter().map(...).collect::<Vec<_>>()),
    reply: tx,
})?;
let signatures = rx.await?;
```

- Pro: the tokio worker that posts the job yields immediately and
  becomes available for other futures. No tokio worker is ever
  blocked on rayon.
- Pro: rayon never calls `spawn_blocking`. No blocking-pool
  back-pressure mode possible.
- Pro: the dispatcher is a natural place to enforce a global concurrency
  cap on rayon work coming from the async side, separate from the
  CLI's direct rayon use. This matters when the daemon is serving
  many connections and we want to bound CPU contention.
- Pro: the dispatcher pattern composes with the io_uring +
  rayon dispatcher in `docs/design/io-uring-rayon-composition.md`.
  Both use the same shape - submit to a queue, wait on a per-op
  channel - which keeps the mental model uniform.
- Con: extra channel hop and one allocation per job (`Box<dyn FnOnce>`).
  Negligible relative to the work being dispatched (parallel `stat`
  on thousands of files, MB-scale signature compute).
- Con: requires a long-lived dispatcher thread (or a `rayon::scope`
  inside a single OS thread that drains the channel). The cost is
  one OS thread per session, or one global dispatcher per process.

This is the recommendation.

## 4. Recommended Approach

The recommendation is option D with these specifics:

1. **One rayon dispatcher per process.** A single OS thread, started
   on first use, that owns a `crossbeam_channel::Receiver<RayonJob>`
   and runs each job inside `rayon::scope` or directly via
   `par_iter`. Process-wide because rayon is process-global already;
   per-session would not isolate anything.
2. **Bounded channel.** Channel capacity = `2 *
   rayon::current_num_threads()`. Senders that find the channel full
   apply back-pressure to the async caller via
   `try_send` + `tokio::time::sleep(...).await` or - cleaner -
   `flume::Sender::send_async` (the `flume` crate exposes async-aware
   send on a crossbeam-shaped channel; if we add that dependency, it
   replaces the manual sleep loop). Default to crossbeam +
   `try_send`/yield; reconsider `flume` if we hit measurable
   contention.
3. **Reply via `tokio::sync::oneshot`.** The job carries a
   `oneshot::Sender<T>`; the dispatcher sends the result and drops
   the sender; the awaiting future resolves.
4. **No `spawn_blocking` from rayon.** Hard rule. Any syscall a
   rayon worker needs that cannot go through io_uring is either
   (a) a sync syscall the rayon worker performs directly (it is on
   the rayon pool, not the tokio runtime, so blocking the rayon
   worker is fine and expected), or (b) bounced back to the
   dispatcher's caller side, which can choose to spawn it on the
   tokio blocking pool independently. Rayon never reaches into the
   tokio runtime.
5. **No `block_on` from rayon.** Hard rule. Calling
   `tokio::runtime::Handle::block_on` from a rayon worker risks
   blocking a tokio worker thread (if the handle's runtime is
   multi-thread and the worker happens to be the one that posted
   the rayon job). Use the channel handoff instead.
6. **CLI is unchanged.** The CLI binary has no runtime and does not
   need the dispatcher. It continues to call rayon directly.

The dispatcher is created lazily when the first async caller asks for
it. A `OnceLock<RayonDispatcher>` in `crates/transfer/src/parallel_io.rs`
or a sibling module owns the channel sender and the join handle of
the dispatcher thread.

## 5. Deadlock Analysis

The recommendation eliminates the two failure modes from section 2.3.

### 5.1 Tokio worker starvation - eliminated

The async caller does:

```rust
let result = rx.await?;  // tokio yields here
```

`oneshot::Receiver::await` is a proper `Future` that yields when not
ready. The tokio worker that called it returns to the runtime and is
free to drive other futures. When the dispatcher sends through the
oneshot, the runtime wakes the suspended future on whichever worker
is available.

### 5.2 Blocking-pool back-pressure - eliminated

The dispatcher never calls `spawn_blocking`. The rayon pool runs the
job entirely; the tokio blocking pool is untouched. The two pools
share no queue, no semaphore, no mutex. There is no edge in the
wait-for graph from rayon to tokio's blocking pool.

### 5.3 Residual deadlock surfaces

Two narrow surfaces remain. Both are bounded.

- **Channel-full + tokio-full.** If the dispatcher channel is full
  *and* the tokio runtime is fully scheduled with futures all
  trying to post jobs, the senders await on `try_send` retries.
  This is a *throughput* problem, not a deadlock: the dispatcher is
  draining jobs, just slower than they arrive. Bounding the channel
  capacity at `2 * num_threads` keeps the queue from growing
  unboundedly while letting the dispatcher run at full throttle.
- **Rayon job re-entry.** If a rayon closure itself posts another
  job to the dispatcher and waits on the oneshot via `block_on`,
  that *is* a deadlock: the rayon thread is the dispatcher's
  worker, so it cannot drain its own queue. Hard rule: rayon
  closures may not call back into the dispatcher. Enforced by
  inspection in code review and a debug assertion in
  `RayonDispatcher::send` that checks `rayon::current_thread_index()
  .is_none()` (i.e. the caller is not a rayon worker).

## 6. Test Plan

The dispatcher itself, the deadlock-avoidance properties, and the
integration with the async daemon all need test coverage. The
implementation under #1934 (and its receiver-side companions) ships
the tests; this design note enumerates what they must cover.

### 6.1 Unit tests

- **Dispatcher round-trip.** Post a job that returns a known value;
  assert the oneshot resolves with that value. Bounds: zero work
  (closure returns immediately), one-page work (parallel
  `iter::repeat(1).take(N).sum()`), large work (parallel signature
  generation across 10 K files).
- **Channel back-pressure.** Saturate the channel by posting
  `2 * num_threads + 1` jobs where each closure sleeps; assert
  the (`2 * num_threads + 1`)-th sender either sees `TrySendError::Full`
  or - in the async path - awaits and resolves once a slot opens.
- **Rayon worker re-entry.** A closure posts a second job to the
  dispatcher. In debug builds, assert the inner `send` panics with
  the documented message. In release builds, assert no panic but the
  outer job returns an error rather than deadlocking (timeout-wrapped
  test, 5 s).
- **Drop semantics.** Drop the `oneshot::Sender` before the job
  completes; assert the awaiting future resolves with the documented
  cancellation error and the dispatcher does not panic.

### 6.2 Property tests

- **No tokio worker is blocked on rayon.** Instrument the dispatcher
  to record the `tokio::runtime::Handle::current().runtime_flavor()`
  of the *caller* and the executor of the rayon closure. Property:
  for any sequence of N concurrent jobs on a M-worker tokio runtime,
  the maximum number of *blocked* tokio workers (workers stuck on
  `await` longer than 1 ms with no progress) is zero.
- **No `spawn_blocking` is invoked.** Wrap the dispatcher's rayon
  scope in a thread-local guard that increments a counter on
  `tokio::task::spawn_blocking` calls. Property: the counter stays
  zero for any closure that does not itself opt into
  `spawn_blocking`.

### 6.3 Stress tests

- **Mixed CLI + daemon.** Run the CLI flist build (rayon direct) in
  parallel with the daemon's flist build (rayon via dispatcher) on
  the same process. Assert both complete; assert no thread is in a
  blocked state by the end (`tokio_metrics::RuntimeMonitor` for the
  tokio side, custom probe for the rayon side).
- **High-fanout daemon.** 1 K concurrent async daemon connections,
  each doing a parallel basis-file lookup that posts a job to the
  dispatcher. Assert all 1 K complete inside a time budget that
  reflects the dispatcher's work-conservation, not its serialisation.
- **Blocking-pool isolation.** Saturate the tokio blocking pool with
  long-running `spawn_blocking` tasks (1024 of them with 60 s
  sleeps). Assert the dispatcher continues to make progress, and
  rayon-driven work completes regardless of blocking-pool state.

### 6.4 Integration tests

- **Async daemon flist build.** End-to-end: a tokio-driven daemon
  connection produces a file list whose bytes match the CLI's
  byte-for-byte. The wire-compat invariant from
  `docs/design/io-uring-rayon-composition.md` section 8 applies
  unchanged: the dispatcher is purely a userspace orchestration
  layer and never touches the wire.
- **Interop against upstream rsync 3.4.1** with the async daemon
  enabled, confirming bit-identical output and matching exit codes.

### 6.5 Deadlock fuzz

A short fuzz harness that randomly:

- spawns N tokio tasks each posting M rayon jobs,
- spawns K rayon-direct calls (CLI-shape callers) at the same time,
- randomly drops oneshot senders and channel handles,
- runs under a 30-second hard timeout enforced by
  `tokio::time::timeout`,

and asserts every iteration completes inside the timeout. A timeout
counts as a failed iteration. Run nightly for 1 hour minimum before
marking the design as validated.

## 7. References

- `crates/transfer/src/parallel_io.rs:6,103-125` - the
  `map_blocking` helper and the comment that calls out rayon being
  "lighter than tokio `spawn_blocking` for synchronous I/O
  operations".
- `crates/transfer/src/receiver/transfer/pipeline.rs:182-189` -
  parallel basis-file lookup.
- `crates/transfer/src/delta_pipeline.rs:324` - rayon thread count
  driving the delta pipeline.
- `crates/match/src/index/mod.rs:191,293` - parallel candidate
  verification.
- `crates/flist/src/parallel.rs:81-388` -
  parallel flist build.
- `crates/flist/src/batched_stat/cache.rs:128-131` and
  `dir_stat.rs:150-153` - parallel batched stat.
- `crates/fast_io/src/cached_sort.rs:118-120` - parallel cached sort.
- `crates/engine/src/async_io/copier.rs:184` - the one current
  `spawn_blocking` site.
- `crates/transfer/src/receiver/directory/creation.rs:26` and
  `deletion.rs:26` - documented future `spawn_blocking` users.
- `crates/daemon/src/daemon/async_session/listener.rs:128` - async
  daemon listener (#1934).
- `crates/daemon/src/daemon/async_session/session.rs:68,119` -
  async session handler.
- `crates/protocol/src/negotiation/sniffer/async_read.rs:53` - async
  prologue sniffer.
- `crates/transfer/src/pipeline/async_pipeline.rs:137` - async
  pipeline behind the `async` feature.
- `docs/design/io-uring-rayon-composition.md` (#1283/#1284) -
  rayon + io_uring composition; section 7 sets the policy this note
  refines.
- `docs/design/async-migration-plan.md` (#1594) - the broader
  sync/async migration plan.
- `docs/audits/tokio-dependency-boundary-2026.md` (PR #3706) -
  tokio-feature-gated crate boundary.
- `docs/audits/daemon-thread-per-connection-scalability.md`
  (PR #3705) - daemon scaling problem #1934 addresses.
