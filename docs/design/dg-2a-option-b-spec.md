# DG-2.a: Option B - split `SlotBarrier` into `BarrierState` + `SlotData`

DG-1 (see `docs/design/decrementguard-audit.md`) catalogued the
`finish_file` / `flush_workers` release race in
`crates/engine/src/concurrent_delta/parallel_apply.rs` and recommended
**Option B** as the structural fix. This document specifies that split
in enough detail for DG-3 to implement without re-deriving any of the
DG-1 analysis. DG-2.b will sequence the migration; DG-2.c will pick
atomic vs phased.

DG-1 is the single source of truth for site numbering (C1..C7, D1, H1)
and line references. Every claim below cites the audit by table row.

## 1. Audit cross-reference

Recapping the DG-1 findings this spec acts on:

- **7 clone sites for `Arc<SlotBarrier>`** (audit s.1): C1
  `register_file:573`, C2 `flush_workers:801`, C3 `slot_for:845`, C4
  `SlotHandle::new:395`, C5 `SlotHandle::new:398` (moved, not
  cloned), C6/C7 test-only clones at `parallel_apply.rs:1324` and
  `1327`. Production strong count per file at steady state is 3
  (DashMap shard C1, `SlotHandle.barrier` C5,
  `DecrementGuard.barrier` C4).
- **One `DecrementGuard` construction site** (audit s.2): D1
  `SlotHandle::new` at `parallel_apply.rs:394-396`. The
  `Arc::clone(&barrier)` on line 395 is the second clone in the
  race - C4.
- **One `SlotHandle` construction site** (audit s.2): H1
  `slot_for:847`, called from `apply_one_chunk:625`,
  `apply_batch_parallel:671`, and `bytes_written:686`.
- **One `Condvar` fire site** (audit s.3): `notify_all` at
  `parallel_apply.rs:340` inside `SlotBarrier::decrement_inflight`,
  invoked only from `DecrementGuard::drop` (`367-371`).
- **Race window** (audit s.4): `notify_all` fires from inside
  `DecrementGuard::drop`; the C4 `Arc<SlotBarrier>` clone only drops
  when the implicit field-drop glue runs *after* the drop body
  returns. The flusher therefore wakes while C4 is still alive, and
  `finish_file`'s subsequent `Arc::try_unwrap` (line 749) sees
  `strong_count == 2`.
- **Workaround in place** (audit s.5): the spin-then-yield loop at
  `parallel_apply.rs:729-748` (shipped in PR #4665, commit
  `3e5d83d95dc6`) closes the window empirically but leaves the
  hazard structural.

DG-1 s.7 weighed five restructure options. Option B was preferred
over A (drop-order rearrangement is a non-fix), C (`Weak` keeps the
same race shape with a shorter window), D (cleanest semantics but
removes the in-flight diagnostic), E (atomic-only replacement
reinvents Condvar correctness), and F (formalising the spin defers
DG-3..DG-5). Option B's trade is one extra `Arc` per slot in exchange
for the unwrap target being structurally disjoint from the
notify-bearing Arc.

## 2. Spec: type split

Two new internal types replace `SlotBarrier`. Both remain
crate-private; no `pub` boundary changes. The current
`SlotBarrier` co-located three concerns: the per-file payload
(`slot: Mutex<FileSlot>`), the in-flight counter (`inflight:
Mutex<usize>`), and the wake-up primitive (`notify: Condvar`).
Option B splits along the payload-vs-bookkeeping seam.

### `BarrierState`

Holds the bookkeeping that the drop body touches: the in-flight
counter and the Condvar. This is the Arc the worker's drop body
keeps alive across `notify_all`.

