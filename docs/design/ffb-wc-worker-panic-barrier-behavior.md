# FFB-W.c: Worker panic barrier behavior test spec

Design spec for testing the `ParallelDeltaApplier` barrier under worker
panics. Part of the FFB-W series (barrier behavior under real load).
Predecessors: FFB-W.a (flush_workers file-boundary verification,
completed), FFB-W.d (barrier overhead bench, completed). Successor:
FFB-W.b (rapid drain validation).

Date: 2026-05-26

## 1. Problem statement

A rayon worker processing a `DeltaChunk` may panic at any point during
the verify or write phase. The question is whether the barrier
infrastructure - `BarrierState`, `DecrementGuard`, `SlotHandle`,
`flush_workers`, and `finish_file` - handles this correctly or leaves
the system in a state that hangs, leaks, or silently corrupts.

Three mechanisms interact:

1. **`DecrementGuard::drop`** fires even during unwinding, decrementing
   the inflight counter and calling `notify_all` on the Condvar. This is
   the RAII guarantee at
   `crates/engine/src/concurrent_delta/parallel_apply/decrement_guard.rs:49-53`.

2. **Rayon's `par_iter().collect()`** catches panics from worker
   closures and re-throws them on the thread that calls `collect`. The
   panic payload is stored until the parallel iterator completes, then
   `resume_unwind` propagates it from the caller's stack frame.

3. **`Mutex` poisoning** propagates through two paths: the inflight
   `Mutex<usize>` inside `BarrierState` (locked by `increment_inflight`
   and `decrement_inflight`) and the payload `Mutex<FileSlot>` inside
   `SlotData` (locked by `lock_slot`). If a panic unwinds while either
   mutex guard is held, the mutex is poisoned.

The concern: if a worker panics mid-chunk, the combination of these
three mechanisms may leave the barrier in an inconsistent state that
causes `flush_workers` to hang indefinitely, `finish_file` to observe
unexpected Arc strong counts, or the caller to miss the panic entirely.

## 2. Code path analysis

### 2.1. `apply_batch_parallel` panic path

Source: `crates/engine/src/concurrent_delta/parallel_apply/batch.rs:45-114`.

```
apply_batch_parallel(chunks)
  -> into_par_iter().map(verify_chunk).collect::<Result<Vec, _>>()
  -> [if panic in verify_chunk: rayon catches, resumes on caller]
  -> for v in verified { slot_for(ndx); handle.lock_slot(); slot.ingest() }
```

**Verify phase panic (rayon worker).** `verify_chunk` is a pure function
(`batch.rs:75-76`) that computes a checksum digest. It holds no mutex
guards. A panic here unwinds the rayon worker; rayon stores the panic
payload and continues driving remaining items in the parallel iterator.
Once `collect()` returns control to the caller, rayon calls
`resume_unwind` which propagates the panic out of `apply_batch_parallel`.

Key observation: no `SlotHandle` exists during the verify phase. The
handle is created per-chunk in the serial write loop *after* `collect()`
returns. If `collect()` panics (by re-throwing), the write loop never
executes, so no `SlotHandle` is ever constructed and no inflight counter
is incremented. **The barrier is never touched.** The panic propagates
cleanly out of `apply_batch_parallel`.

**Write phase panic (caller thread).** The serial write loop at
`batch.rs:93-112` creates a `SlotHandle` per verified chunk, locks the
per-file mutex, and calls `slot.ingest()`. If a panic occurs inside
`ingest()` or `lock_slot()`:

- The `SlotHandle` is dropped during unwinding, which drops its
  `DecrementGuard`, which calls `BarrierState::decrement_inflight()`.
  The inflight counter decrements correctly.
- The `MutexGuard<FileSlot>` is dropped during unwinding, which
  **poisons the per-file mutex**. Subsequent `lock_slot` calls for this
  `ndx` will return `ParallelApplyError::SlotPoisoned`.
- The panic unwinds out of `apply_batch_parallel`. Remaining chunks in
  the verified batch are dropped without being written.

### 2.2. `apply_one_chunk` panic path

Source: `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:552-570`.

```
apply_one_chunk(chunk)
  -> slot_for(ndx)          // creates SlotHandle, bumps inflight
  -> rayon::join(verify, || ())
  -> handle.lock_slot(ndx)  // acquires per-file mutex
  -> slot.ingest(chunk)     // writes to destination
```

