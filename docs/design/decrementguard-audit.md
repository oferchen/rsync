# DecrementGuard / SlotBarrier release-race audit (DG-1)

This is the DG-1 catalogue feeding the DG-2 design and the DG-3
implementation. The surface lives entirely inside
`crates/engine/src/concurrent_delta/parallel_apply.rs` (1579 lines);
no other engine or downstream crate constructs, clones, or unwraps an
`Arc<SlotBarrier>`. Confirmed by:

```
grep -rn "SlotBarrier\|DecrementGuard\|SlotHandle" crates/
```

Only one non-source hit exists outside the file: a benchdoc comment in
`crates/engine/benches/br_3j_f_dashmap_cores_vs_throughput.rs:40` that
merely names the type. That keeps the blast radius of any DG-3
refactor confined to one file plus its tests.

## 1. `Arc<SlotBarrier>` clone sites

Every site that produces a fresh strong reference to the per-slot
barrier. The unit of "in-flight" is the strong count itself - every
clone outstanding is bookkeeping debt against
`SlotBarrier::inflight`.

| # | Site | Holder | Lifetime | How released |
|---|------|--------|----------|--------------|
| C1 | `parallel_apply.rs:573` `Arc::new(SlotBarrier::new(...))` | `DashMap<FileNdx, Arc<SlotBarrier>>` entry created in `register_file` | From `register_file` return through the matching `finish_file` (or process shutdown). | `DashMap::remove` in `finish_file:716` drops the map's strong reference; the owned `Arc` then flows into the spin-wait block. |
| C2 | `parallel_apply.rs:801` `Arc::clone(guard.value())` in `flush_workers` | Local `barrier` binding for `wait_until_idle`. | One stack frame in `flush_workers`. | Function-scope drop after `wait_until_idle` returns. Does **not** participate in the in-flight counter. |
| C3 | `parallel_apply.rs:845` `Arc::clone(guard.value())` in `slot_for` | Local `barrier` passed straight into `SlotHandle::new`. | Folded into `SlotHandle.barrier` and `SlotHandle._decrement.barrier`. | Released when the returned `SlotHandle` drops. |
| C4 | `parallel_apply.rs:395` `Arc::clone(&barrier)` inside `SlotHandle::new` | `DecrementGuard.barrier` field. | Same scope as the `SlotHandle` that owns the `DecrementGuard`. | Released **after** `DecrementGuard::drop` finishes; this is the second clone in the race. |
| C5 | `parallel_apply.rs:398` move of `barrier` into `SlotHandle { barrier, .. }` | `SlotHandle.barrier` field. | Same as C4. | Released by `SlotHandle` field-drop order (field 1, before `_decrement`). |
| C6 | `parallel_apply.rs:1324` (test) `Arc::clone(guard.value())` in `flush_workers_survives_spurious_wakeup` | Local `barrier` for cross-thread notify. | Test scope. | Joined notifier thread plus end-of-test drop. |
| C7 | `parallel_apply.rs:1327` (test) `Arc::clone(&barrier)` for notifier thread. | `notifier_barrier`. | Notifier thread scope. | Released on notifier `JoinHandle` completion. |

Net production reference count per file at steady state with one
worker holding a `SlotHandle`:

- DashMap shard (C1): 1
- `SlotHandle.barrier` (C5): 1
- `DecrementGuard.barrier` (C4): 1

`Arc::strong_count` = 3. After `finish_file` removes from the map and
the worker drops `SlotHandle.barrier`, the count drops to 1 only once
the worker's `DecrementGuard` is also fully released.

## 2. `DecrementGuard` construction sites

There is exactly one production constructor: the literal struct
expression at `parallel_apply.rs:394` inside `SlotHandle::new`. No
test, no bench, no other module instantiates `DecrementGuard`
directly.

| # | Site | Constructed with | Tied to |
|---|------|------------------|---------|
| D1 | `parallel_apply.rs:394-396` `DecrementGuard { barrier: Arc::clone(&barrier) }` | A fresh `Arc::clone` of the caller-supplied barrier (clone site C4). | `SlotHandle._decrement` field. |

`SlotHandle` is itself only constructed at one production site:

| # | Site | Constructed by | Caller |
|---|------|----------------|--------|
| H1 | `parallel_apply.rs:847` `SlotHandle::new(barrier)` | `ParallelDeltaApplier::slot_for` (private helper). | `apply_one_chunk:625`, `apply_batch_parallel:671`, `bytes_written:686`. |

