# FFB-W.b: Validate `drain_inflight` under rapid file succession (< 1ms per file)

Date: 2026-05-26
Status: design spec. No source files change as part of this task.
Depends on: DG-3 (BarrierState/SlotData split), FFB-1/FFB-2 (barrier API +
implementation), FFB-W.a (file-boundary correctness under normal load).
Validates: `drain_inflight` and `flush_workers` fire correctly when the
per-file barrier cycle completes in sub-millisecond - often sub-microsecond -
intervals, exercising edge cases in the Condvar notification path that
normal-speed transfers never reach.

## 1. Problem statement

`ParallelDeltaApplier::drain_inflight` iterates every registered file
index and calls `flush_workers` on each (drain.rs:603-611).
`flush_workers` clones the slot's `Arc<BarrierState>` and parks on its
`Condvar` via `wait_until_idle` until the per-file in-flight counter
reaches zero (drain.rs:146-158, slot_barrier.rs:214-224).

When files are very small - zero-length or single-chunk (< 64 bytes) -
the entire barrier cycle (register, dispatch, decrement, notify, wait,
finish) completes in microseconds. At 10K files processed back-to-back,
the in-flight counter transitions 0 -> N -> 0 thousands of times per
second. Three race conditions become plausible under this cadence that
are invisible at normal transfer speeds:

### 1.1 Missed zero-crossing

The `Condvar::wait_while` predicate in `wait_until_idle` checks
`|inflight| *inflight > 0`. If the counter increments and decrements
back to zero between the time the flusher acquires the mutex and enters
the wait, the `notify_all` fires before the flusher is parked. The
flusher then parks on a Condvar that will never fire again for this
cycle. Under the current `Condvar::wait_while` contract, this is safe
because the predicate is re-evaluated before parking (the counter is
already zero, so `wait_while` returns immediately). But a regression
that moves the counter check outside the mutex guard would surface here
first.

### 1.2 Spurious wakeup with stale state

POSIX and Windows Condvar implementations may wake the waiter
spuriously. `wait_while` handles this by re-checking the predicate. But
under rapid succession, the waiter may wake from a spurious signal,
observe `inflight > 0` (because a new batch of workers was dispatched
for the same file index before `finish_file` removed the slot), and
re-park. The concern is not correctness of re-parking itself - that is
correct - but whether the re-park latency under rapid fire causes
observable drain stalls (predicate re-check + Condvar re-park is
non-trivial on Windows where the scheduler granularity is 15.6ms by
default).

### 1.3 Stale state after rapid re-register

If `finish_file(ndx)` removes the slot and `register_file(ndx)` re-
inserts a fresh `SlotEntry` for the same NDX within the same
microsecond, a concurrent `drain_inflight` snapshot (taken before the
remove) holds a stale `FileNdx` key. The subsequent `flush_workers`
lookup finds the new slot (same key, new `Arc<BarrierState>`) and waits
on its counter, which may or may not be zero. This is semantically
correct - `drain_inflight` documented that it "drains the workers that
exist now, not new submissions" - but the test must verify that the
barrier does not deadlock or return an error when the slot identity
changes underneath a snapshot-based iteration.

## 2. Background and prior work

### 2.1 FFB-W.a: file-boundary correctness

FFB-W.a validated that `flush_workers` fires correctly at file
boundaries under realistic receiver load with multi-chunk files (2-4096
chunks per file). It tested sentinel-byte isolation, payload Arc strong-
count invariants, and DashMap shard contention. FFB-W.a explicitly
deferred `drain_inflight` stress to a follow-up (section 11, non-goals).

### 2.2 FFB-W.d: barrier overhead bench

FFB-W.d measures the Condvar signal/wait overhead in isolation (no I/O),
sweeping worker count (4/8/16/32) and file count (100/1K/10K). Its
pass/fail criterion is < 10us per-file barrier latency at 8 workers.
FFB-W.b complements FFB-W.d by testing correctness rather than
throughput: FFB-W.d measures how fast the barrier fires; FFB-W.b
verifies it fires correctly under adversarial timing.

### 2.3 DG-3 structural split

