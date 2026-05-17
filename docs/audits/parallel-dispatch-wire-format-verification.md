# Wire-format unchanged under parallel dispatch (#1548)

Static audit verifying that the parallel-dispatch infrastructure under
`crates/engine/src/concurrent_delta/` never causes oc-rsync to emit a
different byte stream on the wire than the sequential `recv_files()` path
upstream rsync uses. No runtime benchmarks; every claim is derived from
source code citations.

## 1. Hypothesis

Enabling parallel dispatch must never change the bytes oc-rsync sends on the
network. Whether the receiver processes files via `SequentialDeltaPipeline`
or `ParallelDeltaPipeline`, the per-file order, NDX values, signature
requests, and any per-file wire emissions must be byte-identical, matching
upstream's `receiver.c:recv_files()` sequential loop. The reordering
mechanism in `concurrent_delta` exists exclusively to restore submission
order *after* parallel work completes; it must not leak completion-order
artifacts into anything that touches the wire.

## 2. Survey of parallel-dispatch sites

All parallel-dispatch sites in `crates/engine/src/concurrent_delta/` and
their reordering mechanisms.

### 2.1 Work queue drain (rayon scope)

- File: `crates/engine/src/concurrent_delta/work_queue/drain.rs`
- `WorkQueueReceiver::drain_parallel(self, f)` at
  `drain.rs:57` opens `rayon::scope` at `drain.rs:67` and spawns one task
  per `DeltaWork` (`s.spawn` at `drain.rs:71`). Results are written into
  per-thread mutex-guarded shards (`shards[idx % num_shards]` at
  `drain.rs:81`) and flattened after the scope ends (`drain.rs:86-89`).
  Output order is undefined.
- `WorkQueueReceiver::drain_parallel_into(self, f, tx)` at
  `drain.rs:136` opens `rayon::scope` at `drain.rs:141` and spawns one
  task per item (`drain.rs:145`); each task pushes through a cloned
  `crossbeam_channel::Sender<R>` (`drain.rs:149`). Output order is the
  worker-completion order, again undefined.

Reordering is **not** performed at this layer. The contract of
`drain_parallel*` is "return all results, order undefined" - reordering is
the consumer's responsibility.

### 2.2 Strategy dispatch

- File: `crates/engine/src/concurrent_delta/strategy.rs`
- `select_strategy(work)` at `strategy.rs:257` picks a `&'static dyn
  DeltaStrategy` based on `DeltaWorkKind` (`strategy.rs:261-264`).
- `dispatch(work)` at `strategy.rs:275` runs the chosen strategy and stamps
  the result with the work item's sequence number via
  `with_sequence(work.sequence())` (`strategy.rs:278`).

The sequence stamp is what enables downstream reordering. Each result
carries the producer-assigned sequence regardless of which worker thread
ran it. No wire emission happens inside `dispatch` or either strategy; the
strategies operate purely on local files (basis, source, dest paths in
`run_self_contained_delta` at `strategy.rs:171`) or on pre-computed
literal/matched counters (`DeltaTransferStrategy::process` at
`strategy.rs:133-148`).

### 2.3 Consumer pipeline

- File: `crates/engine/src/concurrent_delta/consumer.rs`
- `DeltaConsumer::spawn_inner(rx, capacity, bypass)` at `consumer.rs:148`
  spawns two named threads:
  - `delta-drain` at `consumer.rs:161-166` runs `drain_parallel_into` with
    `strategy::dispatch` as the per-item function. Each result is streamed
    through a bounded `crossbeam_channel` (`stream_tx` constructed at
    `consumer.rs:158`).
  - `delta-reorder` at `consumer.rs:170-216` drains the stream channel,
    feeds each result into a `ReorderBuffer` via `insert(seq, result)`
    (`consumer.rs:182`), and forwards contiguous in-order runs through an
    `mpsc::Sender<DeltaResult>` to the consumer's output channel
    (`consumer.rs:186, 199-203, 207-211`).
- Backpressure: when `insert` returns `Err` (capacity exceeded), the
  reorder thread drains ready items first (`consumer.rs:184-189`) and only
  falls back to `force_insert` (`consumer.rs:193`) when the next-expected
  slot is genuinely empty.
- `DeltaConsumer::spawn_bypass(rx)` at `consumer.rs:143` uses
  `ReorderBuffer::passthrough()` (`consumer.rs:174`), delivering items in
  completion order, not submission order. Selected by the caller via
  `ParallelDeltaPipeline::new_bypass*` (see 2.5).

### 2.4 Reorder buffer

