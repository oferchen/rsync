# Dedicated `drain_parallel` Consumer Thread for `ReorderBuffer` (#1545)

## Summary

The `concurrent_delta` pipeline already runs the parallel drain on its own
thread: `DeltaConsumer::spawn` launches `delta-drain` (which calls
`WorkQueueReceiver::drain_parallel_into` inside `rayon::scope`) and feeds a
second `delta-reorder` thread that owns the `ReorderBuffer`. The reorder
thread receives streamed `DeltaResult` items over a bounded
`crossbeam_channel`, inserts each one, and forwards contiguous in-order runs
to the receiver pipeline over an `mpsc` channel.

The open question that issue #1545 captures is not "should we use
`drain_parallel`?" - we already do - but "is the current arrangement (two
threads, one bounded stream channel, `force_insert` fallback) the right
shape, or should we promote the reorder thread to a true dedicated consumer
that owns the drain call as well, eliminating the stream channel and the
`force_insert` deadlock-breaker?" This document inventories the production
code, proves the ordering contract is preserved by the
`flat_ndx`/`sequence` keys carried on every result, sketches the
alternative single-thread topology, and recommends deferring the change
until the `drain_parallel_alternatives` bench from #4214 lands.

## 1. Today's Pipeline

The production code lives in three files. All line numbers refer to
`origin/master` at the time of writing.

### 1.1 `consumer.rs:148-216` - `spawn_inner`

```text
WorkQueueReceiver
       |
       v  rx.drain_parallel_into(|w| strategy::dispatch(&w), stream_tx)
delta-drain thread (consumer.rs:161-166)
       |
       v  crossbeam_channel::bounded::<DeltaResult>(stream_capacity)
delta-reorder thread (consumer.rs:170-216)
       |
       v  ReorderBuffer::insert + drain_ready
       |
       v  mpsc::Sender<DeltaResult>
DeltaConsumer::iter / into_iter
```

Key facts:

- `stream_capacity = reorder_capacity.max(2 * rayon::current_num_threads())`
  (`consumer.rs:152-157`). The bounded channel is the only mechanism that
  applies backpressure from the reorder thread back to the rayon workers.
- The `delta-reorder` body is a `for result in stream_rx` loop
  (`consumer.rs:179`). It is **not** a `try_recv` poll; it is a blocking
  channel receive. The task description's "sequential `try_recv` loop"
  refers to a pre-#1681 implementation that no longer exists - the
  surviving smell is the `force_insert` fallback below, not the receive
  pattern.
- Each iteration calls `reorder.insert(result.sequence(), result.clone())`
  (`consumer.rs:182`). On `Err(CapacityExceeded)` the loop tries to drain
  ready items to free a slot, then retries.
- `consumer.rs:190-194` is the `force_insert` fallback: if no items are
  ready (the `next_expected` slot is empty) and the buffer is at capacity,
  the loop calls `reorder.force_insert(result.sequence(), result.clone())`
  to break the would-be deadlock. `ReorderBuffer::force_insert`
  (`reorder/mod.rs:502-536`) grows the ring when the sequence exceeds
  current capacity, side-stepping the bound the caller asked for.
- After insert, `consumer.rs:198-203` drains the new contiguous run into
  the output `mpsc` and continues.

### 1.2 `work_queue/drain.rs:136-155` - `drain_parallel_into`

```rust
pub fn drain_parallel_into<F, R>(self, f: F, tx: Sender<R>)
where
    F: Fn(DeltaWork) -> R + Send + Sync,
    R: Send,
{
    rayon::scope(|s| {
        for work in self.into_iter() {
            let f = &f;
            let tx = tx.clone();
            s.spawn(move |_| {
                let result = f(work);
                let _ = tx.send(result); // ignore receiver-dropped
            });
        }
    });
}
```

Properties carried over from the `Vec`-returning sibling
(`drain.rs:57-90`):

- One rayon task per `DeltaWork`. Worker order is non-deterministic; this
  is the whole reason `ReorderBuffer` exists.
- The bounded `tx` clone propagates backpressure to every worker
  individually (`tx.send` blocks each rayon task when the channel is
  full). The mutex-shard collection used by `drain_parallel` is not in
  play here because results are streamed through a channel, not collected
  into per-thread `Vec`s.
- The scope returns only after every spawned task has finished. The
  `delta-drain` thread therefore exits once the work queue is empty
  **and** every per-result `tx.send` has completed (or has been silently
  dropped after a receiver disconnect).

