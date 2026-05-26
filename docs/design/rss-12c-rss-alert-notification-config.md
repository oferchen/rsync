# RSS-12.c: Alert/notification config for RSS regressions

Task: RSS-12.c. Branch: `docs/rss-12c-alert-notification-config`. Parent
series: RSS-12 (CI regression detection). Predecessor: RSS-12.a
(CI workflow spec, PR merged), RSS-12.b (required-check wiring, PR #5006,
merged). Downstream: RSS-12.d (workflow implementation).

Memory note: `[[project_rss_3_11x_upstream]]`.

## 1. Motivation

RSS-12.a designed a CI workflow (`bench-rss.yml`) that measures peak RSS
on a 100K-file daemon pull and compares it to a checked-in baseline.
RSS-12.b recommends keeping the check advisory (`continue-on-error: true`)
because no measured baseline exists yet - the RSS-8..11 arena migration
has not landed, so the pre-migration numbers would set a misleadingly
high ceiling.

The consequence: an advisory check that fails silently. GitHub Actions
does not natively surface advisory-check failures to PR authors or
maintainers unless someone clicks through to the workflow run. A
regression can merge unnoticed because:

- The check appears as a grey skipped/neutral icon, not a red failure.
- No email is sent for `continue-on-error` jobs that fail.
- The step summary is buried under the "Actions" tab, not visible in
  the PR timeline.
- Nightly-schedule failures have no PR to annotate at all.

RSS-12.c addresses this gap by designing the alert and notification
configuration that surfaces RSS regressions to maintainers regardless
of whether the check is advisory or required.

## 2. Design principles

1. **No external services.** The project has no Slack workspace, PagerDuty
   account, or third-party notification integrations. All alerting must
   use GitHub-native mechanisms: check annotations, PR comments, issues,
   and email (via GitHub notification preferences).
2. **Match existing patterns.** The project's bench workflows
   (`bench-daemon-coldstart.yml`, `bench-daemon-concurrency.yml`,
   `bench-drain-throughput.yml`) use a consistent pattern:
   `GITHUB_STEP_SUMMARY` tables, `::error::` annotations, artifact
   uploads, and `continue-on-error: true`. RSS alerts must extend this
   pattern, not replace it.
3. **No noise on green.** Alerts fire only on regression. Passing runs
   produce a step summary and artifact but no PR comment and no issue.
4. **Idempotent comments.** PR comments must be created-or-updated, not
   duplicated on re-runs. Use a hidden HTML marker to find and update
   existing comments.
5. **Minimal permissions.** The workflow requests only the permissions it
   needs: `contents: read` for checkout and baseline reading,
   `pull-requests: write` for PR comments.

## 3. GitHub Actions notification mechanisms - evaluation

| Mechanism | Visibility | Works for nightly? | Requires extra permissions? | Verdict |
|-----------|-----------|-------------------|---------------------------|---------|
| `::error::` annotation | Inline on the check, visible in PR "Checks" tab | No (no PR context on schedule) | No | Use for PR runs |
| `::warning::` annotation | Same as error but yellow | Same | No | Use for near-threshold warnings |
| `GITHUB_STEP_SUMMARY` | "Actions" tab per run | Yes (but nobody checks nightly summaries) | No | Already used; keep |
| PR comment via `actions/github-script` | PR timeline, email notification | No (no PR on schedule/dispatch) | `pull-requests: write` | Use for PR runs |
| GitHub issue creation | Repo issue tracker, email to watchers | Yes | `issues: write` | Use for nightly regressions |
| Slack/Teams webhook | External channel | Yes | `secrets.*` | Not available; skip |
| GitHub Discussions | Discussion thread | Yes | `discussions: write` | Overkill for alerts; skip |

**Selected approach:**

- **PR runs:** `::error::` annotation (already present in RSS-12.a) +
  PR comment with regression details.
- **Nightly/dispatch runs:** GitHub issue creation when regression
  detected. This generates email notifications to repo watchers and
  creates a trackable artifact.

## 4. PR comment specification

### 4.1 When to post

