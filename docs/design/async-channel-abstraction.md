# Async-Compatible Channel Abstraction for the Transfer Pipeline (#1591)

## Summary

Today the oc-rsync transfer pipeline coordinates worker threads with
synchronous channels - `crossbeam_channel::bounded` for the rayon work
queue, a lock-free SPSC built on `crossbeam_queue::ArrayQueue` for the
network-to-disk hand-off, and stdlib `std::sync::mpsc` in a few legacy
sites. The async daemon listener prototype (#1934) and the async SSH
transport evaluation (#1593) need channels that can be awaited inside a
tokio runtime. Naively wrapping a sync channel in `block_on` blocks the
runtime; naively switching everything to `tokio::sync::mpsc` slows the
pure-sync rayon hot path that has no runtime to suspend on.

This note proposes a thin `TransferChannel<T>` trait whose backing
implementation is chosen per call site, and a default of the `flume`
crate at the small set of sites that bridge sync producers and async
consumers (or the reverse). Hot paths that never cross an async
boundary keep their existing `crossbeam` channels untouched.

## Problem Statement

### Current channel inventory

The codebase already mixes three channel libraries.

- `crates/engine/src/concurrent_delta/work_queue/bounded.rs:8,89-104`
  builds the SPMC delta work queue on `crossbeam_channel::bounded`.
  Producer is the single wire reader, consumers are rayon workers.
  Capacity defaults to `2 * rayon::current_num_threads()`
  (`bounded.rs:90`, `concurrent_delta/work_queue/capacity.rs`).
- `crates/engine/src/concurrent_delta/work_queue/iter.rs:3` and
  `concurrent_delta/work_queue/drain.rs:9` consume the same crossbeam
  receiver under `rayon::scope`.
- `crates/engine/src/concurrent_delta/consumer.rs:47` uses
  `std::sync::mpsc` for the `DeltaConsumer` reorder thread - one
  worker thread feeds in-order results back to the caller.
- `crates/transfer/src/pipeline/spsc.rs:1-15` provides a hand-rolled
  lock-free SPSC over `crossbeam_queue::ArrayQueue` for the
  network-to-disk hand-off, deliberately avoiding any park/wake
  syscalls on the hot path.
- `crates/transfer/src/pipeline/async_dispatch.rs:10` and
  `crates/transfer/src/pipeline/async_pipeline.rs:21` use
  `tokio::sync::mpsc` for the new async file-job pipeline behind the
  `async` feature gate.
- `crates/daemon/src/daemon/async_session/listener.rs:13,130,180-256`
  uses `tokio::sync::broadcast` for shutdown fan-out and
  `tokio::sync::Semaphore` for connection admission.
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs:286`
  retains a stdlib `mpsc` for legacy server-side wakeups (audit
  target under #1592).

The mix is an artefact of organic growth: each subsystem reached for
the channel it needed at the time. None of the existing sites cross
sync-async boundaries today because the async paths are still gated.

### What changes with #1934 and #1593

The async daemon listener (#1934, see `async_session/listener.rs`)
will spawn a tokio task per accepted connection
(`async_session/listener.rs:216-249`). Each task negotiates the
greeting and module selection asynchronously, then hands the
connection to a synchronous transfer engine for the actual file
transfer phase. The hand-off is the new sync-async boundary: the
async accept task wants `send().await`, the sync transfer worker
wants blocking `recv()`.

The async SSH transport evaluation (#1593) has the inverse shape -
a sync rayon-driven sender feeding bytes into an async
`tokio::io::AsyncWrite` socket - and runs into the same problem from
the other direction.

Three naive fixes all fail.

1. **Wrap a crossbeam channel in `tokio::task::spawn_blocking +
   recv()`.** The blocking task occupies a tokio blocking-pool
   thread for the channel's full lifetime. That defeats the point
   of the runtime: 200 idle connections (`DEFAULT_MAX_CONNECTIONS` in
   `listener.rs:25`) become 200 parked OS threads.
2. **Switch every channel to `tokio::sync::mpsc`.** The hot delta
   apply loop and the SPSC pipeline have no runtime - introducing
   one to call `.await` at every hand-off costs measurable
   throughput (rough numbers from `crates/transfer/src/pipeline/spsc.rs`
   benchmarks: tokio mpsc adds ~80ns of futex traffic per item; the
   ArrayQueue path is ~12ns).
3. **Wrap a tokio channel in `block_on(tx.send(item))`.** Calling
   `block_on` from inside a tokio worker is a documented runtime
   deadlock and panics under `current_thread`.

A first-class abstraction that lets each call site pick its
implementation, with an explicit place where sync and async meet,
keeps the hot paths cheap and the bridges correct.

## Abstraction Goal

Define a `TransferChannel<T>` trait that exposes both sync and async
send and receive surfaces. The trait does not virtualise the channel
internals - it is a marker plus typed associated handles, so each
implementation can keep its own representation and the caller picks
the implementation explicitly.

Goals.

- Hot paths keep `crossbeam_channel` and `crossbeam_queue::ArrayQueue`
  with zero added cost.
- Bridge sites get one channel that exposes blocking send / blocking
  recv on the sync side and `send().await` / `recv().await` on the
  async side, with the same underlying buffer.
- The trait is small enough that swapping implementations is a
  type-alias change at the call site, not a refactor.
- No runtime is required at compile time for crates that never
  touch the bridge.

Non-goals.

- We do not abstract over MPMC vs SPSC vs SPMC topology - those
  invariants are still enforced by `Sender: !Clone` on the SPSC and
  SPMC sites, exactly as `WorkQueueSender` does today
  (`bounded.rs:21,48-50`).
- We do not abstract over capacity policy - bounded vs unbounded
  is part of the call site's choice and lives in the constructor,
  not the trait.
- We do not try to make `tokio::sync::broadcast` and `mpsc` look
  alike - broadcast stays separate.

## Three Implementation Options

### Option A: tokio::sync::mpsc everywhere

Pick `tokio::sync::mpsc::channel` as the single implementation,
require a tokio runtime in every crate that uses a channel.

Pros.
- Idiomatic for the async daemon and async SSH transport.
- Cancellation-safe `recv` via the `tokio::sync::mpsc::Receiver`
  documented contract.
- Single dependency story.

Cons.
- The pure-sync hot paths (delta apply, work queue drain, SPSC
  network-to-disk) have to spin up a runtime they do not otherwise
  need. That pulls tokio into `crates/engine` unconditionally.
- `Sender::blocking_send` exists but spawns a parked thread internally;
  measured overhead in the SPSC benchmark is ~6x the ArrayQueue
  path.
- Forces every existing call site to migrate, including the SPMC
  `WorkQueueSender` whose `!Clone` invariant has no clean
  equivalent in `tokio::sync::mpsc::Sender` (which is `Clone`).

### Option B: flume

Adopt `flume = "0.11"` for new sync-async bridge sites. `flume`
provides both a sync `Sender::send`/`Receiver::recv` interface and
async `Sender::send_async`/`Receiver::recv_async` on the same
underlying channel.

Pros.
- Single channel object - the sync and async halves observe the
  same FIFO and the same drop semantics, so there is no
  double-buffering or extra hop.
- Drop-in replacement for `crossbeam_channel` at API level (same
  `bounded`/`unbounded` constructors, `Sender`/`Receiver` types,
  `try_send`/`try_recv` helpers).
- No runtime requirement on the sync side. The async side wakes
  through the same waker the runtime already supplies.
- Implementation is a small dependency (~3kLoC, MIT) with stable
  releases since 2020 and a clear maintainer.

Cons.
- Throughput in the pure-sync, contended case is slightly below
  `crossbeam_channel` (flume's own benchmarks show ~15-20% slower
  on uncontended bounded MPMC; both are well under our hot-path
  budget but it matters where every nanosecond counts).
- Adds a new workspace dependency. `Cargo.toml:199-200` already
  pins `crossbeam` and `crossbeam-channel`; this is one more.
- Async cancellation: `recv_async()` returns a `RecvFut` that, if
  dropped after the underlying receiver has observed a producer
  send but before yielding it, will lose that item. This is the
  same hazard as `tokio::sync::mpsc::Receiver::recv` and is
  manageable with structured concurrency, but it is a footgun
  worth documenting at every call site.

### Option C: keep crossbeam, build an async wrapper via spawn_blocking + oneshot

Keep `crossbeam_channel::bounded` and write a thin adapter:
async sends post a `(item, tokio::sync::oneshot::Sender<()>)` via
`spawn_blocking`, await the oneshot for completion. Async recvs do
the symmetric dance.

Pros.
- Zero hot-path cost - the sync workers never see the wrapper,
  they keep their existing `Sender`/`Receiver` types.
- No new third-party crate; tokio is already in the workspace
  (`Cargo.toml:188`).
- The `block_in_place` escape hatch already exists for the rare
  cases where we need to reach back the other way.

Cons.
- One blocking-pool thread per outstanding async send or recv. Tokio's
  default blocking pool is 512 threads; under 200 concurrent daemon
  sessions plus signature workers this is uncomfortable.
- Two-hop latency on every bridge crossing: `await spawn_blocking`,
  then `await oneshot`. Roughly 20-40 microseconds of overhead on
  Linux per crossing - tolerable for the daemon control plane,
  excessive for any byte-frequency hot path.
- The wrapper is fiddly to get right (cancellation, shutdown,
  receiver-drop propagation) and we would re-derive what flume
  already ships.

## Recommended Approach: Option B (flume) for bridges, crossbeam stays in hot paths

We adopt **flume** for the small set of channels that explicitly
cross sync and async boundaries, and **leave crossbeam in place** for
the pure-sync hot paths.

Concretely.

- `crates/engine/src/concurrent_delta/work_queue/bounded.rs` keeps
  `crossbeam_channel::bounded`. The work queue is single-producer,
  rayon-consumer; it never touches a tokio runtime.
- `crates/transfer/src/pipeline/spsc.rs` keeps its hand-rolled
  ArrayQueue SPSC. Network ingest and disk commit are both sync
  threads.
- `crates/transfer/src/pipeline/async_pipeline.rs` (already on
  `tokio::sync::mpsc`) migrates to `flume::bounded` so a future
  sync test harness can drive it without a runtime, and so the
  consumer half can be handed to a sync receiver thread when we
  ship the hybrid path.
- The new `async_session::listener` -> sync transfer worker
  hand-off (#1934) uses a `flume::bounded` channel from day one.
- The async SSH transport evaluation (#1593) uses `flume::bounded`
  for its sync-sender / async-socket bridge.

Justification.
- Two channel libraries, not three: crossbeam for pure-sync,
  flume for sync-async. tokio's mpsc/oneshot stays for control
  signalling inside the runtime where flume offers nothing extra.
- Hot path stays untouched. The decision to keep crossbeam on the
  delta work queue and the network-to-disk SPSC is a hard
  performance constraint, not a stylistic one.
- The `block_on`/`spawn_blocking` adapter approach (Option C) is
  always available as an escape hatch if a one-off site cannot
  accept a flume migration; it is just not the default.

## Trait Sketch

The trait lives in a new module `crates/transfer/src/channel.rs` (no
new crate). It is intentionally minimal - associated types for the
sender and receiver, four required methods, two optional helpers.

```rust
//! Crate-internal abstraction over channel implementations that need
//! both sync and async surfaces. See docs/design/async-channel-abstraction.md.

/// A bounded channel that exposes both sync and async send/recv halves.
///
/// Implementations are chosen per call site. For pure-sync hot paths use
/// [`engine::concurrent_delta::work_queue`] (crossbeam) directly; this trait
/// is for sites that must cross a sync-async boundary.
pub trait TransferChannel: Sized {
    /// Item type carried by the channel.
    type Item: Send + 'static;
    /// Sender handle.  Cloneability is implementation-defined.
    type Sender: TransferSender<Item = Self::Item>;
    /// Receiver handle.
    type Receiver: TransferReceiver<Item = Self::Item>;

    /// Constructs a bounded channel of the requested capacity.
    fn bounded(capacity: usize) -> (Self::Sender, Self::Receiver);
}

pub trait TransferSender: Send {
    type Item: Send + 'static;

    /// Non-blocking send.  Returns the unsent item if the channel is full
    /// or the receiver has been dropped.
    fn try_send(&self, item: Self::Item) -> Result<(), TrySendError<Self::Item>>;

    /// Blocking send for sync producers.  Waits for capacity.
    fn send_blocking(&self, item: Self::Item) -> Result<(), SendError<Self::Item>>;

    /// Async send for tokio producers.  Suspends the task until capacity is
    /// available.  Cancellation-safe in the same sense as flume's `send_async`.
    fn send_async(
        &self,
        item: Self::Item,
    ) -> impl Future<Output = Result<(), SendError<Self::Item>>> + Send + '_;
}

pub trait TransferReceiver: Send {
    type Item: Send + 'static;

    /// Non-blocking recv.
    fn try_recv(&self) -> Result<Self::Item, TryRecvError>;

    /// Blocking recv for sync consumers.  Returns `Err` once the channel is
    /// drained and all senders are dropped.
    fn recv_blocking(&self) -> Result<Self::Item, RecvError>;

    /// Async recv for tokio consumers.
    fn recv_async(
        &self,
    ) -> impl Future<Output = Result<Self::Item, RecvError>> + Send + '_;
}
```

Notes on the sketch.

- `Self::Sender: !Clone` is enforced per implementation. The flume
  bridge for the daemon hand-off keeps `Sender: Clone` (multiple
  accept tasks can produce); the work-queue impl, if ever migrated,
  does not.
- The async methods return `impl Future` to avoid boxing on the hot
  path. Rust 1.75+ async-fn-in-trait carries this for free.
- Cancellation semantics on `recv_async` mirror flume: dropping the
  future before completion is safe; dropping after the future has
  already observed a value but before yielding loses that value, so
  callers either use it inside a `select!` arm with peer-drop
  notification or commit to the `tokio::pin!` pattern. We document
  this once at the trait, not at every call site.
- The trait is `pub(crate)` initially. Public exposure is deferred
  until at least two crates need it (engine + daemon).

A flume-backed implementation is then approximately 40 lines.

```rust
struct FlumeChannel<T>(PhantomData<T>);

impl<T: Send + 'static> TransferChannel for FlumeChannel<T> {
    type Item = T;
    type Sender = flume::Sender<T>;
    type Receiver = flume::Receiver<T>;
    fn bounded(capacity: usize) -> (Self::Sender, Self::Receiver) {
        flume::bounded(capacity)
    }
}

impl<T: Send + 'static> TransferSender for flume::Sender<T> {
    type Item = T;
    fn try_send(&self, item: T) -> Result<(), TrySendError<T>> { /* map flume err */ }
    fn send_blocking(&self, item: T) -> Result<(), SendError<T>> { self.send(item).map_err(into) }
    fn send_async(&self, item: T) -> impl Future<Output = Result<(), SendError<T>>> + Send + '_ {
        async move { self.send_async(item).await.map_err(into) }
    }
}
// Receiver is symmetric.
```

The mapping helpers (`into`) are local error conversions; the public
error types (`SendError`, `RecvError`, `TrySendError`, `TryRecvError`)
live alongside the trait so call sites do not need to depend on
`flume` directly.

## Migration Plan: Five Sites

The five sites that land this abstraction.

1. **Async daemon listener hand-off (#1934).** New code. The
   `async_session::listener` task accepts a TCP connection
   (`listener.rs:185-249`) and pushes a `SessionWork` value through
   `flume::bounded` to a sync transfer worker pool.
   `Sender::send_async` on the accept side, `Receiver::recv_blocking`
   on the worker side. Capacity = `max_connections` from
   `ListenerConfig` (`listener.rs:36-49`).
2. **Async file-job dispatcher (#1591 audit, see
   `crates/transfer/src/pipeline/async_dispatch.rs:29-56`).**
   Replace the current `tokio::sync::mpsc::Sender<FileJob>` with a
   `flume::Sender<FileJob>`. Producer keeps `send().await`; consumer
   gains the option of running synchronously when the transfer
   engine is invoked from a non-async caller (CLI test harness,
   integration shims). Capacity stays at
   `DEFAULT_JOB_CHANNEL_CAPACITY = 32` (`pipeline/mod.rs:206`).
3. **Async SSH transport evaluation (#1593).** Sync rayon sender ->
   async `tokio::io::AsyncWrite` socket bridge. The bridge is a
   `flume::bounded(16)` channel of fixed-size byte chunks; sender
   pushes via `send_blocking`, an async forwarder task does
   `recv_async` and `socket.write_all().await`.
4. **DeltaConsumer reorder thread
   (`crates/engine/src/concurrent_delta/consumer.rs:47`).** Today a
   `std::sync::mpsc` channel feeds in-order `DeltaResult`s back to
   the caller. Migrating to flume gives the async pipeline (#1591)
   a path to await reorder-buffer output without a `block_on`
   shim. The migration is opt-in: sync callers keep their existing
   blocking `recv()` shape via `recv_blocking`.
5. **Legacy daemon connection wakeup
   (`crates/daemon/src/daemon/sections/server_runtime/connection.rs:286`).**
   Audit target from #1592. The legacy `std::sync::mpsc` is used
   only by the sync server runtime; once the async listener path
   subsumes it, this site folds into (1). Until then it stays on
   stdlib `mpsc` - no abstraction needed.

The existing **WorkQueue (#1744)** at
`crates/engine/src/concurrent_delta/work_queue/bounded.rs` is
deliberately **not** in the migration list. It has no async
crossing - producer and all consumers are sync threads under
rayon - and `crossbeam_channel` is faster on the contended SPMC
case than any sync-async hybrid. We revisit only if a future
multi-producer requirement (#1382, #1569) forces a `Clone` sender,
at which point the choice is a separate decision documented in its
own design note.

The hand-rolled SPSC (`crates/transfer/src/pipeline/spsc.rs`) is
also out of scope. It is a pure performance primitive for the
network-to-disk path and never touches a runtime.

## Wire Compatibility

Zero impact. Channels are internal coordination only - they do not
appear on the wire, do not change protocol versioning, do not
affect capability negotiation, and do not touch `protocol::messages`
or any of the multiplex frame helpers in `crates/protocol/src`.
The migration changes only intra-process plumbing.

## Backpressure and Fairness

All channels under this abstraction are **bounded**. Capacity is
chosen by the call site, not by the trait.

- Sync producer on a full channel: blocks in `send_blocking` until
  capacity frees, identical to today's `crossbeam_channel::Sender::send`
  (`bounded.rs:78-80`).
- Async producer on a full channel: `send_async` returns a future
  that suspends the task; the runtime is free to schedule other
  work. Behaviour is identical to `flume::Sender::send_async`.
- Receiver fairness: FIFO by construction. flume's bounded
  channels are MPMC and preserve send order across producers, the
  same property `crossbeam_channel::bounded` provides. We do not
  introduce priority scheduling - if the daemon needs preemption,
  it uses a separate `tokio::sync::Notify` for control plane and
  keeps the data plane FIFO.
- Capacity recommendations.
  - Daemon hand-off: capacity = `max_connections`. A burst that
    saturates accept also saturates the worker pool; queueing
    further is a slow leak.
  - File-job dispatch: capacity = 32 (`DEFAULT_JOB_CHANNEL_CAPACITY`).
    Keeps consumer saturated while bounding the FileJob memory
    footprint at ~16KB.
  - SSH chunk forwarder: capacity = 16. Each chunk is up to 32KB
    so peak buffering is ~512KB.

Backpressure on a full channel is the entire point of bounded
buffering; it is what propagates network slowness back to the disk
reader and disk slowness back to the network. None of the chosen
capacities introduce head-of-line blocking the protocol cannot
already handle - upstream rsync's buffered pipes have the same
property.

## Risks

- **Dual-runtime overhead.** Adding flume to crates that today have
  no async surface (notably `engine`) means those crates compile
  flume even when the async feature is off. Mitigation: gate the
  channel module behind `#[cfg(feature = "async")]` so non-async
  builds (CLI default, embedded scenarios) do not pay the cost.
  flume itself is small, but we keep the discipline.
