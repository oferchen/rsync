# Multi-producer WorkQueue for parallel generator fan-in (#1405)

Tracking: oc-rsync task #1405. This note is the production design discussion
for letting multiple generator threads fan in to a single bounded
`WorkQueue`. It builds on the audit in #4173 and the SP-vs-MP bench in
#4209, narrows the design space to the variants tracked at #1610 and
#1569, and recommends a final disposition for the `multi-producer` cargo
feature.

Companion documents:

- `docs/audits/workqueue-sender-multi-producer-audit.md` (#4173) - the
  call-site inventory whose top-line conclusion this design accepts:
  every live producer is single-producer today.
- `docs/design/arc-workqueuesender.md` (#1610) - the Arc-wrapped sender
  design space.
- `docs/design/arc-workqueue-sender-eval.md` - the focused evaluation
  that rejects Arc-wrapping as a separate primitive.
- `docs/audits/multi-root-transfer-scenarios.md` and
  `docs/design/parallel-source-enumeration.md` - the multi-root and
  enumeration scoping work (#1382, #1573).
- `docs/design/lockfree-mpsc-drain-design.md` - the consumer-side
  fan-in design that the #4214 bench informs.

This is design only. No source files are modified; no cargo features
are flipped.

## 1. Current SP / MP shape

`WorkQueueSender` is the single producer-side handle to the bounded
concurrent-delta queue. Two configurations exist today:

1. **Default build (single-producer).** The sender is `Send + !Clone`:

   - `crates/engine/src/concurrent_delta/work_queue/bounded.rs:48-50`
     defines the newtype `pub struct WorkQueueSender { tx: Sender<DeltaWork> }`.
   - `crates/engine/src/concurrent_delta/work_queue/bounded.rs:74-81`
     declares `WorkQueueSender::send`, the only mutator.
   - The absence of a `Clone` impl in `bounded.rs` is the compile-time
     SPMC invariant. The module-level contract is documented at
     `crates/engine/src/concurrent_delta/work_queue/mod.rs:11-35`.

2. **`multi-producer` feature (gated).** Cloning is added behind a
   cargo feature:

   - `crates/engine/Cargo.toml:88-91` declares
     `multi-producer = []` with no default-feature pull-in.
   - `crates/engine/src/concurrent_delta/work_queue/multi_producer.rs:10-23`
     supplies the gated `impl Clone for WorkQueueSender`, delegating to
     `crossbeam_channel::Sender::clone` (an `Arc` refcount bump on the
     underlying channel).

The #4173 audit
(`docs/audits/workqueue-sender-multi-producer-audit.md:29-65`) tallies
three live production producer sites, all on the receiver wire path
(`crates/transfer/src/delta_pipeline.rs:185`, `:296-308`, `:337-340`,
`:452-465`, `:318-322`), and verifies that every one is correctly
single-producer. The cargo feature is therefore latent infrastructure
plus the integration-test coverage at
`crates/engine/tests/multi_producer_work_queue.rs:51-501`.

## 2. Parallel-generator use case

The "parallel generator fan-in" question this task asks is: under what
circumstances would multiple generator threads on the sender side
actually want to feed a shared `WorkQueue`, and does the answer
motivate promoting the feature to default-on?

### Upstream precedent

Upstream rsync's generator is single-threaded. The driver function
`generate_files` at
`target/interop/upstream-src/rsync-3.4.1/generator.c:2226` runs in one
process. The `recv_generator` per-file routine handles stat, delta
request, hardlink resolution, and metadata fix-up serially. The
upstream model is one generator process, one sender process, one
receiver process, communicating through pipes or sockets. There is no
upstream code path with multiple generator threads writing into a
shared work queue.

This is the load-bearing point. Anything we build with multiple
generators feeding one queue is an oc-rsync extension, not upstream
parity. The project convention is to avoid adding non-upstream
patterns without a strong, measured justification.

### Scenarios where parallel generators *plausibly* want fan-in

The audit at `docs/audits/workqueue-sender-multi-producer-audit.md:245-275`
already rules out the obvious cases for the receiver-side queue. For
the *sender-side* generator-to-shared-queue question, the candidate
scenarios are:

1. **Multi-root push** (`oc-rsync /a/ /b/ /c/ host:dst/`). Each source
   root could be enumerated and have deltas requested in parallel by
   one generator thread per root, fanning into a single shared queue
   that feeds the wire writer. The wire writer remains single-threaded
   because the protocol multiplexes one stream. This scenario is the
   subject of the multi-root design at
   `docs/audits/multi-root-transfer-scenarios.md` (#1382) and is
   distinct from the *receiver* path the #4173 audit covers.

2. **Parallel `--files-from` reader plus walker.** When
   `--files-from` is in effect, a reader thread parses the input list
   while a walker thread visits any directories the input references.
   Two independent producers, one shared downstream queue.

3. **Per-source async generators.** Once an async runtime sits behind
   each source root (the longer-term direction in
   `docs/design/async-channel-abstraction.md`), each per-source future
   becomes one producer. The number of producers is bounded by the
   source-root count, known at construction time.

In every other scenario - incremental recursion segments, the
receiver wire path, local-to-local copy - parallel generation has
been ruled out either by the wire-protocol single-stream invariant
(`multi_producer_audit.rs:36-95`) or because the code path does not
use `WorkQueue` at all (the local copy executor uses `rayon::par_iter`
directly: see `crates/engine/src/local_copy/executor/file/`).

### Why this matters for #1405

The candidate scenarios above are all *sender-side* fan-in. The audit
in #4173 evaluated the *receiver-side* queue and found zero current
producers. For the sender-side queue we have:

- The producer count is always small and known at construction time
  (`plan.sources().len()` or "reader + walker" = 2). It is never an
  unbounded dynamic set.
- The downstream consumer (wire writer or local-copy applier) is
  single-threaded. Fan-in collapses immediately back to a single
  consumer.
- The upstream baseline is one generator thread. We are paying for an
  extension that has no parity benefit.

The decision then turns on whether the fan-in throughput gain
*measured by #4209* exceeds the cost of relaxing the SP-only compile
time invariant, plus the cost of the per-call-site sequence
coordination MP requires.

## 3. MP designs

Three production-shapes have been seriously considered. They differ in
how producers acquire a sender and how the channel close is signalled.

### Design A: Cloneable sender on the feature flag (#1569, what we have)

Each producer holds its own `WorkQueueSender`. The sender implements
`Clone` only when the `multi-producer` cargo feature is on. The
underlying `crossbeam_channel::Sender` is already an `Arc`; cloning
the wrapper bumps that refcount. Channel close happens when the last
clone drops, by crossbeam's natural disconnect semantics.

This is the existing implementation at
`crates/engine/src/concurrent_delta/work_queue/multi_producer.rs:17-23`.
Tests at `crates/engine/tests/multi_producer_work_queue.rs:51-501`
exercise it at 4-16 producers.

**Strengths.**

- Zero overhead vs single-producer: `WorkQueueSender::send` is one
  crossbeam send call regardless of how many clones exist.
- Crossbeam's MPMC channel handles close-on-last-drop correctly; no
  additional synchronisation primitive is needed.
- The compile-time invariant survives in the default build because the
  `Clone` impl is feature-gated.

**Issues.**

- Producer count is implicit. Two cloned senders that both stay alive
  past the expected handoff hang `drain_parallel` forever.
- The audit at `docs/audits/workqueue-sender-multi-producer-audit.md:285-307`
  warns that "two ways to do the same thing" (Clone via feature flag
  vs Arc always available) makes the audit harder to keep
  authoritative if both ship.

### Design B: Arc-wrapped sender (#1610)

A shared `Arc<WorkQueueSender>` is passed to each producer thread. The
channel closes when the last `Arc` drops. Producer count can be
dynamic; producers join by cloning the `Arc` at runtime.

The full design space lives at `docs/design/arc-workqueuesender.md`
(#1610). The focused evaluation at
`docs/design/arc-workqueue-sender-eval.md` rejects this primitive on
four grounds:

1. **Redundant indirection.** `crossbeam_channel::Sender` is already
   an internal `Arc`. Wrapping the public sender in another `Arc`
   doubles the refcount layer.
2. **No type-system signalling.** `Arc<WorkQueueSender>` does not
   surface "this is shared" in the signature. A reviewer cannot tell
   whether SPMC is intended.
3. **No new capability.** Any future call site that needs shared
   ownership without `Clone` can wrap the sender in `Arc` at the call
   site (`Arc::new(tx)`) without library support.
4. **Two ways to do the same thing.** Same audit-discipline issue as
   Design A's "ships alongside Clone" failure mode, but with the
   primitive permanently in the default build instead of behind a
   feature flag.

### Design C: Explicit producer-vector constructor (the #1382 proposal)

Replace the bare `Clone` impl with a constructor that returns
exactly N senders and a single receiver:

```rust
pub fn with_producers(n: NonZeroUsize)
    -> (Vec<WorkQueueSender>, WorkQueueReceiver);
```

The senders themselves remain `!Clone`. The producer count is
visible in the type (a `Vec` of length N). The multi-root scenarios
audit at `docs/audits/multi-root-transfer-scenarios.md:584-625`
considers this shape and rejects it for the multi-root case on the
grounds that the wire path cannot benefit (see audit section 6).

For the #1405 question, Design C is interesting because it surfaces
the producer-count contract that Design A hides. But it requires a
new library API, and the lookalike `with_producers` is itself another
"way to do the same thing" sitting next to the existing feature-gated
`Clone`.

### Disposition

The three designs are mutually exclusive only at the API surface. The
underlying channel mechanism is identical in all three (cloning the
crossbeam sender). The choice is therefore about how the public API
expresses the producer-count contract and where the compile-time
invariant lives.

## 4. Ordering guarantees

The receiver-side ordering contract is "monotonic sequence numbers
assigned by the single producer; the consumer's `ReorderBuffer` walks
the sequence in order"
(`crates/engine/src/concurrent_delta/reorder.rs:1-26`,
`crates/transfer/src/delta_pipeline.rs:298-300`). The audit at
`docs/audits/workqueue-sender-multi-producer-audit.md:160-174`
emphasises that the `&mut self` sequence assignment in
`ParallelDeltaPipeline::submit_work` is the load-bearing single-point
counter.

### SP - trivial FIFO

The bounded channel is naturally FIFO when one producer sends. The
sequence counter is incremented inside `submit_work` and never races.
No coordination beyond `&mut self` is needed. Cost: zero atomics on
the hot path.

### MP - sequence coordination required

With N producers, FIFO is no longer free. Two strategies, both
demonstrated in tests:

1. **Atomic global counter.** Each producer fetches the next sequence
   via `AtomicU64::fetch_add(1, Ordering::Relaxed)` before send. The
   pattern is exercised by
   `crates/engine/src/concurrent_delta/multi_producer_audit.rs:181-197`
   under the `multi-producer` feature gate. Cost: one atomic increment
   per item, plus the implicit ordering edge between the increment
   and the channel send.

2. **Per-producer disjoint ranges.** When each producer's item count
   is known in advance (multi-root: count files per root; reader +
   walker: count input lines), allocate a contiguous sequence range
   per producer. No atomics on the hot path. The multi-root scenarios
   audit at `docs/audits/multi-root-transfer-scenarios.md:549-565`
   discusses the invariants this would have to preserve.

### Does the consumer care?

For the receiver pipeline, yes: the `ReorderBuffer`'s ring layout
(`crates/engine/src/concurrent_delta/reorder.rs:64-80`) silently
overwrites if two items map to the same slot. Sequence-number
collisions corrupt output. The atomic-coordinator or range-allocation
discipline is mandatory.

For the *sender-side* fan-in scenarios in section 2, the downstream
consumer is the wire writer. The wire writer serialises file entries
into the multiplexed stream and stamps them with NDX in arrival
order. As long as each generator thread submits self-consistent work
items, the wire writer assigns its own ordering. The sequence number
on the queue side is then internal bookkeeping for the consumer
thread, not an ordering primitive visible to the protocol.

This is a meaningful distinction. Receiver-side MP would *require*
sequence coordination; sender-side MP can in principle elide it if
the wire writer is allowed to reorder. None of today's plumbing makes
that elision possible because `ParallelDeltaPipeline::submit_work` is
the only place that stamps sequence numbers, and it lives on the
receiver path.

## 5. Capacity and backpressure

The current capacity policy is `2 * rayon::current_num_threads()`
(`crates/engine/src/concurrent_delta/work_queue/bounded.rs:88-92`,
`crates/engine/src/concurrent_delta/work_queue/capacity.rs:7-8,
32-38`).

### SP backpressure

`crossbeam_channel::Sender::send` blocks when the queue is full. The
single producer waits, the consumers drain, the producer unblocks.
Per-item latency is the consumer's drain rate.

### MP backpressure with N producers

When the queue fills, all N producers can be blocked simultaneously
on independent `send` calls. Crossbeam picks one waiter per freed
slot (a single internal mutex enforces approximate arrival order).
Consequences:

- Total in-flight work is still bounded by capacity. No memory blow-up.
- Wakeup throughput scales with the consumer's drain rate divided by
  the per-item cost. With N producers, *each* producer sees roughly
  `1/N` of the wakeups. A producer's perceived send latency therefore
  goes up linearly in N at saturation.
- Fairness is approximate. A consistently slow consumer combined with
  one fast producer and N-1 slow producers can starve a slow producer
  for many slots even though no producer is permanently starved.

The bench at `crates/engine/benches/sp_vs_mp_workqueue.rs` (PR #4209)
holds capacity constant across SP and MP groups
(`sp_vs_mp_workqueue.rs:50-60`) to isolate the producer-count effect
from the capacity effect. Capacity tuning for MP is therefore a
separate question: should `default_capacity()` grow with the producer
count to keep per-producer latency in check?

If MP graduates to default-on, a follow-up exercise would tune
`default_capacity()` upward to keep per-producer latency comparable
to the SP case at saturation (an N-producer queue likely wants
capacity at least `2 * T + N` so each producer always has at least
one slot in flight). This is a tuning question, not a v1 correctness
requirement.

## 6. Bench evidence

Two benches inform this question:

### PR #4209 - SP vs MP overhead

`crates/engine/benches/sp_vs_mp_workqueue.rs` runs two Criterion
groups, both moving 100K `DeltaWork` items through the queue:

- `sp/1p/100k`: one producer, default-build sender path
  (`sp_vs_mp_workqueue.rs:51-54`).
- `mp/4p/100k`: four producers, gated `Clone` impl
  (`sp_vs_mp_workqueue.rs:56-60`), only compiled with
  `--features multi-producer`.

The bench's top-of-file decision criteria
(`sp_vs_mp_workqueue.rs:19-32`) name the thresholds that promote MP
to default-on:

- MP within 5% of SP: feature can graduate to default-on with no
  measurable regression for SP-only callers.
- MP slower than SP by 15%+: feature stays opt-in.
- MP faster than SP by 15%+: feature is a strict win for any future
  fan-in caller.

For #1405 to recommend MP-by-default, the bench must show MP
throughput within 5% of SP at the 4-producer / 100K-item point, *and*
a real caller has to materialise (section 7).

### PR #4214 - drain_parallel alternatives

The drain-side fan-in bench at
`crates/engine/benches/drain_parallel_alternatives.rs` (committed in
the bench tree, referenced by
`docs/design/lockfree-mpsc-drain-design.md:168-243`) compares three
consumer-side collectors: sharded `Mutex<Vec>`, per-thread `Vec` with
final concat, and crossbeam MPSC. Worker counts 4, 8, 16. Item counts
10K and 100K.

For #1405, the #4214 bench is necessary context but not directly
deciding. The consumer side is single in our scenarios; the bench
informs `drain_parallel` itself, which is the same code path
regardless of producer count. If #4214's MPSC arm wins by a wide
margin and the producer-side switch to MP requires us to swap
`drain_parallel`'s collector, the two benches must be read together
before flipping the default.

### What the benches would need to show to motivate MP-by-default

- **#4209 MP arm within 5% of SP.** Producer-side overhead is not the
  bottleneck.
- **#4214 result does not regress under MP fan-in.** The drain path
  handles the changed arrival pattern without throughput loss.
- **A real caller exists.** Neither bench is sufficient on its own;
  the multi-root or `--files-from` parallel generator path has to
  actually be wired up and exercised by an integration test that
  shows wall-time improvement on a representative workload.

Until all three are true, the bench numbers alone do not motivate the
default-on switch.

## 7. Recommendation

**Keep `WorkQueueSender` `Send + !Clone` in the default build. Keep
the `multi-producer` cargo feature gated and opt-in. Do not introduce
`Arc<WorkQueueSender>` as a separate primitive. Do not add
`with_producers(n)` until a real caller emerges.**

The three components of this recommendation:

1. **Default build stays single-producer.** The compile-time SPMC
   invariant
   (`crates/engine/src/concurrent_delta/work_queue/bounded.rs:48-50`,
   absence of `Clone`) is the cheapest possible enforcement of the
   audit's "all live producers are SP" conclusion (#4173). Until #4209
   produces a positive crossover and a real caller is staged, no
   production code can construct a second producer by accident.

2. **`multi-producer` feature stays gated and opt-in.** The feature
   covers every variant of Design A. Its tests at
   `crates/engine/tests/multi_producer_work_queue.rs:51-501` already
   exercise 4-16 producers, ordering, drop-disconnect, and
   backpressure. The cost of keeping it gated is one `#[cfg]` in the
   sender module and a benches-only compile flag; the benefit is the
   compile-time SPMC invariant in production builds.

3. **No Arc-wrapped primitive.** The evaluation at
   `docs/design/arc-workqueue-sender-eval.md:55-76` documents the
   reasoning. The primitive duplicates indirection, surfaces no new
   capability, and breaks the "one obvious way" rule.

The implicit fourth component: **promote the feature to default-on
only when both bench thresholds are crossed *and* a real caller is
queued behind the change.** Today, neither is true. The audit at
#4173 finds zero current producers; the bench at #4209 publishes
numbers but no caller has been wired. Flipping the default on
speculation would relax a compile-time invariant for no measurable
gain, and would force every reviewer to re-derive "is this site
allowed to clone the sender?" on every PR.

The cost of being wrong in the conservative direction is low: a
future PR that genuinely needs MP turns on the feature flag in its
crate's `Cargo.toml` and gets `Clone`. The cost of being wrong in
the aggressive direction is high: an accidental second producer leaks
into production, sequence coordination is silently dropped, and the
`ReorderBuffer` corrupts output (see ordering section above).

The conservative direction wins.

## 8. Document the cost so reviewers can reject naive MP refactors

A common failure mode is a reviewer who reads "the crossbeam channel
underneath is already MPMC" in the module documentation
(`crates/engine/src/concurrent_delta/work_queue/mod.rs:11-35`) and
concludes that adding a `.clone()` to the sender at one call site is
free. It is not.

The hidden costs:

- **Sequence coordination becomes mandatory.** Sites 1, 2, and 3 in
  the audit (`docs/audits/workqueue-sender-multi-producer-audit.md:147-220`)
  rely on `&mut self` to advance the sequence counter. A second
  producer requires either an atomic global counter or per-producer
  ranges. The audit at
  `multi_producer_audit.rs:174-214` shows the atomic pattern.
- **Close semantics flip.** With one producer, drop-on-shutdown ends
  the consumer iterator instantly
  (`crates/transfer/src/delta_pipeline.rs:318-322`). With N producers,
  the consumer iterator blocks until the *last* clone drops. A
  forgotten clone hangs `drain_parallel` forever.
- **Backpressure characteristics change** (section 5). N producers
  blocking on a queue sized for one producer increases tail latency
  proportionally.
- **The default-build compile-time invariant is destroyed at that
  site.** Once `Clone` is invoked anywhere in production, the
  workspace must be re-audited at every PR.

Reviewers should treat any PR that adds `.clone()` on a
`WorkQueueSender` (or enables the `multi-producer` feature for the
`engine` crate dependency) as a substantive design change subject to
this document's recommendations.

## 9. Risks of the recommendation

### Risk: a real caller materialises and we are not ready

Mitigation: the integration tests at
`crates/engine/tests/multi_producer_work_queue.rs:51-501` already
cover the fan-in semantics. The library code path is one feature
flag away. The lag between "real caller needs MP" and "MP available
in their build" is small.

### Risk: the SP-vs-MP bench (#4209) shows MP is materially faster

Mitigation: revisit this recommendation. If MP is strictly faster
even with one producer (which would be surprising, since it adds an
atomic clone count), then the default-build cost of allowing `Clone`
is genuinely negative and the gate becomes purely a compile-time
single-producer enforcement convenience. Re-read section 7 in light
of the bench data and consider promoting.

### Risk: the multi-root or `--files-from` parallel paths land independently

Mitigation: the multi-root scenarios audit at
`docs/audits/multi-root-transfer-scenarios.md:642-699` already
recommends closing #1405 as not-required because the wire path
cannot benefit from per-root multi-producer dispatch. If a future
maintainer overrides that recommendation and ships a multi-root
parallel generator, this document's recommendation does not block
it: the multi-root code can opt into the `multi-producer` cargo
feature locally at its crate boundary, and the audit at #4173 grows
by exactly one row. The default build still has zero MP producers.

## 10. Decision

Keep the current shape:

- `WorkQueueSender` remains `Send + !Clone` in the default build
  (`crates/engine/src/concurrent_delta/work_queue/bounded.rs:48-50`).
- The `multi-producer` cargo feature
  (`crates/engine/Cargo.toml:88-91`) remains opt-in. Its `Clone` impl
  at `crates/engine/src/concurrent_delta/work_queue/multi_producer.rs:10-23`
  stays as the single MP entry point.
- `Arc<WorkQueueSender>` is not introduced
  (`docs/design/arc-workqueue-sender-eval.md` recommendation
  reaffirmed).
- Promotion to default-on is blocked on three independent signals:
  - PR #4209 bench shows MP within 5% of SP.
  - PR #4214 bench shows no drain-side regression under MP fan-in.
  - A real caller (multi-root push, parallel `--files-from`, async
    per-source generators) is staged and ready to use MP.

Until all three are true, the recommendation is to leave the feature
where it is and document why. This document is that documentation.