**Verify phase panic (rayon worker).** `rayon::join` catches the panic
in the first closure and resumes it on the calling thread when `join`
returns. The `SlotHandle` was already constructed (inflight is 1). When
the panic propagates out of `apply_one_chunk`, the `SlotHandle` drops
normally during unwinding: `DecrementGuard::drop` fires, inflight goes
to 0, Condvar notifies. **The barrier is left clean.**

**Write phase panic (caller thread).** Same as the batch write-phase
analysis: `MutexGuard` drop poisons the per-file mutex,
`DecrementGuard::drop` decrements inflight. The barrier counter is
consistent but the per-file mutex is poisoned.

### 2.3. `DecrementGuard::drop` during panic unwinding

Source:
`crates/engine/src/concurrent_delta/parallel_apply/decrement_guard.rs:49-53`.

The drop body calls `BarrierState::decrement_inflight()`, which does:

```rust
let mut guard = self.inflight.lock()
    .expect("inflight mutex poisoned on decrement");
*guard = guard.saturating_sub(1);
self.notify.notify_all();
```

**Critical risk:** the `.expect()` on the inflight mutex lock. If the
inflight mutex is itself poisoned (by a prior panic that held it), then
`decrement_inflight` will panic inside the drop impl. A panic during
unwinding triggers `abort()` - the process terminates immediately.

Current exposure: the inflight mutex is only held momentarily during
`increment_inflight` and `decrement_inflight`. Neither body can panic
while holding the guard (the `checked_add` panics after the guard is
acquired, but `.expect()` on `checked_add` would panic while still
holding the guard - this *would* poison the inflight mutex). However,
`increment_inflight` at `slot_barrier.rs:183-187` does:

```rust
let mut guard = self.inflight.lock()
    .expect("inflight mutex poisoned on increment");
*guard = guard.checked_add(1).expect("inflight counter overflow");
```

If `checked_add` returns `None` (counter at `usize::MAX`), the
`.expect()` panics while `guard` is alive, poisoning the inflight mutex.
Subsequent `decrement_inflight` calls `.expect()` on the poisoned mutex
and aborts the process.

In practice this is unreachable (would require `usize::MAX` concurrent
inflight chunks), but the code path exists.

### 2.4. `flush_workers` behavior after worker panic

Source: `crates/engine/src/concurrent_delta/parallel_apply/drain.rs:146-158`.

```rust
let guard = self.inflight.lock()
    .map_err(|_| ParallelApplyError::SlotPoisoned { ndx, kind })?;
let _final = self.notify
    .wait_while(guard, |inflight| *inflight > 0)
    .map_err(|_| ParallelApplyError::SlotPoisoned { ndx, kind })?;
```

`flush_workers` already handles inflight mutex poisoning gracefully: it
maps `PoisonError` to `SlotPoisoned` and returns `Err`. No hang.

However, if the inflight mutex is *not* poisoned (the common case - the
panic happened in user code, not while holding the inflight mutex), then
`flush_workers` behaves correctly: it waits until `inflight == 0`. The
`DecrementGuard::drop` that fired during unwinding already decremented
the counter and notified, so `flush_workers` will observe zero and
return `Ok(())`.

**No hang risk** in the common panic scenario.

### 2.5. `finish_file` behavior after worker panic

Source: `crates/engine/src/concurrent_delta/parallel_apply/drain.rs:49-121`.

After `flush_workers` succeeds, `finish_file` removes the entry from the
DashMap and calls `SlotData::into_slot()`:

```rust
self.slot.into_inner()
    .map_err(|_| ParallelApplyError::SlotPoisoned { ndx, kind }.into())
```

If the per-file `Mutex<FileSlot>` was poisoned by a panicking write
worker, `into_inner()` returns `PoisonError` and `finish_file` surfaces
`SlotPoisoned`. The caller gets a typed error indicating the file's
writer state is untrustworthy.

**No silent corruption.** The poisoned mutex prevents the caller from
recovering the writer and committing a partial file.

## 3. Rayon panic propagation semantics

### 3.1. `par_iter().map().collect()`

Rayon's `ParallelIterator::collect` catches panics from worker closures
using `std::panic::catch_unwind`. The panic payload is stored. Once all
items have been processed (or short-circuited by the `Result` combiner),
`resume_unwind` re-throws the stored panic on the calling thread. From
the caller's perspective, `collect()` panics.

For `apply_batch_parallel`, this means:

- If one worker panics in `verify_chunk`, the `collect()` call itself
  panics on the caller thread.
- The caller thread's stack unwinds normally. Any RAII guards on the
  caller's stack (including `SlotHandle` if one existed) drop normally.
