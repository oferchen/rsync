# DIS-8.b - Wiring the daemon cold-start bench into required checks

Tracking: DIS-8.b (#2991). Parent: DIS-8 (#2763). Predecessor: DIS-8.a
(#2990, PR #4905). Related: DIS-7, DIS-7.a, DIS-7.b.

Memory note: `[[project_daemon_initial_sync_3x_slow]]`.

Precedent template: ISI.i.1 bake-window design
(`docs/design/isi-h-bake-window-criteria.md`, PR #4917).

## 1. Scope

DIS-8.b plans the promotion of
`.github/workflows/bench-daemon-coldstart.yml` from advisory
(`continue-on-error: true`) to required-check status on the `master`
branch protection ruleset.

This task does **not** change the GitHub branch-protection settings
directly. Branch protection is configured under
Settings -> Branches -> master and is an admin-only operation that
is not source-controllable. DIS-8.b instead delivers:

- (a) the bake-window pre-conditions that must be satisfied before
  the workflow is eligible for promotion;
- (b) the workflow-file edit (remove `continue-on-error`, tighten the
  ratio, add concurrency, narrow PR-event types) that the promotion
  PR must include;
- (c) the procedural steps an admin follows to flip the branch
  protection toggle once the PR merges;
- (d) the rollback procedure if the promoted check destabilises
  master throughput.

## 2. Current state on master

Already shipped via DIS-8.a (PR #4905):

- File: `.github/workflows/bench-daemon-coldstart.yml`.
- Triggers: `workflow_dispatch`, nightly `cron: '17 3 * * *'`, and
  `pull_request` with a `paths` filter on `crates/daemon/**`,
  `crates/core/src/session/**`, `crates/protocol/src/handshake/**`,
  and the workflow file itself.
- Concurrency group: `bench-daemon-coldstart-${{ github.ref }}`
  with `cancel-in-progress: true`.
- Job: `bench-daemon-coldstart` named `Daemon cold-start regression`,
  runs on `ubuntu-latest`, `timeout-minutes: 30`.
- Advisory status: `continue-on-error: true` - failures do not block
  PRs.
- Methodology: `hyperfine --warmup 1 --runs 10` over a 10-file
  fixture, run against both oc-rsync daemon and upstream rsync daemon
  on free localhost ports.
- Pass criterion: oc-rsync mean wall-clock <= 1.5x upstream mean.
  Today this is expected to **fail** because the measured gap is
  ~3.7x (1.35s vs 0.36s) per
  `[[project_daemon_initial_sync_3x_slow]]`.

The 1.5x ceiling is a placeholder bound chosen so the workflow can
exist on master while DIS-7 closes the underlying gap; it is not the
ceiling the required-check version will carry.

## 3. Pre-conditions for opening the promotion window

DIS-8.b may only land on master once every item below is true at the
merge-base SHA. These are the entry gates; the bake window itself
starts only after all entry gates are green.

- **DIS-7 closed.** The cold-start gap must be re-measured at
  <= 1.1x upstream mean using the same methodology
  (`hyperfine --warmup 1 --runs 10`, 10-file fixture, free local
  ports). The 1.1x target is what the bench-cell-ratio of 1.2x is
  built around: 1.1x measured + 0.1x runner variance = 1.2x
  enforced.
- **DIS-7.a re-bench post-DIS-6 fixes.** Results documented in the
  DIS-7.a follow-up artifact and linked from the promotion PR
  description.
- **DIS-7.b close-out.** The DIS series exit task is closed; no open
  daemon cold-start subtasks remain.
- **Bake-window evidence.** Five consecutive nightly runs of
  `bench-daemon-coldstart.yml` on master must show PASS at the
  existing advisory threshold (oc-rsync mean <= 1.5x upstream mean).
  Five greens at 1.5x with a 1.1x typical measurement means a
  comfortable margin and demonstrates the cell is not flaky.
- **No open `regression` issues** tagged against daemon cold-start
  on the GitHub issue tracker.
- **Mean stability.** Across the last 10 advisory runs, the
  oc-rsync mean must be stable within +/- 5% (standard deviation /
  mean <= 0.05). This rules out high-variance regressions that
  would silently increase the flake rate after promotion.

If any pre-condition is red, the DIS-8.b PR does not land and the
promotion window does not open. Pre-conditions are entry criteria;
they are not part of the window duration itself.

## 4. Promotion-window duration

Minimum: **7 calendar days** OR **5 consecutive green nightly runs
of `bench-daemon-coldstart.yml` on master at the tightened (1.2x)
threshold**, whichever is later.

Day 0 is the UTC calendar day the DIS-8.b PR (the one carrying the
workflow-file edit in Section 5) merges to master.

Reset semantics: the counter resets to day 0 on any of:

- a `bench-daemon-coldstart.yml` nightly failure attributable to
  oc-rsync (i.e. not an Ubuntu-image, hyperfine, or rsync-package
  upgrade);
- any new `regression` GitHub issue against the daemon cold-start
  path;
- a forward-fix PR that does not land within 7 calendar days of the
  first observed failure.

Forward-fix that lands within 7 days keeps the existing clock; the
window absorbs the bug as part of the bake.

## 5. Workflow-file change shipped by the DIS-8.b PR

The PR for this task carries the design doc only. The downstream
promotion PR must, once the bake window passes, ship the following
edits to `.github/workflows/bench-daemon-coldstart.yml`:

- **Drop `continue-on-error: true`.** Remove lines 46-48 of the
  current file (the comment block and the directive). The job
  becomes a hard gate.
- **Tighten the pass criterion from `1.5x` to `1.2x`.** Update the
  `RATIO_LIMIT: "1.5"` env at the assertion step
  (currently line 165) to `RATIO_LIMIT: "1.2"`. Update the prose in
  the surrounding `echo`/`printf` calls so the rendered
  `$GITHUB_STEP_SUMMARY` cites `1.2x` as the fail threshold. Update
  the head comment block (lines 1-15) so the placeholder-bound
  language is replaced with the production-bound language and the
  `DIS-8.b` tracker is referenced.
- **Pin the upstream rsync version.** Replace
  `sudo apt-get install -y rsync hyperfine jq` with an explicit
  version pin so the ratio does not drift when the Ubuntu image
  rolls forward. Resolve the exact pin (for example
  `rsync=3.4.1-0ubuntu0.24.04.1`) at promotion time against the
  current `ubuntu-latest` image. Document the chosen pin and a
  short procedural note for refreshing it in the workflow head
  comment.
- **Narrow `pull_request` event types.** Add `types: [opened,
  synchronize, reopened]` under the `pull_request` trigger so the
  cell does not re-run on noisy events (`labeled`, `assigned`,
  `review_requested`, etc.). The job ID and the displayed
  check-name (`Daemon cold-start regression`) must stay identical
  so the existing branch-protection entry resolves to the same
  check.
- **Concurrency group is already set** and does not need to change.
  Keep the existing
  `concurrency.group: bench-daemon-coldstart-${{ github.ref }}`
  with `cancel-in-progress: true`.

These edits are intentionally batched into the promotion PR rather
than this design PR so that the workflow file does not silently
become required between the design landing and the branch-protection
toggle flipping.

## 6. Branch-protection change (post-merge admin action)

This step is performed by a repository admin **after** the promotion
PR (the one carrying the Section 5 edits) merges to master and the
first post-merge nightly run goes green at the tightened threshold.

Steps:

1. Open a dry-run PR (e.g. a no-op whitespace change under
   `crates/daemon/`) to confirm the
   `Daemon cold-start regression` check appears in the PR status
   rollup at the tightened threshold and passes.
2. Navigate to Settings -> Branches -> master -> Edit branch
   protection rule.
3. Under "Require status checks to pass before merging", click
   "Add checks" and search for `Daemon cold-start regression`.
4. Add the check to the required set. Save changes.
5. Close the dry-run PR.

The PR carrying the workflow-file change does **not** modify branch
protection. Branch protection is a GitHub-side admin configuration
and is not source-controllable in this repository.

## 7. Risk analysis

- **Runner CPU variance.** `ubuntu-latest` shared runners have
  documented CPU variability. A 1.2x ceiling is comfortable above
  the 1.1x DIS-7 target but a 1.5%-5% pathological run can still
  blow the ceiling on a worst-case scheduling pattern.
  Mitigation: the Section 3 pre-conditions require 5 consecutive
  greens and +/- 5% mean stability so a known-noisy build cannot
  start the window.
- **Required-check blocks emergency fixes.** Any urgent master push
  during a bench-flake window can be admin-merged using
  `gh pr merge --admin`. The repo admin role is the documented
  escape hatch. The promotion PR description must call this out
  explicitly so future maintainers know the escape exists.
- **Ubuntu image upstream-rsync drift.** When the `ubuntu-latest`
  image upgrades and ships a different upstream rsync version, the
  measured ratio can shift. Mitigation: Section 5 mandates a
  pinned `apt-get install rsync=<version>` so the upstream side is
  reproducible. Refreshing the pin is a routine maintenance task
  (open a `chore:` PR), not an emergency.
- **Path-filter false-negatives.** The PR-trigger paths filter is
  narrow on purpose (daemon/session/handshake only). A regression
  shipped via, for example, `crates/protocol/src/wire/` would
  miss the per-PR cell and only surface in the nightly. The
  nightly remains required regardless, so master would still
  break visibly within 24 hours.
- **Path-filter false-positives.** Trivial PRs touching the daemon
  crate (typo fixes in comments, dependency bumps) will now block
  on a 30-minute bench job. This is the accepted cost of putting
  the check on the required set; the alternative (no per-PR
  signal) means regressions are not caught until the nightly,
  which is too late if a release tag goes out in between.

## 8. Rollback procedure

If the promoted check destabilises master throughput beyond the
forward-fix budget:

1. **Admin removes the check from the required set.**
   Settings -> Branches -> master -> Edit -> uncheck
   `Daemon cold-start regression` from the required set. This is
   immediate and does not need a PR.
2. **Open a revert PR.** Restore `continue-on-error: true` to
   `.github/workflows/bench-daemon-coldstart.yml`. Restore the
   ratio to `1.5x` if the 1.2x ceiling is the destabilising
   factor. Land the revert PR through the standard review flow.
3. **Diagnose the flake.** Either (a) tighten bench
   reproducibility (longer warmup, more runs, larger fixture,
   pinned image), or (b) back the ratio off to 1.3x or 1.4x as
   an intermediate step before re-promoting.
4. **Re-enter Section 3 pre-conditions.** A revert means the
   promotion window restarts from scratch; partial credit is not
   carried.

## 9. Communication template

For the eventual promotion PR description (the PR shipping the
Section 5 workflow-file edits), fill the bracketed values from the
contemporaneous bake-window data:

> **Promotes `bench-daemon-coldstart` from advisory to required.**
> Pre-conditions satisfied per
> `docs/design/dis-8-b-required-check-wiring.md`: DIS-7 closed at
> `[{ratio}]`x upstream; `[{n}]` consecutive nightly runs green;
> flake rate `[{pct}]`% over the last `[{m}]` advisory runs;
> mean-stability sigma/mean `[{sigma_over_mean}]` (target <= 0.05).
> After merge, branch protection on `master` must be updated to add
> the `Daemon cold-start regression` check to the required set; this
> is a one-time admin action documented in Section 6 of the design
> doc. Escape hatch for emergency pushes during a bench flake is
> `gh pr merge --admin`.

## 10. Cross-references

- DIS-8.a workflow file:
  `.github/workflows/bench-daemon-coldstart.yml` (PR #4905).
- DIS-8 parent tracker: #2763.
- DIS-8.b tracker: #2991.
- DIS-7 cold-start gap closure (and follow-ups DIS-7.a, DIS-7.b)
  blocks the Section 3 entry gates.
- Memory note: `[[project_daemon_initial_sync_3x_slow]]`.
- Precedent template: ISI.i.1 bake-window doc -
  `docs/design/isi-h-bake-window-criteria.md` (PR #4917).
