# PIP-9.f.1 - Bake criterion for the parallel-receive-delta default-on flip

Tracking: PIP-9.f.1 (#2643). Parent: PIP-9.f (#2598). Series parent:
PIP-9. Follow-ups: PIP-9.f.2 (Cargo.toml flip PR), PIP-9.f.3
(post-flip bake monitor), PIP-9.f.4 (PIP-9.e closure + memory-note
update).

Memory notes: `[[project_parallel_interop_parity_gap]]`,
`[[project_apply_batch_write_serial]]`.

## 1. Scope

PIP-9.f.1 defines what "N consecutive green CI cycles" means before
PIP-9.f.2 may flip the `parallel-receive-delta` cargo feature default
to ON in workspace `Cargo.toml`. The criterion is the rigorous gate
that prevents shipping a regression behind a silent default change.

The flip target is the `parallel-receive-delta` feature entry in the
workspace `[features]` table and any per-crate forwarding entries.
Today the feature is OFF by default; the production receiver
`token_loop` still takes the sequential `DeltaWork` path on the
default build, while the parallel-applier scaffolding compiles and is
exercised under the PIP-9.d CI matrix cell. See memory note
`[[project_parallel_interop_parity_gap]]` for the current parity gap.

Pattern mirrors ISI.i.1 (#2978, shipped) which defined the bake
window for the sender-side INC_RECURSE default flip. Precedent
document: `docs/design/isi-h-bake-window-criteria.md`.

## 2. Pre-conditions for opening the bake window

The criterion clock cannot start counting until all of the following
are true at master HEAD:

- **PIP-9.b complete.** All sub-tasks PIP-9.b.1 through PIP-9.b.6
  shipped. The production receiver `token_loop` must dispatch
  through `ParallelDeltaApplier` when the feature is enabled (not
  merely compile against it).
- **PIP-9.c regression scenario green.** The sha256 byte-identity
  scenario at `tests/parallel_threshold_trip.rs` must pass against
  both the sequential default and the `--features
  parallel-receive-delta` build.
- **PIP-9.d CI matrix cell green.** The `parallel-receive-delta
  (dist profile, non-required)` job defined at
  `.github/workflows/ci.yml:586` must be green at HEAD. The cell
  builds `--profile dist --features parallel-receive-delta` (the
  PIP-4 corruption only surfaced under dist; release masks it) and
  runs the `parallel_threshold` nextest filter.
- **PIP-9.e closed.** The PIP-7 receiver-corruption issue must be
  confirmed fixed against the parallel-applier path. The PIP-7
  repro at
  `crates/transfer/tests/pip_7_parallel_receive_delta_corruption_repro.rs`
  must remain green under `--features parallel-receive-delta`.
- **No open regression issues.** GitHub query
  `is:open label:regression "parallel-receive-delta" OR
  "parallel-delta-apply"` must return zero hits.

If any pre-condition is red, PIP-9.f.2 does not open and the bake
window does not start. The pre-conditions are entry criteria; they
do not count toward the window duration.

## 3. N consecutive green CI cycles - quantitative definition

The criterion is satisfied when **all of the following hold
simultaneously**:

- **N = 5 consecutive nightly Interop Validation runs green** across
  every supported upstream rsync version (3.0.9, 3.1.3, 3.4.1,
  3.4.2). Nightly cadence per `tools/ci/run_interop.sh`.
- **N = 5 consecutive nightly fuzz-coverage runs green** with zero
  new panics introduced from parallel-path-only code.
- **N = 5 consecutive nightly bench runs** against the PIP-9.d CI
  matrix cell where throughput is `>= 0.95x` the sequential baseline
  (no measurable slowdown vs the current default).
- **Zero CI failures attributable to parallel-receive-delta** across
  the same 5-cycle window. Required CI checks (fmt+clippy, nextest
  stable, Windows stable, macOS stable, Linux musl stable) must stay
  green on master for the same window.
- **Window duration:** minimum **5 calendar days** (one nightly per
  day) AND 5 consecutive green runs, whichever is longer. A skipped
  nightly does not count as green; the clock pauses until the next
  successful nightly run.

Day 0 is the UTC calendar day of the first nightly run that observes
all pre-conditions in Section 2 met at master HEAD.

## 4. Signals to monitor during the bake window

All signals must remain green for the full window. A red signal
conclusively traced to an unrelated cause does not count against the
window but must be documented in the PIP-9.f.2 PR description.

- **Required CI checks on every PR merging to master:**
  - `fmt+clippy`
  - `nextest (stable)`
  - `Windows (stable)`
  - `macOS (stable)`
  - `Linux musl (stable)`
- **Nightly Interop Validation workflow** across all supported
  upstream versions (3.0.9, 3.1.3, 3.4.1, 3.4.2).
- **PIP-9.d CI matrix cell** - the
  `parallel-receive-delta-dist` job at
  `.github/workflows/ci.yml:586`.
- **Production CI for every PR landing during the window.** No
  parallel-path-only regression may surface even on a PR that does
  not touch the parallel path.
- **User-reported bug reports.** GitHub issues tagged with
  `parallel-receive-delta` or `parallel-delta-apply` opened during
  the window must be zero (or be conclusively non-attributable).

## 5. Disqualifying signals

Any of the following resets the criterion clock to day 0:

- **Any nightly Interop failure attributable to the parallel path.**
  Attribution is verified by reverting the parallel-path dispatch on
  a test branch and confirming the failure clears.
- **Any panic surfaced by fuzz-coverage** that is reachable only
  through the parallel path.
- **Wire-byte divergence** detected by the PIP-9.c sha256-asserted
  scenario at `tests/parallel_threshold_trip.rs`.
- **Throughput regression `>= 5%`** on the PIP-9.d bench cell vs the
  sequential baseline. The parallel path must be at least as fast,
  or the outcome moves to Path B (see Section 6).
- **User-reported correctness bug** attributable to the parallel
  path: silent corruption, missing files, wrong file content, wrong
  metadata, wrong hardlink graph.
- **Any silent transfer corruption** detected by the PIP-7 follow-up
  scenario at
  `crates/transfer/tests/pip_7_parallel_receive_delta_corruption_repro.rs`.

Reset semantics: the clock restarts to day 0 only if the regression
cannot be fixed in a forward-fix PR within 7 calendar days. A
forward-fix that lands within 7 days keeps the existing clock; the
window absorbs the bug as part of the bake. Otherwise PIP-9.f.2 is
withdrawn and a specific PIP-9.b sub-task is re-opened to address
the root cause.

## 6. Outcome paths

The window ends in exactly one of three states.

### 6.1 Path A (recommended) - criterion satisfied

All Section 3 sub-criteria met; no Section 5 signal fired.

- **PIP-9.f.2** opens the Cargo.toml flip PR. The feature default
  changes from OFF to ON; the gate remains in the codebase as an
  emergency opt-out.
- **PIP-9.f.3** monitors production CI for **1 additional calendar
  week** after the flip lands on master. Same signals as Section 4
  apply.
- **PIP-9.f.4** closes the PIP-9 series:
  - Mark PIP-9.e closed (PIP-7 receiver-corruption issue resolved).
  - Update memory note
    `[[project_parallel_interop_parity_gap]]` to RESOLVED with a
    pointer to the flip commit.
  - Update memory note `[[project_apply_batch_write_serial]]` with
    the post-flip benchmark numbers.

### 6.2 Path B (acceptable) - correct but slower

Correctness criteria from Section 3 satisfied; throughput criterion
**failed** by `<= 5%` vs the sequential baseline. The parallel path
is correct but slower than sequential for the workloads exercised by
the PIP-9.d bench cell.

- **Keep the feature gate OFF by default.** Do not flip in
  `Cargo.toml`.
- **Ship the parallel path as an opt-in** for users with workloads
  where parallel helps (large basis files + many cores).
- **Document the threshold criterion** in the user-facing docs:
  cite the bench-cell baseline, the workloads under which parallel
  wins, and the build command (`cargo build --features
  parallel-receive-delta`).
- **PIP-9.f.4** closes the PIP-9 series with Path B noted; memory
  notes updated to reflect the opt-in status.

### 6.3 Path C - disqualifying signal fired

Any Section 5 signal fires and the root cause is unambiguous.

- **Revert any in-flight flip PR.** If PIP-9.f.2 has already landed,
  revert it on master as a single-commit revert.
- **Re-open one specific PIP-9.b sub-task** to fix the underlying
  issue. Include a regression test that fails before the fix and
  passes after.
- **Restart the bake window from day 0** once the fix lands. The
  Section 2 pre-conditions must be re-verified.

## 7. Telemetry and observation strategy

The bake criterion must be objectively verifiable. The following
artefacts provide the evidence:

- **Interop Validation history.** GitHub Actions workflow run page
  filtered by workflow `Interop Validation` + branch `master`. Count
  consecutive green runs from the most recent.
- **Fuzz Coverage history.** Same view, workflow `Fuzz Coverage` +
  branch `master`. Count consecutive green runs.
- **PIP-9.d bench artefacts.** The CI cell at
  `.github/workflows/ci.yml:586` is non-required and emits its build
  + nextest output to the workflow logs. PIP-9.f.2 prep adds JSON
  artefact upload per run; the 5-run rolling mean is computed by a
  helper script and compared against the sequential baseline.
- **Regression issue query.** `is:open label:regression
  "parallel-receive-delta" OR "parallel-delta-apply"` must return
  zero hits.
- **Check script.** A helper script under
  `scripts/pip_9_f_1_check.sh` (implemented as part of PIP-9.f.2
  prep) walks all five gates and prints `PASS` or `FAIL <gate>` so
  the criterion does not depend on manual observation. The script
  exits non-zero on any gate failure, allowing PIP-9.f.2 to be
  blocked by a CI check if desired.

## 8. Communication template

PIP-9.f.2 PR description must include the following block, with
bracketed values filled in at PR-open time:

> Enables `parallel-receive-delta` cargo feature by default. This
> was previously gated behind the feature flag pending validation of
> the parallel apply path (PIP-7..PIP-9 series). The bake criterion
> (PIP-9.f.1, `docs/design/pip-9-f-1-bake-criterion.md`) was
> satisfied: `{N}` consecutive green nightlies across Interop
> Validation, Fuzz Coverage, and the PIP-9.d bench cell; zero
> attributable regressions over the `{duration}`-day window starting
> `{start_date}`. The opt-out path (build without
> `--features parallel-receive-delta`) remains available pending
> PIP-9.f.4 closure.

## 9. References

- Parent series: PIP-9.
- Parent task: PIP-9.f (#2598).
- Self: PIP-9.f.1 (#2643).
- Follow-ups: PIP-9.f.2 (#2644), PIP-9.f.3 (#2645), PIP-9.f.4
  (#2646).
- Precedent / template: ISI.i.1
  (`docs/design/isi-h-bake-window-criteria.md`, shipped PR #4917).
- PIP-9 wire-up design:
  `docs/design/pip-9-parallel-receive-delta-wire-up-2026-05-22.md`.
- PIP-9.c sha256 byte-identity scenario:
  `tests/parallel_threshold_trip.rs`.
- PIP-9.d CI matrix cell: `.github/workflows/ci.yml:586`
  (`parallel-receive-delta (dist profile, non-required)`).
- PIP-7 receiver-corruption repro:
  `crates/transfer/tests/pip_7_parallel_receive_delta_corruption_repro.rs`.
- Sequential baseline bench (BR-3i.f harness):
  `crates/engine/benches/parallel_verify_chunk.rs` and
  `crates/core/benches/pip_6_end_to_end_parallel_vs_sequential.rs`.
- Interop harness: `tools/ci/run_interop.sh`.
- Memory notes: `[[project_parallel_interop_parity_gap]]`,
  `[[project_apply_batch_write_serial]]`.
