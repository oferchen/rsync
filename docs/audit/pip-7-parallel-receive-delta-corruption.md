# PIP-7 - parallel-receive-delta receiver corruption audit

Date: 2026-05-24
Status: **OPEN** - blocks v0.6.3 beta promotion. Audit only; fix
deferred to the wire-up PR that follows PIP-9.b (#2594).

Cross-references:

- PIP-7 design doc:
  `docs/design/pip-7-parallel-receive-delta-receiver-corruption-2026-05-22.md`.
- PIP-9 wire-up plan:
  `docs/design/pip-9-parallel-receive-delta-wire-up-2026-05-22.md`.
- PIP-9.a adapter (#2593, merged) - `DeltaWork` -> `DeltaChunk` shape
  bridge in `crates/engine/src/concurrent_delta/chunk_adapter.rs`.
- PIP-9.b rewire (#2594, pending) - production caller into
  `ParallelDeltaApplier` behind the feature flag; gated on this audit.
- PIP-9.c regression test (`parallel_threshold_trip` at
  `tests/parallel_threshold_trip.rs`) - byte-identity guard on master
  HEAD, today exercising only the sequential path.

## Symptom

After the threshold gate trips and the receiver promotes from
sequential dispatch to `ParallelDeltaPipeline` mid-batch, the
**first dispatched file** (`file_1.txt` in the historical
`parallel_threshold` scenario) ends up with the **wrong on-disk bytes**.
All subsequent files (`file_2.txt`, `file_3.txt`, ...) compare equal.

Concretely the CI run that surfaced the corruption
(<https://github.com/oferchen/rsync/actions/runs/26279354408>) reports

```
parallel-threshold: content mismatch for parallel_threshold/file_1.txt
```

for **both** matrix directions (upstream sender -> oc receiver and
oc sender -> upstream receiver). The bytes are real corruption, not
metadata drift: size and mtime compare equal, only `data` differs.

## Reproduction conditions

The bug only fires when every condition below is true. PIP-8's
container repro (`cargo build --release`) missed it; only the CI
`dist` profile is known to reproduce.

1. **Feature flag on.** `--features parallel-receive-delta` enabled on
   `cli`, `core`, `transfer`, and `engine` (see `Cargo.toml` lines
   `cli/Cargo.toml:16,48`, `core/Cargo.toml:66,99`,
   `transfer/Cargo.toml:111`, `engine/Cargo.toml:120`).
2. **Threshold tripped.** The receiver submits at least
   `DEFAULT_PARALLEL_THRESHOLD = 64` files (see
   `crates/transfer/src/delta_pipeline/mod.rs:54`) through the
   `ThresholdDeltaPipeline`. The 120-file `parallel_threshold/` tree
   from the deleted interop scenario satisfies this with room to
   spare.
3. **Real wire-up active.** The `ThresholdDeltaPipeline` (or
   `ParallelDeltaPipeline` directly) is the receiver-loop's drain. On
   master HEAD this path has 1 writer and 0 readers (PIP-7
   investigation in
   `docs/design/pip-7-parallel-receive-delta-receiver-corruption-2026-05-22.md`),
   so the corruption is dormant; PIP-9.b's rewire is the first
   patch that will re-arm it.
4. **`dist` profile.** Built with LTO + panic=abort + opt-level=z (the
   profile used by CI artifacts). `release` plus the same source did
   not reproduce in `rsync-profile`. Scheduling narrows the timing
   window enough that only the `dist` profile exposed it on master
   prior to the PIP-7 mitigation.

## Suspected mechanism

The bug lives at the seam between
`ThresholdDeltaPipeline::promote_to_parallel` and
`ParallelDeltaPipeline::with_capacity`. Two interacting
shape/order invariants conspire to corrupt `file_1`.

### Seam 1: promotion mid-batch resequences buffered work

`ThresholdDeltaPipeline::submit_work` accumulates `DeltaWork` items
into `ThresholdMode::Buffering(Vec<DeltaWork>)` until the threshold
trips. On the trip it calls
`promote_to_parallel(buffered: Vec<DeltaWork>)` which constructs a
fresh `ParallelDeltaPipeline::new_adaptive(...)` and then iterates the
buffered Vec, re-submitting each item:

`crates/transfer/src/delta_pipeline/threshold.rs:98-111`:

```
fn promote_to_parallel(&mut self, buffered: Vec<DeltaWork>) -> io::Result<()> {
    let worker_count = rayon::current_num_threads();
    let avg_target_size = average_target_size(&buffered);
    let mut parallel = if self.bypass_reorder {
        ParallelDeltaPipeline::new_bypass_adaptive(worker_count, avg_target_size)
    } else {
        ParallelDeltaPipeline::new_adaptive(worker_count, avg_target_size)
    };
    for item in buffered {
        parallel.submit_work(item)?;
    }
    ...
}
```

Inside `ParallelDeltaPipeline::submit_work`
(`crates/transfer/src/delta_pipeline/parallel.rs:162-173`) every
re-submitted item is **re-stamped** with a fresh sequence number:

```
fn submit_work(&mut self, mut work: DeltaWork) -> io::Result<()> {
    let seq = self.next_sequence;
    self.next_sequence += 1;
    work.set_sequence(seq);
    ...
}
```

So `file_1` enters the parallel pipeline at `sequence = 0`. The
`DeltaConsumer::spawn` (`crates/engine/src/concurrent_delta/consumer/mod.rs:186`)
spawns a background thread that drains via `drain_parallel_into` and
feeds results into a `ReorderBuffer` keyed on `sequence`. The
`ReorderBuffer` releases contiguous-from-zero runs first; sequence 0
is the first to be eligible for release.

### Seam 2: consumer publishes the first result before the writer is ready

`ParallelDeltaPipeline::with_capacity` constructs both halves in one
expression:

`crates/transfer/src/delta_pipeline/parallel.rs:98-107`:

```
fn with_capacity(capacity: usize) -> Self {
    let (work_tx, work_rx) = work_queue::bounded_with_capacity(capacity);
    let consumer = DeltaConsumer::spawn(work_rx, capacity);
    ...
}
```

`DeltaConsumer::spawn` immediately spawns two background threads
(`consumer/mod.rs:186` -> `spawn_inner`):

- **delta-drain** runs `drain_parallel_into` inside `rayon::scope`
  and starts processing whatever is in `work_rx` the instant the
  thread is scheduled.
- **delta-reorder** receives results and forwards in-order runs to
  the `mpsc::Receiver<DeltaResult>` the pipeline holds.

`ThresholdDeltaPipeline::promote_to_parallel` then **synchronously**
re-submits the buffered items in a tight `for item in buffered`
loop. The drain thread may already have processed `sequence = 0`
through `drain_parallel_into` and pushed the result into the
reorder buffer before the promotion call returns control to the
receiver loop.

That alone is not corruption - the reorder buffer holds the result
until the caller polls. The bug is what happens to the **bytes
attached to the `DeltaResult`**. The `DeltaWork` stamps a fresh
`sequence` but **the receiver-side basis file handle, basis offset,
and per-file write target are not re-bound by the re-stamping
loop**. The work items were originally built with sequence
unspecified (default 0) and a basis tied to per-file dispatch
state. When the threshold trip swings them into the parallel path,
the very first submission

- carries `sequence = 0`,
- has its basis-offset cache (engine-side `apply_delta` caches the
  most recent COPY base offset, see
  `MEMORY.md::project_delta_stats_wire_evidence` and
  `engine/src/delta/script.rs::apply_delta`) freshly initialised,
- runs concurrently with `delta-drain`'s `rayon::scope` already
  picking up additional items from `work_rx`.

The first item's worker therefore reads the **basis bytes for whichever
file the rayon worker grabbed first**, but writes the bytes through the
slot keyed on `file_1`'s `FileNdx`. Result: `file_1.txt` receives
`file_N.txt`'s reconstructed bytes, while `file_N`'s own slot fills
correctly because its later sequence is delivered after the basis
cache has had a chance to re-prime.

### Seam 3: per-file slot map keyed on `FileNdx` doesn't catch the swap

`ParallelDeltaApplier` keys per-file slots on `FileNdx` via a DashMap
(`parallel_apply/mod.rs:394`). The applier's
`apply_one_chunk`/`apply_batch_parallel` look up the slot by
`chunk.ndx`, which was set verbatim from the `DeltaWork::ndx` (see
the adapter at `chunk_adapter.rs:187-195`: `ndx = work.ndx()`). So
once the wrong basis bytes have been resolved into the
`DeltaChunk::data` field by the parallel work-queue worker, the
applier dutifully writes those bytes to the slot for `file_1`
because that is what the chunk's `ndx` says. The applier is
**correct in isolation** - the corruption is upstream, at the
`DeltaWork`-to-`DeltaChunk` resolution step inside the parallel
work-queue path.

## Why only file_1

Three factors point at file_1 specifically:

1. **First sequence wins the reorder release.** `ReorderBuffer`
   delivers `sequence = 0` first by construction. file_1 is the only
   item that gets dispatched in a regime where the drain thread is
   already racing rayon workers but has not yet stabilised any
   per-file basis state.
2. **Basis-offset cache is empty.** `engine::delta::script::apply_delta`
   caches the basis offset for sequential `COPY` tokens (see
   `MEMORY.md::project_delta_stats_wire_evidence`). The first chunk
   for the first file hits an empty cache and is therefore the only
   chunk where a stray basis-bytes read from the wrong file is
   observable as "wrong bytes" rather than a checksum mismatch
   caught by `ParallelApplyError::ChecksumMismatch`.
3. **Worker-pool warm-up.** `ParallelDeltaPipeline::with_capacity`
   sizes the work-queue against
   `rayon::current_num_threads()` (`threshold.rs:99`). The first
   call into the parallel path lazily initialises rayon's global
   pool; the first worker scheduled may be a thread that was
   already mid-flight on the receiver setup before promotion. That
   thread starts its life servicing `sequence = 0` (file_1) but
   resolves basis bytes against whatever buffer the worker was
   already holding.

## Suggested fix shape (do NOT implement here)

Three independent fixes worth weighing in the PIP-9.b wire-up PR:

1. **Stamp sequence at buffering time, not at promotion time.**
   Move sequence assignment out of
   `ParallelDeltaPipeline::submit_work` and into
   `ThresholdDeltaPipeline::submit_work` (and into the receiver's
   token loop for the direct-parallel path). Then `file_1` carries
   its original wire-order sequence through the promotion, and the
   reorder buffer cannot release it ahead of the per-file basis
   state that the receiver had already set up.
2. **Defer `DeltaConsumer::spawn` until after the re-submit loop.**
   `with_capacity` should return a constructed-but-not-spawned
   `ParallelDeltaPipeline`, and `promote_to_parallel` should
   re-submit the entire buffered batch first, then call a new
   `start()` that spawns the drain/reorder threads. This
   eliminates the race between the synchronous re-submit loop and
   the asynchronous drain.
3. **Bind basis-bytes resolution to the file handle, not the
   sequence.** Move basis-bytes I/O off the rayon worker (which
   may inherit a stale per-thread buffer) and onto the
   `ChunkBuilder` path that already runs in the receiver thread
   before dispatch
   (`crates/transfer/src/delta_pipeline/chunk_builder.rs`). The
   applier then only sees pre-resolved `DeltaChunk::data` and the
   parallel work-queue cannot cross-contaminate.

(1) and (2) are independent of (3) and should ship together; (3) is
the long-term fix and the only one that makes the parallel path
safe for `dist`-profile schedules.

## Verification plan

1. Run the new `tests/pip_7_parallel_receive_delta_corruption_repro.rs`
   test under `--features parallel-receive-delta` with
   `--release`/`--profile dist` until it reproduces locally. The test
   stays `#[ignore]` until a fix lands; remove the `ignore` attribute
   in the fix PR to make CI catch any regression.
2. Wire the test into the PIP-9.d CI matrix cell so it runs on every
   PR that touches `crates/engine/src/concurrent_delta/` or
   `crates/transfer/src/delta_pipeline/`.
3. Re-introduce the `parallel-threshold-trip` interop scenario in
   `tools/ci/run_interop.sh` only after both (a) the local repro
   test passes with the new code and (b) the test has been observed
   to reliably **fail** without the fix (i.e. confirms it is a real
   guard, not a vacuous pass).
4. After three consecutive green CI runs across all required-check
   matrices with the fix in place, re-add
   `parallel-receive-delta` to the `default = [...]` feature lists
   per the PIP-9 acceptance checklist.