### 1.3 `reorder/mod.rs:297-323` - `insert`

The ring buffer is a `Box<[Option<T>]>` indexed by `(seq - next_expected +
head) % capacity` (`reorder/mod.rs:272-281`). `insert` rejects sequences
beyond the live window with `CapacityExceeded`. The adaptive policy
(`reorder/mod.rs:328-349`) may grow the ring on the spot when configured;
the production default for `DeltaConsumer::spawn` is fixed capacity.

`reorder/mod.rs:502-536` - `force_insert` - is the only path that grows a
fixed-capacity ring. It is invoked from exactly one site:
`consumer.rs:193`.

### 1.4 Ordering keys: `DeltaResult`

`types.rs:307-325`: `DeltaResult` carries `ndx: FileNdx` (the file-list
NDX) and `sequence: u64` (the pipeline reordering key). The producer
stamps `sequence` monotonically before enqueueing the matching
`DeltaWork` (`types.rs:262-278` for `DeltaWork::set_sequence` /
`with_sequence`). `ReorderBuffer` is keyed on `sequence`; `FileNdx`
travels along for downstream correlation but is never used for ordering.
There is no separate `DispatchedResult` type in production - the task
description's reference is to a planning placeholder; the on-the-wire
unit is `DeltaResult`.

## 2. Proposed: Single Dedicated Consumer Thread

The alternative collapses the two-thread split into one thread that owns
both the drain call and the `ReorderBuffer`:

```text
WorkQueueReceiver
       |
       v  consumer thread: rayon::scope { workers -> SPSC ring -> reorder }
ReorderBuffer (live, on the consumer thread's stack)
       |
       v  mpsc::Sender<DeltaResult>
DeltaConsumer::iter / into_iter
```

Concretely:

1. Move the body that currently lives in the `delta-reorder` thread up
   into a closure passed to `rayon::scope`. The closure runs on the
   thread that called `drain_parallel`, i.e. our dedicated consumer
   thread.
2. Workers no longer push into a `crossbeam_channel::bounded` of
   `DeltaResult`; they push into a lock-free SPSC ring (we already use
   `crossbeam_queue::ArrayQueue` in `transfer/src/pipeline/spsc.rs` for
   the same shape). Capacity stays at
   `max(reorder_capacity, 2 * num_threads)`.
3. The consumer thread spins on the SPSC ring inside the `rayon::scope`,
   inserting into the `ReorderBuffer` and draining ready runs to the
   output `mpsc`. When the scope returns, the producer side is closed
   and the consumer drains the tail and exits.

This is a refactor of the existing two-thread topology, not a new
feature. It does not change the public `DeltaConsumer` API. It does not
require a wire-protocol change.

### 2.1 Why bother

- Eliminates the `crossbeam_channel::bounded::<DeltaResult>(stream_capacity)`
  allocation and its per-result `clone` on the sender side. The reorder
  thread already pays the `result.clone()` on `consumer.rs:182` and
  `consumer.rs:193` because `insert` and `force_insert` take ownership
  while the in-flight `result` is needed for the retry branch; collapsing
  the topology lets us hand the value across by move.
- Removes one OS-level thread from the steady-state pipeline. On
  high-concurrency runs (16 rayon workers + drain + reorder + receiver
  pipeline = ~19 threads), shaving one thread is small but real.
- Pulls the `force_insert` deadlock fallback off the hot path. See
  section 5.

### 2.2 Why we might not bother

- The current topology demonstrably works (the `consumer.rs` tests at
  `consumer.rs:335-702` cover the gamut: order, small-capacity backpressure,
  bypass, drop-before-drain, large batch).
- The bounded `crossbeam_channel` is already a SPMC->SPSC funnel: many
  rayon producers, one reorder consumer. Replacing it with an
  `ArrayQueue` keeps the same shape; the win is the absence of the channel
  metadata, not a fundamentally different concurrency story.
- Pipeline overlap - delta computation continuing while previously
  completed results are reordered and written - is unchanged either way.
  Both topologies preserve it.

## 3. Ordering Correctness Proof

**Claim.** A consumer thread that drains `WorkQueueReceiver` via
`drain_parallel_into` (or via the proposed in-scope SPSC) and feeds every
result through `ReorderBuffer::insert` followed by `drain_ready` emits
results to the downstream `mpsc` in strictly increasing `sequence` order
starting from 0, regardless of the order in which rayon workers
complete.