```rust
/// Per-slot in-flight counter and Condvar.
///
/// The `Arc<BarrierState>` clone held by `DecrementGuard.barrier`
/// is the one whose drop is racy with `notify_all`. By construction,
/// `finish_file` does not `try_unwrap` this Arc: it only waits on
/// the counter via `wait_until_idle`.
struct BarrierState {
    inflight: Mutex<usize>,
    notify: Condvar,
}

impl BarrierState {
    fn new() -> Self {
        Self {
            inflight: Mutex::new(0),
            notify: Condvar::new(),
        }
    }

    fn increment_inflight(&self) { /* moved verbatim from SlotBarrier */ }
    fn decrement_inflight(&self) { /* moved verbatim from SlotBarrier */ }
    fn wait_until_idle(&self, ndx: FileNdx, kind: &'static str) -> io::Result<()> {
        /* moved verbatim from SlotBarrier */
    }
}
```

### `SlotData`

Holds the per-file payload (`FileSlot` behind its existing mutex).
This is the Arc `finish_file` unwraps to recover the destination
writer. `DecrementGuard` never touches it; rayon workers only touch
it through a `SlotHandle::lock_slot` borrow that never escapes the
work closure.

```rust
/// Per-file destination payload behind its own mutex.
///
/// The DashMap value stores `(Arc<SlotData>, Arc<BarrierState>)`.
/// `finish_file` calls `Arc::try_unwrap` (or `Arc::into_inner`) on
/// the `Arc<SlotData>` after `wait_until_idle` returns. Because the
/// worker's `DecrementGuard` only holds `Arc<BarrierState>`, the
/// race window from DG-1 s.4 disappears.
struct SlotData {
    slot: Mutex<FileSlot>,
}

impl SlotData {
    fn new(slot: FileSlot) -> Self {
        Self { slot: Mutex::new(slot) }
    }

    fn lock_slot(&self, ndx: FileNdx, kind: &'static str)
        -> io::Result<MutexGuard<'_, FileSlot>>
    {
        /* moved verbatim from SlotBarrier::lock_slot */
    }
}
```

### Pair carrier

To keep the DashMap value a single move-out, wrap the pair in a
small `Copy`-free struct. This makes `register_file` insertion and
`finish_file` removal symmetric and avoids tuple-field churn in the
five call sites that read both Arcs.

```rust
/// DashMap value: the two Arcs that together replace one
/// `Arc<SlotBarrier>`. Cloning a `SlotEntry` clones both Arcs.
#[derive(Clone)]
struct SlotEntry {
    data: Arc<SlotData>,
    barrier: Arc<BarrierState>,
}

impl SlotEntry {
    fn new(slot: FileSlot) -> Self {
        Self {
            data: Arc::new(SlotData::new(slot)),
            barrier: Arc::new(BarrierState::new()),
        }
    }
}
```

`SlotBarrier` itself is removed in the same change. DG-1 s.4 noted
the type had no callers outside the file, so removal does not break
any downstream crate.

## 3. Spec: Arc ownership model

The DG-1 race exists because two distinct ownership obligations -
"unwrap the payload" and "decrement the counter" - share one Arc.
Option B gives each its own Arc.

### Production strong counts per slot

```
DashMap shard                  : Arc<SlotData>      x1
                                 Arc<BarrierState>  x1
SlotHandle.data                : Arc<SlotData>      x1   (replaces C5)
DecrementGuard.barrier         : Arc<BarrierState>  x1   (replaces C4)
SlotHandle.barrier             : Arc<BarrierState>  x1   (new; needed
                                                          so lock_slot
                                                          + increment
                                                          stay co-located)
```

Steady-state strong counts when one rayon worker holds a `SlotHandle`:

- `Arc<SlotData>`: 2 (DashMap + `SlotHandle.data`).
- `Arc<BarrierState>`: 3 (DashMap + `SlotHandle.barrier` +
  `DecrementGuard.barrier`).

After `finish_file` calls `DashMap::remove`, the local owned binding
holds the only `Arc<SlotData>` clone that is **not** tied to a
worker's drop body, while a worker still mid-drop holds the
`Arc<BarrierState>` clone whose decrement-and-notify just fired.

### Invariant diagram