DG-3 split `SlotBarrier` into `BarrierState` (counter + Condvar) and
`SlotData` (payload mutex), giving each its own `Arc`. The split
eliminated the release race where `DecrementGuard::drop` fired
`notify_all` while the worker's `Arc<SlotBarrier>` clone was still live.
FFB-W.b validates that the split remains correct when the barrier
cycles thousands of times per second - the DG-3 tests exercised single-
file or moderate-load scenarios, not the rapid-fire edge.

### 2.4 Existing `drain_inflight_drains_all_files` test

The existing unit test (`parallel_apply/mod.rs:985-1032`) registers 6
files, holds each SlotHandle for 40ms, and asserts `drain_inflight`
blocks until all workers drop. The hold duration (40ms) is three orders
of magnitude slower than the sub-millisecond regime FFB-W.b targets.

## 3. Definition of "rapid file succession"

A file is processed in "rapid succession" when the wall-clock interval
between `register_file(ndx)` and `finish_file(ndx)` is less than 1ms.
This arises in production when:

- Zero-length files (no chunks submitted; `finish_file` fires
  immediately after register).
- Single-chunk files with < 64 bytes of data (one `apply_one_chunk`
  call, one rayon verify, one write, one barrier drain).
- Mixed workloads where small files interleave with large files
  (the small files complete while large-file workers are still
  in flight on other slots).

The sub-microsecond barrier cycle is the extreme case: files where
`register_file` + `finish_file` back-to-back (zero chunks) exercises
the 0 -> 0 counter path with no workers dispatched at all. The Condvar
path is then `wait_while(|n| *n > 0)` with `n == 0` on entry, which
must return immediately.

## 4. Test scenarios

All scenarios use an in-memory `VecSink` writer (no disk I/O) to
isolate barrier behavior from filesystem latency.

### 4.1 Zero-length file storm

Register and immediately finish 10,000 files with zero chunks each.
No workers are dispatched; the in-flight counter never leaves zero.
This exercises the degenerate barrier path where `flush_workers`
observes `inflight == 0` on entry and returns without parking.

**Parameters:** 10,000 files, 0 chunks each, 4 rayon workers (idle).

**Key assertions:**

- Every `finish_file` returns `Ok` (no `ApplierStillReferenced`,
  no `SlotPoisoned`).
- Total wall-clock for all 10,000 files < 500ms (validates no
  accidental Condvar park on the zero-counter path).
- `drain_inflight` called after the loop returns immediately
  (no registered files remain).

### 4.2 Single-chunk rapid fire

Register 10,000 files. For each file, submit one chunk of 64 bytes
via `apply_one_chunk`, then immediately call `finish_file`. The
barrier cycle is: increment to 1 (SlotHandle), rayon verify (near-
instant for 64 bytes), decrement to 0 (DecrementGuard drop),
`flush_workers` observes zero. This exercises the 0 -> 1 -> 0
transition under back-pressure from the next file's `register_file`.

**Parameters:** 10,000 files, 1 chunk of 64 bytes each, 4 rayon
workers.

**Key assertions:**

- Every `finish_file` returns `Ok`.
- Per-file sentinel byte verified: file `i` uses byte `(i % 256)`,
  output must contain exactly 64 bytes of that sentinel.
- Total wall-clock < 5 seconds on CI (generous bound for loaded
  runners).
- No `ApplierStillReferenced` errors (validates the DG-3 split
  under rapid `Arc::try_unwrap` pressure).

### 4.3 Mixed zero and single-chunk interleaved

Register 10,000 files. Even-indexed files receive zero chunks;
odd-indexed files receive one 64-byte chunk. Process in index order.
This interleaves the degenerate (zero-counter) and normal (0->1->0)
barrier paths, testing that the Condvar state from one file does
not leak into the next.

**Parameters:** 10,000 files, alternating 0/1 chunks, 4 rayon
workers.

**Key assertions:**

- Every `finish_file` returns `Ok`.
- Odd-indexed files contain exactly 64 bytes of sentinel; even-
  indexed files contain zero bytes.
- No barrier stalls (total wall-clock < 5 seconds).

### 4.4 Concurrent rapid drain with active producers

Spawn 4 producer threads. Each producer owns a disjoint range of
2,500 file indices (10,000 total). Each producer runs the single-
chunk rapid-fire pattern from scenario 4.2 on its range. A shared
`ParallelDeltaApplier` backs all producers. After all producers
join, call `drain_inflight` on the shared applier (which should
return immediately since every file was already finished).

