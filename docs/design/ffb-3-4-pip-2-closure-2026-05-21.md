# FFB-3 / FFB-4 / PIP-2 closure - satisfied by FFB-1 design + PIP-3+5 wire-up

Date: 2026-05-21
Scope: combined closure note for three task chains whose intended
deliverables have already shipped (or merged into the design that the
tasks were meant to produce).
Status: PIP-2 (#2565) completed retroactively; FFB-3 (#2576) deferred N-A;
FFB-4 (#2577) satisfied-by-design.
Predecessors and satisfying PRs:

- PR #4319 - parallel-receive-delta scaffold (introduced the umbrella
  design and the gated default-on plan).
- PR #4659 - FFB-1 design (`docs/design/ffb-1-applier-barrier-api.md`,
  merged) - the barrier-API decision that absorbed FFB-3 and FFB-4 into
  the API surface.
- PR #4665 - FFB-2 implementation of `flush_workers` / `drain_inflight`
  and Option D baked-in barrier (in CI at the time of writing).
- PR #4666 - PIP-3 + PIP-5 receiver wire-up via
  `enable_parallel_receive_delta()` (merged), the production callsite
  whose existence was the point of PIP-2.

No source changes in this PR. No new design surface added; this note
discharges three trackers against artifacts that already exist.

## 1. PIP-2 (#2565): "Design - migrate `token_loop` onto `ParallelDeltaApplier`"

**Status: completed retroactively. No fresh artifact required.**

PIP-2 was filed as the design step between PIP-1 (the audit that mapped
the migration surface, PR #4657) and PIP-3 (the production wire-up).
The deliverable was a design document explaining the migration shape:
which receiver entry points adopt `ParallelDeltaApplier`, how the
feature gate flips, how Path A vs Path B is chosen, and what the
default-on promotion criteria are.

That document already exists as
`docs/design/parallel-receive-delta-default-on.md`. It landed with the
parallel-receive-delta scaffold (PR #4319) as the gated-default plan
and was extended by PIP-3 + PIP-5 (PR #4666) when the Path B heuristic
flipped the default to on. The doc covers:

- which `token_loop` call sites adopt the parallel applier;
- the Path A (per-chunk parallel verify) vs Path B (multi-file
  pipeline) selection;
- the gate (`enable_parallel_receive_delta()`) and the conditions
  under which it returns the parallel pipeline instead of the
  sequential `DeltaWork` path;
- the rollback story if the default-on flip needs to be reverted.

PIP-2 was a pre-design artifact whose contents were satisfied by the
design that PIP-3 then extended. Closing PIP-2 as completed
retroactively (no fresh markdown) avoids a doc-of-a-doc.

Re-open trigger: only if the `enable_parallel_receive_delta()` gate is
replaced by a different migration strategy that needs a fresh design
note.

## 2. FFB-3 (#2576): "Migrate existing `finish_file` callers to use `flush_workers` first"

**Status: deferred N-A.**

FFB-3 was a conditional task. It existed in case FFB-1 chose Option A
(`flush_workers` as the only primitive) or Option B
(`drain_inflight` as a separate primitive). Under those options, every
existing `finish_file` caller would need an explicit `flush_workers`
call placed immediately before `finish_file` so the
`ApplierStillReferenced` typed error stops being reachable on the hot
path.

FFB-1 chose differently. The recommendation section of
`docs/design/ffb-1-applier-barrier-api.md` ("Adopt Option A as the
primitive and Option D as the bundled default") routes every existing
call site through Option D - `finish_file` itself calls
`flush_workers` internally before attempting the `Arc::try_unwrap`. Per
the FFB-2 implementation (PR #4665, in CI), every existing caller of
`finish_file` therefore gets the barrier semantics without any
callsite change.

Concretely, the FFB-3 migration list is empty. There is no callsite to
migrate because the primitive callers continue to call
`finish_file`, and `finish_file` now bundles the barrier. The "if
Option A or B" branch of the FFB roadmap was not taken.

Re-open trigger: if Option D is ever rolled back (e.g. because the
internal barrier is shown to cost more than an explicit
opt-in, or because a future caller needs to flush without finalising
the file), FFB-3 re-activates and migrates the existing call sites to
the explicit pattern.

## 3. FFB-4 (#2577): "Use `flush_workers` in PIP-3 production wire-up"

**Status: satisfied-by-design.**

FFB-4 paired with FFB-3: it was the production wire-up step that would
have inserted `flush_workers` at the new PIP-3 callsite. PIP-3 + PIP-5
(PR #4666, merged) wired `ParallelDeltaPipeline` into the receiver via
`enable_parallel_receive_delta()`. The pipeline's chunk handlers reach
`apply_one_chunk` via `slot_for` -> `SlotHandle` drop ->
`DecrementGuard` (the FFB-2 release-race fix; see
`project_slothandle_decrementguard_release_race.md`). When the
receiver completes a file via `finish_file`, Option D's internal
`flush_workers` fires automatically.

No explicit `flush_workers` call is added at the PIP-3 wire-up site.
There is no place in `enable_parallel_receive_delta()` that needs an
explicit barrier: every path that finalises a file goes through
`finish_file`, which already barriers. The FFB-4 deliverable is
satisfied by the FFB-1 design choice (Option D) plus the FFB-2
implementation (the bundled barrier) plus the PIP-3 wire-up (the
caller that exercises it).

Re-open trigger: same as FFB-3 - if Option D is rolled back, FFB-4
re-activates and adds an explicit `flush_workers` call at the PIP-3
wire-up site before `finish_file`.

## 4. Known followup: `DecrementGuard` release-race spin

The FFB-2 implementation (PR #4665) needed a spin-then-yield workaround
around `finish_file`'s `Arc::try_unwrap` to close a Windows release-race
between the `SlotHandle` drop and the `DecrementGuard` release. The
race is captured in
`project_slothandle_decrementguard_release_race.md`. The current fix is
correct but ergonomic; future work might restructure `DecrementGuard`
so the release is observed synchronously and the spin can be removed.
If filed, that work is FFB-5+ and is out of scope for this closure
note.

## 5. Summary table

| Task | Status | Satisfied by | Re-open trigger |
|------|--------|--------------|-----------------|
| PIP-2 (#2565) | completed retroactively | `docs/design/parallel-receive-delta-default-on.md` (shipped in PR #4319; extended by PR #4666) | new migration strategy replaces `enable_parallel_receive_delta()` |
| FFB-3 (#2576) | deferred N-A | FFB-1 design (PR #4659) chose Option D; FFB-2 (PR #4665) baked barrier into `finish_file` | Option D rolled back |
| FFB-4 (#2577) | satisfied-by-design | PIP-3 + PIP-5 wire-up (PR #4666) routes through `finish_file` which already barriers | Option D rolled back |

## 6. Cross-references

- `docs/design/parallel-receive-delta-application.md` - umbrella design.
- `docs/design/parallel-receive-delta-default-on.md` - default-on
  decision (the PIP-2 deliverable).
- `docs/design/ffb-1-applier-barrier-api.md` - FFB-1 barrier-API
  decision (Option A primitive + Option D bundled default).
- `docs/audits/abw-1-apply-batch-verify-write-overlap-2026-05-21.md` -
  related audit on the parallel-apply path; cited here only as
  precedent for the deferred-by-design pattern this note uses.