- File: `crates/engine/src/concurrent_delta/reorder/mod.rs`
- `ReorderBuffer::insert(sequence, item)` at `reorder/mod.rs:297` slots
  items into a pre-allocated ring `slots: Box<[Option<T>]>`
  (`reorder/mod.rs:91`) indexed by `(sequence - next_expected) + head`,
  giving O(1) insert.
- `ReorderBuffer::drain_ready` at `reorder/mod.rs:426` returns an iterator
  that yields contiguous items starting at `next_expected`, advancing the
  cursor as items are released.
- `ReorderBuffer::next_in_order` at `reorder/mod.rs:400` is the single-item
  variant of the same drain.
- `force_insert` at `reorder/mod.rs:502` exists as a deadlock-breaker used
  only by `DeltaConsumer` at `consumer.rs:193`. It is the one path that
  can theoretically violate ordering (it places an item out-of-position
  when the buffer is at capacity but `next_expected` has not yet arrived).
  See gap G3 in section 4.

### 2.5 Pipeline wrapper (transfer crate)

The receiver-facing facade lives in the `transfer` crate but is the only
production caller of `concurrent_delta`:

- File: `crates/transfer/src/delta_pipeline.rs`
- `ParallelDeltaPipeline::with_capacity` at `delta_pipeline.rs:233-242`
  creates a bounded `WorkQueueSender` (via
  `work_queue::bounded_with_capacity` at `delta_pipeline.rs:234`) and a
  `DeltaConsumer::spawn(work_rx, capacity)` (`delta_pipeline.rs:235`).
- `submit_work` at `delta_pipeline.rs:297-308` assigns a monotonic
  `next_sequence` (`delta_pipeline.rs:298-300`) before sending. This is
  what gives the `ReorderBuffer` its single source of truth for ordering.
- `poll_result` at `delta_pipeline.rs:310-316` calls
  `consumer.try_recv()` (`delta_pipeline.rs:315`). The consumer's
  internal `ReorderBuffer` has already restored sequence order, so
  `poll_result` returns items in submission order.
- `flush` at `delta_pipeline.rs:318-333` drops the sender to close the
  work queue, then drains the consumer's iterator (`delta_pipeline.rs:328`)
  to collect remaining results.
- `ThresholdDeltaPipeline` at `delta_pipeline.rs:360-490` buffers items
  until a threshold is reached, then promotes to `ParallelDeltaPipeline`
  or stays sequential. Threshold default `DEFAULT_PARALLEL_THRESHOLD = 64`
  at `delta_pipeline.rs:42`.

### 2.6 Spill buffer

- File: `crates/engine/src/concurrent_delta/spill.rs`
- `SpillableReorderBuffer` at `spill.rs:243` wraps `ReorderBuffer` and
  spills the highest-sequence items to a temp file when in-memory bytes
  exceed a configurable threshold. Delivery order is preserved -
  `next_in_order` at `spill.rs:412` and `drain_ready` at `spill.rs:452`
  reload spilled bytes transparently before yielding them in sequence
  order. No direct wire interaction; spilling is invisible to callers.

### Site count

Five parallel-dispatch sites surveyed (`drain_parallel`,
`drain_parallel_into`, `delta-drain` thread, `delta-reorder` thread,
`ParallelDeltaPipeline`), plus two passive participants (`select_strategy`
and the spill buffer). All seven feed into the same single-producer
`ReorderBuffer` whose output channel is consumed serially by `poll_result`.

## 3. Existing tests that establish the invariant

### 3.1 Internal ordering tests (unit + integration)

These tests exercise the reorder machinery directly and assert that the
parallel pipeline delivers results in submission order. They cover the
internal invariant but **do not assert anything about wire bytes** - they
work on `DeltaResult` structs, not network output.

- `crates/engine/src/concurrent_delta/consumer.rs:336` -
  `delivers_results_in_sequence_order` (50-item pipeline).
- `crates/engine/src/concurrent_delta/consumer.rs:419` -
  `large_batch_in_order` (500-item pipeline through 32-deep queue).
- `crates/engine/src/concurrent_delta/consumer.rs:492` -
  `small_reorder_capacity_still_delivers_all` (exercises the
  `force_insert` backpressure branch).
- `crates/engine/tests/pipeline_reorder_integration.rs:21` -
  `end_to_end_streaming_pipeline_delivers_in_order` (500 items, variable
  per-item cost to induce out-of-order completion, asserts strict
  submission order at the output).
- `crates/transfer/src/delta_pipeline.rs:715` -
  `parallel_submit_and_flush` (10 items, all `is_success`, sequence ==
  ndx).