Test code also drives `slot_for` directly to exercise the barrier
without going through `apply_*` (e.g.
`flush_workers_blocks_until_worker_drops_arc:1182`,
`drain_inflight_drains_all_files:1228`,
`finish_file_calls_flush_workers_internally:1279`,
`flush_workers_survives_spurious_wakeup:1317`). These are the only
paths through which a `DecrementGuard` exists in any test fixture.

## 3. Notify / Condvar ordering trace

`SlotBarrier::notify` (`parallel_apply.rs:295`) is the single
`Condvar` in the design. It has exactly one production fire site:

- `parallel_apply.rs:340` `self.notify.notify_all()` inside
  `SlotBarrier::decrement_inflight`.

Test code at `parallel_apply.rs:1331` also fires `notify_all` to
exercise spurious-wakeup tolerance, but the production fire is
exclusively inside `decrement_inflight`.

`decrement_inflight` is only invoked from `DecrementGuard::drop`
(`parallel_apply.rs:367-371`). At the instant `notify_all` returns,
the call frame is still inside `DecrementGuard::drop`, which means:

1. `self: &mut DecrementGuard` is still live.
2. `self.barrier: Arc<SlotBarrier>` (clone C4) has not yet been
   dropped - drop-glue for the field runs **after** `drop` returns,
   per the standard drop order rules.
3. The `SlotHandle.barrier` field (clone C5) was already released
   because field-drop order in the struct definition is `barrier`
   first, `_decrement` second.

So when the waiter wakes, the live strong count is:

- DashMap shard (C1): about to be removed by `finish_file`.
- `SlotHandle.barrier` (C5): already dropped.
- `DecrementGuard.barrier` (C4): still alive until end of drop body.

Once `finish_file` calls `DashMap::remove`, C1 transfers ownership to
the local binding inside `finish_file`. The barrier's true strong
count is then 2 (the local owned `Arc` plus the still-live C4 clone)
until the worker's drop-glue finishes.

## 4. Race window in plain prose

`finish_file` (`parallel_apply.rs:703-772`) calls
`flush_workers(ndx)` (line 712) which parks on `notify` until
`inflight == 0`. The worker's `DecrementGuard::drop` sets the counter
to zero and fires `notify_all` **from inside its own drop body**. The
Condvar wake-up is observable as soon as the predicate flips - which
is several instructions before the drop body finishes executing the
implicit field drop on `self.barrier`.

The wake-up therefore races against the worker's implicit field-drop
glue. On Linux the scheduler typically retires the few extra
instructions in the same quantum and the wake-up appears atomic with
the Arc release. On Windows the kernel scheduler routinely preempts
the worker between `notify_all` and the end of `DecrementGuard::drop`,
which leaves the `Arc<SlotBarrier>` clone (C4) alive while the
flusher returns and `finish_file` proceeds to call
`Arc::try_unwrap` at line 749. With C4 still alive the strong count
is 2 and `try_unwrap` would surface
`ApplierStillReferenced { strong_count: 2 }`.

The hazard is purely an ownership-encoding artefact, not a logic
error. The decrement counter is the source of truth the API
advertises (`flush_workers` "waits for in-flight handles to release"),
but the *implementation* encodes "released" with two coupled signals:
the `inflight` counter and the strong reference count. The Condvar
fires on signal 1; `try_unwrap` blocks on signal 2; the worker can be
mid-flight between them.

## 5. Spin-then-yield workaround

`parallel_apply.rs:720-748` documents the race and applies the
patch:

- Comment block: lines 720-728 explain the wake-before-drop window
  and tie the spin to the worker's drop body completing.
- Loop: lines 729-748 - first 32 iterations call
  `std::hint::spin_loop()`; the remaining iterations call
  `std::thread::yield_now()`; bounded at 1000 iterations.
- Failure mode: line 736 surfaces
  `ParallelApplyError::ApplierStillReferenced` so a real bug (caller
  raced a fresh `slot_for` against `finish_file`) does not hide.

Origin commit: `3e5d83d95dc6` -
`fix(engine): spin-then-yield around finish_file's try_unwrap (FFB-2)`.
This commit body documents the same drop-order analysis as section 4
of this audit and explicitly defers the structural fix - quoted from
the commit message: "Root-cause-clean fix would refactor
`DecrementGuard` to not hold an `Arc<SlotBarrier>` (use Weak, or a
separate `Arc<(Mutex,Condvar)>` struct), but that is a bigger
surgery; this minimal patch closes the Windows test regression."

PR #4665 (FFB-2 Windows nextest follow-up) shipped the spin. The
regression that originally surfaced the race was
`finish_file_calls_flush_workers_internally`
(`parallel_apply.rs:1259-1297`) panicking on Windows with
`ApplierStillReferenced { strong_count: 2 }`.

## 6. Platform behaviour audit