- **flume version pinning.** Pin `flume = "=0.11.x"` initially to
  avoid surprise behavioural changes. flume's release cadence is
  modest (1-2 minors per year) so this is low maintenance. Track
  upstream in `Cargo.toml` next to the existing crossbeam pins
  (`Cargo.toml:199-200`).
- **`recv_async` cancellation footgun.** Dropping a `recv_async`
  future after it has registered for a wakeup but before it
  returns the value can lose that value. flume documents this;
  `tokio::sync::mpsc::Receiver::recv` has the same shape. We
  mitigate by:
  - Documenting the constraint at the trait level once.
  - Never using `recv_async` inside a `select!` arm without an
    explicit shutdown channel that drains the data channel after
    cancellation.
  - Code review checklist item: any new `recv_async` call must
    either run to completion or follow the shutdown-then-drain
    pattern.
- **Mixed sync and async halves on the same channel.** If a single
  channel is held by both a sync `recv_blocking` consumer and an
  async `recv_async` consumer, dropping one half while the other
  is parked can deadlock the survivor (the disconnect notification
  may only reach one waker family). Mitigation: per-channel
  ownership rule - exactly one consumer surface, chosen at
  construction. We document the rule and rely on the type-level
  separation (`Sender` and `Receiver` distinct, not
  `Channel::send`/`Channel::recv` on a shared object).