- `crates/transfer/src/delta_pipeline.rs:734` -
  `parallel_preserves_submission_order` (50 items, asserts
  `sequence == i` and `ndx == i` for every result).
- `crates/transfer/src/delta_pipeline.rs:863` -
  `parallel_sequence_monotonically_increases` (20 items, asserts strict
  +1 monotonicity).

### 3.2 Protocol golden byte tests

Golden byte tests live under `crates/protocol/tests/`:

- `golden_protocol_v28_wire.rs`
- `golden_protocol_v28_flist.rs`
- `golden_protocol_v28_handshake.rs`
- `golden_protocol_v28_mplex_delta_stats.rs`
- `golden_protocol_v29_flist.rs`
- `golden_protocol_v29_wire.rs`
- `golden_handshakes.rs`
- `iconv_golden_bytes.rs`
- `lz4_golden_bytes.rs`
- `zlib_golden_bytes.rs`
- `zstd_golden_bytes.rs`
- `zstd_daemon_recv_golden.rs`
- `zstd_interop_golden_bytes.rs`

None of these golden tests reference `concurrent_delta`, `DeltaConsumer`,
`ReorderBuffer`, `WorkQueue`, or `drain_parallel`. Verified by:

```
grep -rn "concurrent_delta\|ReorderBuffer\|WorkQueue\|DeltaConsumer" \
    crates/protocol/tests   # produces zero matches
```

They exercise wire encoding directly via the `protocol` crate, which has
no dependency on the parallel pipeline.

### 3.3 Interop tests

- `tools/ci/run_interop.sh` invokes the `oc-rsync` binary against upstream
  rsync versions 3.0.9, 3.1.3, and 3.4.1. The script runs version tests
  in parallel bash subshells (`run_interop.sh:9858, 9861, 9907`), but
  every test uses the production binary with default config - no flag
  selects the parallel pipeline (see section 4, gap G1).

### 3.4 Scenarios run with and without parallel dispatch

| Scenario                                  | Sequential | Parallel |
|-------------------------------------------|------------|----------|
| Golden byte tests (`crates/protocol/tests/golden_*`) | yes | no (untestable, no caller) |
| Internal ordering unit tests (`consumer.rs`, `delta_pipeline.rs`) | yes | yes |
| End-to-end pipeline integration (`pipeline_reorder_integration.rs`) | n/a | yes |
| `cargo nextest run --workspace` | yes | yes |
| `tools/ci/run_interop.sh` (binary vs upstream) | yes | no |
| Transfer integration tests (`crates/transfer/tests/*.rs`) | yes | no |

## 4. Gaps

### G1. `ParallelDeltaPipeline` has no production caller

`Receiver::set_delta_pipeline` at
`crates/transfer/src/receiver/mod.rs:297` is the only public entry point
for selecting a non-sequential pipeline. A repository-wide search shows
zero callers outside its own definition:

```
grep -rn "set_delta_pipeline" crates/   # returns only the definition itself
```

The receiver's `delta_pipeline` field at `receiver/mod.rs:232` is also
never read from inside the receiver - only assigned at `receiver/mod.rs:281`
and `receiver/mod.rs:298`:

```
grep -rn "self.delta_pipeline\|delta_pipeline\." \
    crates/transfer/src/receiver
# crates/transfer/src/receiver/mod.rs:298: self.delta_pipeline = Some(pipeline);
```

No CLI flag, env var, or config setting toggles parallel dispatch on. The
production binary always runs `SequentialDeltaPipeline` (the default at
`receiver/mod.rs:281`). The wire format observed by upstream rsync is
**always** generated by the sequential path; parallel dispatch is
infrastructure that has not yet been wired into the receiver transfer
loop.

This means the hypothesis in section 1 is trivially true in the shipped
binary - but it is also currently unverifiable end-to-end, because there
is no way for a user, a test, or an interop run to actually exercise the
parallel path against the wire.

### G2. No golden test compares sequential vs parallel byte streams

Even at the unit level there is no fixture that:

1. Drives a fixed `DeltaWork` batch through `SequentialDeltaPipeline`.
2. Drives the same batch through `ParallelDeltaPipeline`.
3. Asserts identical `Vec<DeltaResult>` (same NDX order, same
   literal/matched byte counts, same status sequence).

The existing `parallel_*` tests in `delta_pipeline.rs` assert ordering
properties of the parallel path in isolation. They do not cross-check
against the sequential output of the same input. A regression that
re-ordered NDX, mis-stamped sequences, or mismatched
literal-vs-matched accounting between strategies would not be caught.

### G3. `force_insert` deadlock-break path is untested at the integration level