**Proof.** Let `S = {s_0, s_1, ..., s_{n-1}}` be the sequence numbers the
producer stamps onto `DeltaWork` items, with `s_i = i` (the producer is
the only writer of `sequence` and stamps monotonically; see
`types.rs:262-278`).

`drain_parallel_into` applies `f: DeltaWork -> DeltaResult` and forwards
each result over `tx`. `f` is a pure dispatch (`strategy::dispatch`) that
preserves the `sequence` field; the field is `Copy` and is read by
`DeltaResult::sequence` (`types.rs:405-408`). Therefore the multiset of
sequences arriving at the consumer equals `S`.

The consumer inserts each arriving result into `ReorderBuffer` via
`insert(seq, item)`. By construction (`reorder/mod.rs:272-323`):

- If `seq == next_expected`, the slot at `head` is filled and a
  subsequent `next_in_order` call returns it, advancing
  `next_expected -> next_expected + 1`.
- If `seq > next_expected` and `seq - next_expected < capacity`, the slot
  is filled but `next_in_order` returns `None` until the gap closes.
- If `seq - next_expected >= capacity`, `insert` returns
  `CapacityExceeded`. The consumer either drains ready items first (the
  retry branch) or invokes `force_insert`, which grows the ring so the
  slot exists.
- Once `next_expected` is filled, `drain_ready` (`reorder/mod.rs:426-428`)
  yields items in strictly increasing `sequence` order until the next gap
  or the buffer empties.

Because the producer is the sole stamper and every stamp is unique, no
two `insert` calls collide on the same slot. Because every stamped
sequence eventually arrives (`drain_parallel_into` returns only after
every spawned task has finished, and every task does `tx.send`), every
slot is eventually filled. Therefore `next_expected` advances from 0 to
`n` and the consumer forwards exactly `S` in order. **QED.**

What `drain_parallel` is allowed to do - and what it actually does -
within one batch is shuffle the arrival order across workers. The proof
shows that shuffle is invisible downstream as long as every worker
preserves the `sequence` field, which `strategy::dispatch` does.

## 4. Backpressure

Two failure modes; both are continuous-flow, not all-or-nothing.

### 4.1 Drain rate > insert rate (workers fast, consumer slow)

- **Today.** The bounded `stream_capacity` channel fills; rayon worker
  `tx.send` calls block; rayon's work-stealing pool stops scheduling new
  delta tasks because every worker is parked on the send. The
  `WorkQueueReceiver` iterator stops being polled by the scope loop, so
  the bounded work queue (`crossbeam_channel::bounded(capacity)` in
  `bounded.rs:102`) fills; the upstream producer (generator/receiver) is
  blocked on `WorkQueueSender::send`. End-to-end backpressure reaches the
  wire.
- **Proposed.** The SPSC ring fills; rayon workers spin on
  `try_push` (or block on a parking primitive layered on top, matching
  the `transfer::pipeline::spsc` pattern). Same end-to-end effect.

### 4.2 Insert rate > drain rate (consumer fast, workers slow)

- **Today.** The stream channel is empty most of the time; the reorder
  thread blocks on `stream_rx` between bursts. `ReorderBuffer::count`
  stays small. No memory pressure. The output `mpsc` may fill if the
  downstream receiver pipeline is the bottleneck, blocking the reorder
  thread on `result_tx.send`. This is correct - it is precisely the
  signal the downstream consumer needs to slow the whole pipeline.
- **Proposed.** Identical. The change does not move where the
  bottleneck shows up; it only shrinks the buffering between the workers
  and the reorder buffer.

### 4.3 Pathological gap (HoL blocking)

A worker that takes much longer than its peers fills the ring with later
sequences while `next_expected` waits. Today this is exactly the
condition that triggers `force_insert` at `consumer.rs:193`. The
proposed topology has the same failure mode and needs the same
mitigation. See the `streaming-reorder-buffer.md` design for the
spill-to-tempfile resolution; this document does not re-litigate it.

## 5. Interaction with the `force_insert` Smell

The `force_insert` fallback at `consumer.rs:193` exists to keep the
pipeline alive when the bounded ring fills with non-`next_expected`
sequences and `drain_ready` returns nothing. It works by growing the
ring (`reorder/mod.rs:502-536`), which silently violates the capacity
bound the operator configured.

**Does the proposed topology remove the need for `force_insert`?** No.
The smell is in `ReorderBuffer`'s capacity contract under HoL pressure,
not in the channel between drain and reorder. Whether the consumer
thread reads from a `crossbeam_channel` or from a SPSC ring, the same
condition - one slow worker holding `next_expected` while N-1 fast
workers fill the ring with later sequences - hits `CapacityExceeded`
identically.

