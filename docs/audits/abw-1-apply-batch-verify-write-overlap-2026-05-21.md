# ABW-1 - `apply_batch_parallel` verify-vs-write overlap potential

Date: 2026-05-21
Scope: read-only research for ABW-2 design decision
Tracked under: #2570

## Goal

`ParallelDeltaApplier::apply_batch_parallel`
(`crates/engine/src/concurrent_delta/parallel_apply.rs:515`) verifies every
chunk in parallel through a `par_iter`, then drains the verified chunks
through a single `for` loop that acquires the per-file `Mutex<FileSlot>` once
per chunk. The two phases never overlap: every verify completes before the
first write starts.

A pipelined design - verify batch `N+1` on the rayon pool while the writer
thread drains batch `N` - would, in principle, overlap CPU-bound verification
with I/O-bound writing. This audit catalogues the current shape, sketches the
pipelined alternative, and recommends whether ABW-2/3 should proceed.

The naming reflects RJN-2 (PR #4660 merged): the per-chunk entry point is now
`apply_one_chunk`; the batch entry point remains `apply_batch_parallel`.

## 1. Current shape

### 1.1 The two-phase loop

`crates/engine/src/concurrent_delta/parallel_apply.rs:515-542`:

```rust
let verified: Result<Vec<VerifiedChunk>, ParallelApplyError> = chunks
    .into_par_iter()
    .with_min_len(min_len)
    .map(|chunk| Self::verify_chunk(strategy.as_ref(), chunk))
    .collect();                                       // <-- barrier
let verified = verified?;

for v in verified {                                   // <-- serial drain
    let slot = self.slot_for(v.chunk.ndx)?;
    let mut slot = slot
        .lock()
        .map_err(|_| io::Error::other("parallel applier file slot poisoned"))?;
    slot.ingest(v.chunk)?;
}
```

Two phases, separated by `collect`:

1. **Verify phase (parallel).** `par_iter().map(verify_chunk)` fans across the
   ambient rayon pool, bounded by `self.concurrency` (the receiver pipeline
   sizes this from `rayon::current_num_threads()` or a CLI override).
   `verify_chunk` runs `strategy.compute(&chunk.data)` and compares against
   `chunk.expected_strong` when present
   (`crates/engine/src/concurrent_delta/parallel_apply.rs:632-652`).
2. **Write phase (serial).** A `for v in verified` loop acquires
   `slot_for(v.chunk.ndx)` then locks the per-file `Mutex<FileSlot>` for the
   duration of `slot.ingest(v.chunk)`. `FileSlot::ingest`
   (`crates/engine/src/concurrent_delta/parallel_apply.rs:248-258`) inserts
   into the per-file `ReorderBuffer`, drains every chunk that has become
   contiguous, and writes them to the destination.

The `collect` is a hard barrier. Every verify must return before the writer
sees a single chunk. Workers that finish their verifies early sit idle while
the slowest worker (or the slowest single chunk) finishes its `compute`.

### 1.2 Quantification - single file, N-chunk batch

Let `C` = wall-clock cost of one chunk's `verify_chunk`. Let `W` = wall-clock
cost of one chunk's `slot.ingest` (lock + reorder insert + writer
`write_all`). Let `K` = `self.concurrency` (rayon worker count).

Single-file, N-chunk batch on the current shape:

```
verify_wall = ceil(N / K) * C       # par_iter, K workers, near-perfect partition
write_wall  = N * W                 # serial loop, one per-file Mutex acquire per chunk
total_wall  = verify_wall + write_wall
```

Three representative settings for `N = 64`, `K = 8`:

| Setting             | verify_wall | write_wall | total |
|---------------------|-------------|------------|-------|
| Balanced `C = W`    | 8C          | 64C        | 72C   |
| CPU-bound `C = 4W`  | 32W         | 64W        | 96W   |
| I/O-bound `W = 4C`  | 8C          | 256C       | 264C  |

In all three the write phase dominates: verify scales with `K` because it
fans across the rayon pool, write does not. Parallel fan-out reduces the
verify side, but the serial drain remains O(N) per-Mutex-acquire.

### 1.3 Quantification - many-file batch

Cross-file writes can interleave on different `Mutex`es because `slot_for`
returns a distinct `Arc<Mutex<FileSlot>>` per `FileNdx`. But the current
write loop still iterates `verified` serially on the calling thread - it
never spawns the write step across multiple workers. So with `M` files all
of which need writes:

```
write_wall = N * W   (still serial, even with M-many files)
```

The "many files relax the per-file Mutex bottleneck" argument only applies
if the writer phase is multi-threaded. Today it is not. The `Mutex` allows
multi-threaded writing in principle, but `apply_batch_parallel` does not
exploit it.

## 2. Overlap potential

### 2.1 The pipelined design

Replace the `collect` + serial drain with a producer/consumer split:

```
+------------------+    bounded chan (cap = K)    +------------------+
|  par_iter verify | =========================> |  writer thread   |
|  on rayon pool   |    of VerifiedChunk         |  drains in order  |
+------------------+                              +------------------+
```

Concretely:

1. The caller spawns one writer thread (or reuses the calling thread for the
   writer role) and a `crossbeam_channel::bounded::<VerifiedChunk>(cap)`.
2. `par_iter().for_each(|chunk| send(verify_chunk(chunk)?))` replaces the
   `collect`. The verify fans across rayon workers as before, but each
   worker hands its result to the channel as soon as it is ready - no
   barrier.
3. The writer thread loops on `recv()` and runs the same `slot.lock();
   slot.ingest()` body the current serial loop runs.
4. Backpressure: the channel cap bounds how far verify can run ahead of
   write. `cap = K` (one per worker) is the minimal version;
   `cap = batch_size` lets verify drain fully before write starts (i.e.
   identical to current shape, useful as a feature-flag fallback).

Each verified chunk's write needs only its own verified digest, not the
verified digest of any other chunk. The data dependency the writer respects
is the per-file `chunk_sequence` order, which the `ReorderBuffer` inside
`FileSlot::ingest` already handles. The pipelined design does not weaken
that invariant.

### 2.2 Throughput estimate

In the steady state (after the first batch worth of chunks is in flight),
verify and write run concurrently. The bottleneck is `max(verify_rate,
write_rate)` rather than `verify_rate + write_rate`:

```
current_total  = verify_wall + write_wall
pipelined_total = max(verify_wall, write_wall) + startup
startup        ~= one chunk's verify cost (first chunk must verify before
                  the writer has anything to drain)
```

Speedup `S = current_total / pipelined_total`. For the three cases above:

| Case                              | verify_wall | write_wall | current | pipelined | S    |
|-----------------------------------|-------------|------------|---------|-----------|------|
| Balanced (C = W, K=8, N=64)       | 8C          | 64W=64C    | 72C     | 64C       | 1.13x|
| CPU-bound verify (C=4W, K=8, N=64)| 32W         | 64W        | 96W     | 64W       | 1.50x|
| I/O-bound write (W=4C, K=8, N=64) | 8C          | 256C       | 264C    | 256C      | 1.03x|

The classic "1.5-2x speedup of overlapped execution" only materialises when
verify and write costs are within roughly the same order. The current shape
is so write-dominated that pipelining buys little:

- **Balanced case**: ~13% speedup. Marginal; below the design-complexity
  threshold.
- **CPU-bound verify case**: ~50% speedup. Real win.
- **I/O-bound write case**: ~3% speedup. Below noise.

### 2.3 Where the gain is real

| Workload                          | C vs W relationship             | Pipelining gain |
|-----------------------------------|---------------------------------|-----------------|
| Large chunks, MD5/aarch64 software| C dominates (verify is heavy)   | ~25-50%         |
| Small chunks, XXH3/NVMe           | C and W close                   | ~10-20%         |
| Small chunks, slow HDD            | W dominates by 4x+              | < 5%            |
| Single-file, many-chunk batch     | per-file Mutex serialises writes| ~0% (writer stalls on lock; verify drains into a full channel) |
| Many-file batch with current code | writer still serial in caller   | ~0% (no multi-writer-thread today) |

Two observations the table makes explicit:

1. Single-file batches see zero gain. The writer thread holds the per-file
   Mutex for every chunk; verify queues fill instantly. The channel either
   blocks verify (no overlap) or grows unbounded (memory blowup).
2. Many-file batches without a multi-threaded writer also see zero gain
   beyond the single-thread case. The "many files relax the Mutex" argument
   only buys throughput if the writer side parallelises too - that is a
   different change (ABW-x for a future multi-writer applier) with its own
   ordering analysis.

## 3. Constraints

1. **Per-file wire-format determinism.** The golden byte tests in
   `crates/protocol/tests/golden/` and the proptest at
   `crates/engine/src/concurrent_delta/parallel_apply.rs:858+`
   (`random_chunk_sizes_and_permutations_match_sequential`) require that
   per-file bytes hit the writer in exactly the producer's submission
   order. Pipelining MUST preserve this: the writer must drain each file in
   `chunk_sequence` order, which today comes from the `ReorderBuffer`. A
   single writer thread that calls `slot.ingest` per chunk maintains the
   invariant; a multi-writer design would need a per-file work queue plus
   explicit serial-per-file dispatch.
2. **Memory cost.** A bounded channel of `VerifiedChunk` values pays
   `cap * sizeof(VerifiedChunk)` extra peak working set. With `cap = K = 8`
   workers and a 1 MiB max chunk, that is ~8 MiB extra. With `cap = N =
   batch_size` and a 1024-chunk batch of 1 MiB chunks, that is ~1 GiB -
   never use unbounded channels.