- The panic is observable via `std::panic::catch_unwind` or propagates
  to the nearest panic handler.

### 3.2. `rayon::join`

`rayon::join(a, b)` runs `a` and `b` potentially on different threads.
If `a` panics, rayon catches it, runs `b` to completion, then resumes
the panic from `a` on the calling thread. The caller sees a panic from
`join()`.

For `apply_one_chunk`, the second closure is `|| ()` (no-op), so the
semantic is: if `verify_chunk` panics, `join` re-throws on the caller.

### 3.3. What rayon does NOT do

Rayon does not:

- Terminate the thread pool on panic. Other workers continue processing
  other items.
- Propagate panics to unrelated callers. The panic is scoped to the
  `collect()` or `join()` call that spawned the panicking work.
- Invoke any cleanup callback. There is no `on_panic` hook; the caller
  must handle the unwound panic itself.

## 4. Failure mode inventory

| Scenario | DecrementGuard fires? | Inflight reaches 0? | Mutex poisoned? | flush_workers behavior | finish_file behavior |
|---|---|---|---|---|---|
| Panic in verify (batch, par_iter) | N/A - no SlotHandle created | N/A - never incremented | No | Returns Ok | Returns Ok (file intact) |
| Panic in verify (one_chunk, rayon::join) | Yes (unwind drops SlotHandle) | Yes | No | Returns Ok | Returns Ok (file intact) |
| Panic in write (batch, serial loop) | Yes (unwind drops SlotHandle) | Yes | FileSlot mutex poisoned | Returns Ok (inflight clean) | Returns Err(SlotPoisoned) |
| Panic in write (one_chunk, caller) | Yes (unwind drops SlotHandle) | Yes | FileSlot mutex poisoned | Returns Ok (inflight clean) | Returns Err(SlotPoisoned) |
| Panic in first chunk of many (batch) | Depends on phase | See above rows | See above rows | See above rows | Remaining chunks lost |
| Panic in last chunk (batch) | Depends on phase | See above rows | See above rows | See above rows | Prior chunks may have written |

## 5. Poison detection

### 5.1. Current state

The barrier has two mutexes with different poisoning semantics:

- **`BarrierState::inflight`** (`Mutex<usize>`): poisoning is treated as
  fatal. Both `increment_inflight` and `decrement_inflight` call
  `.expect()` on the lock, which panics (and aborts during unwinding).
  `wait_until_idle` uses `.map_err()` and returns a typed error.

- **`SlotData::slot`** (`Mutex<FileSlot>`): poisoning is non-fatal.
  `lock_slot` and `into_slot` map `PoisonError` to
  `ParallelApplyError::SlotPoisoned` and return `Err`.

### 5.2. Should the barrier have a "poisoned" flag?

