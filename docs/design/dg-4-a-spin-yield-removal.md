# DG-4.a - Spec: remove the `finish_file` spin-then-yield workaround

Status: Spec (DG-4.a) - precedes the DG-4 removal PR.
Owner: DG series.
Related tasks: DG-1 (#2940 / PR #4744), DG-2.a-.c (#2941 / PRs #4748, #4769, #4782),
DG-3.a-.e (#2942 / PRs #4826, #4841, #4845, #4855, #4874), DG-4 / DG-4.a (#2944),
DG-4.b (#2945), DG-5.a/.b (#2946 / #2947).

## 1. Scope

DG-4.a specifies the surgical removal of the spin-then-yield workaround
introduced by PR #4665 (`fix(engine): spin-then-yield around finish_file's
try_unwrap (FFB-2)`) as a stop-gap until the DG-3 `BarrierState` /
`SlotData` split shipped. With DG-3.a-.e now in master, that workaround
is redundant and should be deleted.

This document captures:

- The exact lines and doc-comment to delete.
- The post-DG-3 invariant that makes the deletion safe.
- The mechanical removal procedure for the follow-up code PR.
- The regression-test gating that must remain green to validate the
  removal.
- The rollback procedure if the removal regresses.

The actual deletion is the follow-up DG-4 code PR; the memory-note
update is DG-4.b's domain.

## 2. Current state

The workaround lives in
`crates/engine/src/concurrent_delta/parallel_apply/drain.rs`, inside
`ParallelDeltaApplier::finish_file`, at lines 74-103 of the post-SPL-38.e
file:

```rust
        // Post-barrier release-race window: `flush_workers` waits for
        // `inflight==0` via the Condvar, which fires from
        // `DecrementGuard::drop` *before* the guard's own adapter Arc
        // has been released (the notify happens inside the drop body;
        // the inner Arcs only drop after the body returns). The window
        // is typically nanoseconds but is reliably observable on Windows
        // under load. Spin-then-yield until the worker's drop completes;
        // the worker is past the notify and its drop fn is just about
        // to return so the wait is bounded. DG-3.c will retire the
        // adapter and let DG-4 delete the spin entirely.
        let mut spin = 0u32;
        while Arc::strong_count(&data) > 1 {
            spin = spin.saturating_add(1);
            if spin >= 1_000 {
                // Past the typical drop window - surface the typed error
                // so a real bug (e.g. caller raced a new `slot_for`
                // against `finish_file`) does not hide forever.
                return Err(ParallelApplyError::ApplierStillReferenced {
                    ndx,
                    strong_count: Arc::strong_count(&data),
                    kind: "finish_file",
                }
                .into());
            }
            if spin < 32 {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
```

Both the explanatory doc-comment (lines 74-83) and the spin loop (lines
84-103) are part of the workaround. The DG-4 PR removes both. The
subsequent `Arc::try_unwrap(data)` call at line 104 stays - it is the
real fence the spin was guarding.

## 3. Why removal is safe now

DG-3 closed the release race that motivated the spin:

- **DG-3.a** (PR #4826) added the post-split `BarrierState` and
  `SlotData` types per the DG-2.a Option-B spec.
- **DG-3.b** (PR #4841) migrated the `ParallelDeltaApplier` `DashMap`
  value type from `Arc<SlotBarrier>` to `SlotEntry` (which carries one
  `Arc<SlotData>` and one `Arc<BarrierState>` on independent
  allocations).
- **DG-3.c** (PR #4845) retyped `DecrementGuard.barrier` from
  `Arc<SlotBarrier>` to `Arc<BarrierState>` directly. The guard no
  longer holds any strong reference to the payload allocation that
  `finish_file` unwraps.
- **DG-3.d** (PR #4855) verified `finish_file`'s `Arc::try_unwrap` is
  uncontended after the DG-3.c retype.
- **DG-3.e** (PR #4874) added the 1000-thread x 10K-iter stress
  (`concurrent_register_and_dispatch_stress_1000_threads_10k_iter` in
  `crates/engine/tests/parallel_apply_dg3_stress.rs`) and confirmed the
  race window is closed.

The pre-DG-3 race was: `DecrementGuard::drop` fires `notify_all` from
inside the drop body, but the guard's own `Arc<SlotBarrier>` clone is
only released after the body returns. `flush_workers` wakes on the
notify and returns; `finish_file` proceeds to `Arc::try_unwrap` and
observes `strong_count >= 2` because the worker's drop body has not
fully unwound.

Post-DG-3, the notify-bearing allocation (`BarrierState`) and the
payload allocation (`SlotData`) have disjoint strong-count trajectories.
The worker's `DecrementGuard` only holds an `Arc<BarrierState>`. The
flusher's `Arc::try_unwrap` targets the `Arc<SlotData>` half of the
`SlotEntry`, which the worker never touches via the guard. The race
window the spin was masking no longer exists, and the spin is dead code.

## 4. Removal procedure

Mechanical steps for the follow-up DG-4 code PR. Each step is an
edit to `crates/engine/src/concurrent_delta/parallel_apply/drain.rs`.

1. **Delete the workaround doc-comment** at lines 74-83 (from
   `// Post-barrier release-race window:` through `// adapter and let
   DG-4 delete the spin entirely.`).
2. **Delete the spin loop** at lines 84-103 (from `let mut spin = 0u32;`
   through the closing brace of the `while` loop).
3. **Leave the subsequent `Arc::try_unwrap(data)` block untouched.**
   Its `map_err` already returns `ApplierStillReferenced` on the only
   remaining failure mode - a caller-induced race (`slot_for` against
   `finish_file`) - which is a real invariant violation, not a transient
   drop window.
4. **Update the doc-comment on the preceding `drop(barrier);` line** if
   it still references the spin: the surrounding narrative at lines
   66-72 explains why the bookkeeping Arc is dropped first, which
   remains valid; no change needed unless a stale phrase like
   "the spin below" survives.
5. **Memory-note update is out of scope for the code PR.** Marking
   `[[project_slothandle_decrementguard_release_race]]` as SHIPPED with
   the DG-4 removal PR reference is DG-4.b (#2945).

The replacement is "nothing": after the workaround is gone,
`finish_file` reads:

```rust
let SlotEntry { data, barrier } = entry;
drop(barrier);
let slot_data = Arc::try_unwrap(data).map_err(|still_shared| {
    ParallelApplyError::ApplierStillReferenced {
        ndx,
        strong_count: Arc::strong_count(&still_shared),
        kind: "finish_file",
    }
})?;
```

No new wait primitive is needed. `flush_workers` already drains the
in-flight counter via `BarrierState::wait_until_idle` (see
`drain.rs:146-158`); post-DG-3 that drain is sufficient to guarantee
the payload Arc is uncontended by the time `Arc::try_unwrap` runs.

## 5. Regression-test gating

The DG-4 removal PR must keep every test below green:

- **`concurrent_register_and_dispatch_stress_1000_threads_10k_iter`**
  in `crates/engine/tests/parallel_apply_dg3_stress.rs` - the DG-3.e
  stress harness. 1000 OS threads x 10K cycles each = 10M
  register/dispatch/finish cycles. This is the primary witness that the
  race window is closed without the spin.
- **`concurrent_register_and_dispatch_on_overlapping_files`** in
  `crates/engine/tests/parallel_apply_concurrent.rs` - the earlier
  cross-NDX register/dispatch/finish race test (fixed via the SSC-1
  cross-tree bundle, PR #4667, that landed alongside the FFB-2 spin
  workaround).
- **All other tests in `crates/engine/tests/parallel_apply_concurrent.rs`**
  - cover shard discipline, in-flight counter semantics, spurious
  wakeups, and writer reclaim.
- **`tests/parallel_threshold_trip.rs`** - PIP-9.c sha256
  byte-identity scenario through the parallel-apply path.
- **The full required CI matrix:** `fmt+clippy`, `nextest (stable)`,
  `Windows (stable)`, `macOS (stable)`, `Linux musl (stable)`. The
  original race was Windows-dominant, so Windows must stay green
  without the spin.

## 6. Acceptance criteria

The DG-4 removal PR is acceptable iff:

- The spin-loop block (drain.rs lines 84-103) is deleted.
- The workaround doc-comment (drain.rs lines 74-83) is deleted.
- No leftover dead code, dead imports, or stale references to "spin",
  "yield", or PR #4665 remain in `drain.rs`.
- `Arc::try_unwrap(data)` is the only fence guarding the payload
  recovery, and its existing `map_err` still maps a contended unwrap to
  `ApplierStillReferenced`.
- All five required CI checks (fmt+clippy, nextest stable, Windows,
  macOS, Linux musl) pass.
- The DG-3.e stress test passes locally in CI as part of the workspace
  nextest run.
- The PR description cites this spec doc
  (`docs/design/dg-4-a-spin-yield-removal.md`) as the rationale.

## 7. Rollback procedure

If the removal regresses (a flake reappears, or a Windows nextest cell
goes red):

1. **Revert the removal PR** as the first action - keep master green.
2. **Capture diagnostics from the failing run:** worker thread names,
   panic backtraces, the `ApplierStillReferenced` `strong_count` value,
   and whether the failure was reproducible on a single platform or
   across the matrix. The DG-3.e stress harness already eprintln's
   per-worker progress, which simplifies localisation.
3. **Triage which invariant broke:**
   - If `Arc::try_unwrap` observes `strong_count >= 2` after a clean
     `flush_workers` return, the DG-3 split has a residual leak (a
     `SlotData` clone is escaping somewhere the audit missed). File a
     DG-series issue and pause DG-4.a until DG-3 is patched.
   - If the failure is on a different invariant (e.g. spurious wakeup,
     poisoned mutex, ordering bug in `decrement_inflight`), the
     removal exposed a latent bug unrelated to the spin. Re-spec DG-4.a
     to scope the removal more narrowly or to land alongside the new
     fix.
4. **Do not re-apply the spin as a permanent fix.** The workaround was
   always a stop-gap; reverting buys investigation time, not a final
   resolution.

## 8. Why this matters

- **Dead code is technical debt.** The spin-then-yield is ~20 lines of
  hot-path synchronisation that no longer guards anything. Removing it
  simplifies `finish_file` to the post-DG-3 invariant: drain the
  in-flight counter, drop the bookkeeping Arc, unwrap the payload Arc.
- **The doc-comment is now misleading.** It explains a race that the
  Option-B split eliminated. Future maintainers reading the comment
  would either (a) believe the race still exists and over-engineer
  around it, or (b) recognise the comment as stale and waste time
  triangulating which is which.
- **Cleanup unblocks the next DG-series simplification.** Removing the
  spin clarifies that `flush_workers` is the only barrier between
  worker drops and `finish_file`, which is the precondition for the
  future `SlotHandle` retype noted in
  `crates/engine/src/concurrent_delta/parallel_apply/slot_barrier.rs`
  (the deferred DG-3.x task).

## 9. Cross-references

- DG-1 audit: `docs/design/decrementguard-audit.md` (PR #4744).
- DG-2.a Option-B spec: `docs/design/dg-2a-option-b-spec.md` (PR #4748).
- DG-2.b migration order: `docs/design/dg-2b-migration-order.md`
  (PR #4769).
- DG-2.c atomic-vs-phased decision:
  `docs/design/dg-2c-atomic-vs-phased-decision.md` (PR #4782).
- DG-3.a BarrierState + SlotData types: PR #4826.
- DG-3.b DashMap value migration to `SlotEntry`: PR #4841.
- DG-3.c DecrementGuard retype to `Arc<BarrierState>`: PR #4845.
- DG-3.d uncontended-try_unwrap test: PR #4855.
- DG-3.e 1000-thread x 10K-iter stress: PR #4874.
- FFB-2 spin-then-yield workaround introduction: PR #4665.
- Memory note: `[[project_slothandle_decrementguard_release_race]]` -
  to be marked SHIPPED in DG-4.b (#2945) once the removal merges.
- Memory note: `[[project_concurrent_dispatch_test_flake]]` - related
  stress-test flake fixed in the SSC-1 cross-tree bundle (PR #4667).
- DG-4.b follow-up: #2945 (memory-note update).
- DG-5.a / DG-5.b follow-ups: #2946 / #2947 (post-DG-3 stress-test
  scaffolding).
