# PIP-9.f.3 - Bake-window monitor runbook for parallel-receive-delta default-on flip

Tracking: PIP-9.f.3 (#2645). Parent: PIP-9.f (#2598). Series parent:
PIP-9. Siblings: PIP-9.f.1 (#2643, bake criterion, PR #4924), PIP-9.f.2
(Cargo.toml flip PR, pending), PIP-9.f.4 (#2646, closure procedure).

Memory notes: `[[project_parallel_interop_parity_gap]]`,
`[[project_apply_batch_write_serial]]`.

## 1. Audience

SRE and oncall maintainers monitoring production CI during the
`parallel-receive-delta` default-on bake window. This runbook is the
day-to-day operational practice during the bake period defined by
PIP-9.f.1; it tells the oncall what to check, how often, and what to
do when a signal fires. It is not the criterion definition (PIP-9.f.1)
and it is not the flip mechanism (PIP-9.f.2). Read those first if you
have not already; this runbook assumes both are in scope.

## 2. Scope

PIP-9.f.3 is the operational runbook covering the bake window between
the moment PIP-9.f.2 lands on master and the moment PIP-9.f.4 closes
the series. It defines the daily monitor checklist, the weekly summary
practice, the disqualifying-signal response procedure, the daily
communication template, and the bake-window completion procedure.

In scope:

- Daily checks on the signals named by PIP-9.f.1 section 4.
- Triage and response when any signal fires.
- Daily and weekly status communication.
- Completion procedure that hands off to PIP-9.f.4.

Out of scope:

- The criterion itself (what counts as "green" - see PIP-9.f.1).
- The Cargo.toml change that flips the feature default (PIP-9.f.2).
- The closure mechanics for the PIP-9 tracking issues and memory
  notes (PIP-9.f.4).
- Permanent revert of an unrelated feature flag (see the DPC-4
  rollback runbook cross-referenced below for the structural
  precedent).

## 3. Pre-conditions

This runbook applies once all of the following hold:

- **PIP-9.f.2 has landed on master.** The Cargo.toml flip
  (`parallel-receive-delta` moved into the workspace `[features]`
  default list) is in master HEAD. Before that, the feature is
  opt-in and this runbook is dormant.
- **The bake window per PIP-9.f.1 is open.** Day 0 has been declared;
  the first post-flip nightly Interop Validation run has completed
  green; the 5-day minimum / 5-consecutive-green-nightly clock is
  running.
- **All other PIP-9 series prerequisites are met.** PIP-9.b is
  complete (production `token_loop` dispatches through
  `ParallelDeltaApplier` when the feature is enabled), PIP-9.c sha256
  byte-identity scenario is green at HEAD, PIP-9.d CI matrix cell at
  `.github/workflows/ci.yml:586` is green at HEAD, PIP-9.e PIP-7
  receiver-corruption issue is closed.

If any of the three pre-conditions is not satisfied, stop and escalate
to the PIP-9.f.2 author before proceeding. The runbook does not apply
to an unflipped tree.

## 4. Daily monitor checklist

For each of the 5+ bake days, the oncall performs the following six
checks in order. Each check produces a binary PASS or FAIL outcome.
A single FAIL on any check is a disqualifying signal; jump to section
6 for the response procedure.

- **D1: Nightly Interop Validation - all upstream versions green.**
  Confirm last night's Interop Validation workflow ran green across
  all four supported upstream rsync versions (3.0.9, 3.1.3, 3.4.1,
  3.4.2). View at
  `github.com/<repo>/actions/workflows/_interop.yml`. Filter by
  branch `master`. The most recent run must show all matrix legs
  green.

- **D2: Nightly Fuzz Coverage - green with no new panics.** Confirm
  last night's Fuzz Coverage workflow ran green and surfaced no new
  panics from the parallel path. A panic from any other code path is
  noted but does not count against the bake window.

- **D3: PIP-9.d CI matrix cell green on the most recent PR-triggered
  run.** Confirm the `parallel-receive-delta (dist profile,
  non-required)` job defined at `.github/workflows/ci.yml:586` ran
  green on the most recent PR that landed during the previous 24
  hours. The cell is non-required; treat a failure as a
  disqualifying signal anyway during the bake window.

- **D4: No new open regression issues.** Run the GitHub issues query
  `is:open label:regression "parallel-receive-delta" OR
  "parallel-delta-apply"` (via `gh issue list` or the web UI). The
  result must be empty. A single new issue is a disqualifying signal
  pending triage.

- **D5: Bench cell throughput within tolerance.** Pull the JSON
  output from the most recent nightly bench run for the PIP-9.d cell
  (BR-3i.f harness output). Throughput must be `>= 0.95x` the
  sequential baseline. The baseline was recorded from
  `crates/engine/benches/parallel_verify_chunk.rs` outputs prior to
  the PIP-9.f.2 flip and is published alongside the flip PR.

- **D6: PIP-7 corruption repro test still passes.** Confirm
  `crates/transfer/tests/pip_7_parallel_receive_delta_corruption_repro.rs`
  continues to pass in the most recent nightly Interop run. This is
  the silent-corruption canary; a failure here is the highest-
  severity disqualifying signal.

Record each check's outcome in a daily log entry. Suggested location:
`docs/operations/pip-9-f-3-bake-log/{YYYY-MM-DD}.md` (one file per
day, six PASS / FAIL lines plus a one-line note). If the per-day log
is too noisy for the repo, maintain it externally (oncall workspace,
shared doc, ticketing tool) and commit only the weekly summary
(section 5).

## 5. Weekly summary

On day 7 (and again on day 14 if the window is extended after a
forward-fix), produce a single weekly summary covering the previous 7
days. Format is one line per day:

> Day {N}: D1 {PASS/FAIL} | D2 {PASS/FAIL} | D3 {PASS/FAIL} | D4
> {PASS/FAIL} | D5 {ratio}x | D6 {PASS/FAIL}

If all 5+ days hit PASS on every check (D5 expressed as a ratio
`>= 0.95`), the bake window is satisfied and section 8 applies.

The weekly summary lands as a comment on the PIP-9.f.3 tracking issue
(#2645). Do not open a new PR for the summary unless the daily log is
being committed to the repo; in that case the summary header sits at
the top of the bake-log directory.

## 6. Disqualifying signals - response procedure

When any daily check (section 4) returns FAIL, classify the signal
and follow the matching response procedure below. Capture diagnostics
before changing deployment state; the next attempt cannot reproduce
the failure without the evidence.

### DS1: CI required-check failure attributable to the parallel path

Symptom: a required CI check (fmt+clippy, nextest stable, Windows
stable, macOS stable, Linux musl stable) or the PIP-9.d cell fails on
master HEAD during the bake window.

Response:

1. Verify attribution. Open a test branch off master, temporarily
   revert `parallel-receive-delta = true` from the workspace
   `Cargo.toml` default list, push, and confirm the failure clears.
   If the failure persists with the feature disabled, it is not
   attributable to the parallel path; document the unrelated cause
   in the day's log entry and continue the bake.
2. If attribution is confirmed, open a revert-the-flip PR via the
   procedure documented for PIP-9.f.4. Title: `revert: parallel-
   receive-delta default-on (PIP-9.f.2)`. Body cites the failing
   check, the attribution test branch, and the day of the bake
   window on which the failure surfaced.
3. Reset the bake-window clock to day 0. Re-verify PIP-9.f.1 section
   2 pre-conditions before reopening the window.

### DS2: User-reported correctness bug

Symptom: a GitHub issue, mailing-list report, or downstream
maintainer report alleges silent corruption, missing files, wrong
file content, wrong metadata, or wrong hardlink graph and the report
implicates the parallel-apply path.

Response:

1. Assess severity. Critical = silent corruption or data loss.
   Non-critical = behaviour deviation without data risk (for example,
   a metadata edge case under an exotic configuration).
2. Critical: immediate revert via the DS1 procedure step 2. Do not
   wait for the daily check to surface it; trigger the response on
   the report itself.
3. Non-critical: open a tracking issue with the `regression` label
   and the `parallel-receive-delta` label. Decide between (a)
   extending the bake window pending a forward fix or (b) proceeding
   with the flip and shipping the caveat in release notes. Document
   the decision in the day's log entry.

### DS3: Throughput regression > 5%

Symptom: the D5 bench-cell ratio falls below `0.95x` on the daily
check.

Response:

1. Capture the bench JSON for the failing day.
2. Confirm reproducibility. Re-run the bench cell at least twice
   more (manual workflow dispatch); a single FAIL on D5 may be a
   one-off flake from the shared CI runner.
3. If three consecutive runs are below threshold, the bake window
   pauses. Open an investigation issue, attach the bench JSON, and
   block PIP-9.f.4 closure until the regression is resolved or the
   outcome moves to PIP-9.f.1 section 6.2 (Path B - correct but
   slower).
4. If the regression is resolved by a forward-fix PR within 7
   calendar days, the bake window resumes on the existing clock.
   Otherwise reset to day 0.

### DS4: Wire-byte divergence

Symptom: the PIP-9.c sha256 byte-identity scenario at
`tests/parallel_threshold_trip.rs` fails on master HEAD.

Response:

1. Immediate revert via the DS1 procedure step 2. Wire-byte
   divergence is a structural correctness bug; there is no
   forward-fix path that justifies leaving the flip in place.
2. Re-open the specific PIP-9.b sub-task whose component owns the
   divergence (sender flist, applier scheduling, reorder buffer,
   token wire). Attach a regression test that fails before the fix
   and passes after.
3. Reset the bake-window clock to day 0 once the fix lands.

### DS5: Intermittent transfer hang

Symptom: a multi-file delta-apply workload hangs intermittently;
the symptom disappears when the feature is built off; no io_uring,
IOCP, or SSH-transport runbook explains the hang.

Response:

1. Capture stack traces from all worker threads. Linux:
   `gdb -p $PID -ex "thread apply all bt" -ex detach -ex quit` or
   `pstack $PID`. macOS: `lldb -p $PID -o "thread backtrace all" -o
   detach -o quit` or `sample $PID 10`. Save verbatim.
2. File a stress-test reproducer if possible. The PIP-9 series
   stress tests at `crates/transfer/tests/` are the right shape; add
   a new test that reproduces the hang at the smallest worker count
   that triggers it.
3. Decide based on reproducibility. Deterministic reproducer:
   immediate revert via DS1. Non-deterministic: extend the bake
   window pending root-cause analysis; do not flip closure until
   the reproducer is in CI.

## 7. Communication template

Post a daily status update for each day of the bake window. Format:

> **PIP-9 bake day {N}/5**: All checks {GREEN/RED/MIXED}. Interop:
> {pass/fail}. Fuzz: {pass/fail}. PIP-9.d: {pass/fail}. PIP-7 repro:
> {pass/fail}. Throughput: {ratio}x baseline. Open regression
> issues: {count}.

Post the update as a comment on the PIP-9.f.3 tracking issue
(#2645). On any RED day, include a one-paragraph note pointing at
the disqualifying-signal response procedure invoked (DS1 through
DS5).

For weekly summaries see section 5. For the completion announcement
see section 8.

## 8. Bake-window completion procedure

When all 5+ days of the bake window are satisfied (every daily check
returned PASS; D5 stayed `>= 0.95x` baseline; no DS signal fired),
hand off to PIP-9.f.4:

1. **Open the PIP-9.f.4 follow-up PR.** Body cites the bake-window
   start date, the bake-window end date, the daily-log summary, and
   the weekly summary. The PR performs three changes:
   - Close the PIP-9.e tracking issue with a comment pointing at
     the PR.
   - Update memory note `[[project_parallel_interop_parity_gap]]`
     to SHIPPED status with a pointer to the flip commit.
   - Update memory note `[[project_apply_batch_write_serial]]`
     with the post-flip benchmark numbers.
2. **Remove the temporary `parallel-receive-delta` feature flag
   from Cargo.toml.** The flag was retained after PIP-9.f.2 as an
   emergency opt-out. Once the bake window is satisfied and PIP-9.e
   is closed, the flag graduates from feature-gated to always-on.
   Remove the flag from the workspace `[features]` table and every
   per-crate forwarding entry; delete the
   `parallel-receive-delta-dist` CI matrix cell at
   `.github/workflows/ci.yml:586` (it has no signal value once the
   path is always-on).
3. **Communicate completion via release notes.** The next minor
   release announcement carries a section titled "Parallel receive-
   side delta apply is now the default." Cite the flip release, the
   bake-window summary, and the opt-out being removed.

After PIP-9.f.4 lands, archive the daily logs (or close the external
log location), close the PIP-9.f.3 tracking issue, and close the
PIP-9 series parent.

## 9. Tooling pointers

For future automation of the daily checks:

- **Daily-check helper.** Extend the `scripts/pip_9_f_1_check.sh`
  helper from PIP-9.f.1 spec section 7 to emit the daily-check
  matrix in the section 4 order (D1 through D6). One-shot exit code
  0 = all green; non-zero = at least one FAIL.
- **GitHub Actions API rollup.** A per-day rollup of the nightly
  Interop runs over the bake window:

  ```sh
  gh run list \
    --workflow=_interop.yml \
    --branch=master \
    --limit=5 \
    --json conclusion,createdAt
  ```

  Apply the same pattern to `--workflow=fuzz.yml` for D2 and to
  `--workflow=ci.yml` for D3 (filtering on the
  `parallel-receive-delta-dist` job name).
- **Regression query.** `gh issue list --label regression --search
  '"parallel-receive-delta" OR "parallel-delta-apply"' --state open`
  for D4.
- **Bench JSON.** The PIP-9.d cell uploads JSON artefacts per run
  once PIP-9.f.2 prep ships the upload step; pull with
  `gh run download <run-id> --name pip-9-d-bench-json` for D5.

The check script is the canonical interface. Manual web-UI checks
are acceptable on day 1; by day 3 the daily check should be a single
script invocation that emits the section 7 communication template.

## 10. Cross-references

- PIP-9.f.1 bake-criterion doc:
  `docs/design/pip-9-f-1-bake-criterion.md` (PR #4924). Defines
  what "5 consecutive green CI cycles" means; this runbook
  operationalises it.
- PIP-9.f.2 flip PR (pending). The Cargo.toml change that triggers
  this runbook.
- PIP-9.f.4 closure procedure (pending, #2646). The follow-up that
  this runbook hands off to on bake-window completion.
- DPC-4 rollback runbook:
  `docs/operations/drain-restructure-rollback.md` (PR #4915).
  Structural template for the disqualifying-signal response
  procedure and the communication template.
- PIP-9 wire-up design:
  `docs/design/pip-9-parallel-receive-delta-wire-up-2026-05-22.md`.
- PIP-9.c sha256 byte-identity scenario:
  `tests/parallel_threshold_trip.rs`.
- PIP-9.d CI matrix cell: `.github/workflows/ci.yml:586`.
- PIP-7 receiver-corruption repro:
  `crates/transfer/tests/pip_7_parallel_receive_delta_corruption_repro.rs`.
- Sequential baseline bench:
  `crates/engine/benches/parallel_verify_chunk.rs` and
  `crates/core/benches/pip_6_end_to_end_parallel_vs_sequential.rs`.
- Interop harness: `tools/ci/run_interop.sh`.
- Memory notes: `[[project_parallel_interop_parity_gap]]`,
  `[[project_apply_batch_write_serial]]`.
