# DG-2.b: Migration order for the `BarrierState` + `SlotData` split

DG-2.a (see `docs/design/dg-2a-option-b-spec.md`) fixed the target
shape: `SlotBarrier` splits into `BarrierState` (in-flight counter +
Condvar) and `SlotData` (per-file `Mutex<FileSlot>`), packaged in a
`SlotEntry` carrier. DG-2.a s.7 recommended an atomic single-PR
migration as the only sensible cutover for behaviour. This document
takes the opposite-end planning question: **if the team prefers to
ship the same restructure phased across separate, individually
shippable PRs, what is the right order?** DG-2.c will pick between
the atomic recommendation and the phased recipe specified here.

The phased recipe targets three independent goals:

- Each PR compiles, lints, and runs the full nextest suite green in
  isolation. No coexistence gap leaves the DG-1 race observable in any
  reachable code path.
- Each PR is bisectable for regressions: if a future failure shows up,
  `git bisect` can pinpoint exactly which step introduced it.
- Each PR is reversible without rebase pain on the steps that follow.

Source under audit:
`crates/engine/src/concurrent_delta/parallel_apply.rs`
(1579 LoC, commit `04dc51ef1`).

Bug history this sequence retires:

- `project_slothandle_decrementguard_release_race` - the structural
  race between `Condvar::notify_all` and the `Arc<SlotBarrier>` clone
  in `DecrementGuard.barrier`. The DG-1 audit's s.4 trace.
- `project_finish_file_arc_unwrap_ergonomics` - the
  `ApplierStillReferenced` error variant becoming a routine race
  outcome instead of a real "caller still holds a SlotHandle" signal.