- **Tokio runtime requirement leak.** A crate that only needs the
  trait must not transitively pull tokio. flume's async
  implementation hooks into the standard `Future` machinery and
  does not require tokio specifically, so this is structurally
  safe; the test for it is `cargo tree --no-default-features` after
  the migration.
- **Hot-path benchmark regression.** If a future change moves a
  bounded channel from crossbeam to flume in a hot path by
  accident, we want CI to catch it. Mitigation: the benchmark
  suite gains a microbench for the work queue and the SPSC that
  asserts a per-item budget. This lives alongside the existing
  `scripts/benchmark.sh` infrastructure and is wired into the
  benchmark workflow.

## Tracking (follow-up TODOs, listed only)

These are the implementation tickets the design implies. They are
**not** added to the persistent backlog by this note.

1. Implement the `TransferChannel` / `TransferSender` /
   `TransferReceiver` traits and the `flume`-backed implementation in
   `crates/transfer/src/channel.rs`, gated behind the existing
   `async` feature.
2. Migrate `crates/transfer/src/pipeline/async_dispatch.rs` and
   `crates/transfer/src/pipeline/async_pipeline.rs` from
   `tokio::sync::mpsc` to the `TransferChannel` API. Verify the
   existing async tests still pass (the producer/consumer shape
   does not change).
