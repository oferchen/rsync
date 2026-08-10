# DG-5.a: Concurrent `finish_file` Stress Test Specification

**Status:** Design  
**Depends on:** DG-3 (SlotData/BarrierState split), DG-4.a (spin-yield removal)  
**Validates:** Structural fix holds under extreme concurrency  
**Test file:** `crates/engine/tests/parallel_apply_dg5a_finish_stress.rs`

## 1. Background

The DG series addressed a race condition in `DecrementGuard`/`SlotBarrier` where
`finish_file`'s `Arc::try_unwrap` on the payload `Arc<SlotData>` could observe a
non-zero strong count because `DecrementGuard`'s inner Arc had not yet dropped
after notifying the Condvar. The original symptom surface was Windows under load,
where the scheduler widens the drop-body execution window.

DG-3 fixed this structurally by splitting `SlotBarrier` into `BarrierState`
(in-flight counter + Condvar) and `SlotData` (per-file `Mutex<FileSlot>`) so
that `DecrementGuard` holds an `Arc<BarrierState>` that is allocation-disjoint
from the payload `Arc<SlotData>` that `finish_file` unwraps. DG-4.a removed the
spin-then-yield workaround that masked the original race.

DG-5.a validates the structural fix holds under extreme concurrency: 1000 threads
calling `finish_file` concurrently while workers are actively dropping
`SlotHandle`s, with tight timing overlap between the `notify_all` inside
`DecrementGuard::drop` and the `Arc::try_unwrap` inside `finish_file`.

## 2. Distinction from DG-3.e Stress Test

The existing `parallel_apply_dg3_stress.rs` (DG-3.e) uses 1000 threads with per-
worker NDX ranges. Each worker runs register/dispatch/finish sequentially on its
own file indices. There is no cross-thread contention on `finish_file` for the
same file, and no concurrency between finish and active workers.

DG-5.a is fundamentally different:

| Dimension | DG-3.e | DG-5.a |
|-----------|--------|--------|
| `finish_file` concurrency | 1 thread per file | 1000 threads finishing distinct files while workers drop handles |
| Worker/finisher overlap | Sequential within thread | Concurrent - finisher parks while worker's `DecrementGuard` is mid-drop |
| NDX contention | None (per-worker range) | Shared pool - multiple workers submit to a file that another thread then finishes |
| Timing window | Wide (sequential register/apply/finish) | Tight (finish_file called immediately after last chunk) |

## 3. Test Architecture

### 3.1 Thread Topology

```
Main Thread
  |
  +-- 1000 Finisher threads (one per file)
  |     Each: wait for signal -> call finish_file(ndx)
  |
  +-- Worker pool (8-16 rayon workers, shared)
        Dispatch chunks to files via apply_one_chunk
```

### 3.2 Per-Iteration Cycle

Each iteration (10K per finisher thread) executes:

1. Finisher registers its file: `register_file(ndx, sink)`
2. Finisher dispatches N chunks (1-4 per file) via `apply_one_chunk`
3. Finisher calls `finish_file(ndx)` - which internally waits on the barrier
   then attempts `Arc::try_unwrap` on the payload

The critical timing is step 3: `finish_file` calls `flush_workers` which parks on
the Condvar until inflight reaches zero. The `DecrementGuard` fires `notify_all`
from inside its drop body, then the barrier Arc drops. Under the DG-3 structural
fix, the payload Arc's strong count is already 1 (DashMap-only) at this point
because the finisher has removed the entry from the DashMap. The test validates
that `try_unwrap` always succeeds without error across all 10M cycles.

### 3.3 Concurrency Patterns Exercised

**Pattern A - Simultaneous finish_file from multiple threads:**

All 1000 finisher threads call `finish_file` concurrently on distinct NDX values.
This exercises the DashMap's shard-level concurrency under the remove path, and
validates that no shard guard is held across the `Arc::try_unwrap` call.

**Pattern B - Interleaved register/finish on shared applier:**

While some threads are in `finish_file` (blocked on the Condvar inside
`flush_workers`), other threads are registering new files and dispatching chunks.
The DashMap must handle concurrent insert/remove without corrupting unrelated
slots.

**Pattern C - Rapid-fire register-then-immediately-finish:**

A subset of iterations registers a file with zero data chunks and immediately
calls `finish_file`. This exercises the "already idle" fast path in
`flush_workers` where the inflight counter is never bumped. The `try_unwrap` must
succeed on the first attempt since no `SlotHandle` was ever created.

**Pattern D - Multi-worker overlap on a single file:**

For files that receive multiple chunks, the rayon workers hold `SlotHandle` clones
concurrently. The finisher thread calls `finish_file` while one or more workers
are still in the `apply_one_chunk` critical section. The barrier must correctly
wait for all workers to release before the unwrap proceeds.

**Pattern E - Worker drop during barrier wait:**

The `DecrementGuard::drop` fires `notify_all` and then the `Arc<BarrierState>`
field drops. Between those two points, `flush_workers` wakes and finds
inflight==0. It then returns, `finish_file` removes the entry from the DashMap,
drops the `barrier` field, and calls `Arc::try_unwrap` on `data`. Under DG-3's
split, the worker's lingering `Arc<BarrierState>` is on a disjoint allocation -
it cannot interfere with the payload unwrap. This is the exact race window the
test must exercise at scale.