Like `std::sync::Mutex`'s built-in poisoning, a dedicated `poisoned:
AtomicBool` on `BarrierState` would let `flush_workers` return early
with a typed error instead of waiting for inflight to reach zero. This
matters only if `decrement_inflight` fails to fire (process abort
scenario) - in normal panic unwinding the counter always decrements.

**Recommendation: no explicit poison flag.** The existing `Mutex`
poisoning on the inflight mutex already serves this purpose. If the
inflight mutex is poisoned, `wait_until_idle` returns
`Err(SlotPoisoned)`. Adding a second flag would duplicate the existing
mechanism. The per-file `Mutex<FileSlot>` poisoning already catches
write-phase panics and surfaces them through `finish_file`.

### 5.3. The `decrement_inflight` `.expect()` risk

The `.expect()` at `slot_barrier.rs:206` on the inflight mutex lock is
the one genuine hazard. If the inflight mutex is poisoned (by a
`checked_add` panic in `increment_inflight`), then `decrement_inflight`
called from `DecrementGuard::drop` during unwinding will trigger a
double-panic and process abort.

**Recommendation for the implementation PR:** replace the `.expect()` in
`decrement_inflight` with a graceful fallback. If the inflight mutex is
poisoned, log a diagnostic and return without decrementing. The
inflight counter will be wrong, but `flush_workers` will surface
`SlotPoisoned` instead of the process aborting. This is strictly better
than an abort.

```rust
// Proposed change in decrement_inflight:
pub(super) fn decrement_inflight(&self) {
    let Ok(mut guard) = self.inflight.lock() else {
        // Inflight mutex poisoned by a prior panic. Cannot decrement
        // the counter, but returning without panic prevents a
        // double-panic abort during unwinding. flush_workers will
        // surface SlotPoisoned when it tries to lock.
        return;
    };
    *guard = guard.saturating_sub(1);
    self.notify.notify_all();
}
```

The same change should apply to `increment_inflight`: replace
`.expect()` with an early `Err` return mapped to `io::Error`, so the
caller path surfaces the poisoning instead of panicking.

## 6. Recovery path

After a worker panic, the receiver must:

1. **Catch the panic** at the `apply_batch_parallel` or
   `apply_one_chunk` call site. Since rayon re-throws via
   `resume_unwind`, the caller can use `std::panic::catch_unwind`
   around the apply call.

2. **Abort the affected file.** The per-file `Mutex<FileSlot>` is
   poisoned (if the panic was in the write phase) or the file is
   incomplete (if the panic was in the verify phase and remaining
   chunks were not applied). Call `finish_file(ndx)` - it will return
   `Err(SlotPoisoned)` or succeed with a partially written file.
   Either way, the temp file should be deleted and the transfer
   retried or reported as failed.

3. **Continue other files.** The DashMap is per-file. A panic on file
   `ndx=7` does not poison any state for `ndx=8`. Other files can
   proceed normally.

4. **Map to rsync exit code.** A worker panic is an internal error,
   not a protocol violation. Map to exit code 12 (stream I/O error) or
   11 (file I/O error), depending on where the panic originated.

## 7. Test scenarios

Each test verifies the "no hang, panic propagated, partial file cleaned
up" contract.

### 7.1. Panic in verify phase (batch path)

Inject a panic inside `verify_chunk` by supplying a `ChecksumStrategy`
whose `compute()` method panics on a specific chunk payload. Submit a
batch of N chunks via `apply_batch_parallel`. Assert:

- `apply_batch_parallel` panics (caught via `catch_unwind`).
- No `SlotHandle` was constructed (inflight counter remains 0 for the
  batch path - verify completes before handles are created).
- `flush_workers(ndx)` returns `Ok(())`.
- `finish_file(ndx)` returns `Ok(writer)` - no writes occurred, writer
  is intact.
- Zero bytes were written to the destination.

### 7.2. Panic in verify phase (one_chunk path)

Same panicking strategy, but submitted through `apply_one_chunk`. The
`SlotHandle` is created before `rayon::join`, so the inflight counter is
1 when the panic fires. Assert:

- `apply_one_chunk` panics (caught via `catch_unwind`).
- `DecrementGuard::drop` fired: inflight counter is 0 after the panic.
- `flush_workers(ndx)` returns `Ok(())`.
- `finish_file(ndx)` returns `Ok(writer)` - the verify panic prevented
  any write.
- Zero bytes written.

### 7.3. Panic in write phase

Inject a panic via a `Write` impl whose `write_all()` panics on a
specific byte pattern. Submit chunks through `apply_one_chunk`. Assert:

- `apply_one_chunk` panics.
- `DecrementGuard::drop` fired: inflight counter is 0.
- Per-file `Mutex<FileSlot>` is poisoned.
- `flush_workers(ndx)` returns `Ok(())` (inflight mutex is not
  poisoned).
- `finish_file(ndx)` returns `Err` with `SlotPoisoned`.
- Other registered files are unaffected: `finish_file(other_ndx)`
  returns `Ok(writer)`.

### 7.4. Panic in first chunk of multi-chunk batch

Submit a batch of 8 chunks where the first chunk (by `chunk_sequence`)
triggers a verify panic. Assert:

- None of the 8 chunks are written (the verify collect short-circuits).
- All resources for the file are reclaimable.

### 7.5. Panic in last chunk of multi-chunk batch

Submit a batch of 8 chunks where the last chunk triggers a verify panic.
Assert:

- Rayon may have verified chunks 0-6 successfully before the panic in
  chunk 7. The `collect()` still re-throws.
- None of the 8 chunks are written (the serial write loop does not
  execute after `collect()` panics).
- File resources are reclaimable.

### 7.6. Panic in write phase mid-file

Submit chunks 0-3 successfully (written to destination). Then submit
chunk 4 which triggers a write panic. Assert:

- Chunks 0-3 were written (bytes_written > 0).
- Chunk 4's write was partial or absent.
- Per-file mutex is poisoned.
- `finish_file` returns `Err(SlotPoisoned)`.
- The receiver must discard the partial file.

### 7.7. Concurrent file isolation

Register files ndx=0 and ndx=1. Panic a worker on ndx=0. Assert:

- ndx=1 is completely unaffected.
- `flush_workers(1)` returns `Ok`.
- `finish_file(1)` returns `Ok(writer)`.
- The DashMap shard for ndx=1 was never touched by the panic.

### 7.8. Double panic guard (decrement_inflight resilience)

Manually poison the inflight mutex (by panicking inside a scope that
holds the guard). Then trigger a `DecrementGuard::drop`. Assert:

- If the current `.expect()` code: process aborts (document this as a
  known limitation).
- After the proposed `.expect()` replacement: `decrement_inflight`
  returns silently without aborting. `flush_workers` returns
  `Err(SlotPoisoned)`.

## 8. Implementation notes

### 8.1. Panicking test strategy

Tests that assert "this call panics" should use
`std::panic::catch_unwind` with `AssertUnwindSafe` wrappers. The test
itself must not unwind - it catches the panic and asserts on the
payload.

```rust
let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
    applier.apply_one_chunk(panicking_chunk).unwrap();
}));
assert!(result.is_err(), "expected panic from worker");
```

### 8.2. Panicking `ChecksumStrategy` for verify-phase tests

Implement a test-only `ChecksumStrategy` that panics when
`compute()` sees a specific magic byte pattern:

```rust
struct PanickingStrategy {
    inner: Arc<dyn ChecksumStrategy>,
    trigger: Vec<u8>,
}