```
                  +----------------------+
                  |  DashMap<FileNdx,    |
                  |        SlotEntry>    |
                  +----+-------------+---+
                       | data        | barrier
                       v             v
              +--------+----+   +----+----------+
              |  SlotData   |   |  BarrierState |
              |  (Mutex<    |   |  (Mutex<usize>|
              |   FileSlot>)|   |   + Condvar)  |
              +--+----------+   +--^---^--------+
                 |                 |   |
   SlotHandle.data |   SlotHandle.barrier   DecrementGuard.barrier
                 |                 |   |
                 v                 v   v
              [worker]         [worker]  [worker's drop body]
```

The key invariant: **the Arc whose drop body fires `notify_all`
(`Arc<BarrierState>` via `DecrementGuard.barrier`) is a different
allocation from the Arc that `finish_file` calls `Arc::try_unwrap`
on (`Arc<SlotData>`)**. The two Arcs have independent strong-count
trajectories. The worker can be arbitrarily slow to retire its
`DecrementGuard` drop body without holding up the unwrap on the
unrelated payload Arc.

## 4. Spec: notify path

`BarrierState::notify_all` fires from
`BarrierState::decrement_inflight`, which is invoked only from
`DecrementGuard::drop`. The body is unchanged from the audit's s.3
trace; only the Arc the drop body holds is different.

Race-free sequence under Option B, from the worker's perspective:

1. Worker finishes its chunk, lets `SlotHandle` drop.
2. `SlotHandle.barrier: Arc<BarrierState>` is field-dropped first
   (declaration order); `SlotHandle.data: Arc<SlotData>` is
   field-dropped before `_decrement: DecrementGuard` (see s.6 for
   the new field order).
3. `DecrementGuard::drop` runs: calls
   `self.barrier.decrement_inflight()`, which acquires the inflight
   mutex, drops counter, releases the mutex, calls
   `self.barrier.notify.notify_all()`.
4. `DecrementGuard::drop` returns. Implicit field-drop glue runs
   for `DecrementGuard.barrier`, releasing the last
   `Arc<BarrierState>` clone the worker held.

Meanwhile in `finish_file` (the flusher):

A. `wait_until_idle` returns the instant the worker's step 3 sets
   the counter to zero and the Condvar predicate flips.
B. `DashMap::remove` runs; the shard's `Arc<SlotData>` and
   `Arc<BarrierState>` clones flow into a local `SlotEntry`
   binding.
C. The flusher drops the local `barrier: Arc<BarrierState>` (it
   never needed it past the wait).
D. The flusher calls `Arc::try_unwrap(slot_entry.data)`.