A comment is posted when the RSS bench runs on a `pull_request` event
and the measured median exceeds the baseline threshold. No comment is
posted on passing runs - only the step summary records the result.

### 4.2 Comment body format

```markdown
<!-- rss-bench-comment -->
### RSS regression detected

| Metric | Value |
|--------|-------|
| Median peak RSS | **54 MB** |
| Baseline | 42 MB |
| Threshold (+10%) | 46 MB |
| Delta | +12 MB (+28.6%) |
| Commit | `abc1234` |

The `bench-rss` workflow detected a peak RSS increase above the
configured threshold. This check is currently **advisory** and does
not block merge.

**What to check:**
- Did this PR add fields to `FileEntry` or related flist types?
- Did this PR introduce per-entry allocations (new `String`, `Vec`,
  `PathBuf`, or `Box` per file entry)?
- Is the increase expected (new required metadata) or accidental?

<details>
<summary>Raw measurements</summary>

| Run | Peak RSS (MB) |
|-----|---------------|
| 1   | 53            |
| 2   | 54            |
| 3   | 55            |

</details>

---
*Posted by `bench-rss.yml`. Baseline: `.github/baselines/rss-100k.json`.*
```

### 4.3 Implementation

Use `actions/github-script` to find an existing comment by the hidden
`<!-- rss-bench-comment -->` marker and update it, or create a new one.
This prevents duplicate comments on force-pushes and re-runs.

```yaml
- name: Post or update PR comment on regression
  if: >-
    failure()
    && github.event_name == 'pull_request'
  uses: actions/github-script@v9
  with:
    script: |
      const marker = '<!-- rss-bench-comment -->';
      const body = `${marker}\n${process.env.COMMENT_BODY}`;
      const { data: comments } = await github.rest.issues.listComments({
        owner: context.repo.owner,
        repo: context.repo.repo,
        issue_number: context.payload.pull_request.number,
      });
      const existing = comments.find(c => c.body.includes(marker));
      if (existing) {
        await github.rest.issues.updateComment({
          owner: context.repo.owner,
          repo: context.repo.repo,
          comment_id: existing.id,
          body,
        });
      } else {
        await github.rest.issues.createComment({
          owner: context.repo.owner,
          repo: context.repo.repo,
          issue_number: context.payload.pull_request.number,
          body,
        });
      }
```

The `COMMENT_BODY` environment variable is assembled in the preceding
assertion step from the measured values and the baseline JSON.

### 4.4 Permissions

The workflow's `permissions` block must include `pull-requests: write`
in addition to the existing `contents: read`:

```yaml
permissions:
  contents: read
  pull-requests: write
```

This matches the pattern used by `labeler.yml` and
`dependency-review.yml`.

## 5. Nightly regression issue specification

### 5.1 When to create

An issue is created when the RSS bench runs on a `schedule` or
`workflow_dispatch` event and the measured median exceeds the baseline
threshold. Issues are not created for `pull_request` events (those get
PR comments instead).

### 5.2 Deduplication

Before creating an issue, the workflow searches for an open issue with
the label `rss-regression` and the title prefix `RSS regression detected`.
If one exists, it appends a comment with the new measurement instead of
opening a duplicate. This prevents issue spam from consecutive nightly
failures while preserving a timeline of regression data.

### 5.3 Issue body format

```markdown
### RSS regression detected (nightly)

The `bench-rss` workflow detected a peak RSS regression on the
`master` branch.

| Metric | Value |
|--------|-------|
| Median peak RSS | **54 MB** |
| Baseline | 42 MB |
| Threshold (+10%) | 46 MB |
| Delta | +12 MB (+28.6%) |
| Commit | `abc1234` on `master` |
| Workflow run | [Run #1234](link) |

**Next steps:**
1. Identify the commit that introduced the regression using
   `git bisect` with the bench script.
2. If the increase is intentional (new metadata field), update the
   baseline in `.github/baselines/rss-100k.json` via a reviewed PR.
3. If accidental, open a fix PR targeting the regressing commit.

---
*Auto-created by `bench-rss.yml` nightly run.*
```

### 5.4 Issue labels

