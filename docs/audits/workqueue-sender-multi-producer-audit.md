# `WorkQueueSender` multi-producer usage audit (#1383)

Tracking task: oc-rsync task #1383 ("Audit `WorkQueueSender` usage sites for
multi-producer needs"). Companion tasks: #1405 (design multi-producer
`WorkQueue` for parallel generator fan-in), #1569 / #1610 (Arc-wrapped sender
design), #1611 / #1404 (`Clone` impl behind the `multi-producer` feature),
#1614 (compile-time SPMC enforcement), #1572 (SP vs MP overhead benchmark
plan), #1609 (earlier in-source audit).

Related references:
- `docs/audits/workqueue-sp-vs-mp-overhead.md` (#1572) - the benchmark plan
  that gates whether the `multi-producer` feature should graduate from
  forward-looking infrastructure to a default-on capability.
- `docs/design/arc-workqueue-sender-eval.md` (#1383 evaluation note) -
  concludes Arc-wrapping the sender adds no value over the existing
  `Clone`-based path.
- `docs/design/arc-workqueuesender.md` (#1610) - longer Arc-wrapped sender
  design space.
- `docs/design/multi-producer-workqueue.md` (#1405) - the multi-producer
  design exploration.
- `crates/engine/src/concurrent_delta/multi_producer_audit.rs` - in-source
  audit module that this document promotes to a workspace-level audit.

Last verified: 2026-05-16 against
`crates/engine/src/concurrent_delta/work_queue/`,
`crates/engine/src/concurrent_delta/multi_producer_audit.rs`, and
`crates/transfer/src/delta_pipeline.rs`. No source files are modified.

## Summary

The producer side of the concurrent delta `WorkQueue` is exposed as
`engine::concurrent_delta::work_queue::WorkQueueSender`
(`crates/engine/src/concurrent_delta/work_queue/bounded.rs:48`). The type is
`Send + !Clone` by default; a `Clone` impl is gated behind the
`multi-producer` cargo feature
(`crates/engine/src/concurrent_delta/work_queue/multi_producer.rs:17-23`,
`crates/engine/Cargo.toml:91`), so the production build enforces the
single-producer invariant at compile time per #1614.

A workspace grep for `WorkQueueSender` and `WorkQueue\b` returns 42 raw
matches across 12 files. After removing module/struct definitions, doc
comments, re-exports, and references inside the work-queue module itself,
**three live production producer sites** remain. All three are the same
ownership transfer at different layers of the receiver delta pipeline. **All
three are correctly single-producer.** The two production-adjacent surfaces
(integration tests and the in-source audit) exercise the gated `Clone` path
without creating any default-build producer requirement.

| Category                  | Count |
|---------------------------|-------|
| Production producer sites | 3     |
| Single-producer (correct) | 3     |
| Multi-producer-required   | 0     |
| Pseudo-multi-producer     | 0     |
| Test-only producer sites  | 9     |

Top-1 recommendation: **keep `WorkQueueSender` `Send + !Clone` by default,
keep the `multi-producer` feature gated, and do not introduce an
`Arc<WorkQueueSender>` primitive.** The `Clone`-based path from #1611 already
covers every plausible fan-in scenario; the underlying
`crossbeam_channel::Sender` is itself an `Arc`, so wrapping the public
sender in another `Arc` only doubles the refcount layer without adding
behaviour
(`docs/design/arc-workqueue-sender-eval.md` sections 3-4).

## Methodology

1. Workspace-wide grep:

   ```sh
   rg --no-heading "WorkQueueSender|WorkQueue\b" crates/ --type rust
   ```

   Result: 42 matches across 12 files.

2. Each match was classified as one of:
   - **Definition or doc** (struct/type/function header, module doc, re-export).
   - **Type-only reference** (function signature, field type, trait impl).
   - **Producer site** (a thread or function actually constructs or holds the
     sender and calls `WorkQueueSender::send`).
   - **Test producer site** (same as above but inside `#[cfg(test)]` or an
     integration test under `crates/*/tests/`).

3. For each producer site, three properties were captured:
   - **Owner**: which thread holds the sender for the lifetime of the queue.
   - **Producer count**: how many threads can ever call `send` on the same
     queue.
   - **Data**: what flows through (`DeltaWork` items and how they are stamped).

4. Each producer site was assigned a verdict from the task taxonomy:
   - **Single-producer**: one thread ever owns the sender, today and under
     any plausible refactor.
   - **Multi-producer-required**: the call site genuinely needs multiple
     producers feeding the same queue.
   - **Pseudo-multi-producer**: the code uses `Arc`/`Mutex` or `Clone` but
     could collapse to single-producer with a small refactor.

5. The recommendation per site is one of: keep, refactor to single-producer,
   or extend the `WorkQueue` API to support a `Clone`-able sender.

The in-source audit at
`crates/engine/src/concurrent_delta/multi_producer_audit.rs` was treated as
an authoritative source for the rsync-protocol reasoning. This document
extends that audit with explicit file:line citations for every producer site
and folds the test-side picture into the same table.

## Inventory of `WorkQueueSender` surface

Definitions and supporting surface (not producer sites - listed once for
reference, then excluded from the verdict table):

- `crates/engine/src/concurrent_delta/work_queue/bounded.rs:48` -
  `pub struct WorkQueueSender { pub(super) tx: Sender<DeltaWork> }`. The
  newtype over `crossbeam_channel::Sender<DeltaWork>`.
- `crates/engine/src/concurrent_delta/work_queue/bounded.rs:74-81` -
  `WorkQueueSender::send`. The only mutator. Blocks when the bounded channel
  is full; returns `SendError(DeltaWork)` on receiver hang-up.
- `crates/engine/src/concurrent_delta/work_queue/bounded.rs:89-92` -
  `bounded() -> (WorkQueueSender, WorkQueueReceiver)` with default
  capacity `2 * rayon::current_num_threads()`.
- `crates/engine/src/concurrent_delta/work_queue/bounded.rs:100-104` -
  `bounded_with_capacity(capacity)` for explicit sizing.
- `crates/engine/src/concurrent_delta/work_queue/mod.rs:11-35` - module
  doc that codifies the SPMC contract (single-producer multiple-consumer).
- `crates/engine/src/concurrent_delta/work_queue/mod.rs:106` -
  `pub use bounded::{SendError, WorkQueueReceiver, WorkQueueSender, ...}`.
- `crates/engine/src/concurrent_delta/work_queue/multi_producer.rs:10-23` -
  feature-gated `Clone` impl. Empty `multi-producer` feature declared at
  `crates/engine/Cargo.toml:91`.
- `crates/engine/src/concurrent_delta/work_queue/limiter.rs:28` - doc-only
  intra-doc link `[WorkQueueSender]: super::WorkQueueSender`.
- `crates/engine/src/concurrent_delta/mod.rs:13-25` - module-level table
  that names `WorkQueueSender` / `WorkQueueReceiver` and describes the
  bounded `crossbeam_channel` backing.
- `crates/engine/src/concurrent_delta/consumer.rs:3` - module doc that
  references `WorkQueue` (consumer-side; not a producer).
- `crates/transfer/src/delta_pipeline.rs:14, 34, 148, 161` - module doc and
  `use engine::concurrent_delta::work_queue::{self, WorkQueueSender}` import
  plus references inside the architecture diagram.

## Live producer sites (default build)

There are exactly three places in default-feature production code where a
thread holds a `WorkQueueSender` and calls `send`. They form a single
ownership chain through the receiver pipeline.

### Site 1 - `ParallelDeltaPipeline::submit_work`

- **File:** `crates/transfer/src/delta_pipeline.rs:185` (field declaration);
  `crates/transfer/src/delta_pipeline.rs:296-308` (`submit_work` implementation
  of `ReceiverDeltaPipeline::submit_work`); ownership created at
  `crates/transfer/src/delta_pipeline.rs:233-242` (`with_capacity`) and
  `crates/transfer/src/delta_pipeline.rs:263-272` (`with_bypass_capacity`).
- **Owner:** the receiver thread (the thread driving the rsync wire-protocol
  receive loop). The sender lives in `self.work_tx: Option<WorkQueueSender>`
  and is dropped from inside `flush` at
  `crates/transfer/src/delta_pipeline.rs:318-322` to signal shutdown.
- **Producer count:** one. The receiver loop owns `&mut self` for the
  pipeline; there is no path that hands a `&mut self` to a second thread.
- **Data:** `DeltaWork` items with a monotonically increasing `sequence`
  stamped at `crates/transfer/src/delta_pipeline.rs:298-300`
  (`let seq = self.next_sequence; self.next_sequence += 1;
  work.set_sequence(seq);`). The monotonic sequence number is the load-bearing
  invariant for the downstream `ReorderBuffer`.
- **Verdict:** **single-producer (correct)**.
- **Recommendation:** **keep**. The sequence assignment in `submit_work`
  relies on `&mut self`; making it multi-producer would require an
  `AtomicU64` counter shared across producers (see
  `multi_producer_audit.rs:174-214` for the pattern). The rsync wire protocol
  delivers file entries in a single multiplexed stream, so there is exactly
  one thread reading from the wire and exactly one source of monotonic
  sequence numbers. Adding multi-producer here would not improve throughput
  (the bottleneck is the wire stream, not the producer thread) and would
  break the zero-overhead sequence assignment.

### Site 2 - `ThresholdDeltaPipeline` (Parallel mode)

- **File:** `crates/transfer/src/delta_pipeline.rs:337-340`
  (`ThresholdMode::Parallel(ParallelDeltaPipeline)`);
  `crates/transfer/src/delta_pipeline.rs:421-434`
  (`promote_to_parallel` constructs the inner `ParallelDeltaPipeline`);
  `crates/transfer/src/delta_pipeline.rs:452-465`
  (`submit_work` delegates to `par.submit_work`).
- **Owner:** the receiver thread, same as Site 1. The threshold pipeline
  buffers items in a `Vec<DeltaWork>` until it crosses
  `DEFAULT_PARALLEL_THRESHOLD = 64`
  (`crates/transfer/src/delta_pipeline.rs:455-459`), then promotes to a
  `ParallelDeltaPipeline` and forwards every subsequent submission via
  `par.submit_work(work)` (`crates/transfer/src/delta_pipeline.rs:463`).
- **Producer count:** one. This is a pure delegation to Site 1; no
  additional producer threads are introduced. The mode switch happens on the
  same thread that called `submit_work`.
- **Data:** identical to Site 1 (`DeltaWork` items with monotonic sequence
  numbers). Buffered items are replayed in order through
  `parallel.submit_work(item)`
  (`crates/transfer/src/delta_pipeline.rs:429-431`), preserving the wire
  ordering.
- **Verdict:** **single-producer (correct)**.
- **Recommendation:** **keep**. Delegates to Site 1; the verdict and
  recommendation are inherited.

### Site 3 - `DeltaConsumer::spawn` callers (sender drop-side)

- **File:** `crates/engine/src/concurrent_delta/consumer.rs:129` (the
  receiver-side consumer); paired with the sender drop at
  `crates/transfer/src/delta_pipeline.rs:318-322`.
- **Owner:** producer ownership is held by Site 1 or Site 2; this entry is
  listed because the drop of the sender at flush time is the action that
  closes the channel and causes `drain_parallel_into` inside the consumer to
  observe EOF. No third thread ever holds the sender.
- **Producer count:** one. The drop happens on the receiver thread that
  owned the pipeline; this is the same thread as Sites 1 and 2.
- **Data:** N/A; the drop only signals "no more work".
- **Verdict:** **single-producer (correct)**.
- **Recommendation:** **keep**. The shutdown protocol explicitly relies on
  the sender being unique: dropping the sole `Option<WorkQueueSender>` is
  what closes the bounded channel and lets the consumer reach the end of
  its `into_iter()`. Cloning the sender would require coordinating who drops
  the last clone, which is exactly the failure mode the `!Clone` invariant
  prevents at compile time (#1614).

## Test-only producer sites

These exercise the queue but are not production code. They are listed for
completeness so future readers can trace every `send` call.

| File | Line | Producer count | Feature gate | Purpose |
|------|------|----------------|--------------|---------|
| `crates/engine/src/concurrent_delta/multi_producer_audit.rs:118-124` | 1 | none | unit test illustrating monotonic sequence under SP |
| `crates/engine/src/concurrent_delta/multi_producer_audit.rs:150-156` | 1 | none | shows zero-overhead sequence assignment |
| `crates/engine/src/concurrent_delta/multi_producer_audit.rs:181-197` | 2 (cloned sender) | `#[cfg(feature = "multi-producer")]` | demonstrates the atomic-coordinator pattern MP would require |
| `crates/engine/src/concurrent_delta/multi_producer_audit.rs:226-232` | 1 | none | end-to-end SP through `DeltaConsumer` |
| `crates/engine/src/concurrent_delta/consumer.rs:323-330` | 1 | none | `spawn_producer`/`send_items` test helpers |
| `crates/engine/tests/pipeline_reorder_integration.rs:16-112` | 1 | none | integration test for SP -> drain -> reorder |
| `crates/engine/tests/multi_producer_work_queue.rs:51-112` | 4 (cloned sender) | `#![cfg(feature = "multi-producer")]` | MP fan-in completeness |
| `crates/engine/tests/multi_producer_work_queue.rs:120-177` | 4 (cloned sender) | `#![cfg(feature = "multi-producer")]` | MP per-producer ordering |
| `crates/engine/tests/multi_producer_work_queue.rs:185-501` | 4-16 (cloned sender) | `#![cfg(feature = "multi-producer")]` | MP backpressure, mixed work types, high contention, staggered start, streaming drain |

The MP-feature tests prove the gated `Clone` path works correctly under
heavy fan-in load (4 to 16 producers feeding a capacity-1 to capacity-16
bounded queue). They give the project confidence that **if** a production
need for MP emerges, the underlying `crossbeam_channel::Sender::clone`
delegation is already exercised. There is no current production caller.

## Multi-producer opportunities that do not apply

The following scenarios were considered as potential multi-producer call
sites and ruled out. Each is summarised here with the file evidence that
defeats it; see `multi_producer_audit.rs:36-95` for the longer-form
discussion.

- **Multi-root transfers (`--files-from` with disjoint trees).** Even if
  generators run in parallel on the sender side, the wire protocol
  serialises file entries into a single ordered stream consumed by the
  receiver. Site 1 still sees one stream, so a multi-producer queue at the
  receiver provides no benefit. Parallelism, if added, belongs on the
  sender side and would not touch `WorkQueueSender`.

- **Incremental recursion (`--inc-recurse`) segments.** Segments arrive
  sequentially over the wire (one NDX range after another). The receiver
  must process segments in order to keep the monotonic sequence-number
  invariant that Site 1 relies on. Multi-producer would require coordinated
  sequence numbering across segments (an `AtomicU64` shared with the
  segment dispatchers) and would add complexity without throughput benefit,
  since the queue is wire-bound rather than CPU-bound.

- **Local-to-local copy (`oc-rsync /src/ /dst/`).** The local-copy executor
  at `crates/engine/src/local_copy/` does not use `WorkQueue` at all; it
  uses `rayon::par_iter` directly on the file list (see
  `crates/engine/src/local_copy/executor/file/`). The `WorkQueue` and
  `DeltaConsumer` pipeline is specific to the receiver wire-protocol path.
  If a future local-copy refactor wanted to share the work-queue
  infrastructure (tracked under #1565 in the design notes), the call site
  would still consume from a single producer thread that walks the file
  list, so multi-producer would remain unnecessary.

## Per-site recommendations

| Site | File | Line | Verdict | Recommendation |
|------|------|------|---------|----------------|
| 1 | `crates/transfer/src/delta_pipeline.rs` | 185, 296-308 | Single-producer | **Keep.** Sequence assignment uses `&mut self`; MP would require an `AtomicU64` for no throughput gain. |
| 2 | `crates/transfer/src/delta_pipeline.rs` | 337-340, 452-465 | Single-producer | **Keep.** Pure delegation to Site 1. |
| 3 | `crates/transfer/src/delta_pipeline.rs` (sender drop) | 318-322 | Single-producer | **Keep.** Sender drop is the shutdown signal; cloning would require coordinating who drops last, which is exactly the failure mode the `!Clone` invariant prevents. |

## Conclusion

All 3 production producer sites correctly use single-producer ownership.
Zero sites are multi-producer-required. Zero sites are pseudo-multi-producer
(no `Arc<WorkQueueSender>` or `Mutex<WorkQueueSender>` wrappers exist in the
workspace).

The `multi-producer` cargo feature should remain gated. The benchmark plan
at `docs/audits/workqueue-sp-vs-mp-overhead.md` (#1572) describes the
quantitative criteria that would justify graduating the feature; until that
benchmark runs and produces a positive crossover, the feature stays as
forward-looking infrastructure with full integration-test coverage at
`crates/engine/tests/multi_producer_work_queue.rs`.

The `Arc<WorkQueueSender>` primitive proposed under #1610 / #1613 should
not be added. As `docs/design/arc-workqueue-sender-eval.md` documents,
`crossbeam_channel::Sender` is already internally an `Arc`; wrapping the
public sender in another `Arc` doubles the refcount layer for no semantic
gain and produces "two ways to do the same thing" (`Clone` via feature
flag vs `Arc` always available), which makes the audit harder to keep
authoritative. Any future call site that needs shared ownership can wrap
the sender in `Arc` at the call site without library support.

## Feeds into

- **#1405 (multi-producer `WorkQueue` design).** This audit closes the
  "is there a current need" question with a no. The design exploration
  remains valid as preparation for a future need; ship-blockers do not
  exist today.
- **#1569 / #1610 (Arc-wrapped sender).** This audit reaffirms the
  recommendation in `docs/design/arc-workqueue-sender-eval.md`: do not add
  the Arc-wrapped primitive. The default `!Clone` shape and the
  feature-gated `Clone` path together cover the design space.
- **#1572 (SP vs MP overhead benchmark).** No producer site is forcing
  MP today, so the benchmark is the decisive input for any future change
  to the default build's sender shape.
