# FFB-W.a: `flush_workers` barrier file-boundary verification spec

Date: 2026-05-26
Status: design spec. No source files change as part of this task.
Depends on: DG-3 (BarrierState/SlotData split), PIP-9.b.4 (flush_workers
drain at file boundary), FFB-1/FFB-2 (barrier API + implementation).
Validates: barrier fires correctly at file boundaries under real load when
the PIP-9.b parallel receive-delta path is active.

## 1. Problem statement

The `ParallelDeltaApplier` barrier (FFB-1 design, FFB-2 implementation)
ensures that all in-flight workers for a given file have completed before
the receiver transitions to the next file. The barrier consists of:

- A per-file `BarrierState` (in-flight counter + `Condvar`) in
  `crates/engine/src/concurrent_delta/parallel_apply/slot_barrier.rs`.
- `flush_workers(ndx)` in `drain.rs:146` that parks on the `Condvar`
  until `inflight == 0`.
- `finish_file(ndx)` in `drain.rs:49` that bakes `flush_workers` in
  front of the `Arc::try_unwrap` payload reclaim.

The barrier has been unit-tested in isolation (FFB-2 tests in
`parallel_apply/mod.rs`) and stress-tested for concurrent `finish_file`
calls (DG-5.a). What has not been validated is the **file-boundary
transition under realistic receiver load**: multiple files processed in
sequence through the PIP-9.b parallel arm, where the barrier must fire
at every file boundary to prevent:

1. Chunks for file N leaking into file N+1's writer.
2. `checksum_verifier` being swapped while a worker still holds a
   reference to the old verifier's state.
3. `Arc::try_unwrap` failing because a worker's `DecrementGuard` has
   not yet dropped.
4. Per-file metadata (sparse state, temp-file rename, fsync) executing
   before all bytes are committed.

"Fires correctly at file boundaries" means: after `flush_workers(ndx)`
returns, the per-slot in-flight counter is zero, every submitted chunk
has been written to the destination through the reorder buffer, and the
payload `Arc<SlotData>` strong count is 1 (DashMap-only), so
`finish_file` can reclaim the writer on the first `try_unwrap` attempt
without spinning.

## 2. Background and prior work

### 2.1 FFB-1..4 barrier API

