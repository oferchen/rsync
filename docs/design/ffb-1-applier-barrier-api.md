# FFB-1: `flush_workers` / `drain_inflight` barrier API for `ParallelDeltaApplier`

Design note. Companion to
`docs/design/parallel-receive-delta-application.md` (umbrella) and
`docs/design/parallel-receive-delta-default-on.md` (promotion gate). No
source changes accompany this document; FFB-2 (implementation), FFB-3
(existing caller migration), and FFB-4 (PIP-3 wire-up) ship in
follow-up PRs.

## 1. Problem statement

`ParallelDeltaApplier::finish_file` at
`crates/engine/src/concurrent_delta/parallel_apply.rs:555-584` removes
the per-file slot from the outer `DashMap`, then attempts to extract
the inner `FileSlot` via `Arc::try_unwrap`:

```rust
let (_, slot_arc) = self.files.remove(&ndx)
    .ok_or_else(|| io::Error::other(...))?;
let slot = Arc::try_unwrap(slot_arc)
    .map_err(|still_shared| ParallelApplyError::ApplierStillReferenced {
        ndx,
        strong_count: Arc::strong_count(&still_shared),
        kind: "finish_file",
    })?
    .into_inner()
    .map_err(|_| ParallelApplyError::SlotPoisoned { ndx, kind: "finish_file" })?;
```

`ApplierStillReferenced`
(`parallel_apply.rs:66-84`) is reachable whenever a rayon worker still
holds an `Arc<Mutex<FileSlot>>` clone scheduled from
`apply_chunk_parallel` (`parallel_apply.rs:468-485`) or
`apply_batch_parallel` (`parallel_apply.rs:499-526`). Both methods call
`slot_for` (`parallel_apply.rs:586-595`) which clones the slot's
`Arc`; that clone is dropped only when the worker returns from the
per-file mutex critical section. Today the only call sites are unit
tests, which synchronously drive the applier on the caller thread, so
the race is uncommon. The error path is real, diagnostic, and
intentional (see ATU-3..7 background); the receiver path itself is
gated behind `--features parallel-receive-delta` so the error has no
user-facing exposure yet.

**User-facing impact at PIP-3 cutover.** PIP-3 is the production
wire-up plan from
`docs/design/parallel-receive-delta-default-on.md` section 8 step 4:
the receiver in `crates/transfer/src/receiver/mod.rs` will call
`enable_parallel_receive_delta` for any transfer whose file count
crosses a heuristic threshold. At that point, every receiver finalises
files by calling `finish_file` per `ndx` once the per-file token
stream returns `End`. There is no synchronous "the workers are done"
signal on the rayon side: the verify task may still be in flight when
the per-file token loop returns `End` and the receiver calls
`finish_file`. The receiver would then see `ApplierStillReferenced`
non-deterministically under load, with no recovery primitive other
than a retry loop. That is unacceptable for a production caller.

The barrier primitive is the missing piece. With it, the receiver
either waits per file (`flush_workers(ndx)`) or once at session
shutdown (`drain_inflight()`), and `finish_file` can either keep its
typed error as a post-barrier invariant assertion (Option A/B/C) or
absorb the barrier and drop the variant entirely (Option D).

## 2. Constraint inventory

**Synchronisation primitive.**

- *rayon scope*. `rayon::scope` (or `rayon::in_place_scope`) blocks
  until every nested spawn returns. Fits well for `apply_batch_parallel`
  because the chunk fan-out is bounded and known upfront, but it does
  not fit `apply_chunk_parallel`: that call dispatches a single
  `rayon::join` per chunk and returns immediately, so the worker
  lifetime spans calls. A scope-per-chunk would defeat the parallelism.
- *`crossbeam::scope`*. Same blocking shape as rayon's scope, but
  spawns OS threads rather than reusing the rayon pool. Wrong fit:
  the verify work is meant to amortise across the rayon ambient pool
  to match the cargo-feature-gated bench (`parallel_receive_delta_perf`).
