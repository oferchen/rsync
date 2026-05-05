# Arc-wrapped `WorkQueueSender` for Multi-Producer Fan-In (#1610)

Companion note to `multi-producer-workqueue.md` (#1382). That note recommends
Design A (a vector of independent senders returned from a `with_producers(n)`
constructor). This note covers the variant tracked at #1610 in depth: a
shared `Arc<WorkQueueSender>` handed to N producer threads. Both designs
target the same use cases - parallel source enumeration (#1573) and
multi-root fan-in (#1382) - and both must coexist with the wire-protocol
single-producer path documented in
`crates/engine/src/concurrent_delta/multi_producer_audit.rs:8-19`.

The deliverable here is the API shape, lifecycle, ordering invariants, and
test plan needed to land Design B without invalidating Design A. The two
designs are not mutually exclusive; #1404 (the umbrella fan-in design) will
choose between them based on benchmark data captured by #1613.

## 1. Problem

Three production scenarios need more than one thread feeding the bounded
delta work queue at
`crates/engine/src/concurrent_delta/work_queue/bounded.rs:48-50`:

1. **Parallel source enumeration (#1573).** A single root with millions of
   small files spends most of its wall time inside the directory walker
   (`crates/engine/src/walk/`), serially issuing `getdents`/`readdir`
   syscalls and `stat` lookups while the rayon consumer pool sits idle.
   Splitting the walker into a coordinator plus N child walkers, each
   producing into the shared delta queue, lets the consumer side stay
   saturated.

2. **Multi-root transfers (#1382).** `oc-rsync src1/ src2/ src3/ dst/`
   today walks each root sequentially under
   `crates/engine/src/local_copy/executor/sources/orchestration.rs:79`
   (the `for source in plan.sources()` loop). The roots are disjoint
   I/O subgraphs and benefit linearly from concurrent enumeration.

3. **`--files-from` reader plus walker.** When `--files-from` lists
   directories that the main walker must still expand, the reader and the
   walker can run as two producers feeding one queue.

In all three cases the consumer side already supports parallel drain via
`crates/engine/src/concurrent_delta/work_queue/drain.rs:14-156`. The
limiter is the single-producer compile-time invariant on
`WorkQueueSender`
(`crates/engine/src/concurrent_delta/work_queue/bounded.rs:21-23`,
`crates/engine/src/concurrent_delta/work_queue/mod.rs:11-21`), which
withholds `Clone` to prevent multi-producer use outside the
`multi-producer` cargo feature
(`crates/engine/Cargo.toml:87-90`).

The wire-protocol receive path stays single-producer regardless; that
constraint is intrinsic to the protocol's single multiplexed stream and
is not affected by this design.

## 2. Current Single-Producer Architecture

```text
+-----------------+      crossbeam_channel::bounded(N)      +-----------------+
| local planner   | --------------------------------------> | rayon workers   |
| OR wire reader  |          DeltaWork                      | drain_parallel  |
| (one tx)        | --------------------------------------> | (M consumers)   |
+-----------------+                                          +-----------------+
        |                                                            |
        | blocks when full (backpressure)                             v
        v                                                       ReorderBuffer
   monotonic seq#                                                    |
                                                                     v
                                                              in-order sink
```

Concrete citations against the current tree:

- Sender type:
  `crates/engine/src/concurrent_delta/work_queue/bounded.rs:48-50`
  (field `tx: Sender<DeltaWork>` at `pub(super)` visibility).
- Producer entry point `WorkQueueSender::send`:
  `crates/engine/src/concurrent_delta/work_queue/bounded.rs:74-81`.
- Constructors `bounded` and `bounded_with_capacity`:
  `crates/engine/src/concurrent_delta/work_queue/bounded.rs:88-104`
  (uses `crossbeam_channel::bounded(capacity)` on line 102).
- Default capacity (`2 * rayon::current_num_threads()`):
  `crates/engine/src/concurrent_delta/work_queue/capacity.rs:7-8,32-38`.
  Adaptive sizing: same file, lines 40-76.
- Receiver iterator that closes on channel disconnect:
  `crates/engine/src/concurrent_delta/work_queue/iter.rs:29-35`.
- Parallel drain implementations:
  `crates/engine/src/concurrent_delta/work_queue/drain.rs:14-156`.
- SPMC contract documented at module level
  (`crates/engine/src/concurrent_delta/work_queue/mod.rs:11-21`) and
  on the type itself
  (`crates/engine/src/concurrent_delta/work_queue/bounded.rs:19-26`).
- Call-site audit (all existing sites are correctly single-producer):
  `crates/engine/src/concurrent_delta/multi_producer_audit.rs:1-95`.
- Production single-sender owner is `ParallelDeltaPipeline` at
  `crates/transfer/src/delta_pipeline.rs:181-219`; the only
  `submit_work` call site is at lines 222-234.

## 3. Why `Arc<WorkQueueSender>` Instead of Clone-able Sender

Design A from `multi-producer-workqueue.md` returns a
`Vec<WorkQueueSender>` of length N from a `with_producers(n)`
constructor; each sender owns one crossbeam sender clone. Design B
(this note) wraps a single `WorkQueueSender` in an `Arc` and shares the
`Arc` among N producers.

The two designs differ on three axes.

### 3.1 Producer-count flexibility

- Design A fixes the producer count at construction. Adding a producer
  later requires another constructor call or feature-gated `Clone`
  on `WorkQueueSender`.
- Design B allows a producer to spawn a child producer mid-flight by
  calling `Arc::clone` on its handle. The reference count tracks the
  live producer count; the channel closes only when the count reaches
  zero. This is the right shape for hierarchical walkers where a
  parent enumerator discovers a deep subtree partway through and wants
  to delegate it to a freshly spawned worker.

### 3.2 Refcount cost

- Design A has zero per-send refcount work on the hot path. Each
  producer holds its own crossbeam sender clone (which is itself an Arc
  internally, but cloning happens once at construction).
- Design B adds one Arc-deref per `send` call (the producer dereferences
  the `Arc<WorkQueueSender>` to get at `&self.tx`). This is cheap (no
  atomic on read), but Arc cloning when forking off a child producer
  involves an atomic increment. For small messages and many producers,
  the cumulative refcount traffic is measurable; #1613 is the benchmark
  task that quantifies it.

### 3.3 Type-level expression of the contract

- Design A returns `Vec<WorkQueueSender>` of length N. The caller
  cannot accidentally produce N+1 senders without going through
  `with_producers` again.
- Design B returns one `Arc<WorkQueueSender>`. Cloning the Arc is
  unmarked at the call site; a stray `Arc::clone` in a debug helper or
  scope-spawned closure can leave the channel open past the intended
  shutdown. The lifecycle is correct but not visible in the type
  signature.

The trade-off summary is therefore:

| Axis                       | Design A (Clone-vec) | Design B (Arc)        |
| -------------------------- | -------------------- | --------------------- |
| Dynamic producer count     | No                   | Yes                   |
| Hot-path refcount cost     | None                 | One Arc deref / send  |
| Producer-count contract    | Type-encoded         | Runtime               |
| Fan-out from one node      | Awkward              | Natural               |
| Static fan-in (N roots)    | Natural              | Awkward               |

For the static-fan-in cases (multi-root, two-producer
`--files-from` + walker), Design A is the better fit. For dynamic
fan-out from a coordinator that does not know the producer count up
front, Design B is the better fit. Both can coexist; the engine
exposes both shapes and the caller picks.

## 4. The Crossbeam Baseline

The underlying channel is `crossbeam_channel::bounded` (created at
`crates/engine/src/concurrent_delta/work_queue/bounded.rs:102`). The
crossbeam sender is itself `Clone` and is reference-counted internally;
its `Clone` impl is the standard MPMC lifecycle. When the last
`crossbeam_channel::Sender` clone drops, the channel disconnects and
the receiver's `recv` returns `Err(RecvError)`. This is the property
that
`crates/engine/src/concurrent_delta/work_queue/iter.rs:29-35`
relies on to terminate the iterator (via `self.rx.recv().ok()`).

Design B does not replace crossbeam's internal reference counting; it
layers an additional Arc on top of the engine-level `WorkQueueSender`
type:

```text
   Arc<WorkQueueSender>  -->  WorkQueueSender { tx: crossbeam::Sender }
       (engine layer)              (engine layer)        (channel layer)
              |                          |                    |
              | Arc::clone               | (no clone needed)  | internal Arc::clone
              v                          v                    v
       Arc<WorkQueueSender>  -->  same WorkQueueSender   same crossbeam channel
```

The double-Arc layout is the cost (one extra atomic per fork-off) and
the benefit: the engine-level `WorkQueueSender` stays `!Clone` outside
the `multi-producer` cargo feature, so existing wire-protocol callers
cannot accidentally fan out. Only callers that opt into the Arc
constructor get the Arc handle, and only those callers can clone.

## 5. API Surface

The proposal adds two constructors and one type alias to
`crates/engine/src/concurrent_delta/work_queue/bounded.rs`. No
existing API is removed or altered.

```rust
use std::sync::Arc;

/// Multi-producer handle for the bounded delta work queue.
///
/// Wraps a `WorkQueueSender` in an `Arc` so multiple producer threads
/// can share one channel. The channel disconnects when the last
/// `SharedSender` clone drops, enabling consumer termination.
pub type SharedSender = Arc<WorkQueueSender>;

/// Creates a bounded work queue whose sender is wrapped in an `Arc`,
/// suitable for fan-in from multiple producer threads.
///
/// Capacity follows the same default policy as `bounded`
/// (`2 * rayon::current_num_threads()`).
pub fn shared() -> (SharedSender, WorkQueueReceiver);

/// Creates an Arc-wrapped sender with explicit capacity. The capacity
/// is the underlying crossbeam channel bound and is shared across all
/// producers.
pub fn shared_with_capacity(capacity: usize)
    -> (SharedSender, WorkQueueReceiver);
```

Notes on the surface:

- `SharedSender` is a type alias rather than a newtype to keep the
  surface area small. Producer code that wants the `Arc::clone` ergonomics
  works with the type directly.
- `WorkQueueSender` itself remains `Send + !Clone` outside the
  `multi-producer` feature. The Arc is the only public path to multiple
  producers, ensuring the producer-count contract is observable in code
  review (every `Arc::clone(&shared_tx)` is a deliberate fan-out).
- `shared` and `shared_with_capacity` mirror the names of the existing
  constructors `bounded` and `bounded_with_capacity` at
  `crates/engine/src/concurrent_delta/work_queue/bounded.rs:88-104`.
- The receiver type is unchanged. The consumer-side API at
  `crates/engine/src/concurrent_delta/work_queue/drain.rs:14-156`
  stays as-is; this design touches only the producer side.
- `Arc::strong_count` on the returned handle is a debug-only
  observability hook; production code should not branch on it.

The implementation is a one-line wrap around the existing constructor:

```rust
pub fn shared_with_capacity(capacity: usize)
    -> (SharedSender, WorkQueueReceiver)
{
    let (tx, rx) = bounded_with_capacity(capacity);
    (Arc::new(tx), rx)
}
```

A cousin constructor `shared` (without an explicit capacity) calls
`shared_with_capacity(default_capacity())`, where `default_capacity` is
re-exported from
`crates/engine/src/concurrent_delta/work_queue/capacity.rs:32-38`.

## 6. Backpressure Preservation

The bounded contract is preserved end-to-end. Capacity is a property of
the underlying crossbeam channel created at
`crates/engine/src/concurrent_delta/work_queue/bounded.rs:102` and is
unchanged by the Arc wrap. With N producers sharing one
`Arc<WorkQueueSender>`:

- Each producer's `send` call goes through one `Arc::deref` and then
  one crossbeam send. When the channel is full, crossbeam blocks the
  caller on its internal mutex until a slot opens.
- Crossbeam's blocked-sender wake-up policy is approximately FIFO
  (one waiter is woken per drained slot), so no producer is starved
  indefinitely as long as the consumer keeps draining.
- The total in-flight count never exceeds the channel capacity
  regardless of N, because the capacity is a hard property of the
  crossbeam channel itself.

Capacity tuning under multi-producer is a follow-up concern. The
existing default of `2 * rayon::current_num_threads()` may be too
small when the producer count exceeds the consumer count - all
producers fight for a small set of slots and progress is
fairness-bounded rather than throughput-bounded. The recommended
heuristic for multi-producer use is:

```text
capacity = max(2 * rayon::current_num_threads(),
               producer_count * SLOTS_PER_PRODUCER)
```

with `SLOTS_PER_PRODUCER` chosen so each producer has at least 2
in-flight slots on average. Picking the constant is a benchmark
question owned by #1613; this design specifies only that the call site
must size capacity with both the consumer count and the producer count
in mind.

## 7. Drop Semantics

The lifecycle invariant is: the receiver iterator at
`crates/engine/src/concurrent_delta/work_queue/iter.rs:29-35`
terminates only when every `SharedSender` clone has been dropped, at
which point the underlying crossbeam channel disconnects and `recv`
returns an error.

Concretely:

1. Producer `P_i` finishes its work, drops its `SharedSender` clone.
2. The `Arc` reference count is decremented.
3. When the last clone drops, `Arc::drop` runs `WorkQueueSender::drop`,
   which drops the inner `crossbeam_channel::Sender`.
4. The crossbeam channel observes that all senders are gone and
   marks itself disconnected.
5. The next call to `WorkQueueIter::next`
   (`crates/engine/src/concurrent_delta/work_queue/iter.rs:32-34`)
   returns `None` and the consumer's drain loop exits.

This matches the multi-sender close semantics already verified for
the `Clone`-based path by the test
`multi_producer_receiver_completes_only_when_all_senders_dropped`
(`crates/engine/src/concurrent_delta/work_queue/tests.rs:651-697`).
The Arc-based path inherits the same semantics because the underlying
crossbeam channel is the same.

The RAII test
`receiver_drop_signals_producer_raii`
(`crates/engine/src/concurrent_delta/work_queue/tests.rs:861-900`)
proves that producers do not hang if the receiver drops first. With
multiple Arc-clone producers, all of them observe the same disconnect
flag inside the crossbeam channel and unblock together.

The risk to watch is a stray `Arc::clone` that escapes the producer
scope. For example, capturing the `Arc` in a debug-print closure that
outlives the producer keeps the channel open and stalls the consumer.
Mitigation:

- Spawn each producer thread with `move` so it owns a unique Arc
  clone, dropped automatically on thread exit.
- For long-lived host objects that hold the Arc as a struct field,
  expose a `shutdown(self)` method that takes `self` by value and
  drops the field explicitly.

## 8. Sequence-Number Ordering Invariants

The downstream consumer reorders work via `ReorderBuffer`
(`crates/engine/src/concurrent_delta/reorder.rs:64-83`,
`crates/engine/src/concurrent_delta/reorder.rs:97-119`), referenced
from the parallel pipeline at
`crates/transfer/src/delta_pipeline.rs:148-167`. Each `DeltaWork`
carries a `sequence: u64` that the buffer uses to gate emission until
a contiguous run becomes available. Single-producer assignment is a
simple counter
(`crates/transfer/src/delta_pipeline.rs:223-226`).

Multi-producer must keep the buffer's invariants intact: every emitted
sequence number is unique, and the set of emitted sequence numbers
forms a dense range starting at 0. Two strategies satisfy this under
Design B; both are also discussed under Design A in
`multi-producer-workqueue.md`. Design B inherits the same options
because the question is "where does the sequence number come from",
not "what shape is the sender handle".

### 8.1 Atomic global counter (default for dynamic fan-out)

A shared `AtomicU64` is incremented per emission:

```text
seq = counter.fetch_add(1, Ordering::Relaxed);
work.set_sequence(seq);
shared_tx.send(work)?;
```

The relaxed ordering is correct because the only invariant is that
each producer observes a unique number; no read-acquire dependency on
prior work exists at the queue layer. The
`multi_producer_requires_atomic_sequence_coordination` test at
`crates/engine/src/concurrent_delta/work_queue/tests.rs:172-214`
already exercises this pattern under the `multi-producer` feature
flag. Design B reuses the same coordination primitive.

This strategy is the natural fit for Arc-based fan-out: the producer
count is dynamic, so static range pre-allocation is not possible, and
the per-emission atomic is the price of dynamism.

### 8.2 Per-producer sequence ranges (when counts are known)

When the caller knows the per-producer emission count up front (the
multi-root case), each producer is given a base offset and emits
`base + i` for its `i`-th item. This avoids the per-send atomic and is
the recommended strategy when applicable.

Design B can support this by augmenting the Arc-based handle with a
per-producer `RangeAllocator`:

```rust
pub struct RangeAllocator { base: u64, next: AtomicU64, end: u64 }
```

Each producer holds a `RangeAllocator` plus a clone of the
`SharedSender`. The allocator's `next.fetch_add(1, Ordering::Relaxed)`
within `[base, end)` is the producer-local equivalent of the global
counter, with no contention against other producers.

The reorder-buffer compatibility constraints from
`multi-producer-workqueue.md` Section "Ordering Semantics" carry over:

- Sparse ranges (a producer's actual emission count is below its
  allocated range) leave gaps that the buffer cannot fill. Either
  size ranges precisely (two-pass enumeration) or accept the
  global-counter strategy instead.
- The reorder buffer's adaptive policy
  (`crates/engine/src/concurrent_delta/reorder.rs:74-83` and the
  state machine in
  `crates/engine/src/concurrent_delta/adaptive.rs`) reacts to the
  high-water gap. Producers finishing their ranges out of order
  inflate the gap; the buffer's adaptive minimum should be set to at
  least the largest expected per-producer range to avoid thrashing.

### 8.3 Why Design B does not pick a single strategy

Design B exposes the sender shape (`Arc<WorkQueueSender>`) but stays
agnostic about sequence assignment. The caller chooses:

- A multi-root call site that knows file counts up front uses the
  range strategy.
- A coordinator that spawns child producers dynamically uses the
  atomic counter strategy.

Both strategies live above the queue and outside this design's
surface. The queue's only contract is "sequence numbers passed in
through `WorkQueueSender::send` reach the consumer"; the consumer's
`ReorderBuffer` enforces the unique-and-dense invariant.

## 9. Migration Path

The upstream sequence is:

1. **#1404 (umbrella fan-in design, pending).** Selects Design A
   versus Design B per call site based on benchmarks. This note
   provides the Design B half of the input.
2. **#1571 (multi-producer test, completed).** The tests at
   `crates/engine/src/concurrent_delta/work_queue/tests.rs:560-771`
   already prove the underlying crossbeam-channel multi-producer
   semantics. Design B's test plan reuses those tests at the
   `Arc`-wrapped boundary.
3. **This design (#1610).** Lands `shared` and
   `shared_with_capacity` constructors plus the `SharedSender` alias
   in
   `crates/engine/src/concurrent_delta/work_queue/bounded.rs`,
   gated by the existing `multi-producer` cargo feature
   (`crates/engine/Cargo.toml:87-90`). No call site migrates yet.
4. **#1613 (benchmark, pending).** Compares Design A
   (`Vec<WorkQueueSender>`), Design B (`Arc<WorkQueueSender>`), and
   single-producer sequential under multi-root and
   `--files-from`-plus-walker workloads. The benchmark output drives
   the per-call-site decision in #1404.
5. **Post-benchmark integration.** Whichever shape wins per call site
   is wired into the local-copy executor at
   `crates/engine/src/local_copy/executor/sources/orchestration.rs:79`
   and any other site #1404 identifies. The wire-protocol receive
   path at
   `crates/transfer/src/delta_pipeline.rs:222-234` does not migrate.

The migration is feature-gated until #1613 picks a winner. Until then
the API is available behind `#[cfg(feature = "multi-producer")]` for
test and bench code only.

## 10. Test Plan

The existing tests at
`crates/engine/src/concurrent_delta/work_queue/tests.rs:560-771`
already cover the underlying multi-producer crossbeam semantics. The
Arc-wrapped variant adds three property-style tests, all gated by the
`multi-producer` feature.

### 10.1 Reuse from #1571

The existing tests at
`crates/engine/src/concurrent_delta/work_queue/tests.rs:560-771` apply
to Design B with no substantive change beyond constructing producers
via `shared`/`shared_with_capacity` and `Arc::clone` rather than
`tx.clone()`. Notably:

- `clone_sender_multiple_producers` (lines 561-587) covers two
  producers feeding one queue.
- `multi_producer_many_senders` (lines 591-623) covers eight
  producers and channel close on last drop.
- `multi_producer_dropping_one_sender_does_not_affect_others`
  (lines 627-649) covers partial drops.
- `multi_producer_receiver_completes_only_when_all_senders_dropped`
  (lines 653-697) is the canonical close-on-last-drop test.
- `multi_producer_send_error_after_receiver_drop` (lines 775-791) is
  the canonical receiver-dropped-first test.

### 10.2 New property tests (three)

The new tests are property-based via `proptest`, mirroring the style
of the existing `drain_parallel_preserves_ordering` property test at
`crates/engine/src/concurrent_delta/work_queue/tests.rs:946-997`.

1. **`shared_sender_completeness_under_n_producers`**

   Property: for any `n in 2..=16` and `items_per_producer in
   1..=200`, all `n * items_per_producer` items reach the consumer
   exactly once. The producers each hold an `Arc::clone` of the
   `SharedSender`. The original Arc is dropped before the consumer
   drains so the channel close depends entirely on the producer
   clones.

2. **`shared_sender_disconnect_signals_all_producers`**

   Property: dropping the receiver while N producers are mid-send
   causes every producer to observe `SendError` within a bounded
   wall-clock budget. Generalises the existing
   `receiver_drop_signals_producer_raii` test
   (`crates/engine/src/concurrent_delta/work_queue/tests.rs:862-900`)
   to the Arc-clone topology with arbitrary N.

3. **`shared_sender_strong_count_matches_live_producers`**

   Property: at any observation point, `Arc::strong_count` on the
   `SharedSender` equals the number of live producer threads plus the
   number of references held by the test harness. Tests the
   reference-counting invariant that drives channel close. Failure
   indicates a stray clone has escaped the producer scope.

All three tests run under both `--features multi-producer` and the
default feature set; under the default set they are compiled out by
`#[cfg(feature = "multi-producer")]`, matching the existing pattern
at
`crates/engine/src/concurrent_delta/work_queue/tests.rs:560`.

### 10.3 Benchmark hook (#1613)

A new benchmark `arc_vs_clone_vec_fanin` extends
`crates/engine/benches/drain_parallel_benchmark.rs` (already declared
at `crates/engine/Cargo.toml:140-142`) with two scenarios:

- N producers each holding a crossbeam-cloned `WorkQueueSender`
  (Design A shape).
- N producers each holding an `Arc::clone` of `SharedSender`
  (Design B shape).

The benchmark reports throughput (items/sec at saturated capacity)
and the 99th-percentile per-send latency under N in `{2, 4, 8, 16}`
producers and item-size mixes from the upstream interop corpus. The
output drives #1404's per-call-site decision.

## 11. Non-Goals

- **No protocol crate change.** The wire format is untouched. This
  design is internal to the engine crate. The protocol golden tests
  in `crates/protocol/tests/golden/` and the interop harness in
  `tools/ci/run_interop.sh` are not affected. Specifically called
  out as non-goal in #2085.
- **No CLI surface change.** No new flags, no behavioural changes
  visible to `oc-rsync` users. Capacity tuning lives in the engine
  call site, not in `clap` parsing. Specifically called out as
  non-goal in #2084.
- **No change to the wire-protocol receive path.** The producer at
  `crates/transfer/src/delta_pipeline.rs:222-234` continues to use
  `bounded`/`bounded_with_capacity` and stays single-producer. The
  audit comment at
  `crates/engine/src/concurrent_delta/multi_producer_audit.rs:8-19`
  remains accurate.
- **No replacement of crossbeam.** The underlying channel type
  stays `crossbeam_channel::bounded`. The Arc wrap is at the engine
  layer only.
- **No change to `ReorderBuffer`.** Sequence numbers continue to be
  unique and dense; the buffer's existing capacity bound and
  adaptive policy at
  `crates/engine/src/concurrent_delta/reorder.rs:64-83` and
  `crates/engine/src/concurrent_delta/adaptive.rs` are unchanged.
- **No removal of the `Clone` impl.** The feature-gated `Clone for
  WorkQueueSender` at
  `crates/engine/src/concurrent_delta/work_queue/multi_producer.rs:17-23`
  remains for the fan-out-from-one-coordinator pattern. The Arc
  shape and the Clone shape coexist.

## 12. Open Questions

1. **Per-call-site selection.** Multi-root has a static count and
   suits Design A's typed `Vec`. Hierarchical walkers suit Design B's
   dynamic fan-out. #1613 confirms both intuitions.
2. **Sequence-allocator placement.** Should `RangeAllocator` live in
   the engine crate or in the planner? Today
   `plan.sources()` carries no per-root sequence metadata; threading
   a range through is a separate refactor that #1404 may subsume.
3. **`SharedSender` as alias versus newtype.** Alias is smallest but
   exposes the inner Arc. Newtype with `fork(&self) -> Self` makes
   intent visible at the cost of more code. Current recommendation
   is the alias, revisited if reviewers flag ergonomic risk.
4. **Capacity tuning for high producer counts.** The
   `SLOTS_PER_PRODUCER` heuristic in Section 6 is unspecified. Pick
   under #1613 with fairness data from a 16-producer stress run.
5. **Adaptive reorder-buffer policy under Arc fan-out.** The
   adaptive minimum may need a multi-producer-aware floor. Owned by
   the reorder-buffer-metrics design.
6. **Telemetry hooks.** Should `SharedSender` expose
   `Arc::strong_count` through a debug helper? The `tracing` cargo
   feature at `crates/engine/Cargo.toml:103-104` is the natural
   integration point.
7. **Async pipeline interaction.** When the async channel
   abstraction (#1591) lands, the Arc handle must round-trip through
   a sync-to-async bridge. The bridge author confirms close-on-last-drop
   semantics survive the bridge.