3. Land the daemon hand-off channel in #1934. The async listener
   task pushes accepted-connection work items through a
   `TransferChannel` to a sync worker pool; capacity = configured
   `max_connections`.
4. Land the SSH transport bridge in #1593. Sync sender, async
   socket forwarder, single `TransferChannel` between them with
   16-deep capacity.
5. Audit the WorkQueue (#1744) for any future multi-producer
   requirement (#1382, #1569). If multi-producer is needed, decide
   in a separate design note whether to keep crossbeam (which
   already supports `Sender: Clone`) or migrate. No action while
   the SPMC contract holds.
6. Benchmark the migrated sites against a baseline `crossbeam_channel`
   build. Acceptance: no regression on the work queue and SPSC
   microbenches; daemon throughput within 5% of the sync baseline at
   200 concurrent sessions.
7. Migrate the legacy stdlib `mpsc` site at
   `crates/daemon/src/daemon/sections/server_runtime/connection.rs:286`
   under #1592 once the async listener path is the production
   default; until then, leave it alone.

## References

- #1591 - this design's tracking issue (channel abstraction audit).
- #1592 - legacy `std::sync::mpsc` audit.
- #1593 - SSH async transport evaluation.
- #1744 - bounded delta work queue, the SPMC channel that stays on
  crossbeam.
- #1856 - work queue decomposition into `bounded`/`drain`/`iter`/
  `capacity` submodules.
- #1934 - tokio-based async daemon listener.
- `crates/engine/src/concurrent_delta/work_queue/bounded.rs` and
  siblings - SPMC work queue (crossbeam).
- `crates/transfer/src/pipeline/spsc.rs` - hand-rolled SPSC for the
  network-to-disk path (ArrayQueue).
- `crates/transfer/src/pipeline/async_dispatch.rs`,
  `crates/transfer/src/pipeline/async_pipeline.rs` - existing
  tokio mpsc usage.
- `crates/daemon/src/daemon/async_session/listener.rs` - tokio
  listener and connection admission.
- `Cargo.toml:188-200` - workspace tokio and crossbeam pins; flume
  is added next to these.
