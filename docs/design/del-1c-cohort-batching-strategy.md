# Cohort batching strategy for the parallel DeleteEmitter consumer (DEL-1.c)

Status: Design (task DEL-1.c; resolves the open questions DEL-1.b section 9
deferred; foundation for DEL-2.b's batching wiring; verified by DEL-3's
wire-byte parity gate)
Audience: engine and transfer maintainers planning the parallel rework of
the `DeleteEmitter` drain, specifically the producer/consumer batching
policies that sit on top of DEL-1.b's re-ordering buffer.
Scope: how rayon producers group per-cohort deletion work, how the
single consumer drains, how `NDX_DEL_STATS` aggregation interacts with the
goodbye fence, and how failure isolation and scheduler choice fall out of
those decisions. Design only; no source changes in this branch.

Out of scope: the re-ordering buffer's data structure (DEL-1.b owns the
`SlotState`/`ReorderBuffer` shape), the wire-byte regression harness
implementation (DEL-3), the cross-cohort early-vs-late `write_del_stats`
question (upstream emits exactly one per transfer; see DEL-1.a section 4),
and the destination-side syscall ordering guarantee (covered by
`docs/design/parallel-deterministic-delete.md` and
`docs/design/delete-during-strict-order-gate.md`).

## 1. Cohort definition (re-affirmed from DEL-1.b)

DEL-1.b section 4.4 fixes the cohort concept and DEL-1.c keeps it
verbatim. The two-level cohort model:

- **Producer/consumer cohort: per-parent-directory.** One
  `CohortDeletionBatch` corresponds to one destination parent directory
  surfaced by `DirTraversalCursor::next_ready`
  (`crates/engine/src/delete/traversal.rs:118-160`). This is the unit a
  rayon producer owns end-to-end: claim slot, run every dispatch under
  that parent (including the ENOTEMPTY recursive fallback that
  `delete_dir_contents` would have made), produce one batch.
- **Goodbye-phase cohort: one per transfer.** Per DEL-1.a section 4,
  upstream emits exactly one `write_del_stats` call site per transfer
  (early xor late, never both). Every per-parent-dir batch the
  consumer drains folds into that single goodbye cohort's
  `DeleteStats`, which the unmodified generator-side writer at
  `crates/transfer/src/generator/transfer/goodbye.rs:79-110` serialises
  as the one `NDX_DEL_STATS` frame.

`cohort_idx` is a dense `u32` assigned by the receiver-side caller in
**pre-order traversal** of `DirTraversalCursor` (the same order the
single-emitter would have visited directories today). Pre-order is the
correct axis because:

- It matches the order `delete_in_dir`/`do_delete_pass` enumerate
  destination directories in upstream (`generator.c:272-347`,
  `generator.c:351-387`). Itemize log lines (the `*deleting   <path>`
  output the consumer emits over `MSG_INFO`) land in the same order a
  user running the sequential emitter would see, which is the byte-stream
  parity the DEL-3 gate checks.
- It is a property of the traversal cursor, not the producer scheduler,
  so it survives any rayon scheduling decision. The buffer's
  monotonic-`u32` invariant (DEL-1.b section 2.1) holds for free.
- Pre-order keeps the index dense (no holes from skipped subtrees) which
  matters because the buffer's ring uses `slot_at(idx) == slots[idx % N]`
  modulo indexing; a sparse `cohort_idx` would waste slots.

The assignment is single-threaded: the receiver-side caller iterates the
cursor once, materialising `(cohort_idx, dir_relative)` tuples into a
`Vec`, then hands that vector to `par_iter()` for producer dispatch.

## 2. Batching dimensions to design

DEL-1.b section 9 left five open questions that all collapse onto four
batching-policy axes. DEL-1.c resolves each one explicitly.

### 2.1 (a) Producer batch granularity

How much work does one rayon task own? The candidate granularities are:

- **One cohort per task** (1 task = 1 parent dir).
- **N cohorts per task** (coalesce successive small dirs into one batch).
- **All-cohorts-in-a-subtree per task** (one task per top-level subtree
  the traversal visits).

### 2.2 (b) Consumer drain batch size

When the consumer wakes on `not_empty`, does it drain:

- **One cohort per wake-up** (today's sequential analogue).
- **N contiguous Filled slots per wake-up** (amortise Condvar overhead).
- **All currently-Filled contiguous slots per wake-up** (greedy).

### 2.3 (c) `NDX_DEL_STATS` coalescing

The goodbye writer at `goodbye.rs:79-110` serialises exactly one frame
per goodbye cohort today. Producers could:

- **Emit per producer cohort** (one frame per parent dir; violates
  DEL-1.a section 5.2's one-frame-per-goodbye-cohort invariant).
- **Aggregate across multiple producer cohorts into one frame at the
  goodbye fence** (matches upstream's single-frame model).

### 2.4 (d) Backpressure drain batching

When a producer blocks on `not_full` at `tail - head >= N`, how much does
the consumer drain before signalling `not_full` and letting producers
resume? Candidates:

- **Drain one slot, signal, resume** (today's natural Condvar semantics;
  causes ping-pong at full buffer).
- **Drain to a low-water mark, then signal** (batched wake of producers).
- **Drain to empty, then signal** (long stalls on uneven cohorts).

## 3. Decisions per dimension

### 3.1 (a) Producer batch granularity: **1 cohort per rayon task**

Decision: each rayon task processes exactly one per-parent-dir cohort.
DEL-1.b's per-dir `CohortDeletionBatch` is the unit of work.

Rationale:

- **Matches upstream's 1-cohort-per-recurse model.** `delete_in_dir`
  (`generator.c:272-347`) processes one destination directory at a
  time, returning before the caller advances to the next. A 1:1 mapping
  preserves that decomposition and makes the wire-byte parity proof
  trivial: every byte a producer emits corresponds to one upstream
  `delete_in_dir` call's worth of work.
- **No intra-task synchronisation needed.** A single task owns one
  parent dir end-to-end. There is no shared mutable state across
  dispatches inside a task, so the local batch builder is a plain
  `Vec<MsgDeletedFrame>` and a `DeleteStats` accumulator on the task
  stack. No locks, no atomics, no `Arc`.
- **Failure isolation is per-cohort by construction.** A panic in
  one task affects exactly one cohort's slot. DEL-1.b section 6.1's
  panic-guard recovery applies directly with no extra bookkeeping.
- **Coalescing is rejected.** The "N small dirs per task" coalescing
  variant from DEL-1.b section 9.1 looks tempting for deep
  `node_modules`-style trees, but it creates two problems: (1)
  per-cohort failure isolation collapses (one panic loses N
  directories' frames), and (2) the `cohort_idx -> slot` mapping is no
  longer dense, so the ring wastes slots when a coalesced task
  produces multiple cohort indices' worth of work into one slot.
  Producer-side overhead is small enough (one `claim_cohort` + one
  `produce_batch` per cohort, both under a single `Mutex<()>` for one
  state transition each) that the win does not justify the complexity.
- **Subtree-per-task is rejected.** Subtree granularity defeats the
  whole point of parallelism on shallow wide trees, because the
  top-level subtree count is the parallelism cap. On a typical 1-level
  destination, the entire transfer would run on one rayon worker.

### 3.2 (b) Consumer drain batch size: **drain up to 8 contiguous Filled slots per wake-up**

Decision: when the consumer wakes on `not_empty`, it drains all
contiguous `Filled` slots starting at `head` up to a hard cap of **N = 8**
slots per wake-up. After draining, it signals `not_full` once and emits
all collected `MsgDeletedFrame`s outside the buffer lock.

Rationale:

- **Amortises Condvar overhead.** The dominant cost in the consumer's
  hot path is the `Condvar::wait` / `Condvar::notify_one` round-trip
  (each is one syscall on Linux via futex). At 100k+ cohorts the
  per-cohort wake-up dominates. Draining 8 cohorts per wake-up cuts
  the Condvar traffic 8x without changing the work-per-cohort.
- **Bounded at 8 to limit head-of-line blocking on slow cohorts.**
  Larger drain batches inflate the latency between a slow cohort's
  completion and its `MSG_DELETED` frames hitting the wire, because
  the consumer holds those frames in a local buffer until it returns
  to the outer wire-emission loop. At N = 8 the worst-case batch is
  ~80 KiB (per DEL-1.b section 4.1's per-slot estimate), small enough
  to keep emission latency low. At N = 64 (the full ring) the worst
  case is 640 KiB held in the consumer-local buffer; that is large
  enough to delay backpressure responsiveness on a saturating
  writer.
- **Why 8 specifically?** It is the smallest power-of-two that
  amortises the Condvar cost below 15% of total consumer CPU at the
  100k-cohort projection point in section 7. Smaller (4) leaves
  too much overhead; larger (16, 32) gives diminishing returns on
  Condvar amortisation while linearly inflating head-of-line latency.
  The cap is a compile-time constant
  (`CONSUMER_DRAIN_BATCH_CAP: usize = 8`); a runtime flag is out of
  scope for DEL-1.c and deferred to DEL-3 once benches inform whether
  per-deployment tuning is warranted.
- **Contiguous-only.** The drain rule from DEL-1.b section 2.3 (strict
  `cohort_idx` order, no skipping) carries through unchanged: the
  consumer stops at the first non-`Filled` slot, even if later slots
  are `Filled`. This preserves byte-for-byte parity with the
  sequential baseline.

### 3.3 (c) `NDX_DEL_STATS` coalescing: **1 frame per goodbye cohort, fold per-producer-cohort counters into a shared `DeleteStats`**

Decision: producer cohorts contribute per-kind counters that the consumer
folds into a single `DeleteStats` accumulator. The accumulator is the same
field the goodbye-phase writer at
`crates/transfer/src/generator/transfer/goodbye.rs:79-110` reads from
today. The `NDX_DEL_STATS` frame is **not** emitted by the consumer at
all; the goodbye writer remains the sole emitter, unchanged.

Rationale:

- **Preserves the strictest wire invariant.** DEL-1.a section 7 fixes
  "exactly one `NDX_DEL_STATS` frame per cohort, carrying exactly five
  varints, between the last `MSG_DELETED` and the closing
  `NDX_DONE`". The frame is owned by the generator goodbye writer;
  DEL-1.c does not move the emission point.
- **Producer cohorts are an internal subdivision.** DEL-1.b
  section 4.4 explicitly notes that per-parent-dir cohorts fold into
  the single upstream goodbye cohort. The folding is commutative
  (DEL-1.a section 5.2: per-kind counters are summed into a single
  global `DeleteStats`), so the order in which producer cohorts
  contribute does not affect the final five varint values.
- **The fold is single-threaded inside the consumer.** Every
  `fold_batch` call happens on the consumer thread, so no atomics or
  inner mutex is needed on `DeleteStats`. The receiver-side caller
  consults the folded stats only after `consumer_handle.join()`
  returns, by which time every producer cohort has been folded.
- **Per-cohort frames would over-count.** Per DEL-1.a section 6.2,
  upstream's `read_del_stats` accumulates additively
  (`main.c:243-246`). Emitting one frame per producer cohort would
  multiply the receiver-side totals by the cohort count. This is the
  reason "emit per producer cohort" is rejected outright; it is a
  wire-protocol bug, not a tuning trade-off.

### 3.4 (d) Backpressure drain batching: **drain to half-empty before signalling `not_full`**

Decision: when `claim_cohort` blocks producers on `not_full`, the
consumer drains slots until `tail - head <= N / 2` (32 slots free with
the N = 64 ring) before signalling `not_full`. The signal then wakes
**all** blocked producers via `Condvar::notify_all`, not just one.

Rationale:

- **Avoids producer-consumer ping-pong at full buffer.** If the
  consumer signalled `not_full` after every single slot eviction,
  producers and consumer would trade the `Mutex<()>` back and forth
  with one slot freed each round. Draining to half-empty lets the
  consumer batch its work and producers batch their wake-ups.
- **`notify_all` is correct here, not `notify_one`.** Producers
  blocked on `not_full` are not interchangeable: each is waiting on
  its specific `cohort_idx`, and the slot is `(cohort_idx % N)`.
  Waking only one might wake the wrong producer (the one whose slot
  is still occupied by a later cohort). Waking all is O(P) wake-ups
  but P is bounded by `rayon::current_num_threads()`, so the cost is
  trivial. Producers re-check the `tail - head < N` predicate on
  wake-up and re-block if their specific slot is still occupied.
- **The N / 2 threshold matches the half-low-water-mark idiom used by
  the delta reorder buffer.** See `streaming-reorder-buffer.md` for
  the prior art; the same trade-off (batched wake vs latency)
  applies here.
- **No starvation.** Because `cohort_idx` is dense and the consumer
  drains in strict order, every producer's slot eventually frees as
  the head advances. The half-empty threshold delays wake-ups but
  does not cap throughput: the consumer is still processing at full
  rate, and producers only block at `tail - head >= N`, which is the
  rare path (the common path is `claim_cohort` returns immediately
  because the consumer is keeping up).

## 4. INC_RECURSE segment boundary

DEL-1.a section 4 establishes that INC_RECURSE does **not** subdivide
the upstream goodbye cohort: `write_del_stats` is called once per
transfer, after the whole flist loop completes, with totals across
every per-segment delete pass. But the producer-side cohort index space
must still survive segment-by-segment construction, because the
receiver-side caller builds `dirs_in_traversal_order` as the flist
arrives - it cannot wait for the final segment before assigning
`cohort_idx` to the first segment's directories (that would defeat
parallelism on long pipelines).

Decision: `cohort_idx` is a **2-tuple `(segment_idx, dir_idx_in_segment)`**
flattened to a single `u32` via `(segment_idx as u64 * SEGMENT_STRIDE +
dir_idx_in_segment as u64) as u32`, where `SEGMENT_STRIDE = 1 << 20`
(1 Mi cohorts per segment). The flattening produces a dense `u32` per
segment with gaps between segments that the ring tolerates as long as
the consumer's drain order remains monotonic.

Rationale:

- **Per-segment density is preserved.** Within a segment,
  `dir_idx_in_segment` is a contiguous pre-order counter, so the ring's
  modulo indexing works correctly. The buffer never sees segment
  boundaries; it only sees the flattened `u32`.
- **Cross-segment ordering is the segment-order itself.** The receiver
  processes one segment at a time (see
  `crates/transfer/src/receiver/transfer/phases.rs`), so all
  `cohort_idx` values for segment N are drained before segment N+1's
  first `claim_cohort` arrives. The ring is always empty at segment
  boundaries; the gap in the index space between
  `(N, last_dir) -> (N+1, 0)` is jumped over by a single explicit
  `buffer.advance_head_to(next_segment_base)` call the receiver makes
  at end-of-segment, which is a no-op if the consumer has already
  drained the segment (the common case) and a brief blocking wait if
  it has not.
- **`SEGMENT_STRIDE = 1 << 20` is a safety margin.** Real INC_RECURSE
  segments are bounded by the upstream flist-segment cap of ~65 535
  directories per segment (see `flist.c`); a 1 Mi stride leaves 16x
  headroom and keeps `segment_idx` well within `u32` range up to
  4096 segments, which is enough for any realistic transfer.
- **Why not separate `segment_idx` and `dir_idx_in_segment` as a struct
  key?** A struct key would force the buffer's slot identity to be a
  composite, which complicates `slots[idx % N]` indexing. A flattened
  `u32` keeps DEL-1.b's `ReorderBuffer` shape unchanged.
- **The goodbye cohort still folds across every segment.** The
  consumer's `DeleteStats` accumulator runs for the whole transfer's
  duration (DEL-1.b section 3.2: the consumer loop runs until
  `producers_done && buffer.is_empty()`), so per-segment cohorts all
  contribute to the single goodbye-phase `NDX_DEL_STATS` frame, exactly
  as upstream does it.

## 5. Work-stealing vs producer-affinity

Decision: **use rayon's default work-stealing scheduler with no producer
pinning**. The receiver hands the pre-built `Vec<(cohort_idx,
dir_relative)>` to `par_iter()` and lets rayon distribute.

Rationale:

- **Cohort sizes are uneven.** A typical destination tree has a long
  tail of small directories and a few large ones. Pinning producers
  to contiguous index ranges would starve workers that drew empty
  subtrees while one worker chewed through a deep
  recursive-fallback. Work-stealing balances naturally.
- **Cohort affinity is not required for correctness.** The buffer's
  ring tolerates non-contiguous fills (DEL-1.b section 3.4): the
  consumer just blocks on `not_empty` until `head`'s specific slot
  fills. Out-of-order fills cost one extra Condvar wait per stall
  but do not corrupt the wire.
- **Producer pinning would break panic isolation.** A pinned worker
  that panics on one cohort tends to panic on subsequent cohorts in
  the same range (because the failure is often
  destination-filesystem-state-correlated). Work-stealing distributes
  the panicked cohorts across workers and lets surviving workers
  continue.
- **DEL-1.b section 9.4's worry about pathological `claim_cohort`
  blocking is empirically bounded.** The worst case is the ring fills
  in the wrong order (all of slots `[1..N]` arrive before slot `[0]`).
  Even then, `claim_cohort` only blocks new arrivals when `tail - head
  >= N`; the in-flight producers complete their batches without
  blocking, and the next `claim_cohort` blocks until the consumer
  unblocks them, which is bounded by the slow producer for cohort 0.
  This is the same head-of-line blocking the sequential emitter
  already has, just expressed as a wait instead of a serial dispatch.
  No throughput regression vs sequential.
- **Implication for the buffer.** Producers may write to non-contiguous
  cohort slots; the buffer's ring head-tail tracking handles this
  because slot state is per-index, not per-position. The
  `is_empty()` peek used by the consumer's exit check
  (`buffer.is_empty() && every_producer_finished()` from DEL-1.b
  section 3.2) tests `head == tail`, which is correct even with
  non-contiguous fills - `tail` advances on every `claim_cohort`, so
  if every producer has joined and `head == tail`, every slot the
  ring ever saw is drained.

## 6. Failure isolation between coalesced cohorts

Because section 3.1 rejected producer-side coalescing, the
failure-isolation concern from DEL-1.b section 9.5 narrows to **how the
consumer handles a panicked cohort when batching the drain (section 3.2,
N = 8)**.

Decision: the consumer checks `producer_panicked: AtomicBool` (DEL-1.b
section 6.1) **per cohort drained in the batch**, not just at the start
of the wake-up. The first panicked cohort encountered terminates the
drain batch: the consumer emits the surviving cohorts' frames that came
before it, observes the panic flag, signals `shutdown`, and exits the
consumer loop with the panic error surfaced to the caller via the
`consumer_handle.join()` path.

Rationale:

- **Per DEL-1.b section 6.1, a producer panic places an empty
  `CohortDeletionBatch` in the slot and sets `producer_panicked`.**
  The empty batch is safely emittable (zero `MSG_DELETED`, zero stats
  contribution), but the panic signals that the transfer is in an
  inconsistent state and must fail with `RERR_PARTIAL` (23) to match
  upstream's `delete_item -> rsyserr + cleanup_and_exit` behaviour
  (`delete.c:201-205`).
- **The check is per-cohort, not per-wake-up.** A drain batch of 8
  cohorts could include both pre-panic and post-panic cohorts (the
  panic flag is set after the panicked cohort is filled, but other
  producers may complete in parallel). Bailing only at wake-up
  start would let post-panic cohort frames hit the wire, which
  contradicts DEL-1.b section 6.1's "panicked cohort is lost on
  wire" semantics and risks divergence if the surviving producers
  for later cohorts made syscalls based on filesystem state that
  the panicked cohort would have changed.
- **Bail at the first panicked cohort, not the last.** Stopping at
  the first preserves the strict cohort-order wire-byte invariant:
  every frame emitted up to and including the panicked cohort is
  exactly what the sequential emitter would have emitted up to the
  panic point. Cohorts after the panic are dropped, which is
  upstream-compatible (a panicked `delete_item` aborts the transfer
  before subsequent dispatches; upstream loses them too).
- **The empty batch for the panicked cohort itself is emitted as
  zero frames, not skipped.** The slot is `Filled(empty_batch)`,
  not `InFlight`, so the consumer drains it (zero frames, zero
  stats), then exits. This satisfies the buffer's drain invariant
  (every Filled slot is consumed exactly once) without violating the
  wire (an empty batch contributes nothing to the byte stream).
- **Shutdown semantics propagate via DEL-1.b section 6.2.** The
  consumer sets `shutdown: AtomicBool` and signals `not_full` once on
  exit; remaining producers wake, observe `shutdown`, and return
  without producing. The receiver-side caller's `rayon::scope` join
  observes the panic propagation and propagates the original panic
  upward.

## 7. Performance projections

Baseline (today's sequential emitter):

- Per cohort: 1 implicit "wake-up" (function call), 1 frame-emit batch
  per cohort, 0 Condvar traffic.
- Bottleneck: the per-syscall wait for `unlink`/`rmdir`/`remove_dir_all`
  serialised on the destination filesystem.

With DEL-1.b's buffer and DEL-1.c's batching:

- Per cohort: 1 `claim_cohort` (lock, state transition, atomic update),
  1 `produce_batch` (lock, state transition, `Condvar::notify_one`),
  1 batched drain (lock, state transition, `Condvar::wait` for the
  whole batch).
- Bottleneck: producer parallelism on the destination filesystem
  syscalls, which is the win.

Cost-model estimate at 100k cohorts on a 16-core host with the syscall
work running on ~12 workers (rayon defaults), unbatched drain (N = 1):

- Condvar wake-ups: ~100 000 (one per cohort).
- Per-wake-up cost on Linux (futex round-trip): ~1.5 us under
  contention.
- Total Condvar overhead: ~150 ms of consumer CPU.

With batched drain at N = 8:

- Condvar wake-ups: ~12 500 (one per 8 cohorts).
- Total Condvar overhead: ~19 ms of consumer CPU.
- Consumer CPU saved: ~131 ms (87% reduction in Condvar overhead).

At 1M cohorts the saving scales linearly to ~1.3 s of consumer CPU.
This is the dominant win: the syscalls themselves cost on the order of
microseconds each (already parallelised across workers), so the
serialised consumer's Condvar overhead is what closes the gap to a
sequential baseline.

Estimated CPU reduction at the 100k+ cohort range: **30-50% of total
consumer CPU**, driven primarily by Condvar amortisation. Throughput
(wire bytes per second) is **unchanged**: the wire bytes are byte-for-byte
identical to the sequential baseline (DEL-3 gate), and the bottleneck on
the wire side is the socket writer, not the consumer's batching.

Cost model assumptions:

- Cohorts arrive faster than the consumer drains in the high-cohort-count
  regime (otherwise the consumer is idle and Condvar cost is moot).
  This is true when destination-side syscalls run in parallel across
  workers but the consumer's wire emission is serial.
- The N = 8 cap is small enough that the consumer's per-batch
  allocation (a `Vec<MsgDeletedFrame>` with up to ~800 entries at
  100 deletions per cohort) stays in the L2 cache. At N = 32 or 64
  the batch spills to L3, eroding the amortisation benefit.
- The `Condvar::notify_one` cost is a single futex syscall; on
  uncontended wake-ups it can avoid the syscall entirely (futex fast
  path), so the model above is the **worst case** at high
  contention. Real workloads see less.

No throughput change is claimed beyond the destination-side parallelism
DEL-1.b already established; DEL-1.c is purely a dispatch-overhead
optimisation.

## 8. Test plan (drives DEL-2.b implementation tests)

DEL-2.b implements the batching wiring on top of DEL-2.a's buffer. The
tests below are the acceptance criteria DEL-2.b must satisfy, in addition
to DEL-3's wire-byte parity gate.

### 8.1 Unit: panic at cohort N, consumer bails exactly at N

Test name: `parallel_consumer_bails_at_panicked_cohort`.

Setup: spawn a producer scope with cohorts 0..32. Make cohort 15's
producer panic via a `should_panic` mock dispatch. All other producers
succeed normally.

Assertions:

1. The consumer emits exactly cohorts 0..15's frames (cohort 15 itself
   contributes zero frames because the panic guard publishes an empty
   batch).
2. The consumer does **not** emit any frame for cohort 16 or later,
   even if those slots are `Filled` by surviving producers when the
   panic is observed.
3. `producer_panicked.load(Acquire)` is `true` at consumer exit.
4. The receiver-side caller observes the panic via `join()` and
   returns `RERR_PARTIAL` (23).

This proves the section 6 decision (per-cohort panic check, bail at
first, not at wake-up start).

### 8.2 Unit: 1000-cohort race with random fill order, drain order strict

Test name: `parallel_consumer_drains_in_strict_cohort_order`.

Setup: spawn 1000 producers that each `claim_cohort` immediately, then
sleep for a random duration before calling `produce_batch` with a single
distinct `MsgDeletedFrame`. Use a fixed PRNG seed for repeatability
across CI runs.

Assertions:

1. The consumer's emitted frame sequence is `frame_0, frame_1, ...,
   frame_999` regardless of which producer finished first.
2. The total wall-clock for the test is bounded by the slowest single
   producer's sleep plus consumer-loop overhead, **not** by the sum of
   sleeps (proves parallelism).
3. No Condvar deadlock: the test completes within a 10 s timeout under
   `RAYON_NUM_THREADS` in `{1, 2, 4, 8, 16}`.

This proves the strict-order drain rule (DEL-1.b section 2.3) survives
the batched drain (section 3.2) and the random fill order section 5
expects.

### 8.3 Wire-byte: parallel + batched vs sequential, byte-for-byte identical

Test name: `delete_wire_parity_batched_drain`.

Setup: extends DEL-3's wire-byte parity harness
(`crates/engine/tests/delete_wire_parity.rs`) with the batched drain
enabled. Uses the same golden capture from
`crates/engine/tests/golden/delete_wire/sequential.bin`.

Assertions:

1. With `parallel-delete-consumer` enabled and
   `CONSUMER_DRAIN_BATCH_CAP = 8`, the captured wire stream equals the
   sequential golden byte-for-byte.
2. Same check with `CONSUMER_DRAIN_BATCH_CAP` set to 1, 8, and 64 via
   a test-only override; all three produce the same byte stream.
   Proves the drain batch size is a pure dispatch-overhead knob with
   zero wire effect.
3. `proptest` variant: random per-dir cohort shapes (varying entry
   counts, kinds, depth, ENOTEMPTY recursion presence) all match the
   sequential capture.

This is the DEL-3 gate's batched-drain variant; it must pass before
DEL-1.c's design ships.

### 8.4 Bench: cohort throughput at 10k / 100k / 1M cohorts

Test name: `bench_parallel_consumer_throughput`.

Setup: a Criterion benchmark in
`crates/engine/benches/delete_parallel.rs` that exercises the consumer
at three cohort counts (10 000, 100 000, 1 000 000) with two
configurations:

- Unbatched (`CONSUMER_DRAIN_BATCH_CAP = 1`).
- Batched at the design default (`CONSUMER_DRAIN_BATCH_CAP = 8`).

Assertions (informational, not asserted as fail/pass in CI):

1. At 10k cohorts, batched-vs-unbatched is within noise (Condvar
   overhead is small relative to syscall cost at low counts).
2. At 100k cohorts, batched is 30-50% faster on consumer-CPU time
   (matches the section 7 projection).
3. At 1M cohorts, batched is 80-90% faster on consumer-CPU time, and
   the consumer is no longer the bottleneck (producer-side syscall
   parallelism becomes the binding constraint).

These numbers gate the design's "30-50% CPU reduction at 100k+" claim.
If the bench shows < 20% reduction the cap N is re-tuned in a follow-up
issue before DEL-3 promotes the feature default-on.

## 9. Cross-references

- DEL-1.a upstream-ordering audit (the strictest invariant this
  design preserves): `docs/design/del-1a-upstream-ordering-audit.md`.
- DEL-1.b re-ordering buffer spec (the buffer shape this design
  layers batching on top of): `docs/design/del-1b-reordering-buffer.md`.
- DEL-2.a (forthcoming): implements `ReorderBuffer` per DEL-1.b.
- DEL-2.b (forthcoming): implements the batching wiring per this
  design; depends on DEL-2.a buffer impl. The unit tests in
  section 8.1 and 8.2 are DEL-2.b's acceptance criteria.
- DEL-3 (forthcoming): wire-byte parity gate; the test in section 8.3
  is DEL-3's batched-drain variant. DEL-3 is the gate that proves
  this design correct end-to-end.
- DDP design (parent context for the single-emitter invariant):
  `docs/design/parallel-deterministic-delete.md`.
- Strict-order gate (the constraint the parallel path eventually
  retires): `docs/design/delete-during-strict-order-gate.md`.
- Existing reorder-buffer prior art for half-low-water-mark drain:
  `docs/design/streaming-reorder-buffer.md`.
- Memory note on the existing reorder default
  (capacity-tuning context): `project_reorder_capacity_hard_default.md`.
- Source surface this design plugs into:
  - `crates/transfer/src/receiver/directory/deletion.rs` (the
    plug-in point DEL-1.b section 8 calls out).
  - `crates/engine/src/delete/emitter/mod.rs` (today's sequential
    emitter).
  - `crates/engine/src/delete/traversal.rs` (the source of
    `cohort_idx` via `DirTraversalCursor::next_ready`).
  - `crates/engine/src/delete/cohort_index.rs` (the cohort-index
    type the receiver-side caller materialises into the
    `Vec<(cohort_idx, dir_relative)>` for `par_iter`).
  - `crates/transfer/src/generator/transfer/goodbye.rs` (the
    untouched `NDX_DEL_STATS` writer that reads the consumer-folded
    `DeleteStats`).
- Upstream references (verbatim from DEL-1.a):
  - `delete.c:130-225` (`delete_item`),
  - `delete.c:48-122` (`delete_dir_contents`),
  - `generator.c:272-347` (`delete_in_dir`),
  - `generator.c:351-387` (`do_delete_pass`),
  - `main.c:225-247` (`write_del_stats` / `read_del_stats`).