The Option B guarantee: the worker's step 4 can be arbitrarily
delayed without affecting step D. The worker still holds an
`Arc<BarrierState>` between steps 3 and 4, but that Arc does not
appear in any strong count `Arc::try_unwrap` looks at. By the time
step B runs, the `Arc<SlotData>` strong count is 2 (DashMap + the
worker's `SlotHandle.data`); after the `SlotHandle` finished
field-dropping `data` in step 2 the count was already 1 (DashMap
only); step B moves that single ownership into the local binding;
step D's unwrap sees strong count 1 deterministically.

The race-prone clone is on the `Arc<BarrierState>` graph - which
`finish_file` no longer inspects.

## 5. Spec: `try_unwrap` site

`finish_file` continues to use `Arc::try_unwrap`. Recommended
signature (paraphrased; DG-3 may inline differently):

```rust
let SlotEntry { data, barrier } = self
    .files
    .remove(&ndx)
    .map(|(_, entry)| entry)
    .ok_or_else(|| io::Error::other(format!("parallel applier file {ndx} unknown")))?;
drop(barrier);                                // explicit; no waiter holds it
let slot_data = Arc::try_unwrap(data).map_err(|still_shared| {
    ParallelApplyError::ApplierStillReferenced {
        ndx,
        strong_count: Arc::strong_count(&still_shared),
        kind: "finish_file",
    }
})?;
let slot = slot_data.slot
    .into_inner()
    .map_err(|_| ParallelApplyError::SlotPoisoned { ndx, kind: "finish_file" })?;
```

`Arc::into_inner` (stable since 1.70) is an acceptable alternative
that returns `Option<T>` instead of `Result<T, Arc<T>>`. DG-3 should
prefer `Arc::try_unwrap` so the diagnostic
(`ApplierStillReferenced { strong_count }`) keeps its existing shape
and the typed error remains identical. Switching to `into_inner`
would lose the live strong-count diagnostic and would need a new
error variant - not worth the churn for DG-2.a.

A Condvar wait on `BarrierState` would be redundant: `wait_until_idle`
already returns only once `inflight == 0`, and the new
`Arc<SlotData>` strong count tracks the worker's `SlotHandle.data`
field, which is dropped *before* `_decrement` (see s.6). When the
flusher resumes after `wait_until_idle`, every worker that decremented
has, by Rust's field-drop ordering, already released its
`Arc<SlotData>` clone. No second wait is needed.

## 6. Spec: worker drop body

`SlotHandle` field declaration order matters - Rust drops fields in
declaration order. The new layout is:

```rust
struct SlotHandle {
    /// Field 1: dropped first. Releases the worker's SlotData clone
    /// before any barrier work runs.
    data: Arc<SlotData>,
    /// Field 2: dropped second. Used by callers via lock_slot path
    /// adapters; held to keep increment+decrement co-located with
    /// the Arc the DecrementGuard tracks.
    barrier: Arc<BarrierState>,
    /// Field 3: dropped last. Its Drop impl runs decrement_inflight
    /// and notify_all on a *different* Arc than `data`.
    _decrement: DecrementGuard,
}

struct DecrementGuard {
    barrier: Arc<BarrierState>,
}

impl Drop for DecrementGuard {
    fn drop(&mut self) {
        self.barrier.decrement_inflight();
    }
}
```

Construction in `SlotHandle::new`:

```rust
fn new(entry: SlotEntry) -> Self {
    entry.barrier.increment_inflight();
    let decrement = DecrementGuard { barrier: Arc::clone(&entry.barrier) };
    Self { data: entry.data, barrier: entry.barrier, _decrement: decrement }
}
```

Sequence inside the worker:

1. `data: Arc<SlotData>` field-drop: strong count on the payload
   drops to (DashMap + flusher-local) = 2 if the flusher already
   removed, or DashMap-only = 1 the moment the flusher's
   `DashMap::remove` runs.
2. `barrier: Arc<BarrierState>` field-drop: harmless; the
   `DecrementGuard` still holds its own clone for the decrement
   call.
3. `_decrement: DecrementGuard` field-drop runs
   `decrement_inflight`, which decrements the counter and fires
   `notify_all`. The drop body completes; the contained
   `Arc<BarrierState>` is field-dropped by implicit glue.

The flusher's `wait_until_idle` returns at step 3's `notify_all`;
the flusher's subsequent `try_unwrap` on `Arc<SlotData>` is
unaffected by step 3's lingering `Arc<BarrierState>` because the
two allocations are independent.

## 7. Spec: backwards-compat shim (DG-2.c input)

**Recommendation: atomic single-PR migration. No shim required.**

Rationale:

- DG-1 s.4 confirmed `SlotBarrier`, `DecrementGuard`, and
  `SlotHandle` have zero callers outside `parallel_apply.rs`. The
  benchdoc reference at
  `crates/engine/benches/br_3j_f_dashmap_cores_vs_throughput.rs:40`
  is a comment, not a call.
- DG-1 s.8 confirmed the only external production caller of
  `ParallelDeltaApplier` is
  `crates/transfer/src/delta_pipeline/chunk_builder.rs`, which uses
  the public `apply_one_chunk` / `with_strategy` surface. None of
  the affected types (`SlotBarrier`, `DecrementGuard`,
  `SlotHandle`, `FileSlot`) appear in any public signature.
- The change is a pure intra-module restructure of crate-private
  types. The public `ParallelDeltaApplier` API, the public
  `ParallelApplyError` variants, and the public `flush_workers` /
  `drain_inflight` / `finish_file` / `register_file` signatures
  stay byte-identical.
- All eight affected sites (s.8) sit in a single 1579-line file
  plus a 41-line test fixture. The diff is mechanical: redirect
  references at the call sites, swap the field types in two
  structs, drop the spin loop.

A phased migration (e.g. introduce `BarrierState` alongside
`SlotBarrier`, migrate one call site per PR) would force the same
file to carry two coexisting implementations and would invite the
DG-1 race in any merge gap. DG-2.c should ratify the atomic
recommendation.

## 8. Spec: migration order (DG-2.b input)

Eight sites (7 audit-numbered Arc-clones + 1 audit-numbered
`DecrementGuard` construction) change. The order below is bottom-up:
types and their constructors before consumers, then call sites in
DashMap-shard-then-handle order, tests last. DG-2.b should adopt
this order so each intermediate compile state is valid.

| Step | Site (DG-1 ref) | File:line | Change |
|------|-----------------|-----------|--------|
| 1 | Type definition (audit s.3 `SlotBarrier`) | `parallel_apply.rs:292-356` | Delete `SlotBarrier`; add `BarrierState`, `SlotData`, `SlotEntry`. Move `lock_slot` to `SlotData`; move `increment_inflight` / `decrement_inflight` / `wait_until_idle` to `BarrierState`. |
| 2 | D1: `DecrementGuard` field | `parallel_apply.rs:363-365` | Retype `barrier: Arc<SlotBarrier>` -> `Arc<BarrierState>`. Drop body is unchanged code-wise (still `self.barrier.decrement_inflight()`). |
| 3 | C4 + C5: `SlotHandle` fields and constructor | `parallel_apply.rs:384-409` | Replace `barrier: Arc<SlotBarrier>` with `data: Arc<SlotData>` and `barrier: Arc<BarrierState>`. Field order: `data`, `barrier`, `_decrement`. Constructor takes `SlotEntry`. |
| 4 | C1: `register_file` constructor | `parallel_apply.rs:573` | Replace `Arc::new(SlotBarrier::new(FileSlot::new(...)))` with `SlotEntry::new(FileSlot::new(...))`. DashMap value type changes from `Arc<SlotBarrier>` to `SlotEntry`. |
| 5 | DashMap field declaration | `parallel_apply.rs:457` | `files: DashMap<FileNdx, Arc<SlotBarrier>>` -> `files: DashMap<FileNdx, SlotEntry>`. |
| 6 | C3: `slot_for` clone | `parallel_apply.rs:835-848` | Replace `Arc::clone(guard.value())` with `SlotEntry::clone(guard.value())`. `SlotHandle::new` now takes the entry. |
| 7 | C2: `flush_workers` clone | `parallel_apply.rs:794-805` | Replace `Arc::clone(guard.value())` with `Arc::clone(&guard.value().barrier)`; the local binding becomes `Arc<BarrierState>`. `wait_until_idle` call unchanged. |
| 8 | `finish_file` unwrap branch | `parallel_apply.rs:703-755` | `DashMap::remove` returns `SlotEntry`. Drop the `barrier` Arc explicitly. Remove the spin loop (lines 720-748). Call `Arc::try_unwrap` on `entry.data`. Source the `slot` from `SlotData::slot.into_inner()`. |
| 9 | C6 + C7: test fixtures | `parallel_apply.rs:1182, 1228, 1279, 1317, 1324, 1327` | Adapt tests that reach into the DashMap value to grab a barrier. Each touches `guard.value().barrier` (the new `Arc<BarrierState>` field) instead of the whole value. Drop the spin-loop test scenario in `finish_file_calls_flush_workers_internally:1259-1297` (the race it covers is gone). |

Steps 1-3 land the new types and the new `DecrementGuard` / `SlotHandle`
shapes without touching any call site. Steps 4-5 swap the DashMap
value type, requiring all consumers to be updated in the same commit -
hence the atomic recommendation in s.7. Steps 6-8 update the three
consumers (`slot_for`, `flush_workers`, `finish_file`) and remove the
spin. Step 9 fixes the test fixtures.

DG-1 s.8 also flags `bytes_written:684-689`, `apply_one_chunk:618-636`,
and `apply_batch_parallel:650-676` as touched. They consume the
output of `slot_for` and so transitively flip through step 6 - no
additional source edits required beyond a possible type-name nudge
if a helper had a `&Arc<SlotBarrier>` parameter.

## 9. What changes in `finish_file`

`finish_file` carries the workaround DG-1 s.5 documents. The current
production code at `crates/engine/src/concurrent_delta/parallel_apply.rs:703-772`
includes:

- A comment block at lines 720-728 explaining the wake-before-drop
  window.
- A bounded spin at lines 729-748 (32 iterations of
  `std::hint::spin_loop()` followed by `std::thread::yield_now()`,
  capped at 1000 iterations) that waits for
  `Arc::strong_count(&slot_arc) > 1` to become false.
- A failure-mode branch at line 736 surfacing
  `ParallelApplyError::ApplierStillReferenced` when the spin times
  out.

Under Option B these all disappear. The new `finish_file` body
flows:

1. `self.flush_workers(ndx)?` - unchanged. `wait_until_idle` on
   `Arc<BarrierState>` returns once the counter hits zero.
2. `let (_, entry) = self.files.remove(&ndx).ok_or_else(...)` -
   replaces line 716; `entry: SlotEntry` carries both Arcs.
3. `drop(entry.barrier);` - explicit release of the
   `Arc<BarrierState>` clone the flusher does not need past the
   wait. Saves the future reader a `let _ = ...` puzzle.
4. `let slot_data = Arc::try_unwrap(entry.data).map_err(...)?` -
   replaces the spin and the wrapped `try_unwrap` at line 749. The
   error variant and `strong_count` diagnostic are preserved. The
   loop comment block at lines 720-728 deletes wholesale.
5. `let slot = slot_data.slot.into_inner().map_err(...)?` -
   unchanged in shape from current line 756, retargeted to the
   `SlotData` field.
6. The drained-check at lines 763-770 is unchanged.

The race goes away because of s.4: the worker's still-live
`Arc<BarrierState>` between `notify_all` and the end of
`DecrementGuard::drop` no longer participates in the
`Arc<SlotData>` strong count. The spin was a probabilistic guard
against a structural mismatch; Option B removes the mismatch and
makes the guard unnecessary.

This unblocks DG-4 (`Remove finish_file spin-then-yield
workaround`) automatically - DG-4 becomes a no-op once DG-3 lands,
or folds into DG-3 itself.

## Closing notes

- DG-1 s.6 (platform behaviour) is the reason Option B has to land
  cross-platform - the underlying memory model permits the race on
  every supported OS; only the scheduler distribution differs.
  Option B's correctness is independent of platform.
- DG-1 s.7 noted Option B's cost as "one extra atomic per
  `register_file` and per `slot_for`". The `slot_for` cost is one
  `SlotEntry::clone` (two `Arc::clone`s) per chunk dispatch instead
  of one `Arc::clone`. On the receive-delta hot path this is
  amortised across the chunk's verify+write cost and was measured
  inconsequential during BR-3j.f benching.
- The new `Arc<SlotData>` and `Arc<BarrierState>` types are
  module-private. No public API, no exported error variant, and
  no downstream-crate caller signature changes. The `transfer`
  crate's `chunk_builder` consumer continues to use
  `apply_one_chunk` / `with_strategy` as today.
- DG-5 (1000-thread concurrent `finish_file` stress test) becomes
  the regression guard for Option B's correctness; it should be
  written against the new shape and run on Windows specifically to
  cover the platform where the race was first observed.
