#!/usr/bin/env bash
# CIM-LOCKFILE-8: shape assertion for the PR-side Cargo.lock auto-sync workflow.
#
# Why this exists
# ---------------
# `.github/workflows/cargo-lockfile-sync.yml` closes the gap between the
# `fmt + clippy` gate (which auto-regenerates `Cargo.lock` offline so a
# stale lockfile does not block CI) and the `--locked` workflows (interop,
# parallel-determinism, MSRV). On every PR that touches a workspace
# `Cargo.toml` it refreshes `Cargo.lock` and pushes the result back to the
# PR branch.
#
# That workflow is load-bearing: if its `on.pull_request.paths` triggers,
# `cargo update` step, or push-back commit shape regress, dep-drift PRs
# silently stop receiving auto-sync and resurface the original CIM-LOCKFILE
# failure mode (PRs blocked on `--locked` mismatches).
#
# Approach
# --------
# A full integration test that opens a synthetic dep-drift PR and asserts
# the workflow ran is feasible but expensive (self-PR loop, requires repo
# write tokens, races with normal CI). This gate takes the static-analysis
# path instead: it parses `cargo-lockfile-sync.yml` and asserts every load-
# bearing piece of its shape. A regression that drops the `Cargo.toml`
# trigger path, the `cargo update` step, the diff detection, or the push-
# back step fails the gate with a precise message.
#
# What it checks
# --------------
# 1. The workflow file exists and parses as valid YAML.
# 2. `on.pull_request_target.paths` (or `on.pull_request.paths`) includes
#    the root `Cargo.toml` and the `crates/**/Cargo.toml` glob (the two
#    paths that drive dep drift in practice). `pull_request_target` is
#    the production form because the workflow needs a write-capable
#    GITHUB_TOKEN against fork PRs to post the fork-fallback comment.
# 3. The job runs `cargo update --workspace` (the refresh step).
# 4. The job runs `cargo update --workspace --offline` (the offline
#    validation step that catches lockfiles that no longer resolve).
# 5. The job has a `git diff --quiet -- Cargo.lock` detection step that
#    sets a `changed` output.
# 6. The job has a conditional `git push` step that fires only when
#    `changed == 'true'`.
# 7. The workflow has `contents: write` permission (required to push
#    back to the PR branch).
# 8. The first-party PR guard is present at either the job level
#    (`head.repo.full_name == github.repository`) or the step level
#    (the push step is gated on `fork == 'false'`). The job also skips
#    the `github-actions[bot]` author so the weekly cron PR does not
#    loop on its own output.
#
# Usage
# -----
#   tools/ci/check_cargo_lockfile_sync.sh
#
# Test override
# -------------
#   OC_RSYNC_LOCKFILE_SYNC_WORKFLOW=/path/to/workflow.yml \
#     tools/ci/check_cargo_lockfile_sync.sh
# Replaces the production workflow path with a supplied file (used by the
# self-test to feed in mutated fixtures).
#
# Exit codes
#   0 - workflow shape matches every assertion.
#   1 - one or more assertions failed; stdout names the missing piece.
#   2 - environment problem (workflow file missing, python3 unavailable,
#       PyYAML unavailable).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

WORKFLOW="${OC_RSYNC_LOCKFILE_SYNC_WORKFLOW:-${REPO_ROOT}/.github/workflows/cargo-lockfile-sync.yml}"

if [[ ! -r "${WORKFLOW}" ]]; then
    echo "ERROR: workflow file not readable: ${WORKFLOW}" >&2
    exit 2
fi

if ! command -v python3 >/dev/null 2>&1; then
    echo "ERROR: python3 is required to parse the workflow YAML" >&2
    exit 2
fi

if ! python3 -c "import yaml" >/dev/null 2>&1; then
    echo "ERROR: python3 PyYAML module is required (pip install pyyaml)" >&2
    exit 2
fi

printf '=== CIM-LOCKFILE-8: cargo-lockfile-sync workflow shape gate ===\n'
printf 'Checking: %s\n\n' "${WORKFLOW}"

# All assertions live in a single python pass so we get a YAML parse once
# and report every missing piece in one shot rather than failing on the
# first mismatch. The script returns a non-zero exit if any check fails.
python3 - "${WORKFLOW}" <<'PY'
import sys
import yaml

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as fh:
    try:
        doc = yaml.safe_load(fh)
    except yaml.YAMLError as exc:
        print(f"FAIL: workflow YAML did not parse: {exc}")
        sys.exit(1)

if not isinstance(doc, dict):
    print("FAIL: workflow root is not a mapping")
    sys.exit(1)

failures = []

def fail(msg):
    failures.append(msg)
    print(f"FAIL: {msg}")

def ok(msg):
    print(f"ok: {msg}")

# PyYAML parses the bare `on:` key as the Python boolean True (YAML 1.1
# "norway problem"). Accept either spelling so the gate keeps working if
# the workflow is later rewritten with quoted "on":.
on_block = doc.get("on", doc.get(True))
if not isinstance(on_block, dict):
    fail("`on:` block missing or not a mapping")
    on_block = {}

# Accept either `pull_request` or `pull_request_target` as the trigger.
# The workflow uses `pull_request_target` so it can run with a write-capable
# GITHUB_TOKEN against fork-originated PRs (needed to push back the
# refreshed lockfile or post the fork-fallback comment); `pull_request`
# was the historical form and is still accepted in case the security
# posture is later relaxed.
pr_block = on_block.get("pull_request_target")
trigger_name = "pull_request_target"
if not isinstance(pr_block, dict):
    pr_block = on_block.get("pull_request")
    trigger_name = "pull_request"