impl ChecksumStrategy for PanickingStrategy {
    fn compute(&self, data: &[u8]) -> ChecksumDigest {
        if data == self.trigger.as_slice() {
            panic!("PanickingStrategy: triggered on magic payload");
        }
        self.inner.compute(data)
    }
    // ... delegate remaining methods to inner
}
```

### 8.3. Panicking writer for write-phase tests

Implement a test-only `Write` that panics on a specific byte pattern:

```rust
struct PanickingWriter {
    inner: Vec<u8>,
    trigger: u8,
}

impl Write for PanickingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.contains(&self.trigger) {
            panic!("PanickingWriter: trigger byte 0x{:02x}", self.trigger);
        }
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}
```

### 8.4. Test module location

All FFB-W.c tests go in
`crates/engine/src/concurrent_delta/parallel_apply/mod.rs` inside a
`#[cfg(test)] mod worker_panic_tests` submodule, co-located with the
existing test infrastructure (`VecSink`, `sequential_apply`, etc.).

## 9. Success criteria

| Criterion | Verification |
|---|---|
| No hangs | Every test completes within 5 seconds (generous bound for CI) |
| Panic propagated to caller | `catch_unwind` captures the panic; caller observes `Err` or panic payload |
| DecrementGuard fires on unwind | Inflight counter is 0 after caught panic (assert via `flush_workers` returning Ok) |
| Poisoned mutex surfaced | `finish_file` returns `Err(SlotPoisoned)` after write-phase panic |
| Partial file not committed | Zero bytes written after verify-phase panic; poisoned mutex blocks writer recovery after write-phase panic |
| Cross-file isolation | Panic on ndx=X does not affect ndx=Y |
| No process abort in common case | Only the `.expect()` in `decrement_inflight` can abort; proposed fix in section 5.3 eliminates this |

## 10. Out of scope

- **Panic in `register_file`**: registration is a caller-thread
  operation with no rayon dispatch. Panics propagate normally. Not
  tested by FFB-W.c.
- **Panic in `drain_inflight`**: iterates `flush_workers` per file.
  If one `flush_workers` panics (inflight mutex poisoned), the iteration
  stops and the panic propagates. This is standard iterator behavior,
  not a barrier concern.
- **Rayon thread pool destruction**: rayon's global pool is
  process-scoped and does not shut down on worker panics. Pool health
  after panics is rayon's responsibility, not ours.
- **`finish_file` called concurrently from multiple threads**: the
  DashMap `remove` is atomic, so only one caller wins. Not a panic
  concern.

## 11. Relationship to other FFB-W tasks

- **FFB-W.a** (completed): verified `flush_workers` blocks at file
  boundaries under normal operation. FFB-W.c extends this to panic
  scenarios.
- **FFB-W.b** (pending): rapid drain validation. Tests the barrier under
  high-frequency register/flush/finish cycles. Orthogonal to panic
  handling.
- **FFB-W.d** (completed): barrier overhead bench. Measured the
  Condvar wait/notify cost. Panic paths add no overhead to the normal
  path (panic is exceptional).