FFB-1 (`docs/design/ffb-1-applier-barrier-api.md`) designed the barrier
as Option A (per-file `flush_workers`) + Option D (baked into
`finish_file`), with Option B (`drain_inflight`) as a thin loop for
shutdown. FFB-2 (PR #4665) implemented the `(Mutex<usize>, Condvar)`
primitive and the `DecrementGuard` RAII pairing. FFB-3/FFB-4
(`docs/design/ffb-3-4-pip-2-closure-2026-05-21.md`) were satisfied by
design - Option D means existing callers get barrier semantics
automatically.

### 2.2 DG-3 restructure

DG-1 identified a release race: `DecrementGuard::drop` fires
`notify_all` before the guard's own `Arc<SlotBarrier>` drops, so
`finish_file`'s `Arc::try_unwrap` sees `strong_count >= 2`. DG-3.a
through DG-3.e (PRs #4826, #4841, #4845, #4855, #4874) split
`SlotBarrier` into `BarrierState` (counter + Condvar) and `SlotData`
(payload mutex), giving each its own `Arc` with an independent
strong-count trajectory. DG-4.a specifies removal of the spin-then-yield
workaround that masked the original race.

### 2.3 PIP-9.b parallel arm

PIP-9.b rebuilds the parallel receive-delta path properly:

- PIP-9.b.1 (`docs/design/pip-9b-call-shape-audit.md`) audited the
  sequential `apply_delta_tokens` call shape and documented 10
  equivalence invariants.
- PIP-9.b.2 (`docs/design/pip-9b2-cfg-dispatch-sketch.md`) chose a
  cfg-gated dispatch at the single cutover site in `sync.rs:241-253`.
- PIP-9.b.3 (`docs/design/pip-9-b-3-parallel-arm-feed-loop.md`)
  specifies the `DeltaWork` -> `DeltaChunk` feed loop.
- PIP-9.b.4 wires `flush_workers(ndx)` at the file boundary inside
  the `End` token handler of `apply_delta_tokens_parallel`.

FFB-W validates that the barrier works correctly at file boundaries
under real load once PIP-9.b.4 has landed.

## 3. Definition of "correct barrier firing"

A barrier firing is correct for file `ndx` when all of the following
hold at the instant `flush_workers(ndx)` returns:

| # | Invariant | Observable witness |
|---|-----------|-------------------|
| I1 | In-flight counter is zero | `BarrierState.inflight` lock reads 0 |
| I2 | Every submitted chunk has been written | `FileSlot.reorder.is_empty() == true` |
| I3 | Bytes written equals sum of chunk data lengths | `FileSlot.bytes_written == expected` |
| I4 | Payload `Arc<SlotData>` strong count is 1 | `Arc::strong_count(&entry.data) == 1` |
| I5 | No worker thread holds a `SlotHandle` for `ndx` | Implied by I1; the `DecrementGuard` RAII pairing ensures I1 implies no live handles |
| I6 | `finish_file(ndx)` succeeds on first `try_unwrap` | No `ApplierStillReferenced` error |

I4 depends on DG-3's structural split: the worker's `DecrementGuard`
holds `Arc<BarrierState>`, not `Arc<SlotData>`, so once the counter
reaches zero (I1) and the DashMap shard guard is released by
`finish_file`'s `remove`, the payload Arc is uncontended.

## 4. Test scenarios

### 4.1 Single file - baseline correctness

Register one file, submit N chunks (N in {1, 16, 256}), call
`finish_file`. Assert I1-I6. This is a smoke test that the barrier
fires at all; existing unit tests already cover this shape, but
FFB-W.a re-validates with the PIP-9.b.3 `DeltaChunk` adapter in the
loop (not raw `apply_one_chunk` calls) to exercise the production feed
path.

**Parameters:** 1 file, N chunks, 1 rayon worker.

### 4.2 Rapid file succession - boundary leak detection

Register files 0..K (K=100). For each file, submit a small number of
chunks (2-8) and immediately call `finish_file` before registering
the next file. The barrier must drain file N completely before file
N+1's first chunk is submitted to the applier. Assert I1-I6 per file.
Insert a sentinel byte pattern per file (e.g., file `i` uses byte
value `i % 256`) so any cross-file leak is detectable in the output
buffer.

**Parameters:** 100 files, 2-8 chunks each, 4 rayon workers.

**Key assertion:** collected output bytes for file `i` contain only
the sentinel byte `i % 256`. Any byte from file `i-1` or `i+1`
indicates a barrier leak.

### 4.3 Large file with many chunks - drain latency

Register one file, submit C chunks (C=4096) of varying size (64B-4KB),
dispatch through `apply_batch_parallel` in batches of 64. Call
`flush_workers` after the last batch. Measure wall-clock time from
the last `apply_batch_parallel` return to `flush_workers` return.
Assert I1-I6.

**Parameters:** 1 file, 4096 chunks, 8 rayon workers, batch size 64.

**Key assertion:** `flush_workers` returns within a bounded wall-clock
window (10 seconds on CI, 1 second on bare metal). A timeout indicates
the barrier is stuck - likely a lost decrement or a Condvar
notification that never fires.

### 4.4 Small files interleaved - concurrent slot lifecycle

Register 50 files. For each file, submit 1-3 chunks using
`apply_one_chunk`. After every 5 files, call `finish_file` on the
oldest 5 that have completed submission. This creates overlapping
slot lifecycles: some files are draining while others are still
receiving chunks. Assert I1-I6 per file at `finish_file` time.

**Parameters:** 50 files, 1-3 chunks each, 4 rayon workers, finish
batches of 5.

**Key assertion:** `finish_file` for file N never blocks waiting for
file M's workers (M != N). Each file's barrier is independent. To
validate independence, instrument the test with per-file timestamps:
`flush_workers(N)` must return before the next `register_file(N+5)`
call, but `flush_workers(N)` must not wait for any inflight workers
on files N+1..N+4.

### 4.5 Concurrency stress - overlapping boundary transitions

Spawn 8 producer threads, each owning a disjoint range of 100 file
indices (800 files total). Each producer runs the rapid-succession
pattern from scenario 4.2 on its own range. A shared
`ParallelDeltaApplier` instance backs all producers. Assert I1-I6
per file across all threads.

**Parameters:** 8 threads, 100 files per thread, 2-4 chunks per file,
8 rayon workers.

**Key assertion:** no `ApplierStillReferenced` errors, no
`SlotPoisoned` errors, no `UndrainedChunks` errors across all 800
files. DashMap shard contention under concurrent `register_file` /
`finish_file` / `apply_one_chunk` must not cause deadlocks or
barrier stalls.

### 4.6 Single-chunk files - degenerate boundary

Register 500 files, each receiving exactly one chunk. Call
`finish_file` immediately after `apply_one_chunk`. This is the
degenerate case where the barrier fires with at most one worker
ever in flight per file. Tests the zero-to-one-to-zero counter
transition under rapid slot creation and teardown.

**Parameters:** 500 files, 1 chunk each, 2 rayon workers.

**Key assertion:** every `finish_file` returns `Ok` within 50ms.
A stall on a single-chunk file indicates a counter that was
incremented but never decremented (lost `DecrementGuard`).

## 5. Verification approach

### 5.1 Instrumented assertions inside test harness

Each test scenario creates a `ParallelDeltaApplier` with the
standard `new(concurrency)` or `with_strategy(concurrency, strategy)`
constructor. The test drives the applier through the scenario's
file/chunk pattern and asserts invariants I1-I6 at each file boundary.

Invariant checks at `finish_file` boundaries:

```
// Before finish_file: verify flush_workers observes idle
applier.flush_workers(ndx)?;

// After finish_file: verify writer reclaimed and bytes match
let writer = applier.finish_file(ndx)?;
// ... assert output bytes match expected sentinel pattern ...
```

For I4 (payload Arc strong count = 1), the test probes the DashMap
entry via the `#[cfg(test)]` accessor pattern already used by
DG-3.d tests (`parallel_apply/mod.rs:1122-1126`):

```
let after_flush = applier
    .files
    .get(&ndx)
    .map(|guard| Arc::strong_count(&guard.value().data))
    .expect("slot present");
assert_eq!(after_flush, 1, "payload Arc must be DashMap-only after flush");
```

This probe runs after `flush_workers` but before `finish_file`
(which removes the entry), catching any regression that re-introduces
a payload-Arc clone on the worker's drop path.

### 5.2 Timestamps for barrier latency

Scenarios 4.3 and 4.4 record `Instant::now()` before and after
`flush_workers` / `finish_file` to bound the drain latency. The
bounds are generous for CI (10 seconds) to avoid flakiness on
loaded hosts, but the primary assertion is that the barrier returns
at all (no deadlock).

### 5.3 Per-file sentinel byte verification

Scenarios 4.2 and 4.5 use per-file sentinel bytes. Each chunk for
file `i` fills its `data: Vec<u8>` with `(i % 256) as u8`. The
in-memory sink (`VecSink` from the existing test module) captures
the output. After `finish_file`, the test asserts every byte in the
sink matches the file's sentinel.

### 5.4 Iteration count for statistical confidence

Scenarios 4.5 and 4.6 run in a loop of N iterations (N=100 by
default, overridable via `FFB_W_ITERATIONS` environment variable)
to amplify timing-dependent races. The success criterion is zero
failures across all iterations.

## 6. Concurrency stress details

### 6.1 Worker/finisher overlap window

The critical race window is between:

1. The last worker's `DecrementGuard::drop` calling
   `BarrierState::decrement_inflight` + `notify_all`, and
2. The finisher's `flush_workers` returning from the `wait_while`
   predicate and proceeding to `finish_file`'s `DashMap::remove` +
   `Arc::try_unwrap`.

Under DG-3's structural split, the worker's lingering
`Arc<BarrierState>` (which drops after the `notify_all` body returns)
is allocation-disjoint from the payload `Arc<SlotData>` that
`try_unwrap` targets. The test validates this disjointness under load
by checking I4 after every `flush_workers` return.

### 6.2 DashMap shard contention

Concurrent `register_file` (inserts) and `finish_file` (removes) on
different NDX values contend on DashMap shard locks. The barrier
design (`drain.rs:146-158`) clones the `Arc<BarrierState>` from the
shard guard and drops the guard before waiting on the Condvar. This
keeps the shard available to other NDX values during the wait. The
test validates this by running scenario 4.5 with 8 concurrent
producers - if the shard guard leaked into the wait, producers on
adjacent NDX values (same shard) would deadlock.

### 6.3 Spurious wakeup resilience

`BarrierState::wait_until_idle` uses `Condvar::wait_while` with a
predicate `|inflight| *inflight > 0`. The existing
`flush_workers_survives_spurious_wakeup` unit test fires manual
`notify_all` calls while the counter is non-zero. FFB-W.a extends
this by verifying the predicate holds under real load: scenario 4.3's
4096-chunk stream generates many real `notify_all` firings from
workers completing out of order, each of which wakes the flusher even
though the counter has not yet reached zero.

## 7. Integration with PIP-9.b feature flag

All FFB-W.a tests are gated behind `#[cfg(feature = "parallel-receive-delta")]`
so they only compile and run when the parallel arm is active. The test
file lives under `crates/engine/tests/` as an integration test:

```
crates/engine/tests/parallel_apply_ffb_wa_barrier_boundary.rs
```

CI runs this test as part of the `--features parallel-receive-delta`
matrix cell. The non-feature default build skips the file entirely.

The test module imports `ParallelDeltaApplier`, `DeltaChunk`, and
the `VecSink` test helper from the engine crate's `#[cfg(test)]`
public-for-test surface. No new public API is added; the test reaches
the same surface the DG-3.d and DG-5.a tests already exercise.

## 8. Relationship to DG-3 restructure

The DG-3 series (PRs #4826, #4841, #4845, #4855, #4874) is the
structural precondition for FFB-W.a. Without the `BarrierState` /
`SlotData` split, invariant I4 (payload Arc strong count = 1 after
flush) is not guaranteed: the worker's `DecrementGuard` would hold
an `Arc<SlotBarrier>` that shares the same allocation as the payload,
leaving `strong_count >= 2` in the window between `notify_all` and
the implicit field-drop glue.

FFB-W.a tests serve as regression witnesses for the DG-3 fix:

| DG-3 invariant | FFB-W.a scenario that regresses if violated |
|----------------|---------------------------------------------|
| Payload and barrier Arcs are distinct allocations | 4.5 (I4 check under concurrent finish) |
| `DecrementGuard` holds `Arc<BarrierState>`, not `Arc<SlotData>` | 4.3 (I4 check after 4096-chunk drain) |
| Worker drop does not extend payload strong count | 4.6 (I6: no `ApplierStillReferenced` on single-chunk files) |
| Spin-then-yield workaround is not load-bearing | All scenarios (none use explicit spin; rely on structural correctness) |

If a future change re-introduces the release race (e.g., by collapsing
`BarrierState` and `SlotData` back into one Arc), FFB-W.a tests will
fail on I4 before the spin-then-yield workaround can mask the regression.

## 9. Success criteria

### 9.1 Per-run criteria

Every scenario must pass with zero errors:

- Zero `ApplierStillReferenced` errors (I6).
- Zero `SlotPoisoned` errors.
- Zero `UndrainedChunks` errors (I2).
- Zero sentinel-byte mismatches (4.2, 4.5).
- Zero barrier timeouts (4.3, 4.6).
- Payload Arc strong count = 1 after every `flush_workers` (I4).

### 9.2 Statistical criteria

Scenario 4.5 (8-thread stress) and 4.6 (500 single-chunk files) run
for N iterations (default 100). The pass criterion is:

- Zero failures across N * total_files barrier firings.
- For N=100: scenario 4.5 fires 80,000 barriers (8 threads x 100 files
  x 100 iterations); scenario 4.6 fires 50,000 barriers (500 files x
  100 iterations). Zero failures across 130,000 barrier firings provides
  statistical confidence that the barrier is correct under load.

### 9.3 CI gate

FFB-W.a tests are required-green in the `--features parallel-receive-delta`
CI cell before PIP-9.f (default-on flip) can proceed. A red FFB-W.a
test blocks the default-on promotion.

## 10. Rollback procedure

If FFB-W.a tests surface a barrier failure:

1. Bisect to the commit that introduced the regression using
   `git bisect` with the failing scenario as the test command.
2. If the regression is in the barrier itself (BarrierState,
   DecrementGuard, flush_workers), revert the offending commit and
   file a DG-series follow-up.
3. If the regression is in the PIP-9.b parallel arm (feed loop,
   adapter, cfg dispatch), revert the PIP-9.b.3/4 commits and
   investigate the wire-up.
4. If the regression is timing-dependent and not reproducible locally,
   increase the iteration count via `FFB_W_ITERATIONS=1000` and run
   on a multi-core CI host to amplify the race.

## 11. Non-goals

- **Benchmark performance of the barrier.** FFB-W.a validates
  correctness, not throughput. Barrier latency benchmarks belong to
  PIP-9.f.1 (bake criterion).
- **Upstream interop parity.** FFB-W.a tests the barrier in isolation
  from the wire protocol. Byte-for-byte parity with upstream rsync
  under the parallel arm is PIP-9.b.6's scope.
- **Async barrier variant.** The engine is threaded; an async-friendly
  `drain_inflight_async` is out of scope until an async caller appears.
- **`drain_inflight` (global barrier) stress.** FFB-W.a focuses on
  per-file `flush_workers`. The global shutdown path
  (`drain_inflight`) is a thin loop over `flush_workers`; its
  correctness follows from per-file correctness. A dedicated
  `drain_inflight` stress test may be filed separately if shutdown
  races emerge.

## 12. Cross-references

- `crates/engine/src/concurrent_delta/parallel_apply/drain.rs` -
  `flush_workers` (line 146) and `finish_file` (line 49).
- `crates/engine/src/concurrent_delta/parallel_apply/slot_barrier.rs` -
  `BarrierState`, `SlotData`, `SlotEntry`.
- `crates/engine/src/concurrent_delta/parallel_apply/decrement_guard.rs` -
  `DecrementGuard` RAII drop.
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` -
  `ParallelDeltaApplier`, `DeltaChunk`, `FileSlot`.
- `docs/design/ffb-1-applier-barrier-api.md` - barrier API design.
- `docs/design/ffb-3-4-pip-2-closure-2026-05-21.md` - closure note.
- `docs/design/dg-2a-option-b-spec.md` - BarrierState/SlotData split
  spec.
- `docs/design/dg-4-a-spin-yield-removal.md` - spin workaround removal
  spec.
- `docs/design/dg-5a-concurrent-finish-file-stress-test.md` - concurrent
  finish_file stress test (validates DG-3 fix, complementary to FFB-W.a).
- `docs/design/pip-9b2-cfg-dispatch-sketch.md` - cfg-gated dispatch.
- `docs/design/pip-9-b-3-parallel-arm-feed-loop.md` - feed loop spec.
- `docs/design/pip-9b-call-shape-audit.md` - equivalence invariants.
