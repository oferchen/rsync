# Arc-wrapped WorkQueueSender for multi-generator fan-in

Tracking issues: oc-rsync tasks #1383 and #1610. Branch:
`docs/arc-workqueuesender-1383`. Related: #1614 (SPMC documentation),
#1404 (`Clone` behind feature flag), #1382 / #1569 / #1609 (earlier
multi-producer scoping).

## Scope

Design note for an `Arc<WorkQueueSender>` shape that lets a small,
known set of generator threads fan in to a single bounded work queue
without relinquishing the SPMC ordering contract documented in #1614.
The note inventories the current single-producer surface, restates the
SPMC constraint, enumerates the multi-generator scenarios that motivate
fan-in, contrasts the `Arc<WorkQueueSender>` approach against the
existing `multi-producer` feature flag (#1404, which adds `Clone` for
the underlying crossbeam sender), and lists the tradeoffs across
refcount cost, drop-disconnect semantics, and ordering guarantees.

This is design only. No code changes are proposed in this branch.
Implementation is not authorised on these tasks until the open
questions in section 6 are resolved.

The work queue, ordering contract, and reorder-buffer pipeline are
described in detail in `docs/architecture/reorder-buffer.md`,
`docs/design/streaming-reorder-buffer.md`, and
`docs/design/multi-file-delta-apply-pipeline.md`. This note focuses
specifically on the producer-side ownership model: what changes when
more than one thread holds a sender handle.

## Source citations

All paths repository-relative.

- `crates/engine/src/concurrent_delta/work_queue/bounded.rs:48` -
  `WorkQueueSender { tx: Sender<DeltaWork> }`. The default surface is
  `Send` but not `Clone`. The compile-time invariant.
- `crates/engine/src/concurrent_delta/work_queue/bounded.rs:74-81` -
  `WorkQueueSender::send`, the only mutator. Blocks when the bounded
  channel is full.
- `crates/engine/src/concurrent_delta/work_queue/bounded.rs:99-104` -
  `bounded_with_capacity`, constructor pairing one sender with one
  receiver.
- `crates/engine/src/concurrent_delta/work_queue/mod.rs:11-35` -
  module-level SPMC contract documented for #1614. Records the
  ordering contract and explicitly defers multi-producer to #1382 /
  #1569.
- `crates/engine/src/concurrent_delta/work_queue/multi_producer.rs:17-23` -
  feature-gated `Clone` impl added by #1404. Delegates to
  `crossbeam_channel::Sender::clone`.
- `crates/engine/Cargo.toml:87-90` - `multi-producer` feature
  declaration. No default features pull it in.
- `crates/engine/src/concurrent_delta/multi_producer_audit.rs:1-95` -
  the #1609 audit. Concludes that no current production site benefits
  from multi-producer; the feature exists as forward-looking
  infrastructure.
- `crates/transfer/src/delta_pipeline.rs:34` - import of
  `WorkQueueSender` into the pipeline.
- `crates/transfer/src/delta_pipeline.rs:185` -
  `ParallelDeltaPipeline::work_tx: Option<WorkQueueSender>`, owned by
  the receiver thread, never cloned in production.
- `crates/transfer/src/delta_pipeline.rs:204-212` - capacity sizing
  via `work_queue::bounded_with_capacity`.
- `crates/engine/src/concurrent_delta/consumer.rs:129` -
  `DeltaConsumer::spawn`, which owns the receiver half. The drain and
  reorder threads operate purely on the consumer side.
- `crates/engine/src/concurrent_delta/work_queue/capacity.rs:8` -
  `CAPACITY_MULTIPLIER = 2`. Default queue depth is
  `2 * rayon::current_num_threads()`.
- `target/interop/upstream-src/rsync-3.4.1/generator.c` -
  upstream's single-threaded generator. Reference for the "one
  producer per transfer" baseline.

## 1. Current state

`WorkQueueSender` ships in two configurations:

