# RJN-4 - scheduler-shape bench (N/A: RJN-3 is a rename, no before/after to measure)

Date: 2026-05-21
Scope: per-tracker closure note for RJN-4, the bench-scheduler-shape task
Status: N/A - RJN-2 shipped a pure rename; there is no semantic delta for
RJN-4 to measure
Predecessors:
  - `docs/audits/rjn-1-apply-chunk-parallel-call-sites-2026-05-21.md` (RJN-1, PR #4656, merged)
  - RJN-2 rename refactor (PR #4660, merged): `apply_chunk_parallel -> apply_one_chunk`
Supersedes: the RJN-4 section of
`docs/design/rjn-3-4-fanout-deferred-2026-05-21.md` (PR #4676, merged). That
doc bundled RJN-3 and RJN-4 closures behind one tracker. This note splits
the RJN-4 closure out cleanly so the tracker (#2560) has a per-task closure
artifact.
Sibling closures:
  - `docs/design/rjn-3-4-fanout-deferred-2026-05-21.md` - RJN-3 deferred N/A.
  - `docs/design/abw-2-pipelined-verify-write-deferred-2026-05-21.md` - the
    batch-level scheduler-shape track this closure defers to for the
    production-relevant bench effort.
Tracker: RJN-4 (#2560); closes with the "N/A: superseded by RJN-2 rename"
label.

## 1. Decision

**N/A.** RJN-3 is a pure rename with zero semantic change. There is no
scheduler-shape difference between `apply_chunk_parallel` (pre-RJN-2) and
`apply_one_chunk` (post-RJN-2) for a bench to capture; the function body,
its `rayon::join(verify, || ())` shape, every call site's dispatch
behaviour, and the per-chunk verify cost are byte-identical. A
before/after cores-vs-throughput bench would compare a path to itself.

## 2. Why

RJN-1 (PR #4656) catalogued
`ParallelDeltaApplier::apply_chunk_parallel` and found two facts that
collapse the RJN-4 measurement target: the function body was already
`rayon::join(|| Self::verify_chunk(...), || ())`, a single-chunk verify
scheduled on a rayon worker with a no-op second closure, so there was no
cross-chunk parallelism for any "after" dispatch shape to deliver
differently; and every non-definition caller sat behind `#[cfg(test)]` or
the `parallel-receive-delta` feature, so even a behavioural refactor would
ship code reachable only from tests. RJN-2's decision matrix offered
rename-for-clarity or refactor-to-real-fanout; RJN-2 chose rename and
shipped as PR #4660. RJN-3 (the fanout refactor that would have produced a
scheduler-shape delta) was deferred in this iteration because zero
production callers consume the per-chunk entry point - the receiver
dispatcher wired by PIP-3+5 (PR #4666) batches through
`apply_batch_parallel`, not the per-chunk path. With RJN-3 deferred, there
is no new dispatch shape for RJN-4 to bench against the rename baseline.

## 3. Re-open trigger

RJN-4 re-opens when batched fan-out lands at the receiver. Concretely, the
gate is: the PIP-3 / PIP-5 default-on path (today routed via
`enable_parallel_receive_delta()` in PR #4666) starts consuming a
multi-chunk batch interface that dispatches per-chunk verify+write through
`apply_one_chunk` instead of, or alongside,
`apply_batch_parallel`. At that point there is a real cores-vs-throughput
delta to characterise against the current single-chunk dispatch shape, and
RJN-4 becomes the bench cell that proves the new shape carries its weight.
Two preconditions, both required, mirror the RJN-3 re-open gates from the
sibling closure: a production caller of the per-chunk entry point ships,
and profiling shows the per-chunk verify dominates either the per-chunk
write or the per-chunk dispatch overhead. Without both, the bench measures
a path with no production-relevant ratio to compare against.

## 4. Reference

The production-weighted scheduler-shape bench effort already lives at the
batch entry point, not the per-chunk one:

- `crates/engine/benches/parallel_receive_delta_perf.rs` exercises
  `apply_batch_parallel` across the existing workload cells (`mixed`,
  `large_files`, NVMe and HDD-ish container variants).
- BR-3i.f and BR-3j.f extend that harness to emit per-batch
  `verify_wall` / `write_wall` ratios (the ABW-1 audit's `C` and `W`
  aggregates) and to wire those into the ABW-2 decision gate.

RJN-4 would have been an apples-to-apples re-bench against a different
dispatch shape at the per-chunk entry point. That dispatch shape does not
exist yet. Until it does, the BR-3i.f / BR-3j.f cells at the batch entry
point are the right home for any cores-vs-throughput investment, and RJN-4
stays N/A behind the RJN-3 re-open gate above. The sibling ABW-2/3/4
closure (`docs/design/abw-2-pipelined-verify-write-deferred-2026-05-21.md`)
carries the matching decision gate for the batch-level pipelined design;
RJN-4 and ABW-4 are deliberately separated so that one re-opens with the
per-chunk fanout and the other re-opens with the verify/write overlap.

## 5. Closure shape for the tracker

- RJN-4 (#2560): closes as "N/A: RJN-3 is a rename, no before/after to
  measure". Label `N/A: superseded by RJN-2 rename`. Linked back to this
  doc, to RJN-1 (PR #4656), to RJN-2 (PR #4660), and to the combined
  RJN-3/4 closure (PR #4676) this note splits out.

Project memory page `project_rayon_join_per_chunk_noop.md` keeps its
existing observation that the per-chunk `rayon::join(verify, || ())`
second closure is a no-op and that real parallelism lives in
`apply_batch_parallel` via `par_iter`. The page already references the
combined RJN-3/4 closure; no new entry is needed because the rename-only
nature of RJN-2 is exactly what makes RJN-4 N/A here.

## 6. References

- `docs/audits/rjn-1-apply-chunk-parallel-call-sites-2026-05-21.md` -
  call-site catalogue and per-chunk dispatch analysis.
- RJN-2 rename refactor (PR #4660, merged) - `apply_chunk_parallel ->
  apply_one_chunk` plus rustdoc redirect to `apply_batch_parallel`.
- `docs/design/rjn-3-4-fanout-deferred-2026-05-21.md` (PR #4676, merged) -
  the combined RJN-3 + RJN-4 closure this note splits the RJN-4 half
  out of.
- `docs/design/abw-2-pipelined-verify-write-deferred-2026-05-21.md`
  (PR #4673, merged) - sibling closure for the batch-level
  scheduler-shape track, gated on BR-3j.f (#2508).
- `crates/engine/src/concurrent_delta/parallel_apply.rs:484` -
  `apply_one_chunk`, the renamed per-chunk entry point with the no-op
  second closure that makes a before/after bench moot.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:515` -
  `apply_batch_parallel`, the actual multi-chunk parallelism entry point.
- `crates/engine/benches/parallel_receive_delta_perf.rs` - the bench
  harness BR-3j.f extends; production-weighted scheduler-shape bench
  effort belongs here, not at the per-chunk entry point.
- PIP-3+5 (PR #4666, merged) - receiver dispatch heuristic
  (`enable_parallel_receive_delta()`) routing production traffic through
  `apply_batch_parallel`.
- BR-3j.f (#2508) - re-bench task; gating dependency for the batch-level
  scheduler-shape track.
- `project_rayon_join_per_chunk_noop.md` - project memory entry for the
  per-chunk dispatch shape; remains accurate under the renamed function.