- `rss-regression` - a new label created for this purpose.
- `performance` - existing label, marks it as a perf concern.

### 5.5 Permissions

Requires `issues: write` in the workflow permissions block:

```yaml
permissions:
  contents: read
  pull-requests: write
  issues: write
```

### 5.6 Implementation

```yaml
- name: Open or update regression issue (nightly)
  if: >-
    failure()
    && github.event_name != 'pull_request'
  uses: actions/github-script@v9
  with:
    script: |
      const label = 'rss-regression';
      const titlePrefix = 'RSS regression detected';

      // Ensure the label exists.
      try {
        await github.rest.issues.getLabel({
          owner: context.repo.owner,
          repo: context.repo.repo,
          name: label,
        });
      } catch {
        await github.rest.issues.createLabel({
          owner: context.repo.owner,
          repo: context.repo.repo,
          name: label,
          color: 'e11d48',
          description: 'RSS regression detected by bench-rss CI',
        });
      }

      // Search for an existing open issue.
      const { data: issues } = await github.rest.issues.listForRepo({
        owner: context.repo.owner,
        repo: context.repo.repo,
        labels: label,
        state: 'open',
      });

      const runUrl = `${context.serverUrl}/${context.repo.owner}/${context.repo.repo}/actions/runs/${context.runId}`;

      if (issues.length > 0) {
        await github.rest.issues.createComment({
          owner: context.repo.owner,
          repo: context.repo.repo,
          issue_number: issues[0].number,
          body: process.env.ISSUE_BODY,
        });
      } else {
        await github.rest.issues.create({
          owner: context.repo.owner,
          repo: context.repo.repo,
          title: `${titlePrefix} (${new Date().toISOString().slice(0, 10)})`,
          body: process.env.ISSUE_BODY,
          labels: [label, 'performance'],
        });
      }
```

## 6. Alert threshold configuration

### 6.1 Baseline file

As specified in RSS-12.a, the baseline is stored in
`.github/baselines/rss-100k.json`:

```json
{
  "fixture": "100k-flat",
  "peak_rss_mb": 42,
  "threshold_percent": 10,
  "updated": "2026-06-01",
  "notes": "Post RSS-8..11 arena migration baseline"
}
```

The workflow reads `peak_rss_mb` and `threshold_percent` from this file.
No hardcoded thresholds in the workflow YAML.

### 6.2 Threshold semantics

```
ceiling_mb = peak_rss_mb * (1 + threshold_percent / 100)
```

Three zones:

| Zone | Condition | Action |
|------|-----------|--------|
| Green | median <= ceiling_mb | Step summary only. No comment, no issue. |
| Yellow | median > peak_rss_mb AND median <= ceiling_mb | `::warning::` annotation. Step summary notes the increase. No comment, no issue. (Near-threshold early warning.) |
| Red | median > ceiling_mb | `::error::` annotation. PR comment (on PR events). Issue (on nightly/dispatch events). |

The yellow zone is informational-only. It produces no PR comment but
adds a `::warning::` annotation to the check so reviewers see a yellow
triangle on the "Checks" tab. This provides early signal before a
regression crosses the fail threshold.

### 6.3 Threshold tuning

The `threshold_percent` value is deliberately separated from the
`peak_rss_mb` baseline so it can be tuned independently:

- **Initial value: 10%.** Matches RSS-12.a rationale: catches structural
  regressions (re-introducing `PathBuf`) while tolerating single-field
  additions and allocator noise.
- **Tightening.** After 4+ weeks of stable nightly runs, consider
  lowering to 7% if the measured variance is consistently < 3%.
- **Loosening.** If false positives appear (>2 in a 2-week window on
  master), raise to 15% temporarily and investigate runner variance.

Any threshold change is a reviewed PR modifying `rss-100k.json`. The
JSON `updated` field and `notes` field must reflect the rationale.

## 7. Baseline management

### 7.1 Initial baseline establishment

No baseline exists today. The workflow ships with a placeholder value
that deliberately triggers the yellow zone (warning) on every run. The
initial baseline is set after the RSS-8..11 arena migration lands:

1. Run `workflow_dispatch` on master after the arena migration merges.
2. Collect 5 consecutive nightly runs.
3. Set `peak_rss_mb` to the median of those 5 medians.
4. Submit a PR updating `rss-100k.json`. The PR description must cite
   the 5 run URLs and the computed median.

### 7.2 Lowering the baseline (ratchet-down)

When optimizations reduce RSS, the baseline should ratchet down to keep
the alert threshold tight. Process:

1. Verify the improvement is stable across 3+ nightly runs.
2. Open a PR that lowers `peak_rss_mb` and updates `updated`/`notes`.
3. The PR diff makes the change auditable.

Future extension (RSS-12.a Section 10.4): an auto-ratchet workflow that
opens a baseline-lowering PR when master nightly median is consistently
below `peak_rss_mb * 0.90` (10% headroom) for 5 consecutive runs. Not
in scope for RSS-12.c.

### 7.3 Raising the baseline

The baseline is never raised without explicit review and justification.
Valid reasons to raise:

- A new required metadata field is added to `FileEntry` (e.g., crtime
  support adds 8 bytes per entry = 800 KB at 100K scale).
- A protocol change requires buffering additional data per entry.

Invalid reasons:

- "The nightly keeps failing" without root-cause analysis.
- Reverting an optimization "because it was too complex."

The PR raising the baseline must include: (a) the root-cause commit,
(b) the per-entry overhead calculation, (c) why the overhead is
unavoidable.

## 8. Escalation path: advisory to required

RSS-12.b (merged) designed the advisory-to-required promotion for the
daemon cold-start bench. The RSS bench follows the same pattern with
RSS-specific criteria.

### 8.1 Promotion pre-conditions

All must be true at the merge-base SHA of the promotion PR:

- **RSS-8..11 arena migration landed.** The `FileEntry` path storage
  uses arena-backed `Spur` handles, not per-entry `PathBuf`.
- **Baseline established from real measurements.** The `peak_rss_mb`
  value in `rss-100k.json` is derived from post-migration nightly
  runs, not a placeholder.
- **10 consecutive nightly greens.** The nightly bench must pass 10
  times in a row at the configured threshold with no manual
  intervention.
- **Flake rate < 5%.** Over the last 20 advisory runs, no more than 1
  false positive (a failure not attributable to a genuine regression).
- **Mean stability.** Across the last 10 nightly runs, standard
  deviation / mean <= 0.05 (same criterion as DIS-8.b).

### 8.2 Promotion procedure

Matches DIS-8.b Section 5 and Section 6:

1. **Workflow-file PR:** remove `continue-on-error: true`, keep
   threshold and notification steps unchanged.
2. **Bake window:** 7 calendar days or 5 consecutive green nightlies
   at the tightened posture, whichever is later.
3. **Admin action:** add `RSS flist regression` (the job display name)
   to the required checks in branch protection.
4. **Rollback procedure:** admin removes the check from required,
   restores `continue-on-error: true`, diagnoses flake, re-enters
   pre-conditions from scratch.

### 8.3 Notification changes on promotion

When the check is promoted to required:

- PR comments become optional (the red check already blocks merge).
  However, keeping the comment provides actionable detail that the
  check annotation alone does not.
- Nightly issue creation remains active as a signal for regressions
  that enter master via admin-merge escapes.
- The `::warning::` yellow zone becomes more important as an early
  warning before the hard gate trips.

## 9. Integration with existing CI patterns

### 9.1 Pattern alignment

The following table maps each existing bench workflow's notification
approach to the RSS bench design:

| Pattern | Cold-start (DIS-8.a) | Concurrency (D10K-7) | Drain (DPC-8) | RSS (this spec) |
|---------|---------------------|---------------------|---------------|-----------------|
| `GITHUB_STEP_SUMMARY` | Yes | Yes | Yes | Yes |
| `::error::` annotation | Yes | Yes | No | Yes |
| `::warning::` annotation | No | No | No | Yes (yellow zone) |
| PR comment | No | No | No | Yes |
| Nightly issue | No | No | No | Yes |
| Artifact upload | JSON | JSON | Bencher text | JSON |
| `continue-on-error` | Yes | Yes | Yes | Yes |