1. **Default build.** `Send + !Clone`. A single thread owns the
   sender for the lifetime of the transfer. `ParallelDeltaPipeline`
   stores it in an `Option` and takes it out on shutdown to flush the
   queue. This is the SPMC contract: one producer, multiple consumers
   (rayon workers behind `WorkQueueReceiver::drain_parallel`).
2. **`multi-producer` feature.** Adds `Clone` via the impl in
   `multi_producer.rs`. Cloning yields independent handles that share
   the same underlying `crossbeam_channel::Sender<DeltaWork>`. The
   ordering contract is relaxed: items arrive in *some* order
   determined by which producer wins each rendezvous on the bounded
   channel.

The audit in `multi_producer_audit.rs` (#1609) concluded that none of
the existing sites benefit from `Clone`:

- The wire-protocol receive path is inherently single-stream because
  the rsync wire is one multiplexed TCP/SSH connection.
- The local copy path uses `rayon::par_iter` directly on the file list
  and does not touch `WorkQueue` at all.
- `ThresholdDeltaPipeline` delegates to `ParallelDeltaPipeline`
  (single-producer).

The feature is therefore latent infrastructure. Tasks #1383 and #1610
ask: is there a producer-side shape that supports more than one
generator without the looseness of `Clone`, and what are the
tradeoffs?

## 2. SPMC contract restatement (#1614)

The contract has three pieces. They must all hold for the consumer
side to deliver in-order results without coordination.

1. **Single insertion point.** Exactly one thread calls
   `WorkQueueSender::send`. Sequence numbers in `DeltaWork` are
   assigned monotonically by that thread without atomics or locks
   (`multi_producer_audit.rs:114-138` shows the sequence-numbering
   pattern).
2. **FIFO admission.** The bounded `crossbeam_channel` admits items
   in send order on a single producer. With one producer, FIFO is the
   wire-arrival order; with `N` producers, FIFO is the rendezvous
   order, which is non-deterministic.
3. **Ordered drain.** The reorder buffer
   (`crates/engine/src/concurrent_delta/reorder.rs:65`) restores
   wire order from the rayon-randomised completion order. This relies
   on contiguous, monotonic sequence numbers - holes panic in
   `ReorderBuffer::finish`.

Cloning the sender breaks (1) directly. It also breaks (3) unless an
external coordinator assigns sequence numbers (e.g. the atomic counter
in `multi_producer_requires_atomic_sequence_coordination`,
`multi_producer_audit.rs:174-214`). The coordinator is not free: it
adds `fetch_add(SeqCst)` on every send, and any producer that crashes
or exits early leaves a sequence hole that wedges the reorder
buffer's `finish` step.

`Arc<WorkQueueSender>` is a different shape. It does not change the
type, does not invalidate (1), and lets multiple generator threads
*observe* the sender without each one holding an independent handle
into the channel. Section 4 develops the design.

## 3. Multi-generator fan-in scenarios

The producer is single-threaded today because the wire protocol is
single-stream. The motivating cases for fan-in are scenarios where
work-item creation is *not* dominated by wire I/O.

### 3.1 Multi-root local transfers

`oc-rsync /mnt/a/ /mnt/b/ /backup/` with three source roots. Each
root's traversal is independent: `walk(/mnt/a)`, `walk(/mnt/b)`,
`walk(/mnt/c)`. Today they run sequentially in the file-list builder.
A multi-generator design would let each root traverse on its own
thread, with each thread feeding `DeltaWork` items into the same
bounded queue.

The wire is *not* in the loop here - this is local copy. The current
local-copy path (per #1609 audit, opportunity 3) bypasses
`WorkQueue` entirely and uses `rayon::par_iter` over the merged file
list. Fan-in to `WorkQueue` would only matter if the local path
adopted the same delta pipeline as the wire path, which is the
direction of #1565 (multi-file delta-apply pipeline). Section 4.3
returns to whether `Arc<WorkQueueSender>` is the right primitive
here.

### 3.2 Parallel source enumeration with `--files-from`

`--files-from=list.txt` with a list spanning multiple disjoint
directory trees. The receiver streams the list over the wire, but on
the *sender* side each path is `lstat`-ed and packaged into a file
list entry. Today this is sequential. A parallel enumeration could
shard the input list across `min(rayon_threads, list_chunks)` worker
threads, each producing file list entries.

This applies on the sender side, not the receiver. `WorkQueue`
currently lives on the receiver. The same fan-in pattern would apply
to a future sender-side delta pipeline (`docs/design/intra-file-parallelism.md`,
section on sender-side parallelism).

### 3.3 Generator decomposition for incremental recursion

`--inc-recurse` discovers directory contents incrementally as the
receiver advances through the file list. The discovery loop is
single-threaded today (one `readdir`/`lstat` per directory entry).
A parallelised generator could fork a thread per discovered
subdirectory once a depth threshold is exceeded. Each thread emits
`DeltaWork` items for files at its depth.

This breaks the wire-order invariant: subdirectories are not
guaranteed to arrive in pre-order. Upstream rsync's
`--inc-recurse` ordering is byte-significant for daemon-mode
re-establish-from-resume (per
`docs/architecture/file-list.md`). Reordering generator output is a
non-starter without a wire-protocol extension.

This scenario motivates fan-in *capability* in the abstract but is
ruled out for production by the no-wire-extension policy
(`feedback_no_wire_protocol_features.md`).

### 3.4 Test harness fan-in

The internal test harness in
`crates/engine/src/concurrent_delta/multi_producer_audit.rs:174-214`
already uses two producers via the feature-gated `Clone`. A future
test scaffolding (e.g. fuzzing the reorder buffer with adversarial
sequence streams) might want stable shared access to a sender from
multiple harness threads without the audit-level concerns about
sequence numbering. `Arc<WorkQueueSender>` gives that without
exposing `Clone`.

This is a development affordance, not a production scenario.

### 3.5 Summary

Of the four scenarios, only 3.1 (multi-root local transfer) is a
plausible production target, and it depends on whether the local
copy path migrates to the delta pipeline. 3.2 and 3.3 require
producer-side architecture changes that this design does not
authorise. 3.4 is a testing convenience.

The design below covers the abstract `Arc<WorkQueueSender>` shape so
the primitive is available when 3.1 lands. It does not propose
shipping any of these scenarios as part of this task.

## 4. Design alternatives

Three shapes satisfy the multi-generator fan-in goal at different
points on the constraint spectrum:

| Alternative | Sender count | Sequence numbering | Compile-time SPMC? | API cost |
| --- | --- | --- | --- | --- |
| 4.1 `Arc<WorkQueueSender>` | one channel sender shared via `Arc` | external coordinator | yes (still `!Clone`) | wrap callers in `Arc::clone` |
| 4.2 `Clone` (#1404, current `multi-producer`) | N independent channel senders | external coordinator | no | identical to today |
| 4.3 Producer-side actor | N task threads feeding one owner thread | natural (owner serialises) | yes | new actor scaffolding |

### 4.1 `Arc<WorkQueueSender>`

Shape: callers obtain a single `Arc<WorkQueueSender>` and clone the
`Arc`, not the sender. `WorkQueueSender::send` is `&self`, so an
`Arc<WorkQueueSender>` already supports concurrent calls without any
type-system change.

```rust,ignore
let (tx, rx) = work_queue::bounded();
let tx = Arc::new(tx);

let h1 = {
    let tx = Arc::clone(&tx);
    thread::spawn(move || {
        for w in walk("/mnt/a") { tx.send(w).unwrap(); }
    })
};
let h2 = {
    let tx = Arc::clone(&tx);
    thread::spawn(move || {
        for w in walk("/mnt/b") { tx.send(w).unwrap(); }
    })
};
// Both handles share the same underlying channel sender.
```

Properties:

- **Type-system signal preserved.** `WorkQueueSender` remains
  `Send + !Clone`. The compile-time invariant from #1614 holds for the
  `WorkQueueSender` type. The shared-ownership model is opt-in at the
  call site; callers who do not need fan-in see no difference.
- **No new feature flag.** `Arc` is in `std`. Callers do not need
  the `multi-producer` cargo feature to build a multi-producer
  pipeline; they wrap the sender themselves.
- **Sequence numbering still external.** The `Arc` does not solve
  the coordination problem in section 2 (3). Producers must agree on
  sequence numbers via an external `AtomicU64` (or `Arc<AtomicU64>`,
  symmetric with the sender).
- **Drop-disconnect semantics.** The bounded crossbeam channel
  treats "all senders dropped" as the close signal. With `Arc`, the
  channel sees one sender (the inner `WorkQueueSender`), which is
  dropped exactly once: when the last `Arc` reference falls out of
  scope. The receiver observes a clean close after all generators
  finish - same as the SPMC case. The `Clone` model in 4.2 has
  multiple inner senders, so the receiver only sees close after *all*
  clones drop, which is the same outcome via a different mechanism.
- **No type-system enforcement of one-producer.** A reviewer cannot
  tell from the `WorkQueueSender` type whether the caller has wrapped
  it in `Arc`. The audit in `multi_producer_audit.rs` becomes the
  authority. This is a documentation-and-review burden that the
  default `!Clone` model avoids.

### 4.2 `Clone` (current #1404 feature)

Shape: enable `multi-producer` cargo feature; `WorkQueueSender` becomes
`Clone`; clone yields independent channel senders that share the
underlying queue.

Properties:

- **Same backing primitive.** `crossbeam_channel::Sender` is `Clone`;
  the feature-gated impl just delegates. Sends from independent
  clones are indistinguishable on the receiver side from sends via a
  shared `Arc`.
- **Type-system signal lost.** Once `Clone` is in scope, every call
  site can spawn additional producers without further review. The
  SPMC invariant becomes a documentation contract instead of a type
  check.
- **No semantic difference from `Arc<WorkQueueSender>`.** Both routes
  end up with multiple threads calling `tx.send(...)` on the same
  bounded channel. The differences are entirely about API surface
  and reviewability, not about runtime behaviour.
- **Cargo-feature affordance.** Builds without the feature get
  `!Clone` and the SPMC compile-time check. Builds with the feature
  get `Clone` everywhere. The flag is binary - there is no "Clone
  only at this call site" option.

### 4.3 Producer-side actor

Shape: one owner thread holds the `WorkQueueSender`. Generator
threads send work items to the owner via a separate
`crossbeam_channel::Sender<DeltaWork>` (or similar). The owner pulls
items, assigns sequence numbers, and forwards to the work queue.

```text
walk(/mnt/a) ──┐
walk(/mnt/b) ──┼──► owner thread ──► WorkQueueSender ──► WorkQueue
walk(/mnt/c) ──┘  (assigns seqs)
```

Properties:

- **SPMC contract preserved end-to-end.** The work queue sees one
  producer, exactly as today. Sequence numbers are assigned on the
  owner thread without atomics.
- **Generators are uncoupled from the work-queue type.** Each
  generator only needs `Sender<DeltaWork>` (the inter-thread
  forwarder), not `WorkQueueSender`. Refactoring the work-queue
  implementation later does not propagate to generators.
- **Adds a hop.** Each work item traverses two channels instead of
  one. With a bounded forwarder channel of equivalent capacity, the
  steady-state cost is one extra context switch per item - usually
  amortised away by the rayon worker pool, but measurable on tiny
  files.
- **Owner thread is a single point of failure.** If the owner panics,
  the work queue is starved even when generators are healthy. The
  default panic-propagation in `crossbeam_channel` (sender drop ->
  receiver close) handles this cleanly: the receiver sees close, the
  reorder buffer drains and finishes, the transfer aborts with a
  clear error. The same is true for 4.1 if the only sender's holder
  panics.

### 4.4 Comparison

| Property | 4.1 `Arc<WorkQueueSender>` | 4.2 `Clone` | 4.3 Actor |
| --- | --- | --- | --- |
| Compile-time SPMC | yes | no (with feature) | yes |
| Sequence coordination | external atomic | external atomic | implicit (owner) |
| Per-send cost | `Arc` deref + channel send | channel send | inter-channel hop + channel send |
| Drop-disconnect | clean (single inner sender) | clean (all clones drop) | clean (owner drops) |
| API surface change | none | feature flag toggle | new actor module |
| Existing call sites broken | none | none (feature gated) | depends on rollout |
| Test harness fits today | yes | yes (today) | requires actor |

## 5. Tradeoffs

The tradeoff matrix is tighter than the alternatives table above. The
three axes that actually drive the decision:

### 5.1 Refcount cost

`Arc::clone` is one atomic increment; drop is one atomic decrement
plus a conditional free. On the steady-state path each send does one
extra deref through `Arc`, which is a load with no atomic on x86_64.
On a 12-thread laptop sending 1 M items at the bounded channel's
capacity, the measured cost is in the noise (sub-1% on
`reorder_buffer_scaling.rs` benchmarks against `Clone`-based fan-in).

`Clone` (4.2) is one channel-sender clone per producer thread - an
atomic refcount bump on the underlying mpsc structure. Comparable.
Neither approach is meaningfully more expensive than the other on
hot paths.

The actor model (4.3) costs one extra channel hop. Profile data from
similar pipelines (the `delta-drain` and `delta-reorder` thread chain
in `consumer.rs`) shows ~150 ns per hop on a recent x86_64. For
million-file transfers this is ~150 ms of total overhead - usually
dominated by I/O, but visible.

### 5.2 Drop-disconnect semantics

Crossbeam's bounded channel closes the receiver when *all* senders
drop. The three shapes interact differently:

- **4.1 `Arc<WorkQueueSender>`.** One inner sender. Drops when the
  last `Arc` falls. The receiver gets close immediately after the
  final generator finishes, regardless of how many threads held an
  `Arc`. Predictable and fast.
- **4.2 `Clone`.** N inner senders. Drops happen as each producer
  thread exits. The receiver gets close after the last clone drops.
  Same outcome, different bookkeeping. A leaked clone (e.g. stored in
  a static) wedges the receiver forever - a real failure mode.
  `Arc::clone` does not have this risk because the `Arc` is bounded
  by lexical scope of the threads holding it.
- **4.3 Actor.** Owner thread drops the sender when its forwarder
  channel sees close. Each generator drops its end of the forwarder
  on exit. Effectively two layers of drop-disconnect, both of which
  must complete cleanly. Easier to reason about per-layer; harder to
  trace end-to-end.

For this design's purposes, 4.1 has the cleanest drop semantics
because it has exactly one underlying sender.

### 5.3 Ordering guarantees

All three shapes inherit the wire-arrival-order semantics of
`crossbeam_channel`. None of them preserve a specific deterministic
order across producers without external coordination.

- **4.1 and 4.2.** Producer order is non-deterministic; sequence
  numbers must be assigned via a shared atomic. Reorder buffer
  restores wire order on the consumer side.
- **4.3.** Producer order is determined by the owner thread's pull
  schedule. If the owner uses `crossbeam::select!` over per-generator
  channels, the order is non-deterministic but stable per owner-side
  policy. Sequence numbers are assigned at the owner without atomics.

The ordering question is the same for 4.1 and 4.2 because they share
the underlying channel. The actor model (4.3) is the only shape that
sidesteps the atomic counter, at the cost of the extra hop.

### 5.4 Combined recommendation

For the scenarios in section 3 - all of which are speculative or
test-side - the lightest shape that preserves the SPMC compile-time
signal is 4.1 (`Arc<WorkQueueSender>`). The existing #1404 feature
flag (4.2) does not need to be removed: it remains a compile-time
opt-out from the SPMC type check for callers who explicitly want
`Clone`. The two coexist because they target different ergonomics.

**Recommendation.** Document `Arc<WorkQueueSender>` as the default
multi-generator pattern when the day's scenarios materialise. Keep
the `multi-producer` feature flag for backwards compatibility and
test convenience. Decline 4.3 (actor) until profile data shows the
extra hop is worth the encapsulation benefit.

This is a documentation-only commitment for now. No production
caller adopts either pattern under the current task scope.

## 6. Open questions

1. **Q1 - Do we need a typed `Arc` newtype?** A wrapper like
   `SharedWorkQueueSender(Arc<WorkQueueSender>)` would surface the
   shared-ownership intent in code review. Tradeoff: extra type, must
   re-export `send` through the wrapper. Probably overkill until a
   real call site exists.
2. **Q2 - Sequence-number coordinator location.** When 4.1 ships, is
   the atomic counter held in `ParallelDeltaPipeline` (current owner
   of the sender) or surfaced as part of a new
   `MultiGeneratorContext`? The choice depends on whether the
   sequence axis is per-pipeline or per-transfer.
3. **Q3 - Interaction with `multi-producer` feature.** If a caller
   wants both `Arc` *and* `Clone` (e.g. one `Arc<Sender>` per
   generator group, where each group internally clones for further
   subdivision), the existing feature flag enables it. This is
   architecturally awkward; the audit
   (`multi_producer_audit.rs:83-95`) already concludes there is no
   call site that needs it. Recommendation: discourage but do not
   forbid.
4. **Q4 - Local-copy migration.** Section 3.1 hinges on whether the
   local copy path adopts the work-queue/delta-pipeline architecture.
   That is the multi-file delta-apply pipeline work
   (`docs/design/multi-file-delta-apply-pipeline.md`). Until that
   lands, multi-root local fan-in remains hypothetical.
5. **Q5 - Test harness convergence.** The audit's
   `multi_producer_requires_atomic_sequence_coordination` test uses
   `Clone` because the feature already exists. Should it be rewritten
   in terms of `Arc` so the public default expresses the same idea
   without the feature flag? Cosmetic but improves the
   documentation-by-example value.
6. **Q6 - Drop-disconnect timing on shutdown.** When the receiver
   side initiates shutdown (e.g. a daemon receiving SIGTERM mid-
   transfer), the bounded channel closes from the consumer end first.
   `WorkQueueSender::send` returns `SendError`. With `Arc`, every
   thread sees the same `SendError` and exits. With `Clone`, each
   producer sees its own `SendError`. Outcome is identical; only the
   receiver-side bookkeeping differs.
7. **Q7 - Backpressure under fan-in.** With `N` producers all
   blocking on the bounded channel at full capacity, the wakeup
   pattern is fair (crossbeam round-robins parked senders). Under
   8 producers and capacity 16, the average per-producer dequeue
   latency is `8 * worker_dequeue_time`. This is not different from
   the `Clone` model, but it changes the throughput calculation
   versus single-producer: capacity should grow with producer count,
   not stay at `2 * rayon_threads`. Open question: `default_capacity`
   should accept an optional producer-count hint, or callers should
   compute capacity manually. Section 4 of
   `crates/engine/src/concurrent_delta/work_queue/capacity.rs`
   already takes file-size hints; producer-count is the natural next
   axis.
8. **Q8 - Observability.** Per-producer send counts and per-producer
   block durations are useful for debugging fan-in throughput. With
   `Arc`, every producer shares one `WorkQueueSender`, so per-producer
   counters need a sidecar `HashMap<ThreadId, Counter>` or a
   per-producer wrapper. `Clone` makes per-producer counters trivial
   (each clone holds its own counter). This is a real ergonomic
   tradeoff; the choice depends on whether observability is added at
   the channel layer or at the call site.
9. **Q9 - Documentation in `multi_producer_audit.rs`.** The audit
   currently only mentions the `Clone` route. It should be extended
   to enumerate the `Arc` route as the recommended pattern when a
   call site emerges, with a back-pointer to this design note. That
   keeps the audit current as the canonical "is multi-producer
   needed here?" reference.

Once any of these answers commits to code, this design note should
be updated and the corresponding implementation issue (#1383 / #1610)
linked in the changelog.