**Does the proposed topology make `force_insert` easier to remove?**
Marginally. With a single consumer thread, the retry loop simplifies: no
inter-thread cloning of the in-flight result is needed (today
`consumer.rs:182` and `consumer.rs:193` both pay `result.clone()`
because the loop owns the value across iterations). A clean removal
still requires either the spill-to-tempfile path
(`reorderbuffer-spill-to-tempfile.md`) or the streaming bound
(`streaming-reorder-buffer.md`) to land first; this refactor does not
change the underlying invariant.

The recommendation in the consumer-force-insert audit (track in project
notes) is to add a metric counter on every `force_insert` invocation
**before** any topology change, so we know whether the fallback fires in
production at all. That metric is independent of this document and
should land first regardless of the section 9 recommendation.

## 6. Throughput Hypothesis

The dedicated `drain_parallel_alternatives` bench planned in #4214
(see `lockfree-mpsc-drain-design.md` for the spec) targets the
`drain_parallel` <code>Vec</code>-returning sibling, not
`drain_parallel_into`. The comparable wins it predicts are:

- Eliminate the `Vec<Mutex<Vec<R>>>` shard fan-in.
- Replace with `crossbeam_channel::unbounded`.

For the streaming variant the analogous wins are smaller:

- Replace `crossbeam_channel::bounded(stream_capacity)` with an
  `ArrayQueue<DeltaResult>` (lock-free, fixed-size, no allocation per
  send).
- Save the per-result `Sender::clone` on every rayon `s.spawn` (line 144
  of `drain.rs`).

Hypothesis: the streaming topology spends most of its time in
`strategy::dispatch` (the per-file delta computation), not in the
channel hop. Replacing the channel with a SPSC ring should be within
noise on production-shaped workloads (file count >> thread count). The
win, if any, is visible only on the trivial-work benchmark
(`drain_parallel_benchmark.rs` already runs a 64-iteration rolling
hash). We need the `drain_parallel_alternatives` numbers to confirm or
refute this before committing to the refactor.

## 7. Proposed Metrics

Independently of the topology change, the operator-visible diagnostics
on `ReorderBuffer::metrics` are too coarse to localise reorder pauses.
Add:

- **Drain batch size histogram.** Buckets:
  `1, 2, 4, 8, 16, 32, 64, 128, 256, 512, >=1024`. Sample on every
  `drain_ready` call: count the items the iterator yields before
  returning `None`. Surfaces how clumpy the in-order delivery is; a
  histogram skewed toward 1 says workers are mostly arriving in order;
  a heavy tail says HoL pressure.
- **Drain pause histogram.** Buckets in microseconds:
  `<1, 1-10, 10-100, 100-1000, 1000-10000, >=10000`. Sample the wall
  time between consecutive `drain_ready` calls that return at least one
  item. Long pauses correlate with the conditions that trigger
  `force_insert`.
- **`force_insert` counter.** A single `u64` bumped on every invocation
  at `consumer.rs:193`. Surface alongside the existing `Metrics`
  struct. Tracking this is a prerequisite for removing the fallback;
  see section 5.

Implementation: extend the `Metrics` struct in `reorder/mod.rs:43-52`
with the new fields, add the bump points, and update the
metrics-and-bypass design doc to reference the new histograms. Wire the
histogram into the existing `engine` metrics surface; no new
dependencies.

## 8. Failure Modes

- **Producer shutdown mid-drain.** Today, the upstream `WorkQueueSender`
  is dropped while rayon workers are still draining queued items. The
  iterator returns `None` and the scope drains to completion. The
  proposed topology behaves identically - SPSC `try_push` from workers
  succeeds until the queue empties, then the scope returns and the
  consumer thread drains the tail. No items are lost.
- **Consumer panic.** Today, a panic in the reorder thread is
  propagated through `thread::JoinHandle::join` in
  `DeltaConsumer::join`. The output `mpsc::Receiver` sees a closed
  channel; downstream iterators terminate. The drain thread, holding the
  `stream_tx` clones, gets `SendError` on the next worker `tx.send` and
  rayon's scope unwinds. The proposed topology fuses the two threads:
  a panic during reorder unwinds the rayon scope directly (rayon
  propagates panics from workers, but the consumer-thread panic happens
  outside the scope spawn, so we need a `catch_unwind` around the
  in-scope reorder logic to convert it into a clean scope exit). The
  refactor must preserve the `DeltaConsumer::join` panic-propagation
  contract; this is testable.
