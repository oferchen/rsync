# Streaming ReorderBuffer with bounded memory

Tracking issue: oc-rsync task #1565. Branch:
`docs/streaming-reorder-buffer-1565`.

## Scope

Design note for a streaming reorder buffer that bounds peak memory use even
under pathological head-of-line (HoL) blocking. The note inventories the two
reorder-buffer implementations that ship today (#1566 was the bounded-window
landing), characterises the memory growth pathology that defeats the existing
bound, surveys three streaming alternatives (spill-to-tempfile, drop-and-replay,
backpressure-to-sender), recommends a path forward, and lists open questions.

This is design only. No code changes are proposed in this branch.
Implementation issues are tracked separately (#1884 spill-to-tempfile, #1885
metrics, #1886 bypass).

The two reorder buffers and the surrounding pipeline are described in detail in
`docs/architecture/reorder-buffer.md` (HoL semantics, audit #1883) and
`docs/design/reorderbuffer-metrics-and-bypass.md` (#1885 / #1886). This note
focuses specifically on the streaming variant: how to keep the memory bound
hard even when the head slot stalls long enough for every successor in the
transfer to complete.

## Source citations

All paths repository-relative.

- `crates/transfer/src/reorder_buffer.rs:55` -
  `BoundedReorderBuffer<T>`, the BTreeMap-backed sliding-window variant
  exposed at the transfer-crate boundary. Returns `BackpressureError` when
  inserts exceed the window; no spill, no force-insert.
- `crates/transfer/src/reorder_buffer.rs:79` - `BackpressureError`,
  carrying the rejected sequence and the current admission window.
- `crates/transfer/src/reorder_buffer.rs:129` - `insert`, the only
  public mutator. Out-of-window inserts return `Err(BackpressureError)`.
- `crates/engine/src/concurrent_delta/reorder.rs:65` - `ReorderBuffer<T>`,
  the production ring-backed buffer used by the parallel delta pipeline.
  O(1) insert and O(1) drain.
- `crates/engine/src/concurrent_delta/reorder.rs:167` - `insert`, returns
  `Err(CapacityExceeded)` when the offset from `next_expected` exceeds the
  ring capacity.
- `crates/engine/src/concurrent_delta/reorder.rs:262` - `next_in_order`,
  yields `Some(T)` only when the head slot is occupied.
- `crates/engine/src/concurrent_delta/reorder.rs:334` - `force_insert`,
  the deadlock-break path that grows the ring without bound when
  `next_expected` is the missing item and the window is full.
- `crates/engine/src/concurrent_delta/consumer.rs:129` -
  `DeltaConsumer::spawn`, instantiates the ring with capacity
  `reorder_capacity` and runs the `delta-drain` and `delta-reorder`
  background threads.
- `crates/engine/src/concurrent_delta/consumer.rs:151-188` - the
  `delta-reorder` loop: drain on full-and-occupied, force-insert on
  full-and-empty-head.
- `crates/engine/src/concurrent_delta/work_queue/capacity.rs:8` -
  `CAPACITY_MULTIPLIER = 2`. Default reorder capacity is
  `2 * rayon::current_num_threads()`.
- `crates/engine/src/concurrent_delta/adaptive.rs:22` -
  `AdaptiveCapacityPolicy`, the opt-in grow / shrink policy.
- `crates/transfer/src/delta_pipeline.rs:209-212` -
  `ParallelDeltaPipeline::new` capacity sizing.
- `target/interop/upstream-src/rsync-3.4.1/receiver.c:522` -
  `recv_files`, upstream's strictly sequential receive loop.

## 1. Existing implementation summary

oc-rsync ships two complementary reorder buffers. Both honour the same
public contract (strict in-order delivery from `next_expected` onward) but
sit at different layers and choose different policies when they fill.

### 1.1 `engine::concurrent_delta::ReorderBuffer` (ring buffer, primary path)

Closed under #1566. The production implementation used by
`ParallelDeltaPipeline`.

- Storage: `Box<[Option<T>]>` indexed by
  `(sequence - next_expected + head) mod capacity`
  (`crates/engine/src/concurrent_delta/reorder.rs:142-151`). Pre-allocated,
  O(1) insert and O(1) drain.
- Default capacity: `2 * rayon::current_num_threads()` slots, supplied by
  `ParallelDeltaPipeline::new` and matching the work-queue multiplier.
- Owner: `delta-reorder` background thread spawned by `DeltaConsumer::spawn`.
- Producer: rayon workers feeding a bounded `crossbeam_channel` of
  `DeltaResult` items.
- Full-buffer policy:
  1. If the head slot is occupied, drain the contiguous run and retry the
     insert.
  2. If the head slot is empty (the slow file is still in flight), call
     `ReorderBuffer::force_insert`
     (`crates/engine/src/concurrent_delta/reorder.rs:334`), which grows the
     ring to fit the new item.
- Adaptive sizing: opt-in via `ReorderBuffer::with_adaptive_policy`
  (`reorder.rs:131-136`). Default `DeltaConsumer::spawn` does not attach a
  policy.
- Termination: `ReorderBuffer::finish` (`reorder.rs:408-425`) panics if any
  items remain buffered when the producer has closed, surfacing upstream
  sequence gaps (a worker dropped a `DeltaWork` item without producing a
  `DeltaResult`).

### 1.2 `transfer::reorder_buffer::BoundedReorderBuffer` (BTreeMap, alt)

A sibling implementation at the `transfer` crate boundary
(`crates/transfer/src/reorder_buffer.rs:55`). Backed by `BTreeMap<u64, T>`
with an explicit acceptance window
`[next_expected, next_expected + window_size)`. `DEFAULT_WINDOW_SIZE = 64`
(`crates/transfer/src/reorder_buffer.rs:26`).

- Storage: `BTreeMap` (O(log n) insert and lookup, O(window) drain).
- Owner: caller-supplied. Designed for layers that hold the producer side and
  can throttle.
- Full-buffer policy: out-of-window inserts return
  `Err(BackpressureError)` carrying the rejected sequence and the current
  window bounds. No grow, no spill, no force-insert.
- Termination: structural - the caller drops the buffer when the producer
  has closed.

The two implementations exist for separate ownership stories. The engine ring
sits inside the consumer thread that owns it (the `delta-reorder` thread is
the sole drainer and the sole inserter, modulo the `crossbeam_channel`
hand-off). `BoundedReorderBuffer` is a value-typed building block whose
backpressure signal must be honoured by the caller, with no implicit grow.

### 1.3 What "bounded" means today

For both buffers, "bounded" is a window over the *acceptance* range:
sequences within `[next_expected, next_expected + capacity)` are admitted,
sequences outside it are rejected. The number of buffered items is bounded
by `capacity` (or `window_size`).

That bound is *soft* on the engine ring because of `force_insert`. When the
head slot stalls long enough for `capacity - 1` successors to arrive, the
next admission cannot drain anything, force-insert grows the ring to fit it,
and the bound silently relaxes. In the worst case the ring grows to hold
every completed successor in the transfer (`total_files - 1` items if
sequence 0 stalls and every other file completes).

The bound is *hard* on `BoundedReorderBuffer` because there is no
force-insert. Out-of-window inserts always fail; the caller must drain or
wait. The cost is that the producer must implement backpressure correctly,
which is exactly what the engine ring tries to avoid by handling the
deadlock internally.

## 2. Memory growth pathology under stalled successors

The HoL behaviour is documented in detail in
`docs/architecture/reorder-buffer.md` (#1883). This section restates the
relevant memory pathology so the streaming alternatives can be evaluated
against a concrete worst case.

### 2.1 Worst-case stall scenario

Three preconditions must coincide:

1. **Parallel dispatch.** The transfer is wide enough to land in
   `ParallelDeltaPipeline` rather than `SequentialDeltaPipeline`
   (`DEFAULT_PARALLEL_THRESHOLD = 64` files,
   `crates/transfer/src/delta_pipeline.rs:42`).
2. **Skewed completion times.** At least one file's delta-compute time is
   substantially larger than the median. The canonical example is one
   multi-gigabyte file at NDX 0 alongside thousands of small config files.
3. **Producer side keeps feeding.** The bounded `crossbeam_channel` between
   `delta-drain` and `delta-reorder` keeps delivering completions for the
   small files while the large file remains in flight on a rayon worker.

In that regime every successor that finishes drains into the
`delta-reorder` thread, gets inserted into the ring, and stalls behind the
empty head slot. The ring fills to capacity, and the next insert triggers
the full-buffer path.

### 2.2 The two full-buffer branches

The `delta-reorder` loop
(`crates/engine/src/concurrent_delta/consumer.rs:151-188`) reaches one of:

1. **Drain branch (head occupied).** A contiguous run is yielded; slots are
   freed; the new item fits. Memory stays at `O(capacity)`. Throughput
   recovers immediately.
2. **Force-insert branch (head empty).** No item at the head slot, no item
   to drain. `ReorderBuffer::force_insert` grows the ring to
   `(seq - next_expected) + 1` slots and stores the item. Memory grows by
   one slot per call.

The force-insert branch is the pathology. While the head file remains in
flight, every additional successor that arrives extends the ring. The work
queue upstream is bounded, so eventually the bounded `crossbeam_channel`
between `delta-drain` and `delta-reorder` fills and rayon workers idle
holding their last result. But the ring itself can already hold an
arbitrarily large number of completed results before that backpressure
propagates: every previously-admitted result is still pinned waiting for
the head, and no force-insert reclaims them.

### 2.3 Concrete memory bound under force-insert

Let:

- `S` = average size of a `DeltaResult` payload in bytes.
- `H` = number of files whose deltas have completed while file 0 is still in
  flight on its worker.

Peak ring memory after force-insert exhaustion is `H * S` bytes. With a
typical `DeltaResult` of ~200 bytes (NDX, byte counts, redo flag, sequence
number) and `H = 100k`, the ring holds ~20 MiB before the head lands -
small in absolute terms. But `DeltaResult` carries a future `T` for richer
applied-result types (e.g. checksum-verified buffer references for the
multi-file delta-apply pipeline,
`docs/design/multi-file-delta-apply-pipeline.md`), and those payloads are
much larger:

- `AppliedFileResult` could carry a `Vec<u8>` of decoded basis bytes per
  file. At an average decoded size of 16 KiB, the same `H = 100k` stall
  yields ~1.6 GiB of pinned heap.
- For workloads that pre-compute strong checksums on the receiver side, the
  payload also carries a temp-file FD plus the recomputed digest, doubling
  the per-item overhead.

The correctness story is fine - the head will eventually land, the ring
will drain in one burst, and post-processing sees NDX-ordered results.
The memory story is unbounded in any axis the user does not directly
control: file count, average payload size, and head-file delta-compute
time.

### 2.4 Why the existing bounds do not save us

- The fixed-capacity ring is only a soft bound; `force_insert` opts out.
- `BoundedReorderBuffer` is a hard bound, but the only signal it emits is
  `BackpressureError` for inserts outside the window. The receiver-side
  pipeline does not have a clean place to honour that signal: the rayon
  workers compute deltas in parallel, the work queue is SPMC-bounded
  upstream, and the only way to "wait" is to wedge a worker on a
  blocking send. That converts the pathology from "unbounded memory" to
  "unbounded thread stalls", which is worse for throughput.
- `AdaptiveCapacityPolicy` reduces the *frequency* of force-insert calls by
  growing pre-emptively under sustained pressure, but it does not change
  the worst case: a single multi-second head-of-line stall can still grow
  the ring to fit every successor.

The streaming alternatives below all aim at the same goal: keep the memory
in-process bounded to `O(W)` for some window `W` independent of the head's
stall duration, while still preserving NDX-ordered delivery to
post-processing.

## 3. Streaming alternatives

Three approaches sit on a spectrum from "preserve all data" to "ask the
sender to slow down":

| Alternative | Memory cost | Disk cost | Latency cost | Wire cost |
| --- | --- | --- | --- | --- |
| 3.1 spill-to-tempfile | O(W) RAM | O(H) disk | one extra read pass | none |
| 3.2 drop-and-replay | O(W) RAM | none | replay round-trip | wire amp |
| 3.3 backpressure-to-sender | O(W) RAM | none | sender stalls on slow head | none, but visible flow stop |

### 3.1 Spill-to-tempfile

When the in-memory window is full and the head slot is empty, serialise the
oldest *non-head* completed successor to a per-transfer tempfile, evict it
from the ring, and restore it on read-back when its sequence becomes the
head. The tempfile is a sequential append-only log keyed by sequence
number; reads are seeks back to recorded offsets.

Mechanics:

- Reserve `W` ring slots in RAM. Once full and the head is empty, start
  spilling.
- Each spilled item writes `(sequence: u64, payload_len: u32, payload:
  bytes)` to the tempfile. An in-memory `BTreeMap<u64, u64>` records
  `sequence -> file_offset`.
- When the head slot lands (the slow file finishes), drain ready items
  from RAM in order. As `next_expected` advances, items either come from
  the ring or from the tempfile (via the offset map).
- After the entire sequence range up to a high-water mark drains, the
  tempfile is unlinked.

Properties:

- **Memory bound becomes hard.** Peak heap is `O(W * S)` plus the offset
  map, which is `O(H)` *very small* entries (each entry is 16 bytes).
  Total heap is dominated by `W * S`, independent of `H`.
- **Disk cost.** One linear write pass plus one seek-and-read pass over
  the spilled data. With `H * S` total bytes and an SSD, this adds a
  fixed constant per spilled byte. Spinning rust amplifies the cost
  significantly because of the seek pattern on read-back.
- **Latency cost.** The first commit after the head lands is delayed by
  the time to read the contiguous prefix from the tempfile. With
  pre-fetch and async I/O this is dominated by the read bandwidth, not
  the seek count.
- **Tempfile placement.** Three options:
  1. `TMPDIR` (RAM-disk on most modern Linux distros). Defeats the purpose
     when the OS pages it back to swap.
  2. Receiver's partial dir. Lives on the same filesystem as the
     destination, so reads are co-located with writes. Interaction with
     `--partial-dir` and `--inplace` needs care: if the user has set a
     custom partial dir, we should respect it; if not, we use a
     receiver-managed scratch path.
  3. A dedicated per-transfer subdirectory under the destination root,
     unlinked at transfer end. Simplest, but pollutes the destination
     namespace transiently.
- **Crash recovery.** On crash mid-transfer the tempfile is orphaned. A
  startup sweep of the partial dir (or scratch dir) cleans it up. This
  matches the existing `--partial-dir` cleanup story.
- **Filesystem semantics.** On Windows the tempfile must use
  `FILE_FLAG_DELETE_ON_CLOSE` to survive crashes cleanly. On POSIX
  `unlink` after `open` is the standard idiom.

### 3.2 Drop-and-replay

When the window is full and the head slot is empty, drop completed
successors that exceed the window and ask the sender to retransmit them
later. The sender already maintains the file list; replay is a matter of
re-issuing the NDX with the same sender-side state.

Mechanics:

- Reserve `W` ring slots. Once full and the head is empty, *discard* the
  oldest non-head successor.
- Send a `MSG_REDO` (or equivalent) frame upstream listing the dropped
  NDX. The sender re-runs delta computation for that NDX once the
  generator gets back to it.
- Receiver tracks dropped NDXs and refuses to ack them until they are
  resent.

Properties:

- **Memory bound becomes hard.** Same as 3.1, no disk required.
- **Wire amplification.** Every dropped NDX costs a full re-send of the
  delta. For a 1 GiB file delta whose computation took 30 seconds, the
  wire cost is also 30 seconds (or more, if the network is saturated).
  Throughput collapses if HoL stalls are common.
- **Protocol fidelity.** Upstream rsync 3.4.1 has no MSG_REDO frame for
  speculative re-sends. Adding one would be a wire extension, ruled out
  by the project's no-wire-protocol-features policy
  (`feedback_no_wire_protocol_features.md`). A no-extension variant
  could re-establish the connection from scratch, but that is much
  worse.
- **Latency cost.** The replay round-trip is at least one network
  RTT plus the re-computation cost. For long-distance transfers this
  is dominant.
- **Asymmetry.** Useful only when the receiver is RAM-constrained but
  the network is fast. The opposite (RAM-rich, network-constrained)
  workload pays the wire cost for no in-RAM benefit.

This alternative is included for completeness. The wire-extension
requirement disqualifies it under existing project policy. The
in-band-only variant (re-establish the connection) is strictly worse
than spill-to-tempfile in every dimension that matters.

### 3.3 Backpressure-to-sender

When the window is full and the head slot is empty, *do not accept further
results*. The bounded `crossbeam_channel` between `delta-drain` and
`delta-reorder` already provides this signal at thread granularity; the
proposal is to extend it to the sender by stopping reads from the wire
until the head completes.

Mechanics:

- The receiver wire reader (the thread driving `read_ndx_and_attrs`)
  monitors reorder-buffer occupancy via a shared atomic.
- When occupancy hits `W`, the wire reader stops issuing
  `read_ndx_and_attrs`. TCP backpressure propagates upstream: the kernel
  stops draining the socket; the sender's `write` calls block; the
  generator pauses dispatching new files.
- When the head lands and successors drain, occupancy drops below the
  threshold and the wire reader resumes.

Properties:

- **Memory bound becomes hard.** The window cannot exceed `W` because the
  sender literally cannot produce more results.
- **No disk cost, no wire cost.** Pure in-process flow control.
- **Latency cost.** The sender stalls for the duration of the head-file
  delta. From the user's perspective this is the worst case: progress
  flat-lines until the head completes, then resumes. Indistinguishable
  from a network outage in observability output.
- **Throughput cost.** During the stall, the sender's CPU is idle on the
  generator side and rayon workers are idle on the receiver side. The
  overall transfer time degrades to `time(head_file)` plus the serial
  remainder of the schedule, which is upstream's behaviour exactly. The
  parallel pipeline buys nothing during the stall.
- **Deadlock risk.** If both sides apply mutual backpressure (sender
  stalls on `write`, receiver stalls on `read`) the pipeline can wedge.
  TCP's window mechanism prevents the wire from deadlocking, but
  application-level frame sequencing may not. Particularly delicate for
  multiplexed streams (`MSG_DATA`, `MSG_INFO`, ...) that share a single
  connection.
- **Compatibility.** Backpressure is the upstream behaviour by
  construction (single-threaded recv loop). Honouring it is wire-safe.

### 3.4 Hybrid: spill plus opportunistic backpressure

The three strategies are not mutually exclusive. A robust implementation
might:

1. Use the in-memory ring up to `W` slots (current behaviour).
2. Spill-to-tempfile when `W` fills and the head is empty (3.1).
3. Apply backpressure-to-sender when the spill *also* exceeds a
   configurable cap `W_max` (3.3).

The first stage handles the common case; the second handles long-tailed
distributions; the third handles pathological edge cases where the head
file's delta runs longer than the entire remainder of the transfer.

The drop-and-replay path (3.2) is excluded.

## 4. Recommendation

**Pursue 3.1 (spill-to-tempfile) as the primary path, with 3.3
(backpressure-to-sender) as a fallback at a configurable spill cap.**
Decline 3.2 (drop-and-replay) on the no-wire-protocol-features policy.

### 4.1 Rationale

- **Hard memory bound.** Spill-to-tempfile is the only option that bounds
  RAM use to `O(W)` regardless of stall duration, payload size, and file
  count. Backpressure-to-sender bounds RAM but at the cost of
  effectively-serial throughput. Drop-and-replay is excluded by policy.
- **No wire changes.** Both 3.1 and 3.3 are receiver-side internal
  optimisations. Tcpdump-replay against an upstream peer remains
  byte-identical. Aligns with `feedback_no_wire_protocol_features.md`.
- **Composable with existing audit findings.** The HoL audit (#1883)
  identified force-insert as the soft-bound escape hatch. Spill-to-tempfile
  replaces force-insert with a bounded equivalent: instead of growing the
  ring without limit, the surplus goes to disk. The deadlock-break
  invariant is preserved (the consumer can always accept the new item).
- **Backpressure as a backstop.** When the spill itself approaches a
  configurable cap (e.g. spill bytes exceeding a percentage of free disk),
  fall back to backpressure-to-sender. This trades throughput for the
  hard-bound guarantee in pathological cases.
- **Progressive rollout.** The spill code can ship behind an environment
  variable or `--max-reorder-spill` flag, gating the new path while
  metrics (#1885) confirm the behaviour matches expectations.

### 4.2 Tradeoffs accepted

- **Disk I/O during stalls.** Spilling adds write+read passes for stalled
  successors. On SSD this is a small constant per spilled byte. On HDD
  the seek pattern on read-back is worse. Acceptable: HoL stalls are by
  definition rare; the alternative is unbounded RAM.
- **Tempfile lifecycle complexity.** Crash recovery, `--partial-dir`
  interaction, and Windows `DELETE_ON_CLOSE` semantics each need explicit
  handling. The existing partial-dir code provides templates.
- **Throughput degradation under spill.** A workload that spills heavily
  will be slower than a hypothetical infinite-RAM run. This is the price
  of a hard bound; users can disable spill (set `--max-reorder-spill 0`)
  to revert to today's force-insert behaviour.

### 4.3 Tradeoffs rejected

- **Drop-and-replay (3.2).** Wire extension required. Wire amplification
  is unbounded in the worst case. Excluded by policy.
- **Pure backpressure-to-sender (3.3) only.** Throughput collapses to
  upstream's serial rate during any stall. Defeats the parallel
  pipeline's purpose. Acceptable as a fallback, not as a primary.
- **Per-thread reorder buffers.** Tempting, but each thread's view is
  not contiguous in NDX space - merging them back into a single
  monotonic stream requires the same ring or BTreeMap structure. No
  bound improvement.

### 4.4 Implementation sketch (non-binding)

A minimal end-to-end design:

1. Extend `ReorderBuffer` with a `SpillBackend` trait
   (`write(seq, payload) -> offset`, `read(seq, offset) -> payload`,
   `erase(seq)`). Default backend is `NoSpill` (current behaviour).
2. Add `SpillToTempfile` backend that owns a `File` opened in the
   partial dir (or a configured scratch path) plus a
   `BTreeMap<u64, u64>` offset index.
3. In `ReorderBuffer::insert`, when the ring is full and the head slot is
   empty, evict the *oldest non-head* item to the spill backend instead
   of calling `force_insert`. The ring slot becomes available for the
   new item.
4. In `next_in_order`, when the head slot has a spill-marker, read the
   payload back from the spill backend before yielding.
5. In `finish`, unlink the tempfile and drop the offset index.
6. Plumb a `--max-reorder-spill BYTES` CLI flag through
   `core::CoreConfig`. When the spill backend exceeds the cap,
   switch to backpressure-to-sender mode (3.3) by signalling the wire
   reader thread.
7. Surface spill counters via `ReorderStats` (per #1885): bytes
   spilled, bytes reloaded, peak spill occupancy, time spent in
   backpressure mode.

The wire reader's backpressure path needs care to avoid deadlocking the
multiplex stream; see open question Q4.

## 5. Open questions

1. **Tempfile placement (Q1).** Partial dir vs `TMPDIR` vs dedicated
   scratch dir. The audit in `docs/architecture/reorder-buffer.md`
   flagged this as open. Recommended default: receiver-managed
   subdirectory under the destination root, named after the transfer's
   PID; respect `--partial-dir` if set; allow `OC_RSYNC_REORDER_SPILL_DIR`
   env override for diagnostics.
2. **Spill encoding (Q2).** Plain bincode? Length-prefixed raw bytes? A
   schema choice has compatibility implications across oc-rsync versions
   only if a transfer can be paused-and-resumed. For single-process
   transfers, length-prefixed raw `DeltaResult` bytes (using the existing
   varint helpers) is sufficient.
3. **Spill bandwidth budget (Q3).** Should we throttle spill writes to
   leave bandwidth for the receiver's commit path? On a saturated disk,
   spilling can starve the head-file commit and prolong the stall. A
   small reserved bandwidth for commits (via `io_uring` priority hints
   on Linux, or O_DIRECT + write-back tuning) may be needed. Initial
   experiments should defer this and measure.
4. **Backpressure deadlock on multiplex (Q4).** The wire is multiplexed:
   `MSG_DATA`, `MSG_INFO`, `MSG_ERROR`, etc. share a single TCP stream.
   If we stop reading the socket entirely, in-band frames the sender
   needs to *receive* (e.g. acks) cannot be transmitted. Need a
   per-frame-type backpressure that admits acks even while pausing data
   frames. The protocol crate's mux helpers
   (`crates/protocol/src/...`) need a `pause_data_frames(true)` knob.
5. **Interaction with `--inplace` (Q5).** `--inplace` means the receiver
   writes directly to the destination, with no temp-file commit. The
   spill tempfile is unrelated to the destination temp-file, but they
   share a parent directory under the default placement. Ensure the
   spill is named distinctively (`oc-rsync-spill-<pid>-<seq>.tmp`) so
   it is not mistaken for a destination temp.
6. **Adaptive policy interaction (Q6).** `AdaptiveCapacityPolicy` grows
   the ring opportunistically. With spill enabled, the policy should
   prefer spilling over growing once the ring exceeds `W`. Either treat
   `policy.max` as a hard cap that triggers spill, or coordinate the
   two policies so they do not fight.
7. **Cross-platform tempfile semantics (Q7).** Linux: `O_TMPFILE` with
   linkat fallback. macOS: `mkstemp` plus `unlink`. Windows:
   `CreateFileW` with `FILE_FLAG_DELETE_ON_CLOSE`. The `fast_io` crate
   is the natural home for the platform abstraction.
8. **CLI naming and discoverability (Q8).** `--max-reorder-spill BYTES`
   is descriptive but verbose. Alternatives: `--reorder-spill-cap`,
   `--reorder-memory-cap`. Pick one that pairs with future
   metrics-emission flags so the cluster of tunables is coherent.
9. **Test plan (Q9).** Property tests for "spill round-trip preserves
   ordering". Stress tests that fabricate a head-of-line stall and
   confirm peak RSS stays below `W * S + spill_overhead`. Interop tests
   to confirm wire output is byte-identical to a no-spill run against
   the same upstream peer. The existing
   `crates/transfer/benches/reorder_buffer_benchmark.rs` and
   `crates/engine/benches/reorder_buffer_scaling.rs` are starting
   points for performance regression coverage.
10. **Upstream alignment (Q10).** Upstream has no reorder buffer and no
    spill mechanism. Should we expose any of the spill counters on the
    wire (e.g. for daemon mode debugging)? Recommended answer: no.
    Counters stay in receiver-side observability only; wire output
    remains upstream-equivalent.

Once #1565 lands as code, these answers become commitments. The intent
of this design note is to make the choices visible before the
implementation pins them.