## 4. Harness Design

### 4.1 Feature Gate

```rust
#![cfg(all(feature = "dg-stress", feature = "parallel-receive-delta"))]
```

Same feature gate as `parallel_apply_dg3_stress.rs`. Not included in standard
`cargo nextest run` - requires explicit `--features dg-stress`.

### 4.2 Constants

```rust
/// Number of finisher threads.
const FINISHERS: u32 = 1_000;

/// Iterations per finisher thread.
const ITERS_PER_FINISHER: u32 = 10_000;

/// Maximum chunks per file per iteration (Pattern D).
const MAX_CHUNKS_PER_FILE: u64 = 4;

/// Payload bytes per chunk.
const CHUNK_BYTES: usize = 8;

/// Fraction of iterations that use zero-chunk fast path (Pattern C).
/// Approximately 1 in 8 iterations registers and immediately finishes.
const ZERO_CHUNK_MODULUS: u32 = 8;
```

### 4.3 Sink

```rust
struct CountingSink {
    written: Arc<AtomicU64>,
}
```

Same pattern as sibling stress tests. Counts bytes via `AtomicU64` so post-join
assertions can verify no bytes were lost or duplicated.

### 4.4 Deterministic RNG

```rust
struct Xorshift(u64);
```

Seeded per-worker for reproducibility. Controls:
- Number of chunks per file (1..=MAX_CHUNKS_PER_FILE)
- Whether this iteration uses the zero-chunk fast path

### 4.5 Applier Instance

A single shared `Arc<ParallelDeltaApplier>` with concurrency sized to the rayon
pool's default thread count (typically 8-16 on CI). Every finisher thread shares
this instance, exercising the DashMap under concurrent insert/remove.

### 4.6 NDX Assignment

Each finisher thread owns a unique NDX range:
```rust
let base = finisher_id * ITERS_PER_FINISHER;
let ndx = base + iter;
```

This ensures no two finishers call `finish_file` on the same NDX (which would be
an API contract violation). The concurrent pressure comes from 1000 threads
hitting the DashMap's shard locks simultaneously during register/remove.

## 5. Success Criteria

All of the following must hold:

1. **Zero panics.** Every worker thread joins successfully.
2. **Zero `ApplierStillReferenced` errors.** Every `finish_file` call succeeds.
   Under the DG-3 structural fix, the payload `Arc<SlotData>` has strong_count==1
   after DashMap removal, so `try_unwrap` succeeds on the first attempt.
3. **All files complete.** Every finisher thread completes all 10K iterations. A
   per-thread `AtomicUsize` completion counter is asserted post-join.
4. **Byte integrity.** Per-worker sink counters match `completed_iters *
   chunks_per_iter * CHUNK_BYTES`. No bytes are lost, duplicated, or cross-routed.
5. **No leaked in-flight counters.** `drain_inflight()` returns `Ok(())` after all
   workers join.
6. **Bounded runtime.** The test must complete within 120 seconds on CI (Linux
   x86_64). If it exceeds this, the spin-yield removal may have introduced a
   livelock - surface it as a timeout failure rather than hanging.

## 6. Platform Considerations

### 6.1 Windows Scheduling

The original DG-1 race symptom was observable primarily on Windows because
Windows' thread scheduler has:
- Coarser time slices (15.6ms default quantum vs 1-4ms on Linux)
- No priority inheritance by default on Condvar wake
- Deferred scheduling of woken threads

These properties widen the window between `DecrementGuard::drop`'s `notify_all`
and the actual field-drop of `Arc<BarrierState>`. Under DG-3's structural split
this window is irrelevant (the `BarrierState` Arc is on a disjoint allocation
from the payload), but the stress test must run on Windows to confirm the
structural fix eliminates the symptom entirely.

### 6.2 macOS Thread Limits

macOS enforces a per-process thread limit (typically 2048 on recent Darwin
kernels, configurable via `PTHREAD_THREADS_MAX`). The 1000-thread count is within
this limit, but each thread creates its own stack (default 8MB virtual). On CI
runners with limited virtual address space, use explicit 512KB stack sizes:

```rust
std::thread::Builder::new()
    .stack_size(512 * 1024)
    .name(format!("dg5a-{finisher_id}"))
    .spawn(...)
```

### 6.3 Linux Thread Overcommit

Linux's `ulimit -u` (max user processes, which includes threads) defaults to
~30K-60K on most systems. 1000 threads is well within range. The test should not
require elevated limits.

## 7. CI Integration

### 7.1 Feature-Gated Exclusion from Default Runs

The test is gated behind `#[cfg(all(feature = "dg-stress", feature =
"parallel-receive-delta"))]`. Standard `cargo nextest run --all-features` does
NOT enable `dg-stress` (it is not listed in the workspace's default features or
`all-features` list). This prevents the 1000-thread harness from bloating every
PR's lint/test cycle.

### 7.2 Dedicated CI Cell

