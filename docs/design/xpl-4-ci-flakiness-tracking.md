# XPL-4: macOS (beta) + Windows (gnu) CI cell flakiness tracking

Status: audit + recommendations (doc-only)
Sampling window: 21 push-to-master CI runs over 2026-05-21 - 2026-05-23
Sources: `gh run list/view` against `ci.yml`, `interop-validation.yml`,
`_interop-macos.yml`, `_interop-windows.yml`, `_ci-skip-interop.yml`.

The window is bounded by `gh`'s 100-run cap; in the current high-throughput
phase that is ~10 hours of PR traffic, which is why the master push axis (one
SHA per merge) is the more useful flakiness signal. The reusable workflows
(`_interop-*`, `_ci-skip-interop`) are not directly invokable, so their stats
are surfaced through the parent `ci.yml` job rows (`interop / ...`, `interop
(macOS) / ...`, `interop (Windows, best-effort) / ...`). `interop-validation.yml`
runs as a separate top-level workflow and is reported alongside.

## 1. Per-cell stats (push-to-master, 21-22 completed runs each)

| Cell | Required? | Success % | Median wall-clock | Max | Top failure pattern |
| --- | --- | --- | --- | --- | --- |
| `Windows (stable)` | yes | 81.0% (17/21) | 7m19s | 9m00s | `concurrent_register_and_dispatch_on_overlapping_files` race (3/4) |
| `Windows ACL/xattr` | yes | 81.0% (17/21) | 10m33s | 14m42s | dead-code lint cascade (`fast_io::read_all` unused on Windows) |
| `Windows IOCP (--features iocp)` | yes | 81.0% (17/21) | 5m10s | 6m23s | same Windows dead-code cascade |
| `interop / interop with upstream rsync` | yes | 90.5% (19/21) | 7m49s | 8m05s | `parallel-threshold-trip` unexpected fails (upstream + oc) |
| `nextest (beta)` | advisory | 90.5% (19/21) | 17m17s | 18m25s | help-flag listing panic (`supported_options_list_mentions_all_help_flags`) |
| `Linux musl (stable/beta/nightly)` | stable required | 95.2% (20/21) | 15m08s | 16m00s | cascade hit (real CLI panic) |
| `Windows (beta)` / `Windows (nightly)` | advisory | 95.2% (20/21) | 7m27s / 7m14s | 11m15s / 8m28s | cascade hit (compile + CLI panic) |
| `macOS (stable/beta/nightly)` | stable required | 95.2% (20/21) | 4m26s / 3m38s / 4m04s | ~6m | cascade hit (CLI panic) |
| `interop (macOS) / interop with upstream rsync (macOS)` | yes | 95.2% (20/21) | 3m15s | 6m01s | cascade hit |
| `nextest (stable)` / `nextest (nightly)` | stable required | 95.2% (20/21) | 17m37s / 17m19s | 18m06s | cascade hit |
| `Feature flag combinations / *` (linux/macos/windows) | yes | 100% (21/21 each) | 0m54s - 5m58s | up to 7m32s | none |
| `Windows GNU cross-check` | yes | 100% (21/21) | 1m43s | 1m51s | none in window |
| `interop (Windows, best-effort) / ...` | advisory | 100% (21/21) | 4m22s | 8m18s | none in window |
| `fmt + clippy` | yes | 95.5% (21/22) | 4m05s | 4m49s | cascade hit |
| `unsafe-safety-comment-audit (informational)` | informational | 100% (21/21) | 0m07s | 0m16s | none |
| `parallel-receive-delta (dist, non-required)` | advisory | 100% (5/5) | 20m58s | 23m13s | none in window (PIP-9.f) |
| `Interop Validation` (top-level workflow) | yes | 96.0% (24/25) | n/a | n/a | one cascade-window failure |

### Push-to-master baseline numbers

* 21 completed CI runs (12 failure, 10 success) over 2026-05-21 - 2026-05-23.
* 16 of the 12 failures map to one of three real-bug cascades that hit
  multiple cells on the same SHA. Net "unique distinct cell failures" once
  duplicates are folded out is 11.
* The remaining 5 failures are the three `concurrent_register_and_dispatch_on_overlapping_files`
  hits on Windows (stable) and the two `parallel-threshold-trip` upstream interop hits
  on Linux.

