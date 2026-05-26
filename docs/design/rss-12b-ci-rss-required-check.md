# RSS-12.b: Wire RSS regression check into required CI

Task: RSS-12.b (#2933). Branch: `docs/rss-12b-required-check`. Parent:
RSS-12.a (#4988, `bench-rss.yml` workflow spec). Series: RSS-8..12
(arena-allocated paths migration and CI enforcement).

## 1. Current state (RSS-12.a)

RSS-12.a specifies a GitHub Actions workflow (`bench-rss.yml`) that
measures peak RSS of `oc-rsync` during a 100K-file daemon pull
transfer. Key properties of the existing workflow:

- **Fixture:** 100 directories x 1000 files = 100K entries, 64-byte
  payload each, generated inline (not committed).
- **Transfer mode:** Daemon pull with `--no-inc-recursive` to force the
  full file list into memory at once.
- **Measurement:** `/usr/bin/time -v` on Linux, 3 measured runs after
  1 warm-up, median taken.
- **Baseline:** Checked-in JSON at `.github/baselines/rss-100k.json`
  with `peak_rss_mb` and `threshold_percent` fields.
- **Threshold:** Fails when median RSS exceeds
  `peak_rss_mb * (1 + threshold_percent / 100)`.
- **Reporting:** Markdown table in `$GITHUB_STEP_SUMMARY`, raw JSON
  uploaded as artifact with 30-day retention.
- **Status:** Ships with `continue-on-error: true` - informational
  only, does not block PRs.
- **Triggers:** `workflow_dispatch`, nightly cron (`47 2 * * *`), and
  `pull_request` with path filters on `crates/protocol/src/flist/**`,
  `crates/protocol/src/recv/**`, `crates/core/src/session/**`,
  `crates/engine/src/**`.

The workflow has been running nightly since its introduction. RSS-12.b
decides whether and how to promote it from informational to required.

## 2. Required check wiring

### 2.1 GitHub rulesets (current mechanism)

Branch protection for `master` uses a GitHub repository ruleset
(ID 12911634, "Protect master") with `required_status_checks`. The
current required checks are:

| Context name | Workflow |
|---|---|
| `fmt + clippy` | `ci.yml` |
| `nextest (stable)` | `ci.yml` |
| `Windows (stable)` | `ci.yml` |
| `macOS (stable)` | `ci.yml` |
| `Linux musl (stable)` | `ci.yml` |
| `interop / interop with upstream rsync` | `ci.yml` (reusable) |

Adding a new required check requires updating this ruleset via the
GitHub API or the repository settings UI:

```bash
# Read current ruleset
gh api repos/{owner}/{repo}/rulesets/12911634

# Update with new check appended to required_status_checks array
gh api repos/{owner}/{repo}/rulesets/12911634 \
  --method PUT \
  --input updated-ruleset.json
```

The new check context name must exactly match the job `name:` field in
`bench-rss.yml`. Per the RSS-12.a spec, the job is named
`bench-rss` in the workflow, so the context would be:

- **Standalone workflow:** `RSS regression bench` (the workflow `name:`)
  or `RSS regression bench / bench-rss` (workflow / job).

The exact context depends on whether `bench-rss.yml` is a standalone
workflow or called as a reusable workflow. For a standalone workflow
triggered on `pull_request`, GitHub reports the context as the job
`name:` field. The recommended context name is `RSS regression bench`.

### 2.2 ci-skip.yml coordination

When a PR touches only docs, scripts, or workflow files - paths that
do not trigger `ci.yml` - the `ci-skip.yml` workflow provides stub
jobs that satisfy the required check contexts. If the RSS bench becomes
a required check, a matching stub must be added to `ci-skip.yml`:

```yaml
rss-bench:
  name: RSS regression bench
  runs-on: ubuntu-latest
  steps:
    - run: echo "Skipped - no code changes"
```

Without this stub, docs-only PRs would hang indefinitely waiting for
the RSS check context to appear.

### 2.3 Required vs. advisory

**Recommendation: advisory (do not add to required checks now).**

Rationale in section 7 below. The implementation plan describes both
paths so the promotion can be executed when readiness criteria are met.

## 3. Threshold configuration

### 3.1 Current threshold

The RSS-12.a baseline file `.github/baselines/rss-100k.json` defines:

```json
{
  "peak_rss_mb": 42,
  "threshold_percent": 10
}
```

This produces a ceiling of `42 * 1.10 = 46.2 MB`. Any median above
46 MB (integer truncation) fails the check.

### 3.2 Threshold tuning strategy

The 10% threshold was chosen to balance sensitivity against noise:

- **Lower bound (5%):** Would catch a single `u64` field addition to
  `FileEntry` (8 B * 100K = 800 KB = ~2% of 42 MB), but risks false
  positives from allocator jitter, kernel THP behavior, and GHA runner
  hardware variation.
- **Current (10%):** Catches structural regressions (re-adding `PathBuf`
  at 24 B * 100K = 2.4 MB = ~6%) while tolerating single-field additions
  and runner variance.
- **Upper bound (15%):** Too loose - would miss two concurrent
  single-field additions that compound.

The threshold should remain at 10% unless nightly run data demonstrates
a consistent false-positive rate above 1 in 20 runs.

### 3.3 Baseline update process

When the baseline needs to change - either because RSS improved
(tighten) or because a legitimate feature increased it (loosen):

1. Author runs the workflow manually via `workflow_dispatch` on the
   branch with the change, collecting at least 5 median measurements.
2. Author opens a PR that updates only `.github/baselines/rss-100k.json`
   with the new `peak_rss_mb` value and a `notes` field explaining why.
3. The PR description must include the 5 measured medians and the
   justification (optimization landed, new required field added, etc.).
4. A reviewer verifies the measurements are consistent (stddev < 2 MB)
   before approving.
5. The `updated` field in the JSON is set to the merge date.

**Baseline updates must never be bundled with the code change that
causes the RSS shift.** Separate PRs ensure the threshold change is
reviewed independently of the feature.

## 4. False positive mitigation

### 4.1 Sources of false positives

| Source | Magnitude | Frequency |
|---|---|---|
| GHA runner hardware generation change | 1-3% | Rare (GitHub rotates quarterly) |
| Kernel page-cache attribution | 0-2% | Low (warm-up run mitigates) |
| Allocator bin rounding (glibc malloc) | 0-1% | Consistent per build |
| THP (transparent huge pages) | 0-2 MB | Low on GHA (usually disabled) |
| Unrelated dependency update pulling in allocations | 0-1% | Per Cargo.lock update |

The 10% threshold absorbs all of these individually. Compounding is
unlikely because runner variance and allocator rounding do not
correlate.

### 4.2 Legitimate RSS increases

Some code changes legitimately increase peak RSS:

- Adding a field to `FileEntry` or `FileList` metadata.
- Increasing buffer pool default capacity.
- Supporting a new protocol extension that adds per-entry state.
- Changing the path interner's bucket count or slab size.

For these changes, the process is:

1. The PR that introduces the change will fail the RSS bench.
2. The author verifies the increase is expected by running the bench
   locally or via `workflow_dispatch`.
3. The author documents the expected per-entry overhead increase in
   the PR description.
4. A separate follow-up PR updates the baseline (section 3.3).
5. During the window between merge and baseline update, the RSS
   bench may fail on subsequent PRs. Since the check is advisory
   (`continue-on-error: true`), this does not block merges.

If the check were required, step 5 would block all PRs until the
baseline update merges. This is the primary argument against making
the check required at this stage (section 7).

### 4.3 Override mechanism for required mode

If the check is later promoted to required, an override label provides
an escape hatch. The workflow adds a condition:

```yaml
if: >-
  github.event_name != 'pull_request' ||
  !contains(github.event.pull_request.labels.*.name, 'rss-override')
```

A maintainer adds the `rss-override` label to a PR to skip the RSS
check. The label must be documented in the repository's contributing
guide and requires a comment explaining why the override is needed.

Alternative: use a `ci-skip-rss` commit trailer parsed by the workflow.
The label approach is preferred because it is visible in the PR UI
without reading commit messages.

## 5. CI integration

### 5.1 Workflow trigger conditions

The RSS bench triggers on `pull_request` only when paths that affect
memory layout are touched:

```yaml
pull_request:
  paths:
    - 'crates/protocol/src/flist/**'
    - 'crates/protocol/src/recv/**'
    - 'crates/core/src/session/**'
    - 'crates/engine/src/**'
    - '.github/workflows/bench-rss.yml'
    - '.github/baselines/rss-100k.json'
```

**Not triggered by:** docs-only changes, CLI help text, filter logic,
checksum algorithm changes, daemon config parsing, transport layer,
or workflow files other than `bench-rss.yml`.

If the check becomes required, the `ci-skip.yml` stub (section 2.2)
handles the non-triggered case.

### 5.2 Timeout configuration

- **Job timeout:** 20 minutes (per RSS-12.a spec).
  - Release build: ~8 minutes on `ubuntu-latest` (cached: ~2 minutes).
  - Fixture generation: ~3 seconds.
  - 4 transfers (1 warm-up + 3 measured): ~20 seconds.
  - Total with cache hit: ~5 minutes. The 20-minute ceiling covers
    cold-cache builds.

### 5.3 Artifact retention

- **RSS results JSON:** 30-day retention. Contains per-run measurements,
  median, baseline, ceiling, pass/fail, commit SHA, timestamp.
- **Daemon logs:** 30-day retention. Included for debugging if the
  daemon fails to start or the transfer produces unexpected output.
- **Step summary:** Persists with the workflow run (no separate
  retention policy).

30 days is sufficient for post-merge regression investigation. Longer
retention (90 days) is unnecessary because the nightly cron provides
continuous measurement - a regression that persists for 30 days without
being caught by nightly runs has deeper issues.

### 5.4 Concurrency

```yaml
concurrency:
  group: bench-rss-${{ github.ref }}
  cancel-in-progress: true
```

This ensures only the latest push to a branch runs the RSS bench,
canceling stale runs. The concurrency group is scoped per-ref to
avoid cross-branch cancellation.

## 6. Promotion criteria

Before promoting the RSS bench to a required check, the following
conditions must be met:

| Criterion | Target | How to verify |
|---|---|---|
| Nightly stability | 14 consecutive nightly runs with zero false positives | Review workflow run history |
| Baseline established | `peak_rss_mb` set from post-RSS-8..11 measurements, not a placeholder | Verify the baseline JSON `notes` field references actual measurements |
| ci-skip.yml stub | Stub job added to `ci-skip.yml` with matching context name | PR that adds the stub |
| Override mechanism | `rss-override` label created in repository | Verify label exists |
| Documentation | Contributing guide updated to explain the RSS check and override process | PR with docs update |
| Median variance | Observed stddev across 14 nightly runs is < 2 MB | Compute from artifact JSON |

## 7. Recommendation: advisory, not required

**The RSS regression bench should remain advisory (`continue-on-error:
true`) and should not be added to the required status checks in the
"Protect master" ruleset at this time.**

### 7.1 Arguments for advisory

1. **No post-migration baseline exists.** The RSS-8..11 arena migration
   has not landed. The `peak_rss_mb: 42` value in the spec is
   projected, not measured. A required check with a projected baseline
   will either be too tight (blocking valid PRs) or too loose (missing
   regressions).

2. **Insufficient bake-in data.** The RSS-12.a spec calls for 2 weeks
   of stable nightly runs before promotion. That data does not exist
   yet. Without it, the false-positive rate is unknown.

3. **Baseline update friction.** When a legitimate change increases RSS,
   the baseline update requires a separate PR. During the window between
   the feature merge and the baseline update, a required check would
   block all PRs that touch memory-sensitive paths. Advisory mode
   surfaces the regression signal without creating a merge queue
   bottleneck.

4. **Existing required checks are deterministic.** The current 6
   required checks (fmt, nextest, Windows, macOS, musl, interop) are
   deterministic - they pass or fail based solely on code correctness.
   The RSS bench has inherent measurement variance from runner hardware,
   allocator behavior, and kernel memory accounting. Adding a
   non-deterministic gate to a deterministic pipeline increases the
   support burden.

5. **Precedent.** Other performance-related checks in this repository
   ship as advisory: `bench-daemon-coldstart.yml`,
   `bench-daemon-concurrency.yml`, `bench-drain-throughput.yml`,
   `ssh-smoke-bench.yml`. None are required. Promoting the RSS bench
   before these creates an inconsistency.

### 7.2 Path to required

The RSS bench should be promoted to required when:

1. RSS-8..11 arena migration has landed and been released.
2. A measured baseline is established from 5+ `workflow_dispatch` runs.
3. 14 consecutive nightly runs pass without false positives.
4. The `ci-skip.yml` stub and `rss-override` label are in place.

At that point, the promotion is a two-step change:

**Step 1: Workflow change (PR)**
- Remove `continue-on-error: true` from `bench-rss.yml`.
- Add the `ci-skip.yml` stub job.
- Add the `rss-override` label condition.

**Step 2: Ruleset update (admin)**
```bash
# Add "RSS regression bench" to the required_status_checks array
gh api repos/{owner}/{repo}/rulesets/12911634 \
  --method PUT \
  --field 'rules[2].parameters.required_status_checks[]={"context":"RSS regression bench"}'
```

Or via Settings > Rules > "Protect master" > Required status checks >
Add "RSS regression bench".

### 7.3 Interim value

Even as advisory, the RSS bench provides:

- **Nightly trend data.** Artifact JSON from nightly runs builds a
  time-series of peak RSS. Regressions are visible in the workflow
  run history even without blocking merges.
- **PR annotations.** The step summary appears on every PR that touches
  memory-sensitive paths, giving reviewers an immediate signal.
- **Baseline anchoring.** The checked-in baseline establishes a
  documented expectation, even if the check does not enforce it.

## 8. Implementation checklist

Tasks to implement RSS-12.b promotion when readiness criteria (section
6) are met:

- [ ] Verify 14 nightly runs pass (zero false positives).
- [ ] Confirm baseline JSON has measured (not projected) `peak_rss_mb`.
- [ ] Create `rss-override` GitHub label with description.
- [ ] Add `ci-skip.yml` stub job with context `RSS regression bench`.
- [ ] Add `rss-override` label skip condition to `bench-rss.yml`.
- [ ] Remove `continue-on-error: true` from `bench-rss.yml`.
- [ ] Update "Protect master" ruleset to add `RSS regression bench`.
- [ ] Document RSS check in contributing guide.
- [ ] Verify a docs-only PR gets the stub (does not hang).
- [ ] Verify a code PR touching `crates/engine/` gets the real bench.
- [ ] Verify `rss-override` label skips the check.