**Parameters:** 4 threads, 2,500 files/thread, 1 chunk of 64 bytes
per file, 8 rayon workers.

**Key assertions:**

- Zero errors across all 10,000 files.
- `drain_inflight` returns `Ok` immediately after producers join.
- No DashMap shard deadlocks (producers and `drain_inflight`
  contend on shard locks for register/remove/iterate).

### 4.5 Interleaved `drain_inflight` during active production

Spawn 2 producer threads running the single-chunk pattern on
disjoint 5,000-file ranges. Spawn a third "drainer" thread that
calls `drain_inflight` every 100 files (sleeping 1ms between
calls). The drainer and producers run concurrently on the same
applier. This tests the snapshot-based iteration in
`drain_inflight` (line 606) against concurrent register/remove
mutations.

**Parameters:** 2 producer threads, 5,000 files each, 1 drainer
thread calling `drain_inflight` 100 times, 8 rayon workers.

**Key assertions:**

- Zero errors from any thread.
- `drain_inflight` never deadlocks or blocks indefinitely (each
  call completes within 2 seconds; timeout kills the test).
- After both producers join, a final `drain_inflight` returns
  `Ok` immediately.
- Validates the stale-snapshot concern from section 1.3: the
  drainer's key snapshot may reference files already removed by
  a producer; `flush_workers` returns `Ok(())` for absent slots
  (drain.rs:153-156).

### 4.6 Worker thread sweep: 1/4/8/16

Scenarios 4.2 and 4.4 are parameterized over worker thread counts
{1, 4, 8, 16} to expose concurrency-dependent race windows.

- **1 worker:** serialized verify+write; the barrier fires
  synchronously on the calling thread. No Condvar park expected.
- **4 workers:** moderate contention on the inflight Mutex.
- **8 workers:** typical production configuration.
- **16 workers:** high contention; the `notify_all` wakes 15 idle
  workers plus the flusher, amplifying spurious-wakeup frequency.

Each configuration runs the full 10,000-file workload. Pass criterion
is identical across all worker counts: zero errors, zero stalls.

### 4.7 Rapid re-register same NDX

Register file NDX 0, finish it (zero chunks), then re-register NDX 0
with a fresh writer. Repeat 10,000 times. This exercises the
DashMap insert/remove/insert cycle on the same key and validates that
`flush_workers` never observes stale `BarrierState` from a previous
generation of the same NDX.

**Parameters:** 1 file NDX recycled 10,000 times, 0 chunks each, 4
rayon workers.

**Key assertions:**

- Every `finish_file` returns `Ok`.
- `register_file` never returns "already registered" (the previous
  slot was removed before re-registration).
- No stale `BarrierState` references leak across generations (the
  `Arc<BarrierState>` from generation N must have strong count 1
  after `finish_file` returns, regardless of how fast generation
  N+1 is registered).

## 5. Timing instrumentation

### 5.1 Barrier cycle time histogram

Each scenario records `Instant::now()` around the `flush_workers` (or
`finish_file`) call. The per-file drain latency is collected into a
histogram (linear bins: 0-1us, 1-10us, 10-100us, 100us-1ms, 1-10ms,
10-100ms, > 100ms). The histogram is emitted to stderr at the end of
each scenario for diagnostic review.

```rust
struct DrainHistogram {
    bins: [u64; 7],  // counts per bin
    max_ns: u64,     // worst-case single drain
    sum_ns: u64,     // total drain time
    count: u64,      // number of drains
}
```

### 5.2 Max drain latency assertion

The worst-case single `flush_workers` / `finish_file` latency must be
less than 10ms at p99 across all 10,000 files in scenarios 4.1-4.3.
This bound is generous enough for CI but catches pathological stalls
(e.g., a Condvar that parks for a full scheduler quantum on Windows).

For scenario 4.5 (concurrent drain), the per-call `drain_inflight`
latency bound is 2 seconds (it may legitimately wait for in-progress
workers on files that have not yet been finished by producers).

### 5.3 End-to-end throughput rate

Each scenario computes files-per-second: `file_count / total_elapsed`.
The rate is logged but not asserted - FFB-W.b is a correctness test,
not a performance bench. The rate provides diagnostic signal if a
regression introduces unexpected overhead.

## 6. Specific race conditions validated

### 6.1 Spurious wakeup under rapid fire