### Three observed cascades

The merge train hit three cascades during the window. All three were real
regressions, not infrastructure flakes:

1. **2026-05-22 14:45 / `26294514946`** - `supported_options_list_mentions_all_help_flags`
   panic in `cli/src/frontend/tests/help.rs:72`. Took down macOS (stable/beta/nightly),
   Windows (stable/beta/nightly), nextest (beta), Linux musl. Diagnosis: real
   regression in the supported-options list.
2. **2026-05-22 23:08 - 2026-05-23 00:33 / `26316272426`, `26317715371`, `26318542552`** -
   `function 'read_all' is never used` dead-code warning on Windows
   x86_64-msvc, fatal under `-D warnings`. Took down Windows ACL/xattr +
   Windows IOCP for three consecutive merges. Diagnosis: missing `#[cfg(unix)]`
   gate; matches the `feedback_proactive_cross_platform.md` checklist.
3. **2026-05-22 09:17 - 09:49 / `26279354408`, `26280808639`** - upstream
   interop matrix saw `parallel-threshold-trip` fail on both `up:` and `oc:`
   sides against 3.0.9, 3.1.3, 3.4.1, 3.4.2. Two consecutive merges. Diagnosis:
   real wire-protocol regression in the parallel-threshold scenario; not a
   runner flake.

### Confirmed pure flakes

* `engine::parallel_apply_concurrent::concurrent_register_and_dispatch_on_overlapping_files`
  on `Windows (stable)`. Asserts `expected.load(Ordering::Relaxed) > 0` and
  races to zero when worker threads outrun the registrar. Caught at runs
  `26216426222`, `26225746638`, `26228915686` (three consecutive pre-cascade
  failures). Memory: `project_concurrent_dispatch_test_flake.md` + spin-then-yield
  fix shipped in PR #4665; SSC-1 PR #4667 added registration counter. Window
  fails were on commits prior to the fix's full reach on push CI.

## 2. Flakiness leaderboard

The window is small (~21 master pushes) so percentages move quickly; rank by
distinct-failure count after de-duplicating cascades:

**Most flaky (need attention):**

1. `Windows (stable)` - 3 unique flakes from one race
   (`concurrent_register_and_dispatch_on_overlapping_files`). This is the
   single flakiest cell.
2. `Windows ACL/xattr` - 3 real-but-Windows-only dead-code failures over
   consecutive merges. Not random, but Windows-specific compile cascades
   recur often enough to behave like flakiness from the dashboard.
3. `Windows IOCP (--features iocp)` - tied with ACL/xattr on the same
   cascade (3 consecutive). Distinct symptom: blocks `fast_io` build under
   `-D warnings`.

**99%+ reliable (in this window):**

* `Feature flag combinations / *` - 17 sub-cells, 100% across the board.
* `Windows GNU cross-check` - 100%. The compile-only check insulates it from
  the runtime flakes that bite `Windows (stable)`.
* `interop (Windows, best-effort) / ...` - 100%, but it is `continue-on-error`
  so a failure would not block. Keep on the advisory tier.
* `interop (macOS) / interop with upstream rsync (macOS)` - 100% modulo one
  cascade hit. Functionally rock-solid in steady state.
* `unsafe-safety-comment-audit (informational)` - 100%; advisory.
* `parallel-receive-delta (dist, non-required)` - 100% in 5 runs; small sample.

**XPL-4 focus (macOS beta, Windows gnu):**

* `macOS (beta)` - 95.2% (1 failure, cascade-only). Beta toolchain has not
  introduced its own flakes in the window. Behaves indistinguishably from
  `macOS (stable)`. No quarantine warranted.
* `Windows GNU cross-check` (the "Windows (gnu)" cell in this repo - we do
  `cargo check --target x86_64-pc-windows-gnu` on Ubuntu) - 100% in the
  window. The cell is compile-only on a Linux runner, so it dodges every
  Windows runtime flake. No action needed.

The real Windows-gnu vs Windows-msvc divergence is that `Windows (stable)`
and `Windows IOCP/ACL` jobs (all msvc) carry every flake while
`Windows GNU cross-check` carries none. Use this contrast as a triage hint:
if a regression shows on msvc cells but cross-check is green, it is almost
always a runtime/race issue, not a compile issue.