3. **Backpressure.** If verify outruns write (write-dominated workloads),
   the bounded channel blocks `par_iter` workers. That recovers the
   sequential bound but does NOT regress below it. If write outruns verify
   (verify-dominated workloads), the writer thread blocks on `recv()`,
   which is the case where the pipelining gain materialises.
4. **Error propagation.** The current `collect` short-circuits the entire
   batch on the first `ParallelApplyError::ChecksumMismatch`. The
   pipelined writer must propagate the same error semantics: any verify
   error or write error must abort the batch and surface a typed error.
   The cleanest shape is a `crossbeam_channel::Sender::<Result<...,
   ParallelApplyError>>` and a writer that bails on the first `Err`.
5. **Lifetime of the strategy `Arc`.** Already addressed in the current
   code: `Arc::clone(&self.strategy)` happens once before `par_iter`. A
   pipelined version would clone it the same way; no new constraint.
6. **Test surface.** The proptest and unit tests in
   `crates/engine/src/concurrent_delta/parallel_apply.rs:666+` and
   `crates/engine/tests/parallel_apply_concurrent.rs` should be re-run
   under the pipelined path. New tests: bounded-channel backpressure under
   write stalls; error propagation when verify fails mid-batch.

## 4. Recommendation

**Skip ABW-2/3 until BR-3i.f bench evidence shows verify cost and write
cost are within 2x of each other on the production workload.**

The reasoning:

- Today's `apply_batch_parallel` has zero production callers (per RJN-1
  call-site catalogue, PR #4656). The path is exercised only by tests and
  the BR-3i.f / `parallel_receive_delta_perf` benches.
- The pipelined design's expected gain is `current_total / max(verify,
  write)`. When one phase dominates by 2x or more, the gain is below ~25%
  and below the threshold where the design complexity (bounded channel,
  separate writer thread, error propagation rework, new test cells) is
  worth it.
- The BR-3i.f bench (`crates/engine/benches/parallel_verify_chunk.rs`)
  already measures verify throughput across MD4/MD5/XXH3 and workload
  shapes. Re-running it alongside `parallel_receive_delta_perf`'s
  parallel-apply cell yields the `verify_wall` and `write_wall` data
  points the recommendation depends on.
- Single-file batches (the worst case for the per-file Mutex) gain
  nothing from pipelining. Production callers driven by a per-file
  `chunk_builder` should be steered toward many-file batching anyway.

**Decision gate for ABW-2:**

1. Run BR-3i.f and `parallel_receive_delta_perf` on the rsync-profile
   container (NVMe + xxh3) and on the rsync-bench container (HDD-ish + MD5
   aarch64 software path).
2. Compute `ratio = verify_wall / write_wall` per workload cell.
3. If `0.5 <= ratio <= 2.0` on any production-relevant cell, proceed to
   ABW-2 (design doc covering the bounded-channel writer thread, error
   propagation, and test surface).
4. If `ratio < 0.5` or `ratio > 2.0` on every cell, mark the project
   memory page `project_apply_batch_write_serial.md` as "investigated;
   pipelining not justified by measurement" and close the line of work.

If ABW-2 proceeds, the implementation sequence is:

- **ABW-2** - design doc: `crossbeam_channel::bounded(cap)`, writer thread
  spawn/join, error propagation, backpressure analysis, memory ceiling.
- **ABW-3** - implementation: gated behind a feature flag or an internal
  selector; existing tests run on both paths; new tests cover
  backpressure and error semantics.
- **ABW-4** - bench: extend `parallel_receive_delta_perf` with a
  `pipelined_apply` cell; report before/after ratios across the three
  workload shapes.

## 5. References

- `crates/engine/src/concurrent_delta/parallel_apply.rs:515` -
  `apply_batch_parallel` entry point.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:248` -
  `FileSlot::ingest` (the per-file Mutex-protected drain).
- `crates/engine/src/concurrent_delta/parallel_apply.rs:632` -
  `verify_chunk` (CPU-bound work).
- `crates/engine/benches/parallel_receive_delta_perf.rs` - existing
  parallel-vs-sequential bench harness; ABW-4 extends this.
- `crates/engine/benches/parallel_verify_chunk.rs` - BR-3i.f
  cores-vs-throughput sweep; provides `verify_wall` measurements.
- `docs/audits/rjn-1-apply-chunk-parallel-call-sites-2026-05-21.md` -
  call-site catalogue confirming zero production callers today.
- `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md` - outer-map
  contention analysis; selected DashMap for the file-slot lookup.
- `project_apply_batch_write_serial.md` - the memory page that motivates
  this audit.
- `project_rayon_join_per_chunk_noop.md` - related observation about
  the per-chunk `rayon::join` no-op closure.
