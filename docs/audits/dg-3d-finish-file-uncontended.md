# DG-3.d: `finish_file` `try_unwrap` Target Uncontended After DG-3.c

Verifies the DG-2.a Option-B strong-count invariant at the actual
`Arc::try_unwrap` call site after DG-3.c (#4845) retyped
`DecrementGuard.barrier` from `Arc<SlotBarrier>` to `Arc<BarrierState>`.

References:

- Design: `docs/design/dg-2a-option-b-spec.md` sections 3 (strong-count
  table) and 4 (race-free notify path).
- DG-3.c PR: #4845 (DG-3.c retype).
- Audit precursors: DG-1 release-race trace (s.5),
  DG-2.a Option-B spec.

## 1. Unwrap-target Arc identification

`finish_file` lives in
`crates/engine/src/concurrent_delta/parallel_apply/drain.rs:49-121`.
The body extracts a `SlotEntry` via `DashMap::remove`, destructures
into `data: Arc<SlotData>` and `barrier: Arc<BarrierState>`, drops the
barrier Arc explicitly, then calls `Arc::try_unwrap(data)`.

**Unwrap target**: `Arc<SlotData>`
(`crates/engine/src/concurrent_delta/parallel_apply/slot_barrier.rs:261`).
`SlotData` wraps `Mutex<FileSlot>` and is the per-file payload half of
the DG-2.a Option-B split.

## 2. Clone trajectory of `Arc<SlotData>`

Every site that touches the payload Arc:

| # | Site | File:line | Effect on strong count |
|---|------|-----------|-----------------------|
| 1 | `SlotEntry::new` | `slot_barrier.rs:341-346` | Constructs at count = 1; lives in the DashMap entry. |
| 2 | `SlotEntry::clone` (derived) | `slot_barrier.rs:325` | Clones the inner Arc - used by `slot_for` step 3. |
| 3 | `slot_for` local entry binding | `mod.rs:630-634` | DashMap read clones the `SlotEntry`; the local `entry` carries one cloned Arc until end-of-expression. |
| 4 | `SlotBarrier::from_entry` | `slot_barrier.rs:76-81` | Clones `entry.data` into the transient adapter's `data` field. |
| 5 | `SlotHandle.barrier: Arc<SlotBarrier>` | `mod.rs:298-323` | The adapter (and thus its inner `Arc<SlotData>` clone) is the only payload reference the worker carries. |
| 6 | `finish_file` local `data` binding | `drain.rs:72` | After `DashMap::remove` the payload Arc moves out of the entry into the flusher's stack frame. |

Sites the payload Arc **does not** touch after DG-3.c:

- `DecrementGuard.barrier: Arc<BarrierState>` (post-DG-3.c) - the
  retyped field lives on the bookkeeping allocation. The guard never
  cloned the payload Arc to begin with under Option B.
- `flush_workers` - clones `Arc<BarrierState>` only
  (`drain.rs:146-158`).

## 3. Worker drop sequence

`SlotHandle` field declaration (`mod.rs:298-301`):

```rust
struct SlotHandle {
    barrier: Arc<SlotBarrier>,
    _decrement: DecrementGuard,
}
```

Drop order under Rust field-drop semantics:

1. `barrier: Arc<SlotBarrier>` drops. The adapter is fresh per
   `slot_for` invocation (line 635: `Arc::new(SlotBarrier::from_entry(&entry))`),
   so its strong count goes 1 -> 0 and the `SlotBarrier` is dropped.
   That releases the adapter's `Arc<SlotData>` clone (site 4). Payload
   Arc strong count: 2 -> 1 (DashMap only).
2. `_decrement: DecrementGuard` drops. `decrement_inflight()` acquires
   the inflight mutex, decrements, releases the mutex, fires
   `notify_all`. The drop body returns; the contained
   `Arc<BarrierState>` is field-dropped by implicit glue.

The DG-1 wakeup-before-drop window survives between the `notify_all`
inside `decrement_inflight` (step 2 mid-body) and the implicit drop
of the guard's `Arc<BarrierState>` (step 2 end). The flusher's
`flush_workers` returns the instant the Condvar predicate flips; if
the flusher then unwrapped `Arc<BarrierState>` it would still race.
But the flusher unwraps `Arc<SlotData>`, which was released back in
step 1 - before the notify fired.

## 4. Strong-count trajectory through `finish_file`

| Step | Action | `Arc<SlotData>` count |
|------|--------|----------------------:|
| A | `flush_workers` returns (inflight=0). Worker has finished step 1; guard is mid-drop. | 1 (DashMap only) |
| B | `DashMap::remove` extracts the entry; `data` moves into local. | 1 (local only) |
| C | `drop(barrier)` releases the local `Arc<BarrierState>`. | 1 (unchanged - barrier is a different allocation) |
| D | `Arc::try_unwrap(data)` runs. | 0 on success |

The trajectory holds even if the guard's `Arc<BarrierState>` is
arbitrarily slow to retire after step A: the payload Arc has no
remaining reference on the worker side past step 1.

## 5. Regression coverage added by DG-3.d

`crates/engine/src/concurrent_delta/parallel_apply/mod.rs` test module:

- `finish_file_payload_arc_uncontended_after_worker_drop` -
  deterministic three-channel handshake (acquired / release / dropped)
  pins the moment the worker has fully retired its `SlotHandle`,
  reads `Arc::strong_count` on the entry's payload Arc, asserts it is
  exactly 1, then runs `finish_file` to confirm `try_unwrap` succeeds.
- `finish_file_payload_arc_uncontended_under_burst` - drives 32
  serial `apply_one_chunk` calls and asserts the per-iteration
  post-drop strong count returns to 1, catching any future change
  that re-introduces a payload-Arc clone on the worker drop path.
- `finish_file_payload_and_barrier_arcs_are_distinct_allocations` -
  structural witness via `Arc::as_ptr` that the Option-B split is
  intact at the type level; a future refactor that collapses the
  pair behind one Arc fails here.

The three tests together pin both the runtime invariant (counts) and
the structural invariant (distinct allocations) so a regression cannot
slip past either gate.

## 6. Residual risk

- `SlotHandle` retype is **deferred**. DG-3.c only retyped
  `DecrementGuard`; `SlotHandle.barrier` is still
  `Arc<SlotBarrier>` (the transitional adapter). Per
  `slot_barrier.rs:48-59`, the adapter survives only as the handle's
  bridge until a follow-on DG-3.x task collapses it. The bridge holds
  one `Arc<SlotData>` + one `Arc<BarrierState>`, both of which drop
  with `SlotHandle.barrier` (step 1 above) - so the deferral does
  not regress the DG-3.d invariant, but it leaves dead code paths
  (`SlotBarrier::lock_slot`, `SlotBarrier::increment_inflight`) that
  the follow-on task should retire.
- `drain.rs::finish_file` retains a spin-then-yield loop
  (`drain.rs:84-103`) for `Arc::strong_count(&data) > 1`. After
  DG-3.d the spin should be a no-op on every path the regression
  tests exercise: the body executes zero iterations. **DG-4 (remove
  the spin)** is now safe to land - the tests added here cover the
  exact precondition DG-4 needs to assume.
- The `dead_code`-allowed `slot_barrier::SlotHandle` (lines 391-417)
  is the eventual replacement type. DG-3.d does not touch it; the
  next DG-3.x task slots it into the mod-level position and deletes
  the adapter.

## 7. Conclusion

After DG-3.c the `Arc<SlotData>` strong count at the `try_unwrap`
call site is **1** on every successful path, deterministically and
without spinning. The DG-2.a Option-B claim ("the worker's
DecrementGuard cannot block the flusher's payload unwrap") holds at
the source level and is now pinned by three regression tests.

DG-4 (spin-loop removal) is unblocked.