A dedicated non-required CI workflow triggers on PRs touching:
- `crates/engine/src/concurrent_delta/parallel_apply/`
- `crates/engine/tests/parallel_apply_dg*`

The workflow runs:
```yaml
cargo nextest run -p engine --features "dg-stress,parallel-receive-delta" \
  -E 'test(dg)' --color never --test-threads 1
```

`--test-threads 1` prevents nextest from spawning multiple stress tests in
parallel, which would exhaust OS thread limits (1000 threads x N tests).

### 7.3 Platform Matrix

The dedicated CI cell runs on:
- `ubuntu-latest` (x86_64) - primary target
- `windows-latest` (x86_64) - historical symptom surface
- `macos-latest` (aarch64) - scheduler differences

### 7.4 Timeout

The CI step sets a 300-second timeout. The test itself should complete in under
60 seconds on a 4-core runner; 300 seconds provides margin for loaded CI hosts
without masking real livelocks.

## 8. Test Function Shape

```rust
#[test]
fn concurrent_finish_file_1000_threads_10k_iter() {
    let applier = Arc::new(ParallelDeltaApplier::new(rayon::current_num_threads()));
    let completed: Vec<Arc<AtomicUsize>> = ...;
    let sink_counters: Vec<Arc<AtomicU64>> = ...;

    let handles: Vec<_> = (0..FINISHERS).map(|finisher_id| {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .name(format!("dg5a-{finisher_id}"))
            .spawn(move || {
                let mut rng = Xorshift::new(0xDG5A_0000 ^ finisher_id as u64);
                let base = finisher_id * ITERS_PER_FINISHER;
                for iter in 0..ITERS_PER_FINISHER {
                    let ndx = base + iter;
                    let sink = CountingSink { written: ... };
                    applier.register_file(ndx, Box::new(sink)).expect("register");

                    let num_chunks = if iter % ZERO_CHUNK_MODULUS == 0 {
                        0  // Pattern C: immediate finish
                    } else {
                        (rng.next_u64() % MAX_CHUNKS_PER_FILE) + 1
                    };

                    for seq in 0..num_chunks {
                        let chunk = DeltaChunk::literal(ndx, seq, vec![finisher_id as u8; CHUNK_BYTES]);
                        applier.apply_one_chunk(chunk).expect("apply");
                    }

                    applier.finish_file(ndx).expect("finish_file under DG-5.a stress");
                    completed[finisher_id as usize].fetch_add(1, Ordering::Relaxed);
                }
            })
    }).collect();

    for handle in handles { handle.join().expect("finisher thread"); }

    // Assertions: completion counts, byte integrity, drain_inflight
}
```

## 9. Failure Modes and Diagnostics

| Failure | Root Cause | Diagnostic |
|---------|-----------|------------|
| `ApplierStillReferenced` | Payload Arc strong_count > 1 at try_unwrap | DG-3 split broken - a code change re-introduced payload Arc clone on worker drop path |
| `SlotPoisoned` | A worker panicked while holding the per-file Mutex | Check test for panicking assertions inside apply_one_chunk |
| `UndrainedChunks` | Chunk sequence gap - chunks dropped or misordered | Bug in per-file reorder buffer or chunk_sequence arithmetic |
| Thread panic (join fails) | Any `.expect()` inside the worker loop hit an Err | Inspect the panic message; likely one of the above typed errors |
| Timeout (>300s) | Livelock in Condvar wait or deadlock in DashMap | Thread dump to identify which threads are parked and on what lock |
| Byte mismatch | Per-worker sink != expected bytes | Cross-file write corruption or dropped write inside ingest path |

## 10. Relationship to DG-4.a Spin-Yield Removal

DG-4.a removes the spin-then-yield loop at lines 84-103 of `drain.rs`. Once
removed, `finish_file` goes directly from `flush_workers` to
`DashMap::remove` to `Arc::try_unwrap`. If the DG-3 structural fix is correct,
`try_unwrap` always succeeds on the first attempt because:

1. `flush_workers` waits until inflight==0 (all `DecrementGuard`s dropped)
2. `DecrementGuard::drop` holds `Arc<BarrierState>` - a disjoint allocation
3. The `SlotHandle`'s adapter `Arc<SlotBarrier>` drops its `Arc<SlotData>` clone
   as part of normal field-drop before the DecrementGuard fires
4. After DashMap removal, the entry's `Arc<SlotData>` is the only remaining clone

DG-5.a MUST pass with the spin-yield loop removed. If it fails without the loop,
the structural fix has a gap and the spin-yield cannot be removed safely.

## 11. Implementation Checklist

- [ ] Create `crates/engine/tests/parallel_apply_dg5a_finish_stress.rs`
- [ ] Add `dg-stress` feature to `crates/engine/Cargo.toml` (if not already present)
- [ ] Implement `concurrent_finish_file_1000_threads_10k_iter` test function
- [ ] Implement `concurrent_finish_file_with_multi_chunk_overlap` variant (Pattern D)
- [ ] Verify test passes on Linux, Windows, macOS locally or via CI
- [ ] Verify test passes with DG-4.a spin-yield removal applied
- [ ] Add CI workflow or extend existing DG stress workflow for the new test file