The race is fundamentally cross-platform - the C++ memory model
behind Rust's `Arc` and `Condvar` allows any reordering of "notify on
the condvar" vs "drop the field after the drop body returns" on every
platform. Empirically:

- **Windows**: reliably observable under nextest stress. Windows
  schedulers preempt at fine granularity, especially in CI VMs with
  oversubscribed vCPUs. Both #4665 (DecrementGuard) and #4667
  (SSC-1 registration flake in
  `concurrent_register_and_dispatch_on_overlapping_files`) hit
  Windows-only at first observation.
- **macOS**: intermittently observable. The
  `flush_workers_blocks_until_worker_drops_arc` test originally used
  a sleep-based barrier that raced on macOS nightly when the OS did
  not schedule the worker before the timer started (see
  `parallel_apply.rs:1189-1196` for the handshake-based fix). The
  underlying drop-order race itself has not been seen flaking
  `finish_file` on macOS, only the surrounding timing tests.
- **Linux**: not observed in CI. The drop body typically retires in
  the same scheduler quantum as the `notify_all`. The hazard exists
  in principle; production workloads have not exposed it yet.

Cross-references:

- `project_slothandle_decrementguard_release_race` (master memory) -
  primary write-up of the Windows symptom and the spin-fix.
- `project_concurrent_dispatch_test_flake` (master memory) -
  unrelated Windows flake on
  `concurrent_register_and_dispatch_on_overlapping_files`; shares
  the same Windows-scheduler-pre-empts-worker pattern but lives in a
  different test and is fixed via the registrar-atomic handshake
  shipped in PR #4667.

The Windows-first failure mode is the trigger; the platform-agnostic
nature of the bug is the reason DG-2 needs a structural fix rather
than a Windows-only band-aid.

## 7. Restructure options

Pure brainstorm. DG-1 does not pick; DG-2 will. Each option includes
the trade-offs that matter to DG-3 (call-site churn, perf impact,
diagnostic surface, cross-platform behaviour).

### Option A: drop-order rearrangement only

Reorder `SlotHandle` fields so `_decrement` drops first, then move
the `notify_all` out of `decrement_inflight` and into the `barrier`
field's own scope-exit. Hard to express cleanly because Rust drop
order is by declaration only and `notify_all` needs to fire after the
last Arc clone the worker holds is gone.

- Pros: zero API change; trivial diff.
- Cons: does not actually close the race because *any* Arc held by
  the drop body still encodes the same hazard. Notification still
  has to fire before the implicit field drop. Likely a non-fix.

### Option B: dedicated barrier-state primitive

Split `SlotBarrier` into two types: `BarrierState` (the
`(Mutex<usize>, Condvar)` pair only) and `SlotData` (the per-file
`Mutex<FileSlot>` only). `SlotHandle` and `DecrementGuard` both hold
`Arc<BarrierState>`. The DashMap value becomes
`(Arc<SlotData>, Arc<BarrierState>)`. `finish_file` only needs to
`try_unwrap` the `Arc<SlotData>`, which `DecrementGuard` never
touches.

- Pros: race goes away because the unwrap target is no longer
  shared with the drop body's `Arc`. Diagnostic story stays good -
  in-flight counter still exists for `flush_workers`.
- Cons: data layout churn. Each slot stores two `Arc`s instead of
  one; one extra atomic per `register_file` and per `slot_for`.
  Public `ParallelApplyError` variants stay the same.

### Option C: Weak<SlotBarrier> in DecrementGuard

`DecrementGuard.barrier` becomes `Weak<SlotBarrier>`; the drop body
calls `weak.upgrade()` to get a short-lived strong clone, runs the
decrement, drops it. The upgrade fails if the slot has already been
torn down, which can happen if the consumer races a `finish_file`
against an in-flight worker.

- Pros: no extra Arcs on the hot path; smaller diff than Option B.
- Cons: race window is *the same shape* - the upgraded strong Arc
  lives across `notify_all` and only drops at the closing brace of
  the drop body. We have effectively moved the C4 clone to a
  shorter, but still non-zero, window. Spin would still be needed
  unless paired with Option E.

### Option D: replace Condvar with a wait list keyed off Arc drop

Have `flush_workers` register a `oneshot::Sender` (or a parking-lot
`Park`) that the `SlotBarrier`'s `Drop` impl fires. The signal then
fires when the *last* Arc is released, not when the counter hits
zero. This eliminates the two-signal coupling: the only release
signal is the Arc drop itself.

- Pros: cleanest semantics. `flush_workers` literally waits for the
  Arc to be uniquely-held, no separate counter to drift.
