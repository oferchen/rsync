# RJN-3/4 - per-chunk fanout refactor and scheduler-shape bench (deferred N/A)

Date: 2026-05-21
Scope: closure note for the RJN-3 and RJN-4 tracks following the RJN-2 rename
Status: deferred N/A - rename path landed via RJN-2 (PR #4660, merged)
Predecessors:
  - `docs/audits/rjn-1-apply-chunk-parallel-call-sites-2026-05-21.md` (PR #4656, merged)
  - RJN-2 rename refactor (PR #4660, merged)
Sibling closure: `docs/design/abw-2-pipelined-verify-write-deferred-2026-05-21.md`
Trackers: RJN-3 (#2559), RJN-4 (#2560); kept open with the
"deferred N/A: superseded by RJN-2 rename" label

## 1. RJN-1 audit and RJN-2 decision recap

RJN-1 (PR #4656) catalogued every workspace caller of
`ParallelDeltaApplier::apply_chunk_parallel` and `apply_batch_parallel`. Two
findings drove the RJN-2 decision matrix:

1. **Per-chunk dispatch is not cross-chunk parallelism.** The body of
   `apply_chunk_parallel` was `rayon::join(|| Self::verify_chunk(...), || ())`:
   a single-chunk verify scheduled on a rayon worker with a no-op second
   closure. The name advertised fan-out the code did not deliver. Real
   multi-chunk parallelism only lives in `apply_batch_parallel` via
   `chunks.into_par_iter()`
   (`crates/engine/src/concurrent_delta/parallel_apply.rs:515`).
2. **Zero production callers on either entry point.** Every non-definition
   call site sat behind `#[cfg(test)]` or the `parallel-receive-delta`
   feature in integration tests and criterion benches.

RJN-2's decision matrix offered two paths: **rename** for clarity (cosmetic,
zero behaviour change) or **refactor** to a fanout primitive that actually
multi-chunks. RJN-2 chose rename and shipped as PR #4660:
`apply_chunk_parallel -> apply_one_chunk`, all 17 call sites updated, plus a
rustdoc paragraph directing multi-chunk callers to `apply_batch_parallel`.
The fanout-refactor branch (RJN-3) and its companion bench (RJN-4) were left
open for this closure.

## 2. Why RJN-3 (implement fanout) is deferred

RJN-3 was the "if RJN-2 chose refactor" branch in the decision matrix. RJN-2
chose rename. Three reasons that branch stays closed:

1. **RJN-2 was a deliberate scope choice, not a punt.** The audit's section 4
   called rename the surgical fix for the naming bug and explicitly deferred
   the behavioural change. PR #4660 shipped the rename plus a rustdoc redirect
   pointing multi-chunk callers at `apply_batch_parallel`. No orphaned
   per-chunk hot path waits for fanout to land.
2. **Production callers still don't exist for `apply_one_chunk`.** The
   parallel-receive-delta dispatcher wired in PIP-3+5 (PR #4666) batches
   work through `apply_batch_parallel`, not the per-chunk entry point.
   RJN-1's call-site table still holds under the new name: every non-bench,
   non-test caller is zero. A fanout refactor at `apply_one_chunk` would
   ship code reachable only from tests.
3. **The real multi-chunk win lives in `apply_batch_parallel`, and ABW-1
   already deferred it there.** ABW-1 (PR #4670) catalogued the
   verify/write barrier in `apply_batch_parallel` as the scheduler shape
   worth optimising and quantified speedup ceilings of 1.03x to 1.50x by
   verify/write cost ratio. The companion closure
   `docs/design/abw-2-pipelined-verify-write-deferred-2026-05-21.md` defers
   ABW-2/3/4 pending BR-3j.f (#2508) bench evidence. Pursuing RJN-3 in
   parallel would split the refactor budget across two entry points the
   dispatcher does not reach today.

## 3. Why RJN-4 (bench scheduler shape) is N/A

RJN-4 was framed as "measure cores-vs-throughput before/after RJN-3". With
RJN-3 deferred, there is no "after" to measure.

- RJN-1 section 3.3 noted that the BR-3i.f cores-vs-throughput harness uses
  `apply_batch_parallel` exclusively and would over-state any per-chunk
  path's scaling if the receiver pipeline ever shipped against
  `apply_one_chunk`. RJN-4 was the proposed extension adding a per-chunk
  cell to that harness.
- That cell only earns its keep if RJN-3 ships; otherwise the harness
  delta measures a path with no production callers and no
  production-relevant ratio to compare against.
- The scheduler shape that does carry production weight -
  `apply_batch_parallel` - is already covered by
  `crates/engine/benches/parallel_receive_delta_perf.rs`. BR-3j.f (#2508
  pending) extends that bench to emit the verify/write wall-clock ratio
  ABW-2's decision gate consumes. That is where scheduler-shape bench
  effort should land in the current quarter.

RJN-4 therefore moves to "N/A pending re-open of RJN-3", not "deferred
pending bench evidence" - the bench is meaningless without the refactor it
would compare against.

## 4. What would re-open RJN-3 (and revive RJN-4)

Two preconditions, both required:

1. **A production caller of `apply_one_chunk` ships.** Today's parallel path
   batches at the receiver. A future design that dispatches per chunk - for
   example, an io_uring path overlapping a single chunk's verify with a
   kernel write completion, or a streaming applier that cannot buffer a
   batch - would reintroduce the per-chunk entry point on the hot path. Such
   a design needs its own audit first; the streaming-vs-batched trade-off is
   not free.
2. **Profiling shows the per-chunk path is hot.** Even with a production
   caller, the fanout refactor only earns its keep when profiling shows the
   per-chunk verify dominates the per-chunk write or per-chunk dispatch
   overhead is the bottleneck. Without evidence, the refactor adds a rayon
   scope and a join point for no measurable win.

If both land, RJN-3 reopens with a fresh audit and RJN-4 reopens as the bench
cell that proves the refactor. Until then, both stay closed.

## 5. Closure shape for the tracker

- RJN-3 (#2559): closes as "deferred N/A: rename path landed via RJN-2
  (PR #4660); fanout refactor superseded by ABW-2 track at the batch entry
  point". Linked back to this doc, RJN-1, RJN-2, and the ABW-2 closure.
- RJN-4 (#2560): closes as "N/A pending re-open of RJN-3". Linked back to
  this doc and to BR-3j.f (#2508).

Project memory page `project_rayon_join_per_chunk_noop.md` keeps its existing
observation that the per-chunk `rayon::join(verify, || ())` second closure is
a no-op and that real parallelism lives in `apply_batch_parallel` via
`par_iter`. The page gains a reference to this closure doc; the entry stays
because the no-op closure is still in the renamed function and is still
load-bearing context for any future RJN-3 re-open.

## 6. References

- `docs/audits/rjn-1-apply-chunk-parallel-call-sites-2026-05-21.md` -
  call-site catalogue and per-chunk dispatch analysis.
- RJN-2 rename refactor (PR #4660, merged) - `apply_chunk_parallel ->
  apply_one_chunk` plus rustdoc redirect to `apply_batch_parallel`.
- `docs/audits/abw-1-apply-batch-verify-write-overlap-2026-05-21.md`
  (PR #4670, merged) - the batch-level audit whose decision gate this
  closure defers to for the real multi-chunk scheduler shape.
- `docs/design/abw-2-pipelined-verify-write-deferred-2026-05-21.md` -
  sibling closure deferring the batch-level pipelined design pending
  BR-3j.f bench evidence.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:484` -
  `apply_one_chunk`, the renamed per-chunk entry point.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:515` -
  `apply_batch_parallel`, the actual multi-chunk parallelism entry point.
- `crates/engine/benches/parallel_receive_delta_perf.rs` - the bench
  harness BR-3j.f extends; the production-weighted scheduler shape lives
  here, not at the per-chunk entry point.
- PIP-3+5 (PR #4666) - the receiver dispatch heuristic that routes
  production traffic through `apply_batch_parallel`, not
  `apply_one_chunk`.
- BR-3j.f (#2508) - re-bench task; where scheduler-shape bench effort
  should land in the current quarter.
- `project_rayon_join_per_chunk_noop.md` - project memory entry for the
  per-chunk dispatch shape; remains accurate under the renamed function.
- `project_apply_batch_write_serial.md` - project memory entry for the
  batch-level barrier this closure defers to ABW-2.
