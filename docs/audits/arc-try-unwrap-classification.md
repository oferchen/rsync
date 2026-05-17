# `Arc::try_unwrap` Call-Site Classification

Tracking issues: [#2378](https://github.com/oferchen/oc-rsync/issues/2378),
[#2379](https://github.com/oferchen/oc-rsync/issues/2379).

Combined audit for ATU-1 (catalogue) and ATU-2 (fragile-vs-robust split).
Downstream remediation tracks: ATU-3 (typed error variants), ATU-4
(channel-based shutdown refactor), ATU-5 (diagnostic logging).

## 1. Baseline

```sh
grep -rn "Arc::try_unwrap" crates/
```

Total raw matches: **11** across 4 source files.

| Crate    | File                                                                | Sites |
|----------|---------------------------------------------------------------------|------:|
| fast_io  | `crates/fast_io/src/iocp/socket.rs`                                 |     7 |
| engine   | `crates/engine/src/delete/context.rs`                               |     2 |
| engine   | `crates/engine/src/concurrent_delta/parallel_apply.rs`              |     1 |
| engine   | `crates/engine/src/concurrent_delta/work_queue/tests.rs`            |     1 |

Production sites (non-test): **3**. Test-only sites: **8**.

## 2. Per-Site Classification

| # | File:Line                                                  | Arc kind                              | Failure behaviour                                                                                                                              | Visibility           | Class    | Remediation track                          |
|---|------------------------------------------------------------|---------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------|----------------------|----------|--------------------------------------------|
| 1 | `fast_io/src/iocp/socket.rs:533`                           | `Arc<CompletionPump>`                 | `.expect("pump must be uniquely owned for shutdown")` - panic in test only                                                                     | test-only            | ROBUST   | none (test scaffold)                       |
| 2 | `fast_io/src/iocp/socket.rs:575`                           | `Arc<CompletionPump>`                 | `.expect("pump must be uniquely owned")` - panic in test only                                                                                  | test-only            | ROBUST   | none (test scaffold)                       |
| 3 | `fast_io/src/iocp/socket.rs:608`                           | `Arc<CompletionPump>`                 | `.expect("pump uniquely owned")` - panic in test only                                                                                          | test-only            | ROBUST   | none (test scaffold)                       |
| 4 | `fast_io/src/iocp/socket.rs:635`                           | `Arc<CompletionPump>`                 | `.expect("pump uniquely owned")` - panic in test only                                                                                          | test-only            | ROBUST   | none (test scaffold)                       |
| 5 | `fast_io/src/iocp/socket.rs:662`                           | `Arc<CompletionPump>`                 | `.expect("pump uniquely owned")` - panic in test only                                                                                          | test-only            | ROBUST   | none (test scaffold)                       |
| 6 | `fast_io/src/iocp/socket.rs:711`                           | `Arc<CompletionPump>`                 | `.ok().unwrap()` - bare panic in `try_transmit_file_path` test                                                                                 | test-only            | FRAGILE* | tighten to `.expect(...)` (ATU-5, low-prio)|
| 7 | `fast_io/src/iocp/socket.rs:731`                           | `Arc<CompletionPump>`                 | `.ok().unwrap()` - bare panic in `completion_key_override` test                                                                                | test-only            | FRAGILE* | tighten to `.expect(...)` (ATU-5, low-prio)|
| 8 | `engine/src/delete/context.rs:354`                         | `Arc<DeletePlanMap>` (`self.plans`)   | `io::Error::new(ErrorKind::Other, "DeleteContext::into_emitter: DeletePlanMap still shared")` - opaque kind, no strong-count, no role trailer  | **user-visible**     | FRAGILE  | typed error variant (ATU-3) + diag (ATU-5) |
| 9 | `engine/src/delete/context.rs:360`                         | `Arc<Mutex<DirTraversalCursor>>` (`self.cursor`) | `io::Error::new(ErrorKind::Other, "DeleteContext::into_emitter: DirTraversalCursor still shared")` then `.into_inner().expect("...poisoned")` - opaque, panic on poison | **user-visible** | FRAGILE  | typed error + channel shutdown (ATU-3/4)   |
| 10| `engine/src/concurrent_delta/parallel_apply.rs:375`        | `Arc<Mutex<FileSlot>>`                | `io::Error::other("parallel applier file slot still in flight")` - opaque kind, no `ndx`, no in-flight count                                   | **user-visible**     | FRAGILE  | typed error w/ ndx + count (ATU-3, ATU-5)  |
| 11| `engine/src/concurrent_delta/work_queue/tests.rs:691`      | `Arc<Mutex<Vec<u64>>>`                | `.expect("Arc should have single owner after join")` - panic in test only                                                                      | test-only            | ROBUST   | none (test scaffold)                       |

\* Test-only fragility: bare `.unwrap()` produces a useless `called Option::unwrap()
on a None value` panic when the underlying assumption breaks. Cheap to fix but
never reached by users.

### Summary

| Class   | Production | Test | Total |
|---------|-----------:|-----:|------:|
| ROBUST  |          0 |    6 |     6 |
| FRAGILE |          3 |    2 |     5 |
| **All** |          3 |    8 |    11 |

All three production sites are FRAGILE. They emit `io::ErrorKind::Other` with
a string-only message, no strong-count, no role trailer, no `ndx` or path
context.

## 3. Top 5 Fragile Sites by User-Visibility Impact

1. **`engine/src/delete/context.rs:354`** -
   `Arc<DeletePlanMap>` unwrap in `DeleteContext::into_emitter`. Driven by
   `local_copy::executor::cleanup::delete_extraneous_entries_via_emitter`
   (line 115, `ctx.emit_one(fs)`), reached on every `--delete` transfer.
   Surfaces as opaque `io::ErrorKind::Other` wrapped in
   `LocalCopyError::io("emit delete plan", ...)`. User sees a generic I/O
   error with no clue an internal `Arc` was still shared (e.g. by an
   orphaned receiver clone).
   Remediation: ATU-3 - introduce
   `DeleteError::PlanMapStillShared { strong_count: usize }`; ATU-5 - log
   the holder identity at debug level.

2. **`engine/src/delete/context.rs:360`** -
   `Arc<Mutex<DirTraversalCursor>>` unwrap in the same hot path. Compounds
   the previous site: even when the `Arc` is unique, the mutex `.into_inner()`
   uses `.expect("DeleteContext cursor mutex poisoned")`, panicking instead
   of returning an error. Reached by every `--delete` drain.
   Remediation: ATU-3 typed error + ATU-4 channel-driven cursor handoff so
   only one owner ever holds the Arc (designed shutdown, no try-unwrap).

3. **`engine/src/concurrent_delta/parallel_apply.rs:375`** -
   `Arc<Mutex<FileSlot>>` unwrap in `ParallelApplier::finish_file`. Hits the
   delta-apply pipeline whenever rayon workers race the finalisation step.
   Failure message omits `ndx`, in-flight strong-count, and the file path,
   so an operator cannot diagnose which file or which worker still holds
   the slot. Tracked alongside `consumer_force_insert_smell` in MEMORY.md.
   Remediation: ATU-3 typed
   `ParallelApplyError::SlotInFlight { ndx, strong_count }`; ATU-5 add
   in-flight-tracker debug log.

4. **`fast_io/src/iocp/socket.rs:711`** - test-only `.ok().unwrap()` in
   `try_transmit_file_path_round_trips_a_file`. Not user-visible but masks
   real pump-leak bugs in CI with an unhelpful `None` panic.
   Remediation: ATU-5 - replace with descriptive `.expect(...)` matching
   sibling tests.

5. **`fast_io/src/iocp/socket.rs:731`** - test-only `.ok().unwrap()` in
   `completion_key_override_round_trips`. Same class as #4.
   Remediation: ATU-5 - descriptive `.expect(...)`.

## 4. Recommended Remediation by Track

| Track | Sites                                       | Action                                                                                                                       |
|-------|---------------------------------------------|------------------------------------------------------------------------------------------------------------------------------|
| ATU-3 | context.rs:354, context.rs:360, parallel_apply.rs:375 | Define `DeleteError::{PlanMapStillShared, CursorStillShared, CursorPoisoned}` and `ParallelApplyError::SlotInFlight { ndx, strong_count }`. Map both to `io::Error::other` only at the public boundary so the inner crate keeps strong typing. |
| ATU-4 | context.rs:360, parallel_apply.rs:375       | Replace `Arc<Mutex<_>>` handoff with single-owner channel transfer: producer sends the owned cursor / slot to the consumer over an `mpsc::SyncSender`, eliminating the try-unwrap dance entirely. Mirrors `ParallelApplier::files` already using a per-file removal step. |
| ATU-5 | context.rs:354/360, parallel_apply.rs:375, socket.rs:711/731 | Add `tracing::debug!` capturing `Arc::strong_count`, `Arc::weak_count`, and identifying ndx/path before the try-unwrap; on failure include same telemetry in the error message. Tighten the two test `.ok().unwrap()` sites to descriptive `.expect(...)`. |

## 5. Patterns and Invariants

- Every production site assumes "no other clones outlive me". None of them
  log the actual `strong_count` on failure, so a bug that leaks a clone
  surfaces as a generic I/O error rather than a diagnosable invariant
  violation.
- Two production sites (`context.rs:360`, `parallel_apply.rs:375`) chain
  try-unwrap with `Mutex::into_inner`. The mutex case is independently
  fragile: poisoned mutexes either panic (`context.rs`) or map to opaque
  `io::Error::other` (`parallel_apply.rs`). Channel-based shutdown
  (ATU-4) removes both halves of the chain.
- The `concurrent_delta` family (parallel_apply, work_queue, consumer,
  reorder) is the primary owner of multi-clone Arc patterns in the engine.
  Cross-reference MEMORY.md entries
  `consumer_force_insert_smell` and `reorder_spill_fragility` - the same
  shutdown-race class drives both.
- No `Arc::try_unwrap` exists in `core`, `transfer`, `cli`, `daemon`,
  `protocol`, `checksums`, `compress`, `filters`, `bandwidth`,
  `signature`, `metadata`, `rsync_io`, `logging`, or `batch`. The audit
  surface is contained to `engine` and `fast_io`.
