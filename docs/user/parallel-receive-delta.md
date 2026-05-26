# Parallel receive-delta

This guide describes the parallel receive-delta feature, which parallelizes
the CPU-bound checksum verification step of the receiver-side delta apply
pipeline across multiple cores while preserving the per-file byte-stream
order that the rsync protocol requires.

## What it does

When a receiver reconstructs files from delta instructions, each chunk of
data must be checksum-verified before it can be written to the destination
file. In the default sequential path, verification and writing happen one
chunk at a time on a single thread - the receiver processes chunk N, verifies
it, writes it, then moves to chunk N+1.

The parallel receive-delta feature splits this into two phases:

1. **Parallel verify.** Chunks are fanned out across rayon worker threads for
   strong-checksum verification (MD5, XXH3, or whichever algorithm the
   handshake negotiated). Multiple chunks verify concurrently, spreading CPU
   work across available cores.

2. **Serialized write.** Once verification completes, a per-file `Mutex`
   serializes the actual I/O writes. A per-file reorder buffer re-establishes
   the original chunk submission order so bytes hit the destination file in
   the exact sequence the sender emitted, regardless of which rayon worker
   finished first.

This design means verification scales with core count while writes remain
deterministic and safe. Cross-file chunks are independent - chunks for
different files verify and write without contending on each other's locks.

## Current status

**Feature-gated, not default.** The feature compiles behind the
`parallel-receive-delta` Cargo feature flag and is excluded from the default
feature set. The production receiver token loop still takes the sequential
`DeltaWork` path on default builds.

### History

The feature has been through several integration attempts:

- **PIP-3/5** wired an initial dispatch into the receiver context and flipped
  the feature to default-on across `cli`, `core`, `transfer`, and `engine`.
- **PIP-4** surfaced receiver-side corruption - the first dispatched file in
  a directory wrote wrong bytes.
- **PIP-7** bisected the corruption to a dead scaffolding problem: the
  parallel pipeline had one writer (the enable setter) and zero readers (no
  production code path drained the pipeline output).
- **PIP-8** tore out the dead wiring, leaving the core types
  (`ParallelDeltaApplier`, `ParallelDeltaPipeline`, `DeltaConsumer`)
  compiled but unwired. The feature flag became a no-op.
- **PIP-9** is rebuilding the integration properly, routing the receiver's
  token loop through `ParallelDeltaApplier` via a real fan-out caller with
  observable production readers.

### Safety verification

The ABW-5 audit series verified the correctness of the verify-write
separation:

- **ABW-5.c** proved that the per-file `Mutex` scope prevents data races
  between the verify step of one batch and the write step of another, even
  under concurrent batch dispatch. The safety rests on three invariants:
  (1) verify is pure - it reads only owned chunk data and an immutable
  shared strategy, never touching the destination file or per-file Mutex;
  (2) writes are Mutex-guarded - the writer, reorder buffer, and
  bytes-written counter are protected as an atomic unit;
  (3) the reorder buffer restores chunk-sequence order regardless of Mutex
  acquisition order.
- **ABW-5.a** added debug-mode assertions that witness these invariants at
  runtime: the rayon collect barrier must resolve all chunks before writes
  begin, and bytes-written must not decrease after ingest.
- **ABW-5.d** documented the safety contract in the crate-level module docs.

## When to enable

The parallel path benefits workloads where checksum verification is a
measurable fraction of wall-clock time. This happens when:

- **High file count.** Transfers with hundreds or thousands of files provide
  enough independent chunks to keep multiple rayon workers busy. The
  amortization of per-file slot setup (DashMap insert, Mutex construction,
  reorder buffer allocation) is negligible at scale.

- **NVMe or fast SSD storage.** When the disk can absorb writes faster than
  a single core can verify checksums, the sequential path is CPU-bound. The
  parallel path moves verification off the critical path.

- **Multi-core systems.** The feature uses rayon's ambient thread pool. On
  machines with four or more cores, the verify step can run on idle cores
  that the sequential path leaves unused.

- **Network-bound delta transfers.** When the sender pushes data faster than
  a single core can verify, chunks queue up. Parallel verification drains
  the queue across multiple cores and keeps the pipeline from stalling.

## When NOT to enable

- **Low memory systems.** Each active file occupies a per-file slot in a
  `DashMap` containing a `Mutex<FileSlot>`, a reorder buffer (default
  capacity 64 entries), and a boxed writer. On memory-constrained systems
  this overhead may matter, especially with many concurrent files.

- **Few or very large files.** With only a handful of files, there are not
  enough independent chunks in flight to justify the rayon dispatch overhead.
  For one large file the write path is serialized on a single Mutex anyway,
  and the single-chunk `rayon::join` scheduling is a no-op wrapper.

- **Spinning disks (HDD).** On rotational media the bottleneck is seek
  latency and sequential bandwidth, not CPU. Parallel verification finishes
  faster than the disk can accept writes, so the reorder buffer fills and
  the pipeline stalls on I/O. The overhead of the concurrent machinery adds
  latency without throughput gain.

- **Single-core or dual-core systems.** Rayon's thread pool contends with
  the receiver's main loop for the same cores. The scheduling overhead
  exceeds the verification savings when there are no spare cores.

## Known limitations

1. **Single writer per file (Mutex serialized).** Chunks for the same file
   are written under a per-file `Mutex<FileSlot>`. The verify step
   parallelizes across cores, but the write step remains serial per file.
   A future pipelined design could overlap write batch N with verify batch
   N+1, but this would require the write loop to run on a dedicated thread
   and changes the error-propagation and backpressure model.

2. **Reorder buffer memory overhead.** Each file maintains a reorder buffer
   with a fixed capacity (default 64 entries). If chunks arrive far out of
   order, the buffer fills and the producer blocks. The capacity is tunable
   via `ParallelDeltaApplier::with_per_file_reorder_capacity` but is not
   exposed as a CLI flag.