- `project_concurrent_dispatch_test_flake` - the registrar-race in
  `concurrent_register_and_dispatch_on_overlapping_files` that
  SSC-1 (PR #4667) patched with a `registrations_done` gate. The
  phased migration's DG-3.e step re-exercises this test under the
  Option B shape to confirm the existing fix still holds.

## 1. Enumeration of every call site that touches `SlotBarrier` today

The DG-1 audit numbered seven `Arc::clone` sites (C1..C7) and one
`DecrementGuard` construction (D1). DG-2.a s.8 confirmed those eight
sites plus the type definitions form the entire diff surface. The
table below maps each one to current `parallel_apply.rs` line
numbers and indicates the migration step it belongs to under the
phased recipe in s.3.

| # | Site | File:line | Kind | Phased step |
|---|------|-----------|------|-------------|
| T1 | `SlotBarrier` struct | `parallel_apply.rs:292-296` | Type definition | DG-3.a (add), DG-3.b (deprecate), DG-3.c (delete) |
| T2 | `impl SlotBarrier` | `parallel_apply.rs:298-356` | Inherent impl | Same as T1 |
| T3 | `DecrementGuard` struct + field | `parallel_apply.rs:363-365` | Type definition | DG-3.c (retype field) |
| T4 | `impl Drop for DecrementGuard` | `parallel_apply.rs:367-371` | Trait impl | DG-3.c (body unchanged; only field type flips) |
| T5 | `SlotHandle` struct + fields | `parallel_apply.rs:384-387` | Type definition | DG-3.c (add `data`, rename `barrier`, reorder fields) |
| T6 | `SlotHandle::new` | `parallel_apply.rs:389-401` | Constructor | DG-3.c (takes `SlotEntry`; clones `barrier` for `DecrementGuard`) |
| T7 | `SlotHandle::lock_slot` | `parallel_apply.rs:403-408` | Method | DG-3.c (delegates to `SlotData::lock_slot`) |
| C1 | `register_file` SlotBarrier ctor | `parallel_apply.rs:573` | Arc-allocating constructor | DG-3.b (DashMap value-type swap) |
| F1 | `files: DashMap<FileNdx, Arc<SlotBarrier>>` field | `parallel_apply.rs:457` | Struct field | DG-3.b (field type swap) |
| C2 | `flush_workers` Arc::clone | `parallel_apply.rs:801` | Arc::clone of shard value | DG-3.b (clone only `entry.barrier`) |
| W1 | `flush_workers` wait | `parallel_apply.rs:804` | `barrier.wait_until_idle(...)` | DG-3.b (binding becomes `Arc<BarrierState>`) |
| C3 | `slot_for` Arc::clone | `parallel_apply.rs:845` | Arc::clone of shard value | DG-3.b (clones the `SlotEntry`) |
| H1 | `slot_for` constructs `SlotHandle` | `parallel_apply.rs:847` | SlotHandle::new call | DG-3.c (constructor signature flips) |
| C4 | `DecrementGuard.barrier` from `&barrier` | `parallel_apply.rs:395` | Arc::clone for DecrementGuard | DG-3.c (clones `entry.barrier`) |
| C5 | `SlotHandle.barrier` field write | `parallel_apply.rs:398` | Arc move into SlotHandle | DG-3.c (one `barrier`, one `data`) |
| H2 | `apply_one_chunk` handle | `parallel_apply.rs:625` | `slot_for(...)` consumer | No edit (return type stable) |
| H3 | `apply_batch_parallel` handle | `parallel_apply.rs:671` | `slot_for(...)` consumer | No edit |
| H4 | `bytes_written` handle | `parallel_apply.rs:686` | `slot_for(...)` consumer | No edit |
| U1 | `finish_file` `Arc::try_unwrap` | `parallel_apply.rs:749-755` | Arc::try_unwrap target | DG-3.b (target becomes `entry.data`), DG-4 (delete spin) |
| S1 | `finish_file` spin-then-yield loop | `parallel_apply.rs:729-748` | Workaround block | DG-4 (delete) |
| B1 | `finish_file` `slot.into_inner()` | `parallel_apply.rs:756-762` | Mutex unwrap | DG-3.b (sourced from `slot_data.slot`) |
| C6 | Test `flush_workers_survives_spurious_wakeup` Arc::clone | `parallel_apply.rs:1324` | Test-only Arc::clone | DG-3.b (clones `entry.barrier`; touches `notify` directly) |
| C7 | Same test, `notifier_barrier` Arc::clone | `parallel_apply.rs:1327` | Test-only Arc::clone | DG-3.b (clones `Arc<BarrierState>`) |
| N1 | Same test, `barrier.notify.notify_all()` | `parallel_apply.rs:1331` | Direct Condvar access | DG-3.b (path becomes `barrier.notify.notify_all()` on the new `BarrierState`) |
| T8 | `finish_file_calls_flush_workers_internally` | `parallel_apply.rs:1259-1297` | Test scenario for the spin path | DG-4 (delete - the race the spin hides is gone) |

**Total identified call sites: 25.** Eight match DG-1's structural
sites (C1..C7, D1); the rest are the type definitions (T1..T8), the
DashMap field (F1), the `wait_until_idle` invocation (W1), the
unwrap and write-back path in `finish_file` (U1, S1, B1), the three
`slot_for` consumers (H2..H4), the new `SlotEntry` clone in `slot_for`
(C3), the `SlotHandle::new` call (H1), and the test-only Condvar
notify path (N1).

DG-2.a s.7 cited "eight affected sites" by counting the structural
Arc-clones (seven) plus the `DecrementGuard` construction (one). The
broader enumeration above reflects every line a phased PR has to
touch or audit, not just the Arc-graph mutations.

External crates: zero. The DG-1 audit confirmed that `SlotBarrier`,
`DecrementGuard`, and `SlotHandle` have no callers outside
`parallel_apply.rs`. The benchdoc reference at
`crates/engine/benches/br_3j_f_dashmap_cores_vs_throughput.rs:40`
is a comment. The `transfer` crate's `chunk_builder.rs` consumer uses
only the public `apply_one_chunk` / `with_strategy` surface and is
unaffected.

## 2. Dependency graph

Edges below capture compile-time and behavioural dependencies, not
file co-location.

```
  T1/T2 (SlotBarrier type)
    |
    | introduces BarrierState + SlotData + SlotEntry as new types
    v
  DG-3.a: add new types alongside SlotBarrier
    |
    | (zero call-site change; SlotBarrier still in use)
    v
  F1 + C1 + C2 + C3 + W1 + B1 + U1 + C6 + C7 + N1
    |
    | DashMap value-type swap; payload-vs-barrier ownership split
    | becomes observable in test fixtures and finish_file
    v
  DG-3.b: migrate files map + finish_file to SlotEntry / SlotData
    |
    | (SlotHandle still keyed on a single Arc; spin loop still present)
    v
  T3 + T4 + T5 + T6 + T7 + H1 + C4 + C5
    |
    | DecrementGuard / SlotHandle adopt the new ownership shape;
    | the structural race window closes here
    v
  DG-3.c: migrate DecrementGuard + SlotHandle (race fix)
    |
    v
  DG-3.d: audit-only verification of finish_file try_unwrap invariants
    |
    v
  DG-3.e: stress-test concurrent_register_and_dispatch under the
          Option B shape; confirm SSC-1's gate still holds
    |
    v
  SPL-38.e: extract finish_file + flush_workers into drain.rs;
            the spin-then-yield workaround can be deleted (not moved)
            because DG-3.e proved it is no longer load-bearing
    |
    v
  S1: delete spin-then-yield workaround
    |
    v
  DG-4: drop S1 + T8 (the test that probed the spin-loop scenario)
    |
    v
  DG-2.c (decision): atomic vs phased; this document is its input
```

### What must change together vs what can move independently

- T1 and T2 are inseparable: an `impl` block with no struct, or vice
  versa, does not compile. They move together within DG-3.a.
- T3 and T4 are inseparable for the same reason; both flip in DG-3.c.
- T5, T6, T7 are inseparable: changing the field set forces a
  matching constructor and any methods that read the fields.
- F1 and C1 must change together: the DashMap value type and the
  `register_file` insertion expression share a static type signature.
- C2 and W1 must change together: the local binding's type
  (`Arc<SlotBarrier>` -> `Arc<BarrierState>`) is observable to the
  next-line `wait_until_idle` call.
- C3 and H1 must change together: `slot_for`'s `Arc::clone` is
  consumed by `SlotHandle::new` on the very next line.
- U1, S1, B1 must be reasoned about together: removing the spin (S1)
  is only safe once U1 targets `SlotData` instead of `SlotBarrier`
  and B1 sources from `SlotData::slot`. The DG-3.c -> DG-4 sequence
  encodes this dependency.
- H2, H3, H4 do not change: they consume `slot_for`'s return value
  through `.lock_slot(...)`, whose signature is preserved across all
  phased steps.

### What can move independently

- DG-3.a (introduce types) can land any time after DG-2.a is merged.
  It is self-contained: zero call-site change.
- DG-3.d (audit) does not touch any code; it can run in parallel
  with DG-3.c review.
- DG-3.e (stress test) lands as a test-only PR; it depends on
  DG-3.c's race fix but does not modify production code.
- SPL-38.e (extract `drain.rs`) is independent of DG-3.c as a code
  move, but the SPL-38 audit (s.5) and this document both
  recommend ordering it **after** DG-3.e so the spin-loop block is
  deleted (in DG-4) rather than transcribed verbatim into a new file
  and deleted on the round-trip.

## 3. Recommended migration order

Seven shippable steps. Each is sized so one reviewer can read it in
one sitting, the diff fits on one screen for the smaller steps, and
the largest step (DG-3.b) is still mechanical enough to bisect
against.

### DG-3.a - Add `BarrierState`, `SlotData`, `SlotEntry` alongside `SlotBarrier`

**Goal**: introduce the three new types and their methods without
removing or renaming anything. `SlotBarrier` continues to back every
production call site.

**Code edits** (all in `crates/engine/src/concurrent_delta/parallel_apply.rs`):

- Add `struct BarrierState { inflight: Mutex<usize>, notify: Condvar }`
  with `new`, `increment_inflight`, `decrement_inflight`,
  `wait_until_idle` - bodies copied verbatim from `SlotBarrier`.
- Add `struct SlotData { slot: Mutex<FileSlot> }` with `new` and
  `lock_slot` - body copied verbatim from `SlotBarrier::lock_slot`.
- Add `struct SlotEntry { data: Arc<SlotData>, barrier: Arc<BarrierState> }`
  with `#[derive(Clone)]` and an `fn new(slot: FileSlot) -> Self`.
- Gate the new types behind `#[allow(dead_code)]` until DG-3.b wires
  them in.

**Why this first**:

- Zero behavioural change. The compiled binary is byte-identical
  modulo the new (unused) symbol table entries.
- Builds in isolation: no call site flips, no test edit. Clippy stays
  green because `dead_code` is allowed scope-locally.
- Lets DG-3.b and DG-3.c bisect cleanly. If a regression surfaces
  after DG-3.b, the bisect endpoints become "before new types
  existed" (master) and "after new types wired in" (DG-3.b head),
  which is exactly the call-site change set.

**Diff size**: ~70 LoC added (struct definitions + verbatim impl
copies). Zero LoC removed.

**Bug history**: none retired. Setup-only.

### DG-3.b - Migrate `ParallelDeltaApplier::files` map and `finish_file` to `SlotEntry` / `SlotData`

**Goal**: swap the DashMap value type from `Arc<SlotBarrier>` to
`SlotEntry`. Update every shard-guard reader. Retarget
`finish_file`'s `Arc::try_unwrap` from the (combined) barrier to
`entry.data`. **Leave the spin-then-yield loop in place** -
verbatim - so this step is a pure structural swap without changing
the race-mitigation contract. **Leave `DecrementGuard` /
`SlotHandle` untouched** - they still hold `Arc<SlotBarrier>`, with
`SlotBarrier` becoming a transitional adapter that wraps a
`SlotEntry`.

**Transitional shape**: `SlotBarrier` becomes a thin wrapper around
`SlotEntry`. Its `new(slot: FileSlot)` allocates the entry; its
`lock_slot` delegates to `SlotData::lock_slot`; its
`increment_inflight`, `decrement_inflight`, and `wait_until_idle`
delegate to `BarrierState`. The DashMap stores
`(FileNdx, SlotEntry)` directly. `slot_for` builds an
`Arc<SlotBarrier>` from the entry to keep the existing handle
constructor signature. This is the only step that introduces a
short-lived adapter; DG-3.c removes the adapter when it migrates
the handle types.

**Code edits** (all in `parallel_apply.rs`):

- F1: change `files: DashMap<FileNdx, Arc<SlotBarrier>>` to
  `files: DashMap<FileNdx, SlotEntry>`.
- C1: change `Arc::new(SlotBarrier::new(...))` to `SlotEntry::new(...)`.
- C2: rewrite `Arc::clone(guard.value())` as
  `Arc::clone(&guard.value().barrier)` so the local binding is
  `Arc<BarrierState>`.
- W1: no source change; the binding's new type makes the call
  resolve to `BarrierState::wait_until_idle`.
- C3: rewrite `slot_for`'s clone as
  `let entry = guard.value().clone();` followed by an adapter step
  that builds an `Arc<SlotBarrier>` from the entry's components.
- U1, B1: `finish_file` removes the entry via
  `self.files.remove(&ndx)`, drops `entry.barrier` explicitly, and
  calls `Arc::try_unwrap(entry.data)` to recover the `SlotData`;
  `slot_data.slot.into_inner()` sources the writer.
- S1: **unchanged**. The spin-then-yield loop stays exactly where
  it sits at lines 729-748. The comment block at 720-728 stays.
- C6, C7, N1: tests touch `entry.barrier` instead of the combined
  `SlotBarrier` Arc; the manual `notify_all()` call resolves to
  `BarrierState::notify`.

**Why this ordering**:

- The DashMap value-type swap is the single biggest mechanical edit.
  Doing it before the handle refactor means DG-3.c has a narrow,
  focused diff (handle field shape only) instead of being entangled
  with map-shape changes.
- Keeping the spin loop in place means this step is verifiably
  behaviour-preserving: the race-mitigation window the spin covers
  is identical before and after. Reviewers can confirm via
  `git diff --stat` that lines 720-748 are untouched.
- The transitional `SlotBarrier` wrapper introduces no new race -
  the DG-1 race is structural to the
  `DecrementGuard.barrier: Arc<SlotBarrier>` field, which DG-3.b
  leaves intact. The wrapper is dead by DG-3.c and gets dropped
  there.

**Diff size**: ~120 LoC changed across F1, C1, C2, C3, U1, B1, and
the three test-fixture sites. Spin loop and comment block untouched.

**Bug history**: none retired here. Sets the stage for DG-3.c.

### DG-3.c - Migrate `DecrementGuard` + `SlotHandle` (the race-fix step)

**Goal**: retype `DecrementGuard.barrier` from `Arc<SlotBarrier>` to
`Arc<BarrierState>`. Reshape `SlotHandle` to hold both
`data: Arc<SlotData>` and `barrier: Arc<BarrierState>` with the
declaration order spec'd in DG-2.a s.6. Delete the transitional
`SlotBarrier` adapter. This is the structural fix for DG-1's race -
the `Arc::try_unwrap` target (`SlotData`) is now a different
allocation from the Arc the `DecrementGuard` drop body holds
(`BarrierState`).

**Code edits**:

- T3, T4: `DecrementGuard.barrier: Arc<SlotBarrier>` becomes
  `Arc<BarrierState>`. Drop body unchanged.
- T5: `SlotHandle` declares fields in order
  `data: Arc<SlotData>`, `barrier: Arc<BarrierState>`,
  `_decrement: DecrementGuard`.
- T6: `SlotHandle::new` takes `entry: SlotEntry`, calls
  `entry.barrier.increment_inflight()`, builds the
  `DecrementGuard` from `Arc::clone(&entry.barrier)`, then moves
  `entry.data` and `entry.barrier` into the new handle.
- T7: `SlotHandle::lock_slot` delegates to `self.data.lock_slot(...)`.
- C4: `DecrementGuard { barrier: Arc::clone(&entry.barrier) }`.
- C5: replaced by the moves in T6 (no separate field write).
- H1: `slot_for` returns `SlotHandle::new(entry)` directly; the
  `Arc<SlotBarrier>` adapter from DG-3.b is gone.
- T1, T2: delete `SlotBarrier` and its impl.

**Why this is the race-fix step and not DG-3.b**:

The DG-1 race lives in `DecrementGuard.barrier`. As long as that
field holds an `Arc<SlotBarrier>` (DG-3.b's interim shape), the
notify-before-Arc-drop race is unchanged. DG-3.c is the first step
where the worker's `DecrementGuard` drop body holds an Arc whose
allocation is independent of the one `finish_file` unwraps. The
spin in `finish_file` becomes structurally redundant from this
step forward, but it is **not** removed yet - DG-4 owns the
deletion, gated on DG-3.e's stress test.

**Diff size**: ~60 LoC changed (handle structs + constructor + the
adapter removal), ~80 LoC removed (the transitional SlotBarrier
adapter from DG-3.b).

**Bug history retired**: `project_slothandle_decrementguard_release_race`.
The structural race that the spin-then-yield loop in `finish_file`
papers over is closed at this step.

### DG-3.d - Verify `finish_file` `try_unwrap` target invariants (audit-only)

**Goal**: confirm in writing that DG-3.c's restructure satisfies the
invariants DG-2.a s.4 and s.5 enumerate. No code changes.

**Deliverable**: short audit document
(`docs/audits/dg-3d-finish-file-invariant-check.md`, ~150 lines)
that walks through:

- The four Arc allocations alive when `finish_file` runs (DashMap
  `SlotData`, DashMap `BarrierState`, worker `SlotHandle.data`,
  worker `DecrementGuard.barrier`).
- Strong-count trajectories per allocation across the worker's
  field-drop sequence (DG-2.a s.6 steps 1-3).
- The flusher's wait-then-remove-then-unwrap sequence (DG-2.a s.4
  steps A-D).
- A line-by-line confirmation that `Arc::try_unwrap(entry.data)`
  sees strong count 1 deterministically given Rust's field-drop
  order and the `wait_until_idle` contract.

**Why this exists as a separate step**: the DG-3.c review will be
focused on the mechanical type churn. The invariant proof deserves
its own page so the next person to touch `finish_file` does not
have to re-derive it from scratch. Audit-only steps are zero-risk
and parallelisable.

**Diff size**: new markdown document, ~150 lines. Zero code.

**Bug history retired**: documents the closure of
`project_finish_file_arc_unwrap_ergonomics` - the
`ApplierStillReferenced` error variant continues to exist as a
diagnostic but is no longer a routine race outcome. The audit
states precisely when it can fire (caller raced a new `slot_for`
against `finish_file` - DG-1 s.5's "real bug" condition).

### DG-3.e - Stress test `concurrent_register_and_dispatch` under Option B

**Goal**: re-exercise the SSC-1 stress test
(`concurrent_register_and_dispatch_on_overlapping_files` at
`crates/engine/tests/parallel_apply_concurrent.rs:178`) under the
new Option B shape. Confirm the `registrations_done` gate added in
PR #4667 still holds, and that no `ApplierStillReferenced` error
fires from the worker race even when the spin loop in
`finish_file` is conceptually unnecessary.

**Test additions** (in
`crates/engine/tests/parallel_apply_concurrent.rs`):

- A second stress scenario,
  `finish_file_under_concurrent_dispatch_no_spin`, that registers a
  file, dispatches a chunk on a rayon worker, calls `finish_file`
  on the main thread, and asserts the call returns `Ok(writer)`
  without ever entering the spin path. Use a feature gate (or a
  debug-only counter) on the spin loop so the test can assert
  `spin_iterations_observed == 0`.
- A Windows-targeted variant (`#[cfg(windows)]`) that runs the
  same scenario on a single-threaded rayon pool to maximise the
  scheduler-preemption window DG-1 s.6 identifies.

**Why before DG-4 / SPL-38.e**: DG-3.e is the empirical gate for
removing the spin loop. The DG-3.d invariant proof is necessary but
not sufficient on its own - the project memory note
`project_concurrent_dispatch_test_flake` describes a master-side
flake that was masked by the spin path on Windows. DG-3.e proves
the flake does not return when the spin path becomes a no-op.

If DG-3.e reveals a new race, DG-4 and SPL-38.e are blocked. The
fix lands as either a follow-up to DG-3.c or as a new ticket
DG-3.f, and the spin loop stays put until the new race is closed.

**Diff size**: ~80 LoC of new tests. Zero production change.

**Bug history retired**: re-validates the closure of
`project_concurrent_dispatch_test_flake` under the Option B shape.
The `registrations_done` gate from PR #4667 is preserved; this step
proves the Option B restructure does not regress its behaviour.

### SPL-38.e - Extract `finish_file` + `flush_workers` into `drain.rs`

**Goal**: lift the three drain-surface methods (`finish_file`,
`flush_workers`, `drain_inflight`) out of `parallel_apply.rs` and
into a new `parallel_apply/drain.rs` submodule per the SPL-38 spec.
The spin-then-yield workaround can be **deleted at this step rather
than moved**, because DG-3.e proved it is no longer load-bearing.

**Code edits**: per SPL-38 spec s.3.4. The deletion of lines 729-748
(spin loop) and the comment block at 720-728 lands in this PR
rather than in DG-4. DG-4 then becomes a no-op marker ticket.

**Alternative**: if the team prefers, SPL-38.e moves the spin loop
verbatim and DG-4 deletes it from `drain.rs` afterwards. Both
orderings produce the same final state; the recommendation here is
to bundle the deletion with the move so the SPL-38.e diff stands
on its own as "drain extraction is complete".

**Why after DG-3.e**: see DG-3.e's "Why before DG-4" note. The spin
loop cannot be removed until the stress test confirms the race it
guards is gone.

**Diff size**: ~170 LoC moved, ~25 LoC deleted (spin + comment),
~10 LoC added (file rustdoc + use statements).

**Bug history retired**: closes the audit-trail link between
`project_slothandle_decrementguard_release_race` and the actual
removal of the workaround. The race fix landed in DG-3.c; the
workaround removal lands here.

### DG-4 - Remove `finish_file` spin-then-yield workaround (cleanup)

**Goal**: if SPL-38.e chose the "move verbatim" alternative, delete
the spin loop and the comment block from `drain.rs`. If SPL-38.e
chose the recommended "delete during move" approach, DG-4 closes
as a no-op with a one-line note pointing at the SPL-38.e commit.

**Code edits**: delete the spin loop (lines 729-748 in the
pre-extraction file; equivalent lines in `drain.rs` post-extraction)
and the comment block at lines 720-728. Delete the test scenario
`finish_file_calls_flush_workers_internally` (T8) because the race
it covers no longer exists.

**Why last**: the spin loop is the user-visible artifact of the
DG-1 race. Removing it last gives every prior step a chance to
demonstrate that the race is closed in production conditions before
the safety net comes out.

**Diff size**: ~25 LoC removed (spin + comment) + ~40 LoC removed
(deleted test scenario T8). Net: ~65 LoC removed, zero added.

**Bug history retired**: closes
`project_slothandle_decrementguard_release_race` for good. The
project memory note can be moved to the "Completed Initiatives"
section.

### DG-2.c - Decide atomic swap vs phased (decision step)

**Goal**: pick between this document's seven-step phased recipe and
DG-2.a s.7's atomic single-PR recommendation. The decision depends
on DG-3.b's risk assessment: if DG-3.b's transitional
`SlotBarrier` adapter introduces any review friction or hidden
race, the atomic recommendation wins on simplicity. If the adapter
is mechanical and the steps land cleanly, the phased recipe wins on
bisectability.

**Deliverable**: a short decision document
(`docs/design/dg-2c-decision.md`, ~80 lines) that picks one of:

- "Adopt the atomic recommendation from DG-2.a s.7. The phased
  recipe specified here remains a fallback if the atomic PR proves
  too large for one review pass."
- "Adopt the phased recipe from DG-2.b. The atomic recommendation
  from DG-2.a s.7 is retained as a target shape for the post-DG-4
  state."

**Why this is the last step**: the decision needs concrete review
feedback on DG-3.b's adapter to be useful. Making it earlier would
mean predicting the shape of code that does not yet exist.

**Diff size**: new markdown document, ~80 lines. Zero code.

## 4. Risk classification per step

| Step | Bisectable | Reversible | User-observable | Risk |
|------|:----------:|:----------:|:---------------:|------|
| DG-3.a | yes | yes (pure delete) | no | low - dead code only |
| DG-3.b | yes | hard (downstream rebases) | no | medium - DashMap value swap; spin still in place so race window is unchanged from master |
| DG-3.c | yes | hard (race-fix step; revert reopens race) | no | medium - structural ownership change; covered by DG-3.e |
| DG-3.d | yes (audit) | yes (delete doc) | no | none - audit only |
| DG-3.e | yes (test-only) | yes (delete test) | no | none - validation only |
| SPL-38.e | yes | yes (move-back) | no | low - mechanical move; spin deletion is the only behavioural delta |
| DG-4 | yes | yes (re-add spin) | no | low - cleanup; depends on DG-3.e |
| DG-2.c | n/a (audit) | n/a | no | none |

"Hard to reverse" means a revert produces a compile, but subsequent
steps that landed on top must be reverted in the same operation.
DG-3.c is the structural inflection point; reverting DG-3.c without
also reverting DG-3.b leaves the codebase in an inconsistent state
(DashMap value is `SlotEntry`, but handles consume an adapter Arc
that no longer exists). The phased recipe accepts this in exchange
for bisectability of forward regressions.

User-observable impact is "no" at every step: the public
`ParallelDeltaApplier` API, the `ParallelApplyError` variants, and
the wire protocol are unchanged. The transfer crate's
`chunk_builder` consumer continues to compile against an unchanged
public surface.

## 5. Test-coverage checklist per step

Each gate below must be green before the step's PR is merged. CI
runs the full nextest suite on every PR; the per-step checklist
below highlights the targeted tests that bound each step's risk.

| Step | Targeted tests | Full suite |
|------|----------------|------------|
| DG-3.a | `cargo nextest run -p engine --all-features` (compile + dead-code clippy stays green) | required |
| DG-3.b | `nextest -p engine -E 'test(parallel_apply::tests::)'` - every `flush_workers_*`, `drain_inflight_*`, `finish_file_*` test in `parallel_apply.rs:903-1579`. Special focus on `flush_workers_survives_spurious_wakeup` (touches new `entry.barrier` path). | required |
| DG-3.c | All targeted tests above, plus `nextest -p engine -E 'test(parallel_apply_concurrent::)'` for the SSC-1 stress harness. | required |
| DG-3.d | None (audit). | n/a |
| DG-3.e | New tests added in this step: `finish_file_under_concurrent_dispatch_no_spin`, the Windows-targeted variant. Plus the existing SSC-1 stress test. | required |
| SPL-38.e | All `parallel_apply::tests::*` tests; all `parallel_apply_concurrent::*` tests; the `arc_drain_panic_recovery` integration test at `crates/engine/tests/arc_drain_panic_recovery.rs`. | required |
| DG-4 | All `parallel_apply::tests::*`. The `finish_file_calls_flush_workers_internally` test goes away in this step. | required |

Cross-step contract: at no point in the phased sequence may the
following tests be skipped or weakened to allow a step to land:

- `concurrent_register_and_dispatch_on_overlapping_files` (the
  SSC-1 flake's regression guard).
- `flush_workers_blocks_until_worker_drops_arc` and
  `drain_inflight_drains_all_files` (the FFB-2 barrier contract).
- `arc_drain_panic_recovery` (the panic-during-drop invariant).

If any of those tests turn red at any step, halt the sequence,
revert to the last green step, and re-plan.

## 6. Rollback plan per step

| Step | Clean revert | Side effects |
|------|--------------|--------------|
| DG-3.a | `git revert <sha>` | none; new types were `dead_code`-gated. |
| DG-3.b | `git revert <sha>` | restores DashMap value to `Arc<SlotBarrier>` and `finish_file` to its master shape. The new types added in DG-3.a remain dead-code-gated. **If DG-3.c has already landed**, DG-3.c must be reverted first or the project won't compile. |
| DG-3.c | `git revert <sha>` | restores the transitional `SlotBarrier` adapter, the original `DecrementGuard`, and the original `SlotHandle`. Reopens the DG-1 race window the step closed. The spin loop in `finish_file` is still in place so production behaviour reverts to the master baseline. |
| DG-3.d | `git rm docs/audits/dg-3d-finish-file-invariant-check.md` | none. |
| DG-3.e | `git revert <sha>` | removes the new stress test and its observable counter. The SSC-1 stress test is unchanged. |
| SPL-38.e | `git revert <sha>` | re-inlines the three drain methods into `parallel_apply.rs`. If the spin loop was deleted during the move, the revert restores it from the SPL-38.e diff. |
| DG-4 | `git revert <sha>` | restores the spin loop and the T8 test scenario. No production impact - the spin was structurally redundant from DG-3.c onward. |
| DG-2.c | `git rm docs/design/dg-2c-decision.md` | none. |

**Compound rollbacks**: reverting any step after DG-3.b requires
reverting in reverse order (e.g. revert DG-4 then SPL-38.e then
DG-3.e then DG-3.c then DG-3.b). Each individual revert is a clean
`git revert`; the compound is mechanical, not a manual rebase.

**Worst-case scenario**: if DG-3.e surfaces a new race that
DG-3.c's restructure did not anticipate, revert DG-3.c. The DG-3.b
adapter shape remains in place and the spin loop continues to
mitigate the original DG-1 race in production. The phased recipe's
forward path then becomes: diagnose the new race, file DG-3.f,
land it, retry DG-3.c on top.

## 7. Closing notes

- The phased recipe trades one structural property (atomic
  cutover, DG-2.a s.7's recommendation) for two operational
  properties (per-step bisectability, per-step rollback). On a
  single-author codebase the trade-off is usually not worth it; on
  a multi-author codebase with concurrent feature work the
  bisectability per-step is structurally valuable.
- DG-3.b's transitional `SlotBarrier` adapter is the only
  meaningful design risk in the phased recipe. Reviewers should
  read DG-3.b's adapter implementation against the DG-1 audit
  carefully: the adapter must not accidentally introduce a third
  Arc allocation that closes the race window earlier than DG-3.c
  intends. DG-2.c is the gate where that risk gets weighed against
  the bisectability gain.
- SPL-38.e and DG-4 land independently of DG-2.c's decision. The
  atomic alternative from DG-2.a s.7 deletes the spin loop in the
  same PR as the type split; the phased alternative defers the
  deletion to DG-4. Either way, the final state is the same.
- DG-5 (1000-thread concurrent `finish_file` stress test) is
  out-of-scope for this document. It is the regression guard for
  Option B's correctness and runs after DG-4 closes; DG-3.e is the
  intermediate check that bounds the phased recipe's risk.
