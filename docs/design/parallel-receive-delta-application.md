# Parallel receive-side delta application (#1368)

Design note for the receiver path that today applies delta tokens to the
destination file sequentially. Task #1368 asks for parallel application
across files while preserving the per-file ordering required for
in-place writes and wire-format parity. This document records the current
sequential surface, the invariants any parallel scheme must preserve, the
existing dormant infrastructure that would host the change, the
back-pressure model, and the gating prerequisites that block adoption.

The dominant gate is the parity-test gap flagged by the wire-format
audit (#4205). Until that gap closes, parallel application stays behind
an opt-in switch at most and remains off by default.

## 1. Current sequential apply

### 1.1 Per-file token loop (single thread)

`crates/transfer/src/receiver/transfer.rs:127` opens the
`for (file_idx, file_entry) in self.file_list.iter().enumerate()` loop
that walks the entire received file list one entry at a time. Inside that
loop, `crates/transfer/src/receiver/transfer.rs:259` opens an inner
`loop { match token_reader.read_token(reader)? { ... } }` that consumes
delta tokens until `TokenReaderDeltaToken::End` and writes them into the
temp file:

- Literal tokens write through `output.write_all(...)` or the sparse
  state at `transfer.rs:298-329`.
- `BlockRef(block_idx)` resolves the basis-file offset via `basis_map`
  and writes the matched block at `transfer.rs:331-385`.
- `End` runs the per-file checksum compare-and-replace at
  `transfer.rs:261-296`, then breaks out of the inner loop.

Per-file finalization (sparse pad, `into_inner`, optional `fsync`, temp
guard rename, metadata application) follows the loop at
`transfer.rs:388-475`. The full body is single-threaded and strictly
ordered.

### 1.2 Lower-level applicator

The token-driven inner loop above is one of two call sites for the same
algorithm. The other is the standalone applicator in
`crates/transfer/src/delta_apply/applicator.rs`:

- `DeltaApplicator::apply_token` at `applicator.rs:326-382` decodes one
  token and applies it.
- `apply_delta_stream` at `applicator.rs:436-445` drives
  `while applicator.apply_token(reader)? {}` to drain the per-file
  stream.

Both paths share the property that a single thread owns the destination
writer for the entire file, and tokens are applied strictly in the order
the sender emits them.

### 1.3 Concurrent-delta infrastructure already present

The receiver does **not** today route work through the concurrent-delta
pipeline. The infrastructure exists, fully tested in isolation, and is
documented at `crates/engine/src/concurrent_delta/mod.rs:1-188`. The
production wiring stops at the dormant trait object:

- `crates/transfer/src/receiver/mod.rs:281` instantiates
  `Some(Box::new(SequentialDeltaPipeline::new()))`.
- `crates/transfer/src/receiver/mod.rs:297` exposes
  `set_delta_pipeline` for callers to substitute a parallel
  implementation.
- No production caller ever swaps the default. The
  `ParallelDeltaPipeline` constructor at
  `crates/transfer/src/delta_pipeline.rs:212` is reachable only from
  unit tests.
- `SequentialDeltaPipeline` at `crates/transfer/src/delta_pipeline.rs:118-144`
  is the mode that runs in the shipped binary; its `submit_work` calls
  `dispatch(&work)` synchronously and buffers the result.

The wire-format audit at
`docs/audits/parallel-dispatch-wire-format-verification.md:241-256`
confirms the dormant state: "The production binary always runs
`SequentialDeltaPipeline` ... parallel dispatch is infrastructure that
has not yet been wired into the receiver transfer loop."

## 2. Ordering invariants

Any parallel design must preserve the following invariants. They are
non-negotiable - violating any one of them changes the bytes on the wire
or corrupts the destination file.

### 2.1 Per-file token order

Within a single file, tokens must apply in the order the sender emits
them. This is hard:

- Literal tokens are positional. A literal at offset 4096 followed by a
  literal at offset 8192 must hit the temp file in that order. Reversing
  them either inverts file content or invalidates the rolling checksum
  state inside `ChecksumVerifier::update` at `transfer.rs:306`.
- `BlockRef` tokens advance the basis-file mmap window through
  `basis_map.map_ptr` at `transfer.rs:361` and feed `token_reader.see_token`
  at `transfer.rs:371`, which mutates the compression context. The
  compression decoder is a stateful stream across tokens of one file.
- `--inplace` writes directly to the destination at the offset implied by
  the running `total_bytes` counter. Out-of-order application would
  overwrite future bytes with stale literals.
- The per-file checksum at `transfer.rs:272-295` is a hash of the byte
  stream in apply order; reordering corrupts the digest and triggers a
  phase-2 redo (best case) or undetected corruption (worst case if the
  hash happens to match).

Within a file, parallelism is not safe.

### 2.2 Cross-file independence (with caveats)

Across distinct files, parallelism is safe **provided** these conditions
hold:

- The temp-file or destination path of file A does not overlap file B.
  Standard rsync semantics already guarantee this through the file-list
  walk.
- Per-file `BasisFile` mmaps are independent. The `MapFile::open` call at
  `transfer.rs:243` opens a fresh mmap per file, so no shared state.
- The compression context, despite being session-scoped at
  `transfer.rs:122-123` (`let mut token_reader = TokenReader::new(compression)`),
  is currently a single mutable resource. Any parallel design must either
  (a) shard the wire stream such that each worker receives a self-contained
  token stream with its own decompressor, or (b) keep wire decoding on
  the producer thread and only fan out the post-decode work. Option (b)
  is the only one compatible with the current `TokenReader` shape.
- Wire-emitted side effects (NDX writes, sender attribute echoes, ack
  frames at `transfer.rs:178-220`) must happen in NDX-sorted order to
  match upstream `receiver.c:720` (`recv_files()` main loop).
  `ReorderBuffer` exists to enforce this.

### 2.3 Wire-output order

The sender requires the receiver to emit per-file acknowledgements and
itemized output in NDX order. The concurrent_delta module's audit at
`crates/engine/src/concurrent_delta/mod.rs:52-166` already classifies
all parallel sites. The receiver dispatch is the only one not yet
wired; the `ReorderBuffer` is the mechanism that closes the cycle for it
too.

## 3. Sketch

The parallel design is the production wiring of the dormant
`ParallelDeltaPipeline` plus a small amount of plumbing to feed token
streams to per-file workers without losing per-file order.

### 3.1 Pipeline topology

```text
Network reader (producer, single thread)
   |  reads NDX, sum_head, signature ack
   |  reads delta token stream for file F (decompresses serially)
   |  parks the (NDX, decoded-token-stream) handle into DeltaWork
   v
WorkQueueSender (bounded crossbeam_channel)
   |  capacity = adaptive_queue_depth(worker_count, avg_target_size)
   |  blocks the producer when full (backpressure)
   v
DeltaConsumer (background thread, owns rayon::scope)
   |  drain_parallel_into() dispatches one task per DeltaWork
   |  each task is a single-threaded per-file applicator
   |  inside the task: while apply_token(&mut reader)? {}
   v
ReorderBuffer (inside DeltaConsumer)
   |  insert by sequence number; drain_ready() yields contiguous run
   v
poll_result() returns DeltaResult in submission order
   |  receiver finalizes: checksum verify, temp rename, metadata, redo collect
```

The shape above is already implemented at
`crates/transfer/src/delta_pipeline.rs:155-168` and
`crates/engine/src/concurrent_delta/consumer.rs:147-222`. The new work is
on the *producer side*: split the current per-file token loop into "read
+ buffer tokens" (producer) and "apply tokens to file" (worker).

### 3.2 Per-file worker contract

A worker is a self-contained applicator. It owns:

- An owned token buffer (or a streaming token reader fed from a
  per-file SPSC channel from the producer).
- An owned temp-file writer (or the destination writer for `--inplace`).
- A fresh `ChecksumVerifier` for that file.
- Its own `BasisFile` mmap handle.

The worker is structurally identical to a single iteration of the
existing `for file_entry` loop body in `transfer.rs:127-475`. It runs
single-threaded. Per-file token order is preserved trivially because
*one* worker processes the entire file. The parallelism is at the
file-level granularity, not the token-level granularity.

This matches the existing `WholeFileStrategy` and
`DeltaTransferStrategy` shapes already present in
`crates/engine/src/concurrent_delta/strategy.rs` and consumed by
`dispatch()` at the same file's `strategy.rs:275-279`.

### 3.3 Sequence numbering

The producer stamps every `DeltaWork` with a monotonic sequence number
at `crates/transfer/src/delta_pipeline.rs:297-308`. The
`ReorderBuffer::insert` call at
`crates/engine/src/concurrent_delta/consumer.rs:182` indexes by that
sequence. The `drain_ready` consumer side at `consumer.rs:184` and
`consumer.rs:199` emits results in contiguous monotonic runs. The
sequence number is the NDX-equivalent that re-establishes wire order
*after* parallel workers complete.

### 3.4 Producer responsibilities (what stays single-threaded)

These steps must remain on the producer thread to preserve wire-format
behaviour:

- NDX write at `transfer.rs:177-178` (must be monotonic by upstream
  contract).
- Sender-attribute / `ITEM_TRANSFER` echo at `transfer.rs:181-183`.
- `sum_head.write` at `transfer.rs:202` and signature emission at
  `transfer.rs:206-208`.
- `SenderAttrs::read_with_codec_xattr` at `transfer.rs:212-220` and the
  echoed NDX/SumHead reads at `transfer.rs:222-227`.
- Token *decoding* through the session-scoped `TokenReader` at
  `transfer.rs:123`. Compression decode state crosses file boundaries
  and cannot be parallelized without splitting the wire stream.

The producer hands off **decoded** per-file token batches to the
worker. The worker then runs the apply loop, including basis-mmap
reads, sparse writes, and `ChecksumVerifier::update` calls. This split
preserves the wire-side invariants while exposing the apply-side CPU and
syscall cost for parallelism.

## 4. Backpressure

Filesystem speed - especially on NFS, SMB, and other network-backed
destinations - varies wildly. The pipeline must not buffer unbounded
work when the destination cannot keep up.

### 4.1 Bounded work queue

`WorkQueueSender` is built from
`work_queue::bounded_with_capacity(capacity)` at
`crates/transfer/src/delta_pipeline.rs:234`. The capacity comes from
`adaptive_capacity` at `delta_pipeline.rs:281-294`, which scales
2x-8x worker count by average file size. When the bounded channel is
full, `send` blocks the producer thread.

The producer is the wire reader. Blocking it stops the receiver from
draining the socket. The kernel's TCP receive buffer fills, the TCP
window narrows, and the sender slows down. This is the standard
end-to-end flow control loop. No special signal is needed - the
back-pressure is implicit in the bounded channel.

### 4.2 ReorderBuffer pushback

When workers complete out of order, results pile up in the
`ReorderBuffer` until the missing head sequence arrives. The buffer is
fixed-capacity (matched to the work-queue capacity at
`consumer.rs:153-158`). When full and the head sequence is still in
flight, the reorder thread stops draining the worker output channel,
which in turn stops workers from progressing (because their result
channel back-pressures), which stops them from pulling new work,
which propagates back to the producer.

The current code includes a `force_insert` deadlock-break at
`consumer.rs:191-194` for the case where the buffer is full but the
head is still missing. This branch is a known smell (see
`project_consumer_force_insert_smell` in `MEMORY.md`). For receive-side
delta apply, where wire-order is mandatory, `force_insert` must be
either removed or gated behind a guarantee that it never fires during
ordered operation. The wire-format audit's recommended follow-up G3 at
`docs/audits/parallel-dispatch-wire-format-verification.md:273-293`
calls for a deterministic test that pins the head sequence and verifies
the resulting delivery order. That test must exist and prove
`force_insert` does not violate the wire-order contract before this
design can ship as the default path.

### 4.3 Slow filesystem worst case

A pathological slow destination (NFS at 1 MB/s under network loss, for
example) under parallel apply collapses to the sequential rate plus
queueing overhead. The bounded queue and reorder buffer cap memory at
`O(capacity * avg_file_size)`. The producer stalls behind the channel,
the TCP window closes, and the sender pauses. No memory growth, no
buffer bloat. The only added cost is the queue + reorder slab
allocation, which is `O(capacity)` regardless of transfer size.

### 4.4 Per-file size variance

A 64-file transfer with one 10 GB file and 63 1 KB files would stall the
reorder buffer behind the 10 GB worker. The `ReorderBuffer` capacity
must be large enough to hold the 63 completed small files while waiting
for the head sequence. `adaptive_capacity` already accounts for this by
giving small-file workloads an 8x multiplier at `delta_pipeline.rs:284-292`.
For mixed workloads, the bypass-reorder variant
(`spawn_bypass` at `consumer.rs:142-145`) is an escape hatch that
delivers results in completion order - but only safe when downstream
ordering is unnecessary, which is **not** the case for the receiver's
wire-output path.

## 5. Cross-references

The infrastructure this design plugs into has accumulated several
benches and audits. The relevant ones:

- **#1885 - ReorderBuffer metrics.** Surfaces queue depth, peak depth,
  and stall time so we can observe whether the buffer is actually
  saturating in production. See
  `docs/design/reorderbuffer-metrics-and-bypass.md:1-63` and the
  `Metrics` struct at
  `crates/engine/src/concurrent_delta/reorder/mod.rs:44`. The metrics
  must be wired into the receiver telemetry before promoting parallel
  to default so we can diagnose stalls.
- **#4180 - reorder_buffer_cache bench.** Cache-residency at 1M items
  with varied payload sizes. Source:
  `crates/engine/benches/reorder_buffer_cache.rs`. Establishes that
  the ring buffer fits cache at production drift windows.
- **#4204 - reorderbuffer_memory bench.** Peak occupancy under varied
  drift. Source: `crates/engine/benches/reorderbuffer_memory.rs:154-164`,
  see `docs/design/capacity-multiplier-tuning.md:124-220`. Confirms the
  reorder ring tolerates higher drift cheaply.
- **#4214 - drain_parallel contention bench.** Source:
  `crates/engine/benches/drain_parallel_benchmark.rs`, see
  `docs/design/iouring-rayon-submission.md:279-280` and
  `docs/design/lockfree-mpsc-drain-design.md:14-69`. Names the
  threshold at which the work-queue mutex becomes the bottleneck.
- **#4205 - wire-format-unchanged audit.** Source:
  `docs/audits/parallel-dispatch-wire-format-verification.md`. Verdict
  at `parallel-dispatch-wire-format-verification.md:303-321`:
  "Conditional PASS" - the parallel pipeline has zero callers today,
  so wire output is trivially the sequential output. Once the receiver
  routes through `ParallelDeltaPipeline`, the verdict flips to FAIL
  until the follow-ups in section 6 land. This is the gating audit
  for #1368.
- **#4173 - WorkQueueSender audit.** Source:
  `docs/design/parallel-source-enumeration-eval.md:13-27, 215-237, 297`.
  Concluded `WorkQueueSender` is single-producer (SPMC). The
  receive-side producer here is the wire reader, which is a single
  thread by construction, so the SPMC conclusion holds.
- **#4206 - parallel_dispatch_overhead bench.** Source:
  `crates/engine/benches/parallel_dispatch_overhead.rs:1-83`, see
  `docs/design/capacity-multiplier-tuning.md:92-117`. Decomposes the
  dispatch budget into thread-spawn, channel, and reorder cost. Tells
  us which of the three to optimise once measurements arrive from the
  wired-up parallel path.

## 6. Recommendation

Adopt as a CLI/config-gated opt-in. Do **not** promote to default
until two conditions hold:

### 6.1 Gating prerequisite

The parity-test gap (G2) named in
`docs/audits/parallel-dispatch-wire-format-verification.md:258-271` must
close. That gap is the absence of any test that drives a fixed
`DeltaWork` batch through both `SequentialDeltaPipeline` and
`ParallelDeltaPipeline` and asserts identical `Vec<DeltaResult>`
(same NDX order, same sequence, same literal/matched counts, same
status). Without that test, a regression in the parallel path could
silently change the wire bytes oc-rsync emits, breaking interop.

Follow-up 1 in the audit (`parallel-dispatch-wire-format-verification.md:328-337`)
defines the exact test contract. Land it first. Then this design's
opt-in CLI gate can be turned on for end-to-end interop runs.

### 6.2 Bench evidence prerequisite

#4214 bench data (`drain_parallel_benchmark.rs`) must show parallel
dispatch is actually faster than sequential at the receiver's target
workload (median file size, common file counts). The dispatch cost
decomposition from #4206 must show the work-queue mutex is not the
dominant cost - otherwise the parallel path's overhead exceeds its
benefit at receive-side scale.

Both prerequisites are independent. The parity test can land first
because it does not require the production wiring; the bench evidence
needs the wiring to be at least available behind the gate.

### 6.3 Phased rollout

1. **Phase 1 - parity gap close.** Land the
   `crates/transfer/tests/parallel_pipeline_wire_parity.rs` test
   (audit follow-up 1). No receiver changes. Existing infra only.
2. **Phase 2 - opt-in CLI gate.** Add a hidden `--experimental-parallel-apply`
   flag (or env var) that calls `set_delta_pipeline` at
   `crates/transfer/src/receiver/mod.rs:297` with a
   `ThresholdDeltaPipeline` from
   `crates/transfer/src/delta_pipeline.rs:383-413`. Default off. CI
   adds a matrix dimension running interop with the flag on, per audit
   follow-up 4 (`parallel-dispatch-wire-format-verification.md:355-357`).
3. **Phase 3 - measure.** Collect #4214 drain-parallel and #4206
   dispatch-overhead numbers against representative receive-side
   workloads. Pair with #1885 metrics to confirm reorder stall time
   stays low.
4. **Phase 4 - default on.** Only after Phases 1-3 ship cleanly and the
   parallel path beats sequential on benchmarks **and** matches it
   byte-for-byte on the parity test and on the interop matrix.

### 6.4 Force-insert resolution

Before Phase 2, the `force_insert` deadlock-break at
`crates/engine/src/concurrent_delta/consumer.rs:191-194` must be
either removed or proven not to fire in the receive-side path. Audit
follow-up G3 (`parallel-dispatch-wire-format-verification.md:273-293`)
defines the test. The receive-side path *cannot* use a delivery order
that differs from submission order, so an active `force_insert` is a
correctness bug here, not just a smell.

## 7. Migration safety

Per #4205's audit verdict, the parallel infrastructure is sound in
isolation but unobserved end-to-end. Migrating the receiver to actually
use it has the following safety failure modes:

| Failure | Symptom | Detector |
|--------|---------|----------|
| Wire-order divergence | Sender reports NDX-out-of-sequence or hangs | `parallel_pipeline_wire_parity.rs` (G2 close) |
| Compression-context corruption | Decode error on file N+1 after file N | Existing decode error path in `TokenReader` |
| Per-file order violation | Checksum verify failure or content corruption | Existing per-file checksum at `transfer.rs:272-295` |
| `force_insert` triggers under load | Silent wire-order violation | New deterministic test (G3 close) |
| Bounded queue starves producer | Throughput collapse under slow filesystem | #1885 metrics + #4214 bench |
| Reorder buffer OOM | Memory growth on long-tailed file size distributions | `reorderbuffer_memory` bench (#4204) |

The first row is the load-bearing one. Without the parity test, we
have no evidence that the parallel path produces the same wire bytes
as sequential. Every other row is a smaller correctness or performance
concern; the parity test is the necessary precondition for trusting any
of the rest.

**Decision: defer the parallel-by-default wiring until #4205 follow-up
G2 (`parallel_pipeline_wire_parity.rs`) lands.** Until then, the
infrastructure stays available behind an opt-in gate added in Phase 2,
which is itself contingent on the parity test existing. Treat #1368 as
*design accepted, implementation gated on #4205 G2 closure*.

## 8. References

### Code
- `crates/transfer/src/receiver/transfer.rs:127` - per-file loop entry
- `crates/transfer/src/receiver/transfer.rs:259` - sequential token apply loop
- `crates/transfer/src/receiver/transfer.rs:272-295` - per-file checksum verify
- `crates/transfer/src/receiver/mod.rs:281,297` - dormant pipeline setter
- `crates/transfer/src/delta_apply/applicator.rs:326-445` - reusable applicator
- `crates/transfer/src/delta_pipeline.rs:118-333` - Sequential and Parallel pipelines
- `crates/engine/src/concurrent_delta/mod.rs:1-188` - parallel-dispatch module docs
- `crates/engine/src/concurrent_delta/consumer.rs:147-222` - reorder thread
- `crates/engine/src/concurrent_delta/reorder/mod.rs:44-130` - ReorderBuffer types
- `crates/engine/benches/drain_parallel_benchmark.rs` - #4214 bench
- `crates/engine/benches/reorderbuffer_memory.rs` - #4204 bench
- `crates/engine/benches/reorder_buffer_cache.rs` - #4180 bench
- `crates/engine/benches/parallel_dispatch_overhead.rs` - #4206 bench

### Audits and design notes
- `docs/audits/parallel-dispatch-wire-format-verification.md` - #4205
- `docs/design/reorderbuffer-metrics-and-bypass.md` - #1885
- `docs/design/capacity-multiplier-tuning.md` - #4204, #4206 tuning
- `docs/design/parallel-source-enumeration-eval.md:13-27` - #4173 audit
- `docs/design/lockfree-mpsc-drain-design.md:14-69` - #4214 follow-up
- `docs/design/iouring-rayon-submission.md:279-280` - #4214 cross-ref
- `docs/design/streaming-reorder-buffer.md` - reorder buffer design

### Upstream
- `target/interop/upstream-src/rsync-3.4.1/receiver.c:720` -
  `recv_files()` main loop (the sequential reference)
- `target/interop/upstream-src/rsync-3.4.1/receiver.c:240` -
  `receive_data()` per-file token apply (the inner loop reference)