- Cons: requires either a custom drop hook on a wrapper around
  `Arc<SlotBarrier>` or moving to `arc-swap`/`triomphe` semantics.
  Bigger diff; loses the explicit in-flight diagnostic for
  observability.

### Option E: ditch Condvar, use blocking on a separate `Arc<AtomicBool>`

Each `SlotHandle` increments a per-slot `AtomicUsize` and decrements
on drop *before* dropping its `Arc<SlotBarrier>` clone. `flush_workers`
spins+parks on the atomic counter. The atomic decrement does **not**
need to happen inside the drop body if we reorganise to use a
`scopeguard`-style RAII that decrements as the very last step.

- Pros: minimal data layout change; same single-Arc story.
- Cons: spin-park machinery is non-trivial; risks reinventing
  Condvar correctness; still needs careful ordering to ensure the
  atomic decrement is the *literal last* observable side-effect.

### Option F: keep spin, formalise its semantics

Promote the 1000-iteration spin to a documented "drop-window
backoff" primitive with metrics (counter of times spin > 32, max
spin observed). No structural change.

- Pros: zero risk.
- Cons: leaves the imperfection in place; perpetuates the
  invariant-by-prayer pattern. Defers DG-3..DG-5 indefinitely.
  Listed for completeness.

The DG-2 designer should weigh B against D. B is the smaller diff
and preserves the diagnostic counter; D is the cleaner semantic
model but requires touching the public observability story.

## 8. Call sites affected per option

Mapped against the audit above so DG-2 can size the blast radius.

| Option | Production sites that change | Test sites that change | Public API impact |
|--------|------------------------------|------------------------|-------------------|
| A drop-order only | None (just reordering inside `SlotHandle`). | None. | None. |
| B split BarrierState/SlotData | `SlotBarrier` definition (`290-296`), `Arc::new(SlotBarrier::new)` (`573`), `DashMap` value type (`457`), `slot_for` (`835-848`), `flush_workers` (`794-805`), `finish_file` Arc-unwrap branch (`716-755`), `apply_one_chunk` (`618-636`), `apply_batch_parallel` (`650-676`), `bytes_written` (`684-689`), `SlotHandle` (`384-409`), `DecrementGuard` (`363-371`). | `flush_workers_survives_spurious_wakeup` (`1321-1325`) reaches into the DashMap to grab a barrier; reshape to grab the new `Arc<BarrierState>`. Spin-loop test in `finish_file:729-748` removed. | None - all internal types stay private. |
| C Weak in DecrementGuard | `DecrementGuard` struct (`363-365`), `DecrementGuard::drop` (`367-371`), `SlotHandle::new` (`392-400`). | None directly; spin still wanted unless paired with E. | None. |
| D wait-list keyed off Arc drop | `SlotBarrier` (define on-drop hook or wrap in a custom Arc-like), `flush_workers` (rewrite to register a oneshot), `drain_inflight` (`825-833`), `finish_file:712`, all spin-loop logic (`729-748`). | Same tests as B plus spurious-wakeup test rewritten to fire the new signal. | Same observable behaviour; internal diagnostic counter goes away unless duplicated. |
| E AtomicUsize replaces Condvar | `SlotBarrier` (`290-356`), `flush_workers` (`794-805`), `DecrementGuard::drop` (`367-371`), `SlotHandle::new` (`392-401`). Spin-loop in `finish_file` removed. | Spurious-wakeup test (`1300-1359`) becomes irrelevant; replace with an atomic-store ordering test. | None. |
| F formalise spin | Spin loop in `finish_file:729-748` plus a new metric struct and getter on `ParallelDeltaApplier`. | New observability test for the metric. | Adds one public getter; no behavioural change. |

External callers - confirmed by grep - are limited to the production
wire-up in `crates/transfer/src/delta_pipeline/chunk_builder.rs`
(uses `apply_one_chunk` plus `with_strategy`) and the chunk-adapter
documentation in `crates/engine/src/concurrent_delta/chunk_adapter.rs`
(no direct calls). None of the listed options change the
`ParallelDeltaApplier` public surface, so DG-3 does not need a
downstream-caller migration step.

## Summary

The race is one ownership-encoding bug in one file, with one
production constructor and a single Condvar fire site. The audit
counts 7 `Arc<SlotBarrier>` clone sites (5 production, 2 test), 1
`DecrementGuard` construction site, and 1 `SlotHandle` construction
site. The spin-then-yield workaround at `parallel_apply.rs:729-748`
is the only thing keeping `finish_file` correct on Windows; PR #4665
landed it as a deliberate stop-gap. DG-2 should pick between
Option B (cleanest structural fix, contained blast radius) and
Option D (cleanest semantics, broader change to the diagnostic
story).