- *Channel-based shutdown*. The workers could post a completion
  message on a per-file `crossbeam::channel`; the barrier blocks on
  `recv` until the channel has drained `N` completions. Cost: one extra
  allocation per chunk plus per-worker channel handle bookkeeping.
- *`Condvar` on the slot*. A `(Mutex<usize>, Condvar)` pair colocated
  with the `FileSlot` tracks the in-flight worker count for that
  `ndx`. The barrier blocks on `cv.wait_while(|n| *n > 0)`. Cheap (no
  per-chunk allocation, no extra channel), reuses the existing
  per-slot lock, and composes cleanly with the existing `DashMap`
  shard. This is the only primitive that scales per-file without
  enlarging the hot path.

**Per-file vs global granularity.**

- *Per-file barrier* (Option A) blocks the receiver only on the file
  it is finalising. Other in-flight files continue. This matches the
  receiver loop in `crates/transfer/src/receiver/transfer.rs:127` that
  finalises each file as `End` arrives.
- *Global barrier* (Option B) blocks until every slot is idle. Useful
  at session shutdown (drop, abort, panic recovery) but pessimistic
  for normal per-file completion.

**DashMap migration (BR-3j.c, PR #4634).**

The slot is `Arc<Mutex<FileSlot>>` stored inside
`DashMap<FileNdx, Arc<Mutex<FileSlot>>>` at `parallel_apply.rs:323`.
The shard guard is dropped immediately after `Arc::clone` (see
`parallel_apply.rs:586-595` locking discipline). Two consequences for
the barrier:

1. The barrier state must live inside the per-slot value (alongside
   the `Mutex<FileSlot>`), not the outer map. Embedding `(Mutex<usize>,
   Condvar)` extends the slot struct without touching shard locking.
2. The barrier must not re-acquire the `DashMap` shard while a worker
   holds an `Arc` clone. The slot's `Arc` is enough; the locking
   discipline already documented at `parallel_apply.rs:316-322` keeps
   shard guards out of long waits.

**Sync vs async semantics.** The engine is threaded, not async. Blocking
the caller thread on a `Condvar` matches the rest of the receive
pipeline. An async-friendly variant (`drain_inflight_async`) is out of
scope until any async caller appears.

**`finish_file`'s existing typed error.** `ApplierStillReferenced`
carries `ndx`, `strong_count`, and a static `kind` tag for diagnostic
value (ATU-3..7). The barrier reframes the variant from "transient
race the caller might hit" to "post-barrier invariant violation worth
panicking on". The variant stays. The `Display` payload is unchanged.

## 3. API options

### Option A: per-file barrier (`flush_workers`)

```rust
impl ParallelDeltaApplier {
    /// Blocks until every in-flight worker for `ndx` has released
    /// its slot `Arc` clone. Returns once the slot's strong count is
    /// observed to be 1 (the map-owned reference) or once the slot is
    /// already absent (no-op).
    pub fn flush_workers(&self, ndx: impl Into<FileNdx>) -> io::Result<()> { ... }
}
```

Slot extension:

```rust
struct FileSlot {
    writer: Box<dyn Write + Send>,
    reorder: ReorderBuffer<DeltaChunk>,
    bytes_written: u64,
    inflight: Arc<(Mutex<usize>, Condvar)>, // bumped by apply_chunk_parallel
}
```

Each `slot_for` increments `inflight.0` while holding the slot mutex
(O(1), already in the critical section); each worker decrements and
notifies on return. `flush_workers` reads `inflight.0` under the
mutex and waits on the `Condvar` until it observes zero. The barrier
itself takes no DashMap shard write.

### Option B: global barrier (`drain_inflight`)

```rust
impl ParallelDeltaApplier {
    /// Blocks until every slot in the applier has zero in-flight
    /// workers. Used at session shutdown and panic recovery.
    pub fn drain_inflight(&self) -> io::Result<()> { ... }
}
```

Implementation iterates `self.files.iter()` (shard-by-shard), clones
each slot `Arc`, drops the iterator (drops shard guards), and waits
on each slot's `Condvar` in turn. Pessimistic, but session shutdown
runs at most once per transfer so the cost is acceptable.

### Option C: callback-based completion

```rust
impl ParallelDeltaApplier {
    pub fn on_file_complete<F: FnOnce() + Send + 'static>(
        &self,
        ndx: impl Into<FileNdx>,
        cb: F,
    );
}
```

Stores `cb` in the slot. The decrement path that drops the last
in-flight worker invokes `cb`. Inverts control: the receiver no
longer blocks, it hands work to the applier. Awkward fit for the
receiver loop in `transfer.rs:127` which is otherwise strictly
synchronous; the callback would have to post into a channel the loop
drains, re-introducing a barrier shape one layer up.

### Option D: barrier baked into `finish_file`

```rust
pub fn finish_file(&self, ndx: impl Into<FileNdx>) -> io::Result<Box<dyn Write + Send>> {
    let ndx = ndx.into();
    let (_, slot_arc) = self.files.remove(&ndx).ok_or_else(...)?;
    // Wait until all workers have released the slot Arc.
    {
        let (lock, cv) = &*slot_arc_inflight(&slot_arc);
        let mut guard = lock.lock().map_err(...)?;
        while *guard > 0 { guard = cv.wait(guard).map_err(...)?; }
    }
    let slot = Arc::try_unwrap(slot_arc)
        .unwrap_or_else(|_| unreachable!("post-barrier invariant violation: ..."))
        .into_inner()
        .map_err(|_| ParallelApplyError::SlotPoisoned { ndx, kind: "finish_file" })?;
    if !slot.drained() { return Err(ParallelApplyError::UndrainedChunks { ... }.into()); }
    Ok(slot.writer)
}
```

`finish_file` becomes a barrier-and-finalise primitive. The
`ApplierStillReferenced` variant either disappears or becomes a
`panic!` path (since post-barrier it indicates a logic bug, not a
race).

## 4. Decision matrix

| Concern                          | A: `flush_workers`         | B: `drain_inflight`             | C: callback                  | D: bake into `finish_file`     |
|----------------------------------|----------------------------|---------------------------------|------------------------------|--------------------------------|
| Latency for normal completion    | one cv wait per file       | one cv wait per file at end     | none (caller drives)         | one cv wait per file           |
| Memory overhead per slot         | `Arc<(Mutex<usize>, Condvar)>` | same                        | same + boxed `FnOnce`        | same                           |
| Per-chunk overhead               | one inc/dec under slot lock | same                            | same                         | same                           |
| Error propagation                | typed `io::Result`         | typed `io::Result`              | callback obscures errors     | absorbed into `finish_file` result |
| Fit with `apply_batch_parallel`  | composes (one flush per ndx) | composes (single end-of-batch) | awkward (callback per file)  | composes (caller still calls `finish_file`) |
| Fit with PIP-3 receiver loop     | natural per-`End` token    | only at session end             | inverts control flow         | natural, single call           |
| Existing test migration cost     | swap `finish_file` retries for `flush_workers` + `finish_file` | one call at test end | rewrite tests around callbacks | zero (existing callers unchanged) |
| Diagnostic value preserved       | yes (variant stays for asserts) | yes                          | weakened (no Result)         | weakened to `panic!` or `unreachable!` |
| Risk of indefinite block         | bounded by chunk lifetime  | bounded by chunk lifetime       | n/a                          | bounded; harder to time-bound from caller |

## 5. Recommendation

**Adopt Option A as the primitive and Option D as the bundled
default**, with Option B added as a thin loop over Option A for
session shutdown.

Rationale, point-by-point against the matrix:

1. **Option A is the only primitive that fits the receiver's
   per-file-`End` rhythm without inverting control flow.** It blocks
   only the file being finalised, leaving cross-file parallelism
   intact - the central premise of the parallel apply scaffold
   (umbrella design section 1.3).
2. **Option D is the right default callable** because every existing
   `finish_file` site (tests today, the PIP-3 receiver tomorrow)
   wants barrier-then-finalise as one operation. Burying the barrier
   inside `finish_file` removes the surface area for the caller to
   call them in the wrong order.
3. **Option B reuses Option A.** `drain_inflight` is a `for ndx in
   self.files { self.flush_workers(ndx)? }` loop. It exists for
   panic/abort/drop paths where the caller wants to retire every slot
   in one shot. Adding it costs no extra primitive.
4. **`ApplierStillReferenced` stays as a debug-build assertion path
   inside `finish_file`.** Post-barrier, observing
   `Arc::strong_count > 1` is a real bug (a worker leaked the `Arc`
   beyond its critical section). The typed variant remains
   meaningful, but the API contract changes from "callers will
   sometimes see this" to "this indicates an applier bug, file an
   issue with the strong_count value". The `Display` payload already
   carries enough context for that.
5. **Option C is rejected.** Async-style completion does not fit the
   threaded engine and forces the receiver to invent a barrier one
   layer up.

The combined surface FFB-2 ships:

```rust
impl ParallelDeltaApplier {
    pub fn flush_workers(&self, ndx: impl Into<FileNdx>) -> io::Result<()>;
    pub fn drain_inflight(&self) -> io::Result<()>;
    pub fn finish_file(&self, ndx: impl Into<FileNdx>) -> io::Result<Box<dyn Write + Send>>; // now internally barriers
}
```

Three public methods, one shared per-slot `(Mutex<usize>, Condvar)`
primitive, zero behaviour change for callers that only ever called
`finish_file`.

## 6. Migration path

**FFB-2 (implementation):** add the `inflight` field to `FileSlot`;
bump it in `slot_for` (or in a thin guard type returned from
`slot_for`) and decrement in a `Drop` impl on that guard to keep the
critical-section bookkeeping exception-safe; implement
`flush_workers`, `drain_inflight`; rewrite `finish_file` to call
`flush_workers` then assert the post-barrier `strong_count == 1`
invariant. Tests stay green without edits.

**FFB-3 (existing callers):** the existing tests at
`crates/engine/src/concurrent_delta/parallel_apply.rs:708-829` call
`finish_file` synchronously. They keep working unchanged because
Option D bakes the barrier in. Tests that want to assert the
post-barrier strong-count invariant can call `flush_workers`
explicitly and then inspect via a `#[cfg(test)]` accessor.

**FFB-4 (PIP-3 wire-up):** the receiver loop in
`crates/transfer/src/receiver/mod.rs::enable_parallel_receive_delta`
and the per-file finaliser in
`crates/transfer/src/receiver/transfer.rs:388-475` call
`finish_file(ndx)` per file as today; the barrier work happens
inside that call. At session shutdown (success or abort),
`ReceiverContext` drops the applier; the `Drop` impl calls
`drain_inflight` so no worker outlives the applier. PIP-3 acceptance
criteria from `docs/design/parallel-receive-delta-default-on.md`
section 8 step 4 are unchanged - the barrier is an invisible
correctness primitive, not a new flag.

## 7. Cross-references

- `crates/engine/src/concurrent_delta/parallel_apply.rs:555-584` -
  `finish_file` and the `Arc::try_unwrap` site.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:66-84` -
  `ApplierStillReferenced` variant.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:586-595` -
  `slot_for` DashMap locking discipline.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:316-322` -
  outer-map locking-discipline contract that the barrier must
  preserve.
- `docs/design/parallel-receive-delta-application.md` - umbrella.
- `docs/design/parallel-receive-delta-default-on.md` - promotion
  gate; PIP-3 is step 4.
- BR-3j.c (PR #4634) - DashMap migration that fixed the outer-map
  contention and shaped the per-slot lifecycle the barrier extends.
- ATU-3..7 - prior audit thread that introduced the typed
  `ParallelApplyError` variants.