Validated by: scenarios 4.2, 4.3, 4.6 (16 workers).

With 16 rayon workers and 10,000 single-chunk files, `notify_all`
fires once per file. Each `notify_all` wakes every thread parked on
the slot's Condvar. If the flusher is between `wait_while` iterations
(re-acquiring the mutex after a spurious wake), a concurrent
`notify_all` from the same or a different file's worker can fire. The
`wait_while` predicate must re-evaluate `inflight > 0` under the mutex
and only return when the counter is truly zero.

The test catches a failure mode where the predicate is accidentally
inverted or the mutex guard is dropped before the check: the flusher
would return while workers are still in flight, and `finish_file`'s
`Arc::try_unwrap` would fail with `ApplierStillReferenced`.

### 6.2 Missed zero-crossing (notify before park)

Validated by: scenario 4.1 (zero-length files), scenario 4.7 (rapid
re-register).

When no chunks are submitted, `flush_workers` is called with
`inflight == 0` on entry. The `wait_while` predicate evaluates to
`false` immediately and returns without parking. If a regression
changed the predicate to `inflight >= 0` or moved the mutex acquire
after the park, the flusher would park indefinitely on a Condvar
that nobody will notify.

The test catches this by asserting `finish_file` completes within
500ms for 10,000 zero-length files. A single deadlocked file would
cause the test to time out.

### 6.3 Stale state after rapid re-register

Validated by: scenario 4.5 (interleaved drain during production),
scenario 4.7 (same-NDX recycling).

When `drain_inflight` snapshots the key set (line 606), concurrent
producers may remove and re-register the same NDX. The subsequent
`flush_workers(ndx)` call finds the new slot (or no slot, if the
remove raced). Both outcomes must be handled:

- **New slot found:** `flush_workers` waits on the new slot's
  `BarrierState`, which may or may not have in-flight workers. The
  wait is correct by construction (the new slot's counter is
  independent of the old).
- **No slot found:** `flush_workers` returns `Ok(())` immediately
  (drain.rs:153-156). The absent slot is treated as idle.

The test catches a failure mode where `flush_workers` holds a stale
`Arc<BarrierState>` from a previous generation and waits on a Condvar
that nobody will notify (because the workers decremented the new
generation's counter, not the old one's).

## 7. Platform considerations

### 7.1 Windows scheduler granularity

The default Windows scheduler quantum is 15.6ms. A `Condvar::wait`
that parks the thread may not wake for up to 15.6ms after
`notify_all`, even if no actual work remains. Under rapid file
succession, this means:

- `flush_workers` for a single zero-length file could take 15.6ms
  if the OS decides to deschedule the flusher between the
  `wait_while` predicate check and the return.
- 10,000 files at 15.6ms/file = 156 seconds, which exceeds any
  reasonable test timeout.

In practice, `wait_while` with `inflight == 0` returns without
parking (the predicate is false on entry), so the scheduler quantum
does not apply. The concern is only relevant if the flusher actually
parks - which happens when `inflight > 0` at entry time. For single-
chunk files, the park duration is bounded by the rayon verify latency
(microseconds for 64 bytes), not the scheduler quantum.

The test timeout for scenarios 4.2 and 4.3 is set to 30 seconds
(5 seconds on Linux/macOS, 30 seconds on Windows) to accommodate
worst-case scheduler behavior while still catching genuine stalls.

### 7.2 Linux futex-based Condvar

On Linux, `std::sync::Condvar` uses `futex(FUTEX_WAIT)` /
`futex(FUTEX_WAKE)`. The wake latency is typically < 10us. Under
rapid fire, the flusher may never actually park because the
`wait_while` predicate evaluates to `false` before `futex` is
called. This is the fast path and is validated by scenario 4.1.

### 7.3 macOS `pthread_cond_timedwait` behavior

macOS uses `pthread_cond_wait` which has similar semantics to Linux
futex but slightly higher base latency due to the Mach scheduler.
Spurious wakeups are more common on macOS than on Linux. Scenario 4.6
with 16 workers amplifies the spurious-wakeup rate to stress the
predicate re-check path on this platform.

## 8. Implementation structure

### 8.1 Test file location

```
crates/engine/tests/parallel_apply_ffb_wb_rapid_drain.rs
```