3. **Feature flag required.** The feature does not compile into default
   builds. You must opt in at build time (see Configuration below).

4. **PIP-9 integration in progress.** The production receiver token loop
   does not yet dispatch through the parallel applier. The core types are
   compiled and exercised by benchmarks and tests, but the end-to-end wire
   integration is pending PIP-9.b completion.

5. **`finish_file` spin-then-yield window.** After `flush_workers` drains
   the in-flight counter to zero, a brief window exists where the worker's
   `SlotHandle` drop has notified the Condvar but has not yet released its
   `Arc` clone. `finish_file` spins (up to 1,000 iterations) then yields
   until the strong count reaches 1. This window is typically nanoseconds
   but is observable under load on Windows.

6. **No spill-to-disk for the parallel path.** The `SpillableReorderBuffer`
   and `SpillPolicy` configuration exist in the concurrent delta module but
   are wired to the `DeltaConsumer` path, not to `ParallelDeltaApplier`
   directly. Under the parallel applier, reorder buffers are memory-only.

## Performance expectations

- **Verify parallelizes.** Strong-checksum computation (MD5, XXH3)
  distributes across rayon workers. On an 8-core machine, the verify step
  for a batch of chunks runs up to 8x faster than sequential verification,
  bounded by rayon's work-stealing overhead and the minimum chunk-length
  heuristic.

- **Writes remain serial per file.** Every write to a file's destination
  passes through a single `Mutex<FileSlot>`. Two threads writing to the
  same file serialize on the Mutex. The reorder buffer inside the slot
  handles interleaved chunk sequences - if thread A holds chunk 5 and
  thread B holds chunk 4, the buffer queues chunk 5 and waits for chunk 4
  before draining both in order.

- **Cross-file independence.** Chunks for different files contend only on
  DashMap shard locks during the brief slot lookup. The per-file Mutex is
  independent per file, so N files under parallel apply achieve close to N
  independent write streams.

- **Batch vs single-chunk entry points.** `apply_batch_parallel` collects a
  `Vec<DeltaChunk>` through `into_par_iter` and fans verifies out subject
  to the concurrency limit. `apply_one_chunk` schedules a single verify via
  `rayon::join` - a single-worker scheduling primitive, not cross-chunk
  parallelism. The batch path is where the real multi-core wins live.

## Configuration

### Build-time

Enable the feature when building oc-rsync:

```sh
cargo build --release --features parallel-receive-delta
```

The feature cascades through the crate graph:

```
workspace  parallel-receive-delta
  -> cli/parallel-receive-delta
  -> core/parallel-receive-delta
  -> transfer/parallel-receive-delta
  -> engine/parallel-receive-delta
```

There is no runtime CLI flag to toggle the feature - it is a compile-time
gate. When the feature is absent, the parallel-apply types are not compiled
and the receiver takes the sequential path unconditionally.

### Tuning knobs (API-level)

These are not exposed as CLI flags today. They are available to callers that
construct a `ParallelDeltaApplier` directly:

| Knob | Default | Description |
|------|---------|-------------|
| `concurrency` | `0` (use ambient rayon pool) | Maximum chunks dispatched to rayon in parallel per batch. |
| `per_file_reorder_capacity` | `64` | Per-file reorder buffer size. Controls how far out of order chunks can arrive before the producer blocks. |
| `strategy` | MD5 (seed 0) | Strong-checksum algorithm for the verify step. Overridden by `with_strategy` when the receiver pipeline threads the negotiated algorithm. |

## Roadmap

| Task | Description | Status |
|------|-------------|--------|
| PIP-9.b.1-3 | Wire `ParallelDeltaApplier` into the receiver token loop via RJN-3 fan-out. | In progress |
| PIP-9.b.4 | `flush_workers` drain - ensure every registered slot reaches zero in-flight before the transfer phase closes. | Pending |
| PIP-9.c | Re-validate `parallel-threshold-trip` interop scenario under dist profile. | Blocked on PIP-9.b |
| PIP-9.d | CI matrix cell: `--profile dist --features parallel-receive-delta` nextest run. | Blocked on PIP-9.b |
| PIP-9.e | Confirm PIP-7 receiver-corruption fix against the parallel-applier path. | Blocked on PIP-9.b |
| PIP-9.f | Bake criterion gate - N consecutive green CI cycles before default-on flip. | Blocked on PIP-9.c-e |
| PIP-10 | End-to-end validation: full interop suite through the parallel path against upstream 3.0.9, 3.1.3, 3.4.1, 3.4.2. | Future |
| Default-on | Flip `parallel-receive-delta` into workspace default features. | After PIP-9.f bake window passes |

## Architecture reference

The implementation lives in `crates/engine/src/concurrent_delta/parallel_apply/`:

- `mod.rs` - `ParallelDeltaApplier`, `DeltaChunk`, `FileSlot`, `SlotHandle`,
  `verify_chunk`, `apply_one_chunk`.
- `batch.rs` - `apply_batch_parallel` - the batched entry point that fans
  verify across rayon's `into_par_iter`.
- `drain.rs` - `finish_file`, `flush_workers` - per-file drain and barrier
  primitives.
- `slot_barrier.rs` - `SlotBarrier`, `SlotData`, `BarrierState`, `SlotEntry`
  - per-slot synchronization (in-flight counter, Condvar, Mutex).
- `decrement_guard.rs` - RAII guard that decrements the in-flight counter on
  drop.

Safety audit: `docs/audit/abw-5c-verify-write-mutex-scope.md`.
Design doc: `docs/design/pip-9-parallel-receive-delta-wire-up-2026-05-22.md`.