## 3. Failure-pattern dictionary

| Signature | Cluster | What to do |
| --- | --- | --- |
| `assertion failed: expected.load(Ordering::Relaxed) > 0` in `concurrent_register_and_dispatch_on_overlapping_files` | Windows race flake | Re-merge or empty-commit kick; long-term fix tracked under SSC-1 / `project_concurrent_dispatch_test_flake.md`. |
| `function '<name>' is never used ... could not compile '<crate>' (lib test) due to 1 previous error` on Windows | Windows-only dead-code under `-D warnings` | Add `#[cfg(unix)]` gate or `#[allow(dead_code)]` for stub. Verify with `cargo check --target x86_64-pc-windows-gnu` locally before pushing. |
| `UNEXPECTED FAIL: up:parallel-threshold-trip (fp=native)` / `oc:parallel-threshold-trip` across 3.0.9 / 3.1.3 / 3.4.1 / 3.4.2 | Interop matrix wire-protocol regression | Real bug. Triage against `crates/transfer` parallel-receive-delta wire-up (PIP-9). Do not retry. |
| `panicked at crates/cli/src/frontend/tests/help.rs:72: assertion failed: supported_options_list_mentions_all_help_flags` | CLI help-flag list out of sync | Real bug. Add the missing help flag to the supported-options test data. |
| `error: command 'cargo-nextest.exe' ... exited with code 101` after `Downloaded` lines but no failed test in summary | Windows nextest install/runner OOM (none observed in window) | Retry once; if persistent, bump runner RAM or split test groups. |
| `taiki-e/install-action` bash startup failure on Windows | Known partner-runner issue (#169) - already worked around | None; the workaround in `ci.yml` PowerShell-installs nextest directly. |
| `Cannot allocate memory` / `linker exited with signal 9` | Linker OOM (none observed in window) | Bump runner size (use larger SKU) or strip debuginfo on the failing crate. |
| `Resource not accessible by integration` / GH API 403 | Token scoping (none observed) | Re-check `permissions:` block on the workflow. Retry will not help. |
| `No space left on device` | Runner disk exhaustion (none observed) | Rerun the job. If recurring, add `swatinem/rust-cache` save-misses or prune `target/` between matrix legs. |
| `timeout-minutes: N exceeded` | Test hang or slow build (none observed; longest run was 23m13s for `parallel-receive-delta (dist)` against 30m budget) | Profile, do not blanket-bump timeouts. Investigate as a real hang first. |

## 4. Recommended weekly dashboard query

Run from the repo root. Produces per-cell success/failure counts for the
last 50 push-to-master CI runs and writes raw JSON for further slicing.

```sh
# 1. Capture the run-level list (push only, master, completed, success or failure).
gh run list --workflow=ci.yml --branch master --status completed --limit 50 \
  --json databaseId,conclusion,createdAt,displayTitle \
  | jq '[.[] | select(.conclusion=="success" or .conclusion=="failure")]' \
  > /tmp/xpl4-runs.json

# 2. Pull job-level data per run.
> /tmp/xpl4-jobs.jsonl
for id in $(jq -r '.[].databaseId' /tmp/xpl4-runs.json); do
  gh run view "$id" --json jobs \
    --jq ".jobs[] | {name: .name, conclusion: .conclusion, started: .startedAt, completed: .completedAt, runId: $id}"
done >> /tmp/xpl4-jobs.jsonl

# 3. Per-cell summary (success %, median wall-clock minutes).
jq -s '
  group_by(.name) | map({
    name: .[0].name,
    total: length,
    success: ([.[] | select(.conclusion=="success")] | length),
    failure: ([.[] | select(.conclusion=="failure")] | length),
    success_pct: (
      ([.[] | select(.conclusion=="success")] | length) * 100 /
      ([.[] | select(.conclusion=="success" or .conclusion=="failure")] | length // 1)
    ),
    median_min: (
      [.[] | select(.conclusion=="success" or .conclusion=="failure") |
        ((.completed | fromdateiso8601) - (.started | fromdateiso8601)) / 60]
      | sort | .[length/2 | floor]
    )
  }) | sort_by(.success_pct)
' /tmp/xpl4-jobs.jsonl

# 4. Failure-signature scan over the failing cells (top symptom line each).
for id in $(jq -r '.[] | select(.conclusion=="failure") | .databaseId' /tmp/xpl4-runs.json); do
  echo "=== run $id ==="
  gh run view "$id" --log-failed 2>&1 \
    | grep -iE 'UNEXPECTED FAIL|FAIL \[|panicked at|^error|could not compile' \
    | grep -vE 'shell:|CACHE_ON_FAILURE|curl|cargo\.exe|cargo'\''' \
    | head -5
done
```

Extend the window by sweeping the same query against `interop-validation.yml`
once a week; the rest of the interop cells surface through `ci.yml` rows.

## 5. Triage rules for the next sprint

**Required (fail-fast in PRs) - already correct, keep as-is:**

* `lint (fmt + clippy)`
* `nextest (stable)`
* `Windows (stable)`
* `Windows GNU cross-check`
* `Windows IOCP (--features iocp)`
* `Windows ACL/xattr`
* `macOS (stable)`
* `Linux musl (stable)`
* `interop / interop with upstream rsync`
* `interop (macOS) / interop with upstream rsync (macOS)`
* `Feature flag combinations / *` (all matrix legs)

**Advisory (continue-on-error, do not gate merge):**

* `nextest (beta)`, `nextest (nightly)`
* `Windows (beta)`, `Windows (nightly)`
* `macOS (beta)`, `macOS (nightly)`
* `Linux musl (beta)`, `Linux musl (nightly)`
* `interop (Windows, best-effort)`
* `parallel-receive-delta (dist, non-required)`
* `unsafe-safety-comment-audit (informational)`

These already carry `continue-on-error: true`. The 95.2% success rate on
`macOS (beta)` and friends comes from one cascade and zero toolchain-specific
issues - no movement needed.

**Quarantine candidates (escalate, do not just mute):**

* `engine::parallel_apply_concurrent::concurrent_register_and_dispatch_on_overlapping_files`
  - the single source of `Windows (stable)` flakes in the window. SSC-1
  fix in PR #4667 is in flight; track and verify the next 20 master pushes
  before lowering the cell from "flaky-watch" status.

**Process tightening:**

* For every red `Windows ACL/xattr` or `Windows IOCP` that prints
  `could not compile ... due to N previous error`, the immediate triage step
  is to reproduce locally with
  `cargo check --target x86_64-pc-windows-gnu --workspace`. That single
  command would have caught all three dead-code cascades in this window
  before push. Per `feedback_proactive_cross_platform.md`.
* Multi-cell same-SHA failures (all toolchain rows red on one commit) almost
  always indicate a real regression, not flakiness. Skip retry; bisect.
* Single-cell failures on `Windows (stable)` where `Windows GNU cross-check`
  passed are race or platform-runtime bugs - retry once, then file under
  `engine` if it re-trips.

## Appendix: workflow-to-cell map

```
ci.yml
 |-- lint (fmt + clippy)                                        [ubuntu]
 |-- unsafe-safety-comment-audit (informational)                [ubuntu]
 |-- nextest (stable | beta | nightly)                          [ubuntu]
 |-- Feature flag combinations / *                              [_test-features.yml]
 |-- Windows (stable | beta | nightly)                          [windows-latest, msvc]
 |-- Windows IOCP (--features iocp)                             [windows-latest, msvc]
 |-- Windows ACL/xattr                                          [windows-latest, msvc]
 |-- Windows GNU cross-check                                    [ubuntu, x86_64-pc-windows-gnu]
 |-- macOS (stable | beta | nightly)                            [macos-latest]
 |-- Linux musl (stable | beta | nightly)                       [ubuntu, x86_64-unknown-linux-musl]
 |-- interop / interop with upstream rsync                      [_interop.yml]
 |-- interop (macOS) / interop with upstream rsync (macOS)      [_interop-macos.yml]
 |-- interop (Windows, best-effort) / ...                       [_interop-windows.yml, advisory]
 |-- parallel-receive-delta (dist profile, non-required)        [ubuntu, advisory]

interop-validation.yml (standalone)
 |-- separate exit-code + behavior matrix; rolls up its own success rate
```

Reusable workflows (`_*.yml`) do not show in `gh workflow list` as their own
runs - their jobs surface under the calling row in `ci.yml`.