The RSS bench is the first workflow to use PR comments and nightly issue
creation. If this pattern proves effective, the same mechanism can be
retrofitted to the cold-start and concurrency benches.

### 9.2 Cron scheduling

Existing nightly cron schedule:

| Workflow | Cron (UTC) |
|----------|-----------|
| Coverage | 03:17 |
| Cold-start bench | 03:17 |
| Concurrency bench | 04:37 |
| Drain bench | 05:47 |
| **RSS bench** | **06:17** |

The RSS bench is scheduled at 06:17 UTC, offset by 30 minutes from the
drain bench to avoid runner contention.

### 9.3 Path filter alignment

The RSS bench triggers on PR paths that overlap with the daemon benches
in the `crates/core/src/session/` subtree. This is intentional - session
changes can affect both daemon latency and RSS. The path filter list from
RSS-12.a Section 7.1 is unchanged.

## 10. Workflow permissions summary

```yaml
permissions:
  contents: read
  pull-requests: write
  issues: write
```

- `contents: read` - checkout, read baseline JSON.
- `pull-requests: write` - post/update PR comments on regression.
- `issues: write` - create/update regression issues on nightly failures.

## 11. Step ordering in the workflow

The notification steps slot into the existing RSS-12.a workflow structure
after the assertion step (step 10 in RSS-12.a Section 7.3):

1-9. (Existing steps per RSS-12.a: checkout through measure RSS.)
10. **Compute median and assert.** Sets output variables `MEDIAN_MB`,
    `BASELINE_MB`, `CEILING_MB`, `DELTA_MB`, `DELTA_PCT`, `PASS`.
    Exits non-zero on regression.
11. **Generate step summary.** Writes the markdown table to
    `GITHUB_STEP_SUMMARY`. Runs `if: always()`.
12. **Compose alert body.** Assembles `COMMENT_BODY` and `ISSUE_BODY`
    environment variables from step 10 outputs. Runs
    `if: failure()`.
13. **Post or update PR comment.** Runs
    `if: failure() && github.event_name == 'pull_request'`.
14. **Open or update regression issue.** Runs
    `if: failure() && github.event_name != 'pull_request'`.
15. **Stop daemon.** Runs `if: always()`. (Existing step.)
16. **Upload results.** Runs `if: always()`. (Existing step.)

## 12. Risks and mitigations

| Risk | Mitigation |
|------|-----------|
| PR comment noise on legitimate regressions | Comment includes "What to check" guidance so the author can self-triage. No comment on green. |
| Issue spam from consecutive nightly failures | Deduplication: append to existing open `rss-regression` issue instead of creating new ones. |
| Stale open issues after fix lands | The nightly run that first passes after a regression closes the issue automatically (future extension) or maintainer closes manually. |
| `actions/github-script` version drift | Pin to `@v9` with hash; update alongside other action pins in the repo. |
| `pull-requests: write` permission too broad | Standard permission used by `labeler.yml` and `dependency-review.yml`. No secrets exposed. |
| False-positive PR comments erode trust | 10% threshold is tuned conservatively. Yellow-zone warnings surface near-misses without full alerts. |
| Runner variance inflates RSS on schedule runs | Warm-up run + 3 samples + median selection (per RSS-12.a). Schedule runs have identical methodology to PR runs. |

## 13. Cross-references

- RSS-12.a workflow spec: `docs/design/rss-12a-ci-rss-regression-workflow.md`.
- RSS-12.b required-check wiring: `docs/design/dis-8-b-required-check-wiring.md`
  (used as DIS-8.b template; RSS equivalent follows same structure).
- PR #5006: RSS-12.b merged, recommends advisory-only posture.
- DIS-8.a cold-start bench: `.github/workflows/bench-daemon-coldstart.yml`.
- Labeler workflow (PR comment permission precedent):
  `.github/workflows/labeler.yml`.
- Dependency review (PR comment permission precedent):
  `.github/workflows/dependency-review.yml`.
- Memory note: `[[project_rss_3_11x_upstream]]`.