Integration test under the engine crate, gated behind
`#[cfg(feature = "parallel-receive-delta")]` (same gate as FFB-W.a).

### 8.2 Test module layout

```rust
// crates/engine/tests/parallel_apply_ffb_wb_rapid_drain.rs

#![cfg(feature = "parallel-receive-delta")]

use std::sync::Arc;
use std::time::{Duration, Instant};

// Imports from engine crate's test-visible surface.
// ParallelDeltaApplier, DeltaChunk, FileNdx, VecSink.

mod helpers {
    // DrainHistogram, per-file sentinel builder, timeout wrapper.
}

// 4.1
#[test]
fn zero_length_file_storm_10k() { /* ... */ }

// 4.2 (parameterized by worker count via inner fn)
#[test]
fn single_chunk_rapid_fire_1w()  { /* ... */ }
#[test]
fn single_chunk_rapid_fire_4w()  { /* ... */ }
#[test]
fn single_chunk_rapid_fire_8w()  { /* ... */ }
#[test]
fn single_chunk_rapid_fire_16w() { /* ... */ }

// 4.3
#[test]
fn mixed_zero_and_single_chunk_interleaved_10k() { /* ... */ }

// 4.4
#[test]
fn concurrent_rapid_drain_4_producers() { /* ... */ }

// 4.5
#[test]
fn interleaved_drain_during_active_production() { /* ... */ }

// 4.6 - covered by parameterized 4.2 tests above

// 4.7
#[test]
fn rapid_re_register_same_ndx_10k() { /* ... */ }
```

### 8.3 Iteration count for statistical confidence