`ReorderBuffer::force_insert` at `reorder/mod.rs:502` is called by the
consumer thread (`consumer.rs:193`) when the buffer is full but
`next_expected` has not yet arrived. By construction it places an item
out of sequence order. The only test that exercises this path is
`consumer.rs:492` (`small_reorder_capacity_still_delivers_all`), and
that test still asserts in-order delivery (`r.sequence() == i`), which
means either the branch is being taken without violating order (because
of timing) or the test does not actually trigger `force_insert`. There
is no test that:

- Forces `force_insert` to fire deterministically (for example by
  blocking the head sequence indefinitely).
- Captures the resulting delivery order to confirm/refute ordering
  violations.
- Asserts the receiver would still produce a wire-conformant byte stream
  in that pathological case.

This matches existing project memory note
`project_consumer_force_insert_smell` (see `MEMORY.md`).

### G4. Interop harness does not exercise parallel dispatch

`tools/ci/run_interop.sh` never sets the parallel pipeline, so the
upstream/oc-rsync byte-for-byte interop matrix only covers the sequential
path. If parallel dispatch is wired up in the future, the interop matrix
must be re-run in both modes, otherwise wire divergence introduced by
parallel dispatch would ship undetected.

## 5. Verdict

**Conditional PASS**.

For the current shipped binary the hypothesis holds vacuously:
`ParallelDeltaPipeline` is not wired into the receiver, so wire output is
always produced by `SequentialDeltaPipeline`, whose 1:1 dispatch matches
upstream `receiver.c:recv_files()`. All existing golden byte and interop
tests exercise this single code path. Internal unit tests on the parallel
path establish that the reorder buffer restores submission order before
hand-off, which is the right invariant if/when the parallel path is later
wired up.

For any future change that flips a CLI/config switch to actually route
the receiver through `ParallelDeltaPipeline`, this PASS becomes a FAIL
until the follow-ups in section 6 land. The infrastructure is sound, but
there is no end-to-end byte-equivalence assertion to back the claim once
the parallel path goes live.

## 6. Recommended follow-ups

These should be merged **before** any change that promotes
`ParallelDeltaPipeline` from "available infrastructure" to "default or
opt-in user-visible mode".

1. `crates/transfer/tests/parallel_pipeline_wire_parity.rs` - new
   integration test that:
   - Constructs a fixed batch of `DeltaWork` items (mix of whole-file and
     delta kinds, varied sizes including zero-byte and >block-size).
   - Runs the batch through `SequentialDeltaPipeline` and collects the
     `Vec<DeltaResult>`.
   - Runs the same batch through `ParallelDeltaPipeline::new(N)` for
     `N in [1, 2, 4, 8]` and collects results.
   - Asserts vector equality (same NDX order, same sequence, same
     literal/matched counts, same status) across all `N`. Closes G2.

2. `crates/engine/tests/force_insert_ordering.rs` - deterministic test
   that pins the head sequence (e.g. by holding a barrier on the worker
   that owns sequence 0) until `force_insert` is provably triggered, then
   inspects the delivered order. Either prove `force_insert` never
   violates the wire-order contract or document and gate its use behind
   a non-default config. Closes G3.

3. `crates/protocol/tests/golden_protocol_delta_parallel.rs` - golden
   byte test that captures the wire bytes emitted by the receiver-side
   acknowledgement / itemize stream for a small fixture (e.g. 16 files
   with mixed sizes) under both `SequentialDeltaPipeline` and
   `ParallelDeltaPipeline`, asserting byte-for-byte equality of the
   captured streams. This requires a CLI/config knob to select the
   pipeline; that knob landing is a prerequisite. Closes G1 + G2 at the
   wire layer.

4. `tools/ci/run_interop.sh` - add a parallel-dispatch matrix dimension
   once the CLI knob exists. Each version × scenario combination must
   pass with sequential **and** parallel pipelines. Closes G4.

5. `crates/transfer/src/receiver/mod.rs:232` - either wire
   `delta_pipeline` into the receiver transfer loop (with a config or
   CLI selector) or remove the dead field and its setter. The current
   "set but never read" state is a latent maintenance trap and obscures
   the actual code path being exercised. (Companion of follow-up 3.)

## 7. References

- `crates/engine/src/concurrent_delta/mod.rs:52-166` - existing Rayon
  Ordering Audit in the module-level docs. This document is the wire-
  facing companion to that internal audit.
- `target/interop/upstream-src/rsync-3.4.1/receiver.c` -
  `recv_files()` is the sequential per-file loop that
  `concurrent_delta` parallelizes. Upstream has no parallel equivalent;
  the wire-format invariant is "indistinguishable from sequential".
