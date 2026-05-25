# MDF-8 - filter-decision diff harness

Companion spec for `scripts/mdf_8_filter_diff_harness.sh`. The harness
turns the MDF-7 complex `.rsync-filter` fixture into a comparable
upstream-vs-oc-rsync signal so MDF-* fix PRs can be graded by how much
filter divergence they remove.

## 1. Scope

- Run upstream rsync with `--debug=FILTER1,2,3,4 --dry-run` against
  `tests/fixtures/filter-rules/mdf-7-complex/source/`.
- Run oc-rsync with the same switches against the same fixture.
- Capture only filter-decision log lines on stderr (`[FILTER]` upstream,
  `[Filter]` in oc-rsync's diagnostic emitter).
- Normalise both streams to a stable form, diff them, and report the
  line count.

The harness does not transfer data (`--dry-run`) and does not exercise
non-filter code paths. It is a tracer comparator, not a transfer
correctness check; the latter is MDF-2 and tracked separately.

## 2. What is compared

Default mode (no `--strict`):

- Level-1-shaped lines only: messages matching the `excluding` or
  `including` keywords after lowercasing. This is the subset both
  binaries are documented to emit today (see
  `docs/user/filter-rules-status.md`).
- Everything else (transfer summaries, timings, `sending incremental
  file list`, `total size is N`, role banners) is dropped by the
  initial `grep '[FILTER'` / `grep '[Filter]'` filter.

`--strict` mode additionally retains level-2+ lines (rule-load echoes,
per-decision traces). It exists so a future MDF task can flip the
harness to required-check status once oc-rsync wires those levels
through. The strict diff is expected to be large today.

## 3. Normalisation rules

The two binaries produce equivalent semantic information in slightly
different surface forms. Normalisation strips known cosmetic
differences without losing semantic content:

- Absolute fixture path -> relative form. CI workspaces live under
  `/home/runner/work/...`; local checkouts live elsewhere. Both must
  reduce to the same string.
- Destination-root prefixes (`/tmp/mdf-8/upstream-dest/`,
  `/tmp/mdf-8/oc-rsync-dest/`) are stripped likewise.
- Leading `[role=version]` tags (oc-rsync error-message convention,
  not present in upstream) are removed.
- ANSI colour escapes are removed.
- Lines are lowercased so `[FILTER]` vs `[Filter]` and
  `Excluding` vs `excluding` do not contribute false diff noise.
- Output is sorted with `LC_ALL=C sort -u`. Filter order is
  deterministic per-binary but differs across binaries (different
  rule-evaluation walk orders); set-equality is the meaningful
  property at this stage.

A future tightening pass can switch to ordered diff once both binaries
agree on traversal order.

## 4. Expected vs known-divergent output

Today's divergences (do not flag as regressions):

- oc-rsync's `--debug=FILTER` only wires level 1 (audit MDF-9, doc
  `docs/user/filter-rules-status.md`). Levels 2-4 are accepted as
  argument values but do not produce additional log lines. The
  default-mode harness therefore restricts comparison to level-1
  output.
- Several MDF-1 audit findings (#2895..#2900) are open. The diff line
  count will be non-zero until those ship.

Expected after MDF-2..MDF-6 close:

- Default-mode diff line count: 0.
- `--strict` mode diff line count: still non-zero pending the
  level-2/3/4 wiring task.

## 5. CI wiring

`.github/workflows/mdf-8-filter-diff.yml` runs the harness under
`workflow_dispatch` only. The job:

1. Builds oc-rsync in release mode.
2. Installs upstream rsync via apt.
3. Invokes the harness with default arguments.
4. Uploads `/tmp/mdf-8-diff/diff.txt` as an artifact.

The workflow is advisory (`continue-on-error: true`). A failing diff
does not block merges; it shows up in the workflow summary so MDF-* PR
authors can grade their fix. The follow-up task to promote the
workflow to required-check is gated on default-mode diff hitting zero
and staying there for one release cycle.

## 6. Acceptance for future MDF-* fix PRs

Every PR that lands a fix for an MDF-1 audit finding (#2895..#2900)
should:

1. Run the harness locally (or trigger the workflow on the PR branch).
2. Include the `diff lines` count from the harness summary in the PR
   body, comparing pre- and post-fix.
3. The post-fix count must be strictly less than the pre-fix count.
   Regressions (count increases) block merge.
4. When all MDF-1 audit findings close and the default-mode count
   reaches zero, the harness flips to required-check and the
   acceptance bar moves to "count must remain at zero".
