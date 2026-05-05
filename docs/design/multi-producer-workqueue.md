# Multi-Producer WorkQueue for Multi-Root Transfer Support (#1382)

## Summary

The bounded delta `WorkQueue` (`crates/engine/src/concurrent_delta/work_queue/`)
is documented and enforced as Single-Producer Multiple-Consumer (SPMC). One
upstream thread - the wire reader during a remote transfer, or the planning
thread during a local copy - feeds `DeltaWork` items into a single
`WorkQueueSender`, and a rayon-driven pool of consumers drains them in
parallel. The single-producer invariant is enforced at compile time: the
sender is `Send` but not `Clone` outside the `multi-producer` feature gate
(`crates/engine/src/concurrent_delta/work_queue/bounded.rs:48-50,74-81`).

Multi-root transfers (`oc-rsync foo/ bar/ baz/ dst/`) and the in-progress
parallel directory enumeration work (#1573) need a multi-producer story.
With one queue and N enumerator threads, directory walks can run in parallel
across roots while the consumer side keeps draining work as it lands. The
crossbeam channel underneath is already MPMC; the SPMC restriction is ours
and is documented at #1614.

This note evaluates three multi-producer designs, recommends design A
(Clone-based fan-in) with an explicit producer-count contract, and
specifies the API, ordering, backpressure, drain, and migration semantics
needed to land it without disturbing the wire-protocol single-producer
path. There is zero wire-protocol impact - work-queue coordination is
internal to the engine.

## Problem Statement

### Today: serial enumeration of multiple roots

When a user invokes `oc-rsync foo/ bar/ baz/ dst/`, the local-copy
executor iterates the source list sequentially:

```text
crates/engine/src/local_copy/executor/sources/orchestration.rs:79
    for source in plan.sources() {
        let result = process_single_source(...);
        ...
    }
```

Each call drives a directory walk via the abstractions in
`crates/engine/src/walk/` (`DirectoryWalker` trait, `WalkdirWalker`,
`FilteredWalker`, `WalkConfig`, `WalkEntry`). Walks for `foo/`, `bar/`, and
`baz/` run one after the other. Per-root I/O latency therefore stacks
linearly even though the trees are disjoint and the consumers (rayon
workers feeding the delta pipeline) are idle whenever the walker is
blocked on `getdents`/`readdir`.

The receiver-side wire path is genuinely single-stream and stays
single-producer (see `multi_producer_audit.rs:8-19`). The
opportunity is on the local-copy and pre-sender enumeration paths where
work can be produced concurrently if the queue allows it.

### Today: SPMC contract

`crates/engine/src/concurrent_delta/work_queue/bounded.rs:11-21` documents
the invariant:

> This is SPMC rather than MPMC because the rsync wire protocol is
> inherently single-threaded on the receiving side. `WorkQueueSender`
> enforces this by being `Send` but not `Clone`, preventing multiple
> producers at compile time.

The crossbeam channel underneath
(`crossbeam_channel::bounded` in `bounded.rs:8,102`) is naturally MPMC -
the SPMC restriction is enforced only by withholding `Clone`. The
`multi-producer` cargo feature
(`crates/engine/Cargo.toml:90`) flips this on by adding a `Clone` impl in
`crates/engine/src/concurrent_delta/work_queue/multi_producer.rs:10-23`,
but no production call site uses it yet. Tests behind the same feature
gate exercise multi-producer behaviour
(`crates/engine/src/concurrent_delta/work_queue/tests.rs:560-771`),
including the closure semantics the design below depends on.

### What multi-root needs

A multi-producer queue allows one rayon thread per source root to walk
concurrently and to fan its `DeltaWork` items into the same shared
queue. Because directory walks are I/O-bound and stat-heavy, parallelism
across disjoint roots is approximately linear in the number of roots up
to the queue capacity bound.

The design must keep three properties intact:

1. The wire-protocol receive path stays single-producer with no behaviour
   change (the channel must still close when the lone wire reader drops
   its sender).
2. The `ReorderBuffer` (`crates/engine/src/concurrent_delta/reorder.rs`)
   continues to deliver results in sequence order, regardless of how
   many producers fed the queue.
3. Backpressure remains effective per producer, so a fast walker cannot
   force the engine to buffer an unbounded number of work items.

## Current SPMC Design

```text
+----------------+        bounded(N)         +-----------------+
| wire reader OR | ------------------------> | rayon workers   |
| local planner  |    crossbeam_channel       | drain_parallel  |
|  (single tx)   | ------------------------> |   (M consumers) |
+----------------+                            +-----------------+
        ^                                              |
        |  blocks when full                            v
        +-----------+                          ReorderBuffer
                    |                                  |
                    +-- monotonic seq# (single-       v
                        threaded counter)        in-order sink
```

- One `WorkQueueSender` (`bounded.rs:48-50`).
- One or more `WorkQueueReceiver` consumers, but in practice exactly
  one receiver fed into `drain_parallel` or `drain_parallel_into`
  (`crates/engine/src/concurrent_delta/work_queue/drain.rs:14-156`),
  which spawns rayon tasks per item.
- Capacity defaults to `2 * rayon::current_num_threads()`
  (`bounded.rs:88-92`, `capacity.rs:7-8,32-38`); adaptive sizing per
  average file size lives in `capacity.rs:40-76`.
- Sequence numbers are assigned by the single producer with a plain
  counter; the audit comment in `multi_producer_audit.rs:140-167`
  emphasises the zero-overhead nature of this assignment.
- Channel close is automatic: when the sole `WorkQueueSender` drops,
  the iterator yielded by `WorkQueueIter`
  (`crates/engine/src/concurrent_delta/work_queue/iter.rs:29-35`)
  returns `None`, ending the rayon scope inside `drain_parallel`.

## Three Multi-Producer Designs

### Design A: Multiple `WorkQueueSender` clones (recommended)

Each producer holds its own `WorkQueueSender`, all referencing the same
underlying `crossbeam_channel::Sender`. This is exactly what the
existing `multi-producer` cargo feature provides
(`work_queue/multi_producer.rs:10-23`); the existing tests at
`tests.rs:560-771` already cover correctness:

- `clone_sender_multiple_producers` shows two producers feeding one queue.
- `multi_producer_many_senders` runs eight cloned senders concurrently.
- `multi_producer_receiver_completes_only_when_all_senders_dropped`
  pins the close semantics: the consumer iterator terminates only after
  every cloned sender drops, matching crossbeam's MPMC disconnect rule.
- `multi_producer_send_error_after_receiver_drop` confirms that all
  cloned senders observe `SendError` as soon as the receiver drops.

**Strengths.**

- Matches crossbeam's natural close semantics. The channel disconnects
  only when the reference count of the underlying sender hits zero, so
  RAII-driven thread joins close the queue automatically.
- Zero per-send overhead. Cloning increments crossbeam's internal Arc
  refcount once at construction; sends are unaffected.
- No runtime synchronisation beyond what crossbeam already does.
- Preserves the SPMC code path for the wire reader unchanged - no clone
  call, no extra sender, no behavioural difference.

**Issue.**

- The number of producers must be known at construction time so that
  exactly that many `WorkQueueSender` handles are emitted and dropped.
  An off-by-one mismatch (one sender that never drops) leaves the
  consumer waiting forever. The recommended API surfaces this contract
  explicitly.

### Design B: Arc-wrapped sender

A single `Arc<WorkQueueSender>` is shared among producers. The channel
closes when the last `Arc` drops. This is the variant tracked at #1610.

**Strengths.**

- Producer count can be dynamic; new producers can join by cloning the
  Arc at runtime.

**Issues.**

- Extra atomic refcount on every send call site (the `Arc::clone` cost
  is amortised, but every producer thread holds an `Arc` indirection).
- Less idiomatic than crossbeam's native MPMC sender. Crossbeam already
  uses an Arc internally; layering another one is redundant.
- Hides the producer-count contract instead of making it explicit, which
  is the wrong trade-off for our use cases (multi-root and parallel
  enumeration both know N up front).

### Design C: Producer pool with explicit join

Build N producer threads with a barrier; the queue closes when the
barrier hits N. The receiver pulls until barrier-closed.

**Strengths.**

- Explicit lifecycle control - no reliance on Drop ordering.

**Issues.**

- Producer count must still be known up front, so the supposed
  flexibility advantage over Design A is illusory.
- Requires a second synchronisation primitive on top of the channel.
- Doesn't compose with rayon's work-stealing pool because barriers
  block worker threads; the wire reader path would need a different
  termination story.

## Recommendation: Design A (Clone) with a Producer-Count Contract

Design A is the right shape for our use cases. Multi-root transfers
know the producer count at construction (it equals
`plan.sources().len()`); parallel enumeration knows it at the point we
decide how many walker threads to spawn. We get the close-on-last-drop
semantics for free from crossbeam.

The recommendation is to keep the existing `bounded()` /
`bounded_with_capacity()` constructors (single-producer) and add an
explicit constructor that surfaces the contract:

```rust
/// Returns one receiver and exactly `n` independent senders. The channel
/// closes when all `n` senders have been dropped. Each sender is `Send`
/// and may be passed to a producer thread; the senders do not implement
/// `Clone` so the producer count cannot drift.
pub fn with_producers(n: NonZeroUsize)
    -> (Vec<WorkQueueSender>, WorkQueueReceiver);

pub fn with_producers_and_capacity(n: NonZeroUsize, capacity: usize)
    -> (Vec<WorkQueueSender>, WorkQueueReceiver);
```

The existing `Clone` impl gated by `multi-producer` is retained for the
narrower use case where producers fan out from one upstream walker that
needs to hand a sender to each child it spawns. The two use cases differ:

- `with_producers(n)` is the *static fan-in* case: N independent
  enumerator threads, each owning one sender.
- `Clone` is the *fan-out from a single root* case: a single coordinator
  spawns workers on demand and clones the sender each time. The
  coordinator must remember to drop its own sender after spawning.

Both can coexist; `with_producers` builds N senders by cloning the
underlying crossbeam handle internally and then dropping the seed
sender, so the public API does not expose `Clone` unless the feature
flag is on.

## API Sketch

```rust
use std::num::NonZeroUsize;

// Existing - unchanged.
pub fn bounded() -> (WorkQueueSender, WorkQueueReceiver);
pub fn bounded_with_capacity(capacity: usize)
    -> (WorkQueueSender, WorkQueueReceiver);

// New - explicit multi-producer construction.
pub fn with_producers(n: NonZeroUsize)
    -> (Vec<WorkQueueSender>, WorkQueueReceiver);
pub fn with_producers_and_capacity(n: NonZeroUsize, capacity: usize)
    -> (Vec<WorkQueueSender>, WorkQueueReceiver);
```

Sketch of the implementation, mirroring the existing constructor at
`bounded.rs:99-104`:

```rust
pub fn with_producers_and_capacity(
    n: NonZeroUsize,
    capacity: usize,
) -> (Vec<WorkQueueSender>, WorkQueueReceiver) {
    assert!(capacity > 0, "work queue capacity must be non-zero");
    let (tx, rx) = crossbeam_channel::bounded(capacity);
    let senders = (0..n.get())
        .map(|_| WorkQueueSender { tx: tx.clone() })
        .collect();
    drop(tx); // seed sender goes away; only the n cloned senders remain.
    (senders, WorkQueueReceiver { rx })
}
```

`WorkQueueSender` continues to be `Send + !Clone` outside the
`multi-producer` feature, so the only way for callers to obtain
multiple senders is through `with_producers`. That keeps the
producer-count contract observable in the type system: a `Vec` with
length N is the proof.

`bounded()` is implemented as `with_producers_and_capacity(NonZeroUsize::new(1).unwrap(), default_capacity()).pop()` semantically, except it returns the one sender directly to keep the existing API ergonomic.

## Wire-Compat Invariants

Zero impact on the wire protocol. The work queue is internal engine
coordination only; no bytes traverse a network as a result of this
change. The wire-protocol receive path documented in
`multi_producer_audit.rs:8-19` continues to use `bounded()` /
`bounded_with_capacity()` and stays single-producer.

The protocol-level golden tests in `crates/protocol/tests/golden/` are
untouched. The interop harness (`tools/ci/run_interop.sh`) is
unaffected; multi-root transfers are local-copy operations and never
hit the wire.

## Ordering Semantics

The receive path's existing contract is "monotonic sequence numbers
assigned by the single producer; the consumer's `ReorderBuffer` walks
the sequence in order"
(`reorder.rs:1-26`,
`multi_producer_audit.rs:113-138`). Multi-producer changes the source
of the sequence number but not the receiver's contract.

Two viable allocation strategies:

1. **Per-producer ranges (recommended for multi-root).** When the
   producer count and the work bound per producer are known, give each
   producer a disjoint sequence range. For multi-root, the planning
   step that builds the file list per source can also assign a base
   sequence offset:

   ```text
   producer 0 (foo/) -> seq [0, len(foo/))
   producer 1 (bar/) -> seq [len(foo/), len(foo/) + len(bar/))
   producer 2 (baz/) -> seq [len(foo/) + len(bar/), total)
   ```

   No coordination, no atomic operations on the hot path. The
   `ReorderBuffer` capacity must be at least the maximum producer
   range in use simultaneously; today's adaptive policy
   (`adaptive.rs`) already grows under sustained pressure.

2. **Atomic global counter (fallback when ranges aren't predictable).**
   A shared `AtomicU64::fetch_add(1, Ordering::Relaxed)` per send. The
   `multi_producer_requires_atomic_sequence_coordination` test at
   `tests.rs:172-214` already shows this pattern works, at the cost of
   one atomic per item.

For multi-root the per-producer-range approach is the right default:
each root's file count is known after enumeration, and the receiver's
`ReorderBuffer` continues to walk a contiguous sequence.

When per-producer ranges are sparse (e.g., a producer enumerates 1000
items but its allocated range is 10000), the `ReorderBuffer` would
stall waiting for non-existent sequences. Mitigations:

- Allocate ranges *after* enumeration completes, so the range size
  matches the actual emission count exactly. This requires a two-pass
  walker (count, then emit) or a pre-walk index.
- Allow producers to publish a "no more items in my range" sentinel
  that compacts the gap. This is more complex and is deferred until a
  use case demands it.

## Backpressure

The bounded channel still applies. Each producer awaits the bound
independently - `crossbeam_channel::Sender::send` blocks per call until
the channel has room. With N producers all blocked on a full queue,
they will each unblock as the consumers drain items. The crossbeam
documentation guarantees that pending sends are released roughly in
arrival order (a single internal mutex picks one waiter per slot), so
fairness is approximate but bounded - no producer is starved
indefinitely as long as consumers keep draining.

In practice the queue depth is set by `default_capacity()` =
`2 * rayon::current_num_threads()`. For multi-root transfers with N
roots and T rayon threads, the depth might be tuned upward to `2 * T +
N` so each producer always has at least one slot regardless of how
many other producers are blocked. The adaptive policy in
`capacity.rs:40-76` is unchanged but its multipliers may need
re-evaluation under high producer counts; that is a follow-up
benchmark, not a correctness issue.

## Interaction With `drain_parallel`

`drain_parallel` and `drain_parallel_into`
(`drain.rs:57-90,136-156`) iterate the receiver until the channel
disconnects. Today, channel disconnect happens when the lone
`WorkQueueSender` drops. With multi-producer, the channel disconnects
only when *all* cloned senders drop. This is the crossbeam behaviour
that
`multi_producer_receiver_completes_only_when_all_senders_dropped`
already verifies (`tests.rs:651-697`).

The drain functions need no changes. They block on
`WorkQueueIter::next` (`iter.rs:29-35`) until the channel ends, which
is the desired termination condition for both single-producer and
multi-producer cases.

The corresponding RAII test
(`receiver_drop_signals_producer_raii` at `tests.rs:861-900`) ensures
that producers do not hang if the receiver drops first - that property
is preserved automatically by crossbeam in the multi-producer case
because all cloned senders share one underlying disconnect flag.

## Migration Plan

Three call sites benefit from multi-producer in the near term:

### 1. Multi-root local-copy transfers

Site: `crates/engine/src/local_copy/executor/sources/orchestration.rs:79`.
Today the `for source in plan.sources()` loop is sequential. Migration
runs each iteration in its own rayon task, with each task owning one
of the N senders returned from `with_producers(plan.sources().len())`.
The `process_single_source` function consumes the sender for the
duration of its walk. Sequence-range allocation: producer i gets the
range `[base_i, base_i + count_i)`, where `count_i` is determined by a
quick pre-walk or by the planner if file counts are already known
during file-list construction.

### 2. Parallel `--files-from` reader plus main walker

When `--files-from` is in effect, today's reader synchronously parses
the input file before the main walker starts. The reader and the
walker can run concurrently, each as one of two producers, using
`with_producers(NonZeroUsize::new(2).unwrap())`. The reader handles
explicitly listed paths; the walker handles any directories the reader
referenced. Sequence ranges: reserve a range for the reader sized to
its input length; the walker takes the rest.

### 3. Future: per-source async readers

Once the async pipeline (#1591, see
`docs/design/async-channel-abstraction.md`) lands, each source root
could be backed by an async reader feeding into the shared queue via a
sync-to-async bridge channel. This is a longer-term integration; the
multi-producer queue becomes the join point at the boundary between
async producers and the synchronous rayon consumer pool.

The wire-protocol receive path in `transfer/src/delta_pipeline.rs:185`
(documented in `multi_producer_audit.rs:8-19`) does **not** migrate.
It stays single-producer. The audit document is updated to reference
this design note.

## Risks

### Producer-count mismatch causes deadlock

If callers create `with_producers(n)` but only drop `n - 1` senders,
the channel never disconnects, and `drain_parallel` waits forever.
Mitigation:

- The `Vec<WorkQueueSender>` returned by `with_producers` makes the
  count visible. Callers iterate the vec to spawn threads, so missing
  a sender means missing a thread, which is easy to spot.
- A debug-only assertion can record sender drops via a shared
  `Arc<AtomicUsize>` and emit a warning if the counter does not reach
  zero within a transfer-end deadline. This is observability only; the
  primary defence is the API shape.

### Sequence-number collision across producers

Two producers issuing the same sequence number corrupts the
`ReorderBuffer`'s assumption of unique slots. The buffer's ring layout
(`reorder.rs:64-80`) silently overwrites if two items map to the same
slot. Mitigation:

- Per-producer ranges (the recommended allocation strategy) make
  collisions impossible by construction.
- For the atomic-counter fallback, the test
  `multi_producer_requires_atomic_sequence_coordination`
  (`tests.rs:172-214`) shows the safe pattern: `fetch_add(1)` per
  item.
- A debug assertion in `ReorderBuffer::insert` could detect duplicate
  slot writes (`Some` overwriting `Some`) and panic; this is cheap and
  catches integration bugs early.

### Fairness stalls if one producer is much faster

With one fast producer and one slow producer feeding a small bounded
queue, the slow producer may rarely get a slot. This does not cause
data loss or incorrectness but can starve some sources of progress.
Mitigation:

- Tune capacity per producer count (see the Backpressure section).
- For severely skewed workloads, partition the transfer so each source
  gets its own bounded sub-queue and a final merge step on the
  consumer side. This is a future optimisation, not a v1 requirement.

### Adaptive capacity policy under multi-producer

The adaptive policy in `concurrent_delta/adaptive.rs` keys on the gap
between the consumer's `next_expected` and the highest occupied slot
in the `ReorderBuffer`. With multi-producer + per-range sequencing,
that high-water gap can spike when producers complete their ranges
out of order (e.g., producer 2 finishes before producer 0). The buffer
will hold producer 2's full range while waiting for producer 0's first
item, which inflates the gap signal. Mitigation:

- Set the buffer's adaptive minimum to at least the largest expected
  per-producer range, so the gap-driven shrink does not thrash.
- Audit the adaptive policy under representative multi-root workloads
  before enabling by default.

## Tracking (follow-up TODOs, not added to the persistent list)

- Implementation: introduce `with_producers` and
  `with_producers_and_capacity` in
  `crates/engine/src/concurrent_delta/work_queue/bounded.rs`, mirroring
  the existing constructors.
- Multi-root integration test: extend
  `crates/engine/src/local_copy/tests/` with a 4-root tree exercising
  the parallel walker path and asserting the consumer drains in order
  via the `ReorderBuffer`.
- Sequence-range allocation: add a planner step that computes
  `(base, len)` per source root and threads it into the producer
  closure. Today the planner builds `plan.sources()` without per-root
  sequence metadata; the change is local to
  `crates/engine/src/local_copy/executor/sources/`.
- Benchmark on a 4-root tree: extend `scripts/benchmark.sh` to run
  `oc-rsync r1/ r2/ r3/ r4/ dst/` against the upstream binary and
  capture wall time vs serial enumeration.
- Audit update: amend
  `crates/engine/src/concurrent_delta/multi_producer_audit.rs` to
  reference this design note in the "Multi-Producer Opportunities"
  section, replacing the current "Not beneficial" verdicts for the
  local-copy enumeration case.

## Appendix: Why Not Just Use `Clone` Today?

The `multi-producer` feature flag already exists and is exercised by
tests. The reason this design note advocates an explicit
`with_producers(n)` constructor instead of telling callers to enable
the feature flag and clone is twofold:

1. **The producer-count contract is the actual subtle bit.** Callers
   who clone freely can easily leave a sender alive longer than
   intended (e.g., the original sender in the planner thread that
   spawned workers). Returning a `Vec<WorkQueueSender>` of length N
   forces the caller to think about the count and own all N handles
   explicitly.

2. **Crate-wide invariants stay enforceable.** The wire-protocol
   receive path and any future call site that needs strict SPMC keep
   compiling against a non-`Clone` sender. Only callers that opt into
   `with_producers` get the multi-producer contract, and they get it
   without needing the global feature flag.

The feature flag and its `Clone` impl remain useful for the
fan-out-from-one-coordinator pattern and for tests; the new
constructor is the recommended path for multi-root and parallel
enumeration.