Scenarios 4.4 and 4.5 run in a loop of N iterations (default 20,
overridable via `FFB_WB_ITERATIONS` environment variable). The
reduced default (vs FFB-W.a's 100) reflects the higher per-iteration
cost: 10,000 files/iteration x 20 iterations = 200,000 barrier
firings per scenario, which provides sufficient statistical coverage
for timing-dependent races while keeping CI runtime under 60 seconds.

### 8.4 Timeout wrapper

Each scenario is wrapped in a thread with a join timeout:

```rust
fn run_with_timeout<F: FnOnce() + Send + 'static>(
    name: &str,
    timeout: Duration,
    f: F,
) {
    let handle = std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .expect("spawn test thread");
    // Deadlock detection: if the test thread does not join
    // within `timeout`, the barrier is stuck.
    match handle.join_timeout(timeout) {
        Ok(()) => {},
        Err(_) => panic!("{name}: test did not complete within {timeout:?} - barrier deadlock suspected"),
    }
}
```

Default timeouts:

| Scenario | Linux/macOS | Windows |
|----------|-------------|---------|
| 4.1 (zero-length) | 5s | 30s |
| 4.2 (single-chunk) | 10s | 30s |
| 4.3 (mixed) | 10s | 30s |
| 4.4 (concurrent) | 15s | 45s |
| 4.5 (interleaved drain) | 30s | 60s |
| 4.7 (re-register) | 5s | 30s |

Windows timeouts are 3-6x longer to account for scheduler granularity.

## 9. Success criteria

### 9.1 Per-scenario pass/fail

| Criterion | Applies to | Threshold |
|-----------|-----------|-----------|
| Zero `ApplierStillReferenced` errors | All scenarios | 0 occurrences |
| Zero `SlotPoisoned` errors | All scenarios | 0 occurrences |
| Zero `UndrainedChunks` errors | All scenarios | 0 occurrences |
| Sentinel byte correctness | 4.2, 4.3, 4.4 | 100% match |
| No deadlocks (timeout) | All scenarios | Completes within platform timeout |
| Drain latency p99 < 10ms | 4.1, 4.2, 4.3 | Histogram bin 10-100ms has < 1% of samples |
| `drain_inflight` post-production returns immediately | 4.4 | < 50ms |
| No "already registered" on re-register | 4.7 | 0 occurrences |

### 9.2 Statistical criteria

Scenarios 4.4 and 4.5 run for N iterations (default 20). The pass
criterion is zero failures across all iterations:

- 4.4: 20 iterations x 10,000 files = 200,000 barrier firings.
- 4.5: 20 iterations x 10,000 files + 20 x 100 `drain_inflight`
  calls = 202,000 barrier-related operations.

Zero failures across 400,000+ operations provides high confidence
that the barrier is correct under rapid succession.

### 9.3 CI gate

FFB-W.b tests are required-green in the `--features parallel-receive-delta`
CI cell alongside FFB-W.a. A red FFB-W.b test blocks PIP-9.f
(default-on promotion).

## 10. Relationship to FFB-W series

| Task | Focus | Status |
|------|-------|--------|
| FFB-W.a | File-boundary correctness under normal load (multi-chunk files) | Completed (PR #5022) |
| FFB-W.b | `drain_inflight` correctness under rapid succession (< 1ms/file) | This spec |
| FFB-W.c | Worker panic mid-chunk: verify barrier drains despite panic | Planned |
| FFB-W.d | Barrier overhead bench (criterion, no correctness assertions) | Completed |

FFB-W.b is complementary to FFB-W.a: where FFB-W.a validates multi-chunk
files with realistic payloads, FFB-W.b stresses the degenerate edge where
per-file overhead dominates. Together they cover the full spectrum of
file sizes that the `ParallelDeltaApplier` barrier must handle.

FFB-W.b is complementary to FFB-W.d: where FFB-W.d measures how fast
the barrier cycles, FFB-W.b asserts it cycles correctly. A regression
that makes the barrier faster by skipping the predicate check would pass
FFB-W.d but fail FFB-W.b.

## 11. Rollback procedure

If FFB-W.b tests surface a barrier failure:

1. Bisect to the commit that introduced the regression using
   `git bisect` with the failing scenario as the test command.
2. If the failure is a deadlock (timeout), examine the
   `BarrierState::wait_until_idle` predicate and the
   `DecrementGuard::drop` notification path. A lost `notify_all`
   (e.g., decrement without notify) would cause the flusher to park
   indefinitely.
3. If the failure is `ApplierStillReferenced`, the DG-3 split may
   have regressed: check whether a payload-Arc clone leaked onto
   the worker's drop path.
4. If the failure is timing-dependent and not reproducible locally,
   increase the iteration count via `FFB_WB_ITERATIONS=500` and
   run with 16 workers to amplify the race window.
5. If the failure is Windows-specific, check whether the scheduler
   quantum is causing `wait_while` to park for 15.6ms on the
   zero-counter path. The zero-counter path should not park at all;
   if it does, the predicate check order may have regressed.

## 12. Non-goals

- **Benchmark throughput.** FFB-W.b validates correctness under
  adversarial timing, not throughput. Barrier throughput benchmarks
  are FFB-W.d's scope.
- **Disk I/O interaction.** All scenarios use in-memory sinks.
  Filesystem-level races (e.g., temp-file rename contention) are
  out of scope; the engine layer's file-commit path is tested
  separately.
- **Worker panic behavior.** What happens when a rayon worker panics
  mid-chunk during rapid succession is FFB-W.c's scope. FFB-W.b
  assumes workers complete successfully.
- **Large files.** Files with hundreds or thousands of chunks are
  FFB-W.a's domain. FFB-W.b is exclusively about the small-file
  degenerate edge.
- **Wire protocol integration.** FFB-W.b tests the applier in
  isolation. End-to-end parity with upstream under rapid small-file
  transfers is PIP-9.b.6's scope.

## 13. Cross-references

- `crates/engine/src/concurrent_delta/parallel_apply/drain.rs` -
  `flush_workers` (line 146), `finish_file` (line 49),
  `drain_inflight` (line 603).
- `crates/engine/src/concurrent_delta/parallel_apply/slot_barrier.rs` -
  `BarrierState::wait_until_idle` (line 214),
  `BarrierState::decrement_inflight` (line 202),
  `BarrierState::increment_inflight` (line 182).
- `crates/engine/src/concurrent_delta/parallel_apply/decrement_guard.rs` -
  `DecrementGuard::drop` (line 49).
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` -
  `ParallelDeltaApplier`, `DeltaChunk`, `FileSlot`, `SlotHandle`.
- `docs/design/ffb-wa-barrier-file-boundary-verification.md` -
  FFB-W.a spec (file-boundary correctness under normal load).
- `docs/design/ffb-1-applier-barrier-api.md` - barrier API design.
- `docs/design/dg-2a-option-b-spec.md` - BarrierState/SlotData split
  spec.
- `docs/design/dg-4-a-spin-yield-removal.md` - spin workaround removal
  spec.