- **`ReorderBuffer` overflow.** As discussed in section 5, the
  refactor does not change the overflow envelope. The same
  `force_insert` fallback applies; the same recommendation to add a
  counter applies; the same long-term fix (spill-to-tempfile) applies.
- **Worker panic.** Rayon catches panics in `s.spawn` tasks and
  re-raises them when the scope returns. Today, the drain thread
  observes the panic; the reorder thread sees a closed `stream_rx`
  and exits cleanly with a possibly partial result set. The proposed
  topology preserves this: the consumer thread observes the panic when
  the scope returns and propagates it via `JoinHandle::join`.

## 9. Recommendation

**Defer.** Wait for the `drain_parallel_alternatives` bench data from
#4214 to land, then revisit. Rationale:

- The current topology is correct and well-tested; the smell that #1545
  hangs its hat on (`force_insert` on `consumer.rs:193`) is independent
  of the drain/reorder thread split and survives any refactor that does
  not also address the `ReorderBuffer` capacity contract.
- The throughput hypothesis in section 6 says the streaming-channel
  hop is a small fraction of per-result work for production-shaped
  inputs. Without bench data we are guessing.
- The lowest-risk, highest-value change in this area is the
  `force_insert` counter and the two histograms in section 7. They are
  prerequisites to any later capacity-contract work and do not block on
  bench data.

If the bench data shows the streaming channel costs more than ~5% at
T = 16 on the 100K-item workload (the same threshold the
`lockfree-mpsc-drain-design` doc uses for the `Vec` variant), implement
the topology change per the sequencing below. Otherwise reject and
close #1545.

## 10. Implementation Sequencing (if accepted)

1. **Metric foundation.** Land the `force_insert` counter, drain batch
   size histogram, and drain pause histogram from section 7. Verify in
   nextest that the counters behave on the small-capacity test
   (`consumer.rs:493-515` exercises the deadlock path).
2. **SPSC primitive.** Extract the producer-side ring used in
   `transfer::pipeline::spsc` into a workspace-private helper that the
   `engine` crate can consume, or wrap it in a new module under
   `concurrent_delta/`. Keep `crossbeam_queue::ArrayQueue` as the
   storage. Tests: SPSC parity against `crossbeam_channel::bounded`
   semantics (send, recv, close, drain on disconnect).
3. **Wrap the reorder logic.** Move the body of the current
   `delta-reorder` closure (`consumer.rs:170-216`) into a free function
   that takes the SPSC consumer handle, the `result_tx`, and the
   reorder capacity. The function must wrap its insert/drain logic in
   `catch_unwind` to preserve the panic-propagation contract (see
   section 8).
4. **Single-thread spawn path.** Replace `spawn_inner` with a single
   `thread::Builder::new().name("delta-consumer").spawn` that:
   (a) constructs the SPSC pair, (b) enters `rayon::scope`, (c) inside
   the scope, spawns workers that push to the SPSC producer and runs the
   reorder function on the scope thread, (d) on scope exit, finalises
   the reorder buffer and drops `result_tx`. The bypass variant
   (`spawn_bypass`) takes the same path with `ReorderBuffer::passthrough`.
5. **Validation and rollout.** Run the full `consumer.rs` test module
   (including bypass tests at `consumer.rs:624-702`), the
   `drain_parallel_benchmark`, and a comparison run of the planned
   `drain_parallel_alternatives` bench to confirm the throughput
   delta matches the prediction that justified the refactor. If the
   bench shows a regression in any thread-count bucket, revert in step
   4 and leave the metrics from step 1 in place.

## 11. References

- `crates/engine/src/concurrent_delta/consumer.rs:148-216`
- `crates/engine/src/concurrent_delta/work_queue/drain.rs:57-155`
- `crates/engine/src/concurrent_delta/work_queue/bounded.rs:48-104`
- `crates/engine/src/concurrent_delta/reorder/mod.rs:272-536`
- `crates/engine/src/concurrent_delta/types.rs:262-408`
- `crates/engine/benches/drain_parallel_benchmark.rs`
- `docs/design/streaming-reorder-buffer.md` (HoL pressure, spill)
- `docs/design/reorderbuffer-metrics-and-bypass.md` (metrics surface)
- `docs/design/spsc-vs-mpsc-workqueue-bench.md` (channel selection)
- `docs/design/lockfree-mpsc-drain-design.md` (#1681 / #4214 bench plan)
