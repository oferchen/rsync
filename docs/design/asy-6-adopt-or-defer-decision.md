# ASY-6: Adopt / defer / close decision for the async transfer pipeline

Status: Decision. Closes the ASY-1..3 design phase by binding what
happens next. ASY-4 (benchmark) and ASY-5 (embeddability test) are
still pending. Inputs:

- `docs/audits/asy-1-threading-model.md` - 12 boundaries, 8 invariants.
- `docs/design/asy-2-tokio-runtime-feature.md` - `tokio-transfer`
  cargo feature, default off.
- `docs/design/asy-3-async-boundary-spec.md` - per-boundary
  disposition: 6 `.await`, 4 `spawn_blocking`, 1 dissolves, 1
  unchanged.

This document picks one of three options for the next phase, names the
trigger criteria and rollback exit for each, and locks the exit
criteria for the chosen path.

## 1. Options

### Option A: Adopt

Open ASY-7..12 implementation tickets now. Land the `tokio-transfer`
feature behind its existing default-off gate, ship per-boundary
conversions in the order ASY-3 specified, and run the ASY-12
flip-to-on gate when the test contract is green.

- **Trigger criteria.** Would require: (a) ASY-4 bench showing >= 5%
  end-to-end uplift on the `rsync-profile` 100k-file benchmark
  (ASY-2 section 10 #5 floor) or a hard correctness need (e.g.
  embedders blocked on `spawn_blocking` per
  `project_no_async_threaded_only.md`); (b) ASY-5 embeddability test
  demonstrating `core::session()` works under an external tokio
  runtime without `spawn_blocking` wrappers; (c) no contradicting
  signal from `project_io_uring_shared_ring_bottleneck.md` (IUR-3 not
  blocking #9 / #10's shape).
- **Cost estimate.** 6 implementation tickets (ASY-7..12). PR count:
  10..14 PRs (one per boundary cluster: 1+2+4+5 wire-await, 6+7 mpsc
  swap, 3+8 basis-load blocking, 9+10 disk task, 12 SSH dissolve,
  plus runtime-ownership, error-helper, cancellation-token,
  logging-instrument, golden-parity, interop-parity, ASY-12 gate
  flip). Estimate driven by ASY-3 section 5's three cross-cutting
  concerns each becoming their own PR.
- **Risk profile.** High. ASY-3 lists two invariants explicitly
  flagged "at risk -> defended" (shutdown join and flush-before-block,
  rows 5 and 7 of section 3). Wire-byte parity (ASY-2 section 7) is
  non-negotiable and unproven under the tokio path. Tokio CVE
  exposure expands (ASY-2 section 9). Cross-platform IOCP interaction
  is unmeasured. Committing N PRs of churn without ASY-4 numbers
  risks paying the cost for an unknown win and discovering at
  ASY-12 that the bench floor was missed.

### Option B: Defer

Keep the ASY-1..3 design tree as-is. Block ASY-7..12 until ASY-4
benchmark data lands and ASY-5 embeddability test answers whether
external-runtime embedding is achievable through the existing
boundary set. Re-evaluate adopt-vs-close once both arrive.

- **Trigger criteria.** Default state. Triggered automatically by
  ASY-4 or ASY-5 not yet existing. Exit triggers documented in
  section 3.
- **Cost estimate.** 2 follow-up tickets to land: ASY-4 (bench
  harness + run) and ASY-5 (embeddability test). PR count: 2..3 PRs
  total during the defer window. Design docs stay published and
  cross-referenced; no `.rs` churn, no feature-flag plumbing, no CI
  matrix growth.
- **Risk profile.** Low. The threaded model keeps shipping; nothing
  regresses. The cost of being wrong is two PRs of bench / test
  scaffolding, both of which are useful regardless of which option
  we pick afterwards (an Option C close still wants the bench number
  on file to justify closure). ASY-1..3 do not bit-rot because they
  document the current code as a target shape, not as a partial
  conversion; the standing
  `project_no_async_threaded_only.md` constraint already tracks the
  embeddability gap.

### Option C: Close

Mark the threaded pipeline permanent. Convert ASY-1..3 to historical
audit / rejected-design status. Close ASY-4..12 as won't-do. Strip
the `async_pipeline` skeleton (`crates/transfer/src/pipeline/async_pipeline.rs`,
`#[cfg(feature = "async")]`) so the feature graph stops carrying a
dead scaffold. Document the close in `project_no_async_threaded_only.md`
as accepted permanent state.

- **Trigger criteria.** Would require: (a) ASY-4 bench showing < 5%
  end-to-end uplift floor missed AND no degenerate workload above
  10% uplift; (b) ASY-5 confirming the existing `spawn_blocking`
  embedding shim is sufficient for known consumers; (c) explicit
  acceptance that the embeddability constraint stays open
  indefinitely.
- **Cost estimate.** 3..4 PRs to land the close: ASY-1..3 status
  flips, `async_pipeline.rs` removal, feature-flag cleanup in
  `transfer`/`core`/`daemon`, README / AGENTS.md note. Permanent
  cost saved: no tokio-transfer CI matrix, no dual-pipeline test
  contract, no rollback machinery.
- **Risk profile.** Medium. Closing without ASY-4/5 evidence forfeits
  the option to revisit cheaply; reopening would require rebuilding
  the design tree from scratch. Also forfeits embedding use cases
  that the standing `project_no_async_threaded_only.md` flags as
  open. Premature close is harder to reverse than premature adopt
  because the design context disappears from active rotation.

## 2. Decision

**Option B: defer.**

Three design docs without bench data is design-only territory. ASY-2
section 10 #5 already names a 5% bench floor as an unsigned
prerequisite for spending ASY-3+ implementation effort; ASY-4 is the
only thing that can produce that number. Flipping to Option A without
ASY-4 commits 10..14 PRs of churn against two "at risk" invariants
for an unknown win. Flipping to Option C without ASY-4 forfeits the
ability to justify the close and orphans the embeddability question
that `project_no_async_threaded_only.md` keeps open.

The defer path costs 2 PRs (ASY-4 bench, ASY-5 embeddability test).
Both products are useful regardless of which option wins next:
Option A needs ASY-4 to prove the floor, Option C needs ASY-4 to
prove the absence of the floor, and ASY-5 settles the embeddability
question that is independent of the bench result. There is no
scenario in which ASY-4 or ASY-5 is wasted work.

Defer is also the only option compatible with the standing rule that
the threaded pipeline stays the default until ASY-12 gates a flip.
Adopt would consume implementation budget while the ASY-12 gate is
provably unreachable (golden + interop parity requires the
boundary conversions to exist first; ASY-4 is a strictly cheaper
prerequisite). Close would shortcut the gate by removing the option,
which is a directional decision that needs evidence the design phase
has not produced.

## 3. Exit criteria

The defer window exits when **both** of the following land:

1. **ASY-4 benchmark** publishes a `docs/audits/asy-4-*.md` with at
   least: end-to-end transfer wall-time and peak RSS on the
   `rsync-profile` 100k-file benchmark, threaded vs a prototype
   tokio path (need not be production-quality; a single-boundary
   spike on #9 or #6/#7 suffices to characterise overhead). The
   `tokio-profile` container or an equivalent harness is in scope.
2. **ASY-5 embeddability test** publishes a `docs/design/asy-5-*.md`
   or `docs/audits/asy-5-*.md` answering whether `core::session()`
   can be embedded inside an external tokio runtime today (via the
   existing `spawn_blocking` boundary), and whether the answer
   changes under the ASY-2 `tokio-transfer` feature.

On exit, re-evaluate against the trigger criteria in section 1:

- ASY-4 >= 5% uplift on `rsync-profile` 100k-file benchmark
  **AND** ASY-5 shows embeddability requires the feature ->
  re-evaluate Option A. Open ASY-7..12 in the order ASY-3 specifies.
- ASY-4 < 5% uplift **AND** ASY-5 shows embeddability is satisfied
  by the existing `spawn_blocking` shim -> re-evaluate Option C.
  Close ASY-7..12; strip scaffold.
- Mixed result (uplift below floor but embedding gap real, or
  uplift above floor but embedding gap closed by other means) ->
  re-open this document and pick again with the new evidence on
  file. Do not blend criteria.

Until exit, `tokio-transfer` is not implemented, ASY-7..12 are not
opened, and the `async_pipeline` skeleton stays under
`#[cfg(feature = "async")]` exactly as ASY-2 left it. ASY-1..3
status stays "Design"; this document tracks the gate. No bit-rot
checks needed because the docs describe the existing code as the
baseline.

## 4. Out of scope for this decision

- **Native `tokio-uring` driver for boundary #10.** Already punted to
  ASY-9 per ASY-2 section 8; the defer decision does not change that
  punt's status.
- **Parallel-receive-delta interop gap** (`project_parallel_interop_parity_gap.md`).
  Independent of the async pipeline decision; tracked separately.
- **`async-daemon` and `async-ssh` features.** Already shipped,
  default off, and orthogonal to `tokio-transfer`. The defer
  decision does not block further work on either.
- **Removing `#[cfg(feature = "async")]` scaffold.** Only happens
  under Option C; defer leaves it in place.

## 5. Cross-references

- `docs/audits/asy-1-threading-model.md` - boundary inventory.
- `docs/design/asy-2-tokio-runtime-feature.md` - feature flag and
  open questions list (section 10 #5 is the floor this gate
  references).
- `docs/design/asy-3-async-boundary-spec.md` - per-boundary
  contract; `tally` reproduced in section 1 of this doc.
- `project_no_async_threaded_only.md` - standing constraint the
  defer window inherits.
- `project_io_uring_shared_ring_bottleneck.md` - ASY-9 prerequisite
  cited under Option A trigger criteria.
- `project_parallel_interop_parity_gap.md` - explicitly out of scope.
- `docs/design/capture-replay-harness.md` - ASY-5's test harness
  precedent.