if not isinstance(pr_block, dict):
    fail("`on.pull_request_target:` (or `on.pull_request:`) missing or not a mapping")
    pr_block = {}
    trigger_name = "pull_request_target"

paths = pr_block.get("paths") or []
if not isinstance(paths, list):
    fail(f"`on.{trigger_name}.paths` is not a list")
    paths = []

required_paths = ["Cargo.toml", "crates/**/Cargo.toml"]
for required in required_paths:
    if required in paths:
        ok(f"on.{trigger_name}.paths includes {required!r}")
    else:
        fail(f"on.{trigger_name}.paths missing required entry {required!r}")

permissions = doc.get("permissions") or {}
if isinstance(permissions, dict) and permissions.get("contents") == "write":
    ok("permissions.contents == 'write'")
else:
    fail("permissions.contents must be 'write' to push back to PR branch")

jobs = doc.get("jobs") or {}
if not isinstance(jobs, dict) or not jobs:
    fail("workflow has no jobs")
    print()
    sys.exit(1 if failures else 0)

# The workflow has one job today; tolerate future renames by scanning all
# jobs for the load-bearing pieces.
all_steps = []
job_if_clauses = []
for name, job in jobs.items():
    if not isinstance(job, dict):
        continue
    if "if" in job:
        job_if_clauses.append(str(job["if"]))
    steps = job.get("steps") or []
    if isinstance(steps, list):
        all_steps.extend(steps)

def step_run_text(step):
    if isinstance(step, dict):
        run = step.get("run")
        if isinstance(run, str):
            return run
    return ""

run_blob = "\n".join(step_run_text(s) for s in all_steps)

# Refresh step: cargo update --workspace must appear without --offline on
# at least one logical line. Match by stripping --offline matches from the
# blob and checking what remains still contains `cargo update --workspace`.
def has_cargo_update_refresh(text):
    for line in text.splitlines():
        # A logical command line that runs cargo update --workspace and
        # is NOT the --offline variant counts as the refresh step.
        if "cargo update" in line and "--workspace" in line and "--offline" not in line:
            return True
    return False

if has_cargo_update_refresh(run_blob):
    ok("step runs `cargo update --workspace` (refresh)")
else:
    fail("no step runs `cargo update --workspace` (refresh)")

if "cargo update" in run_blob and "--offline" in run_blob and "--workspace" in run_blob:
    # Confirm the --offline appears on the same line as cargo update.
    found = False
    for line in run_blob.splitlines():
        if "cargo update" in line and "--workspace" in line and "--offline" in line:
            found = True
            break
    if found:
        ok("step runs `cargo update --workspace --offline` (offline validation)")
    else:
        fail("no step runs `cargo update --workspace --offline` on a single command line")
else:
    fail("no step runs `cargo update --workspace --offline` (offline validation)")

# Diff detection: a step must run `git diff --quiet -- Cargo.lock` and
# emit a `changed` output to drive the conditional push step.
diff_step = None
for step in all_steps:
    text = step_run_text(step)
    if "git diff --quiet" in text and "Cargo.lock" in text and "changed=" in text:
        diff_step = step
        break

if diff_step is not None:
    ok("diff-detection step runs `git diff --quiet -- Cargo.lock` and sets `changed=` output")
else:
    fail("no step detects lockfile changes via `git diff --quiet -- Cargo.lock` + `changed=` output")

# Conditional push step: must be gated on `changed == 'true'` from the
# diff step and must call `git push` to the PR head ref.
push_step = None
for step in all_steps:
    if not isinstance(step, dict):
        continue
    text = step_run_text(step)
    if "git push" not in text:
        continue
    cond = str(step.get("if") or "")
    if "changed" in cond and "true" in cond:
        push_step = step
        break

if push_step is not None:
    ok("push-back step is conditional on `changed == 'true'` and runs `git push`")
else:
    fail("no conditional `git push` step gated on the diff-detection `changed` output")

# Author/repo guard: must short-circuit fork PRs (cannot push back) and
# the weekly cron bot (would loop on its own output).
#
# The first-party guard may live at the job level
# (`head.repo.full_name == github.repository`) or at the step level via a
# "Classify PR source" step whose `fork` output is then checked on the
# push step (`steps.<id>.outputs.fork == 'false'`). The latter is the
# production shape under `pull_request_target` because fork PRs still
# need to run the workflow (to leave the fork-fallback comment) - they
# just must not reach the push-back step.
job_if_blob = "\n".join(job_if_clauses)
push_step_if = str(push_step.get("if") or "") if push_step else ""

has_job_level_guard = (
    "head.repo.full_name" in job_if_blob and "github.repository" in job_if_blob
)
has_step_level_guard = "fork" in push_step_if and "false" in push_step_if

if has_job_level_guard:
    ok("job guards on first-party PRs (head.repo.full_name == github.repository)")
elif has_step_level_guard:
    ok("push-back step guards on first-party PRs (fork == 'false')")
else:
    fail(
        "first-party PR guard missing: no job-level "
        "`head.repo.full_name == github.repository` and no step-level "
        "`fork == 'false'` condition on the push step"
    )

if "github-actions[bot]" in job_if_blob:
    ok("job skips `github-actions[bot]` author (weekly cron loop guard)")
else:
    fail("job missing `github-actions[bot]` author skip (weekly cron loop guard)")

print()
if failures:
    print(f"FAILED: {len(failures)} assertion(s) did not hold.")
    print("The cargo-lockfile-sync workflow shape has regressed; restore the")
    print("missing piece(s) above so dep-drift PRs continue to receive an")
    print("auto-synced Cargo.lock. See CONTRIBUTING.md > Cargo.lock maintenance.")
    sys.exit(1)

print("PASSED: cargo-lockfile-sync workflow shape is intact.")
sys.exit(0)
PY
