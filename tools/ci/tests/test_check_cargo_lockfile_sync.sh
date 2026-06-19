#!/usr/bin/env bash
# test_check_cargo_lockfile_sync.sh - unit tests for
# check_cargo_lockfile_sync.sh.
#
# Builds mutated copies of `.github/workflows/cargo-lockfile-sync.yml`
# under a tempdir, points the gate at each one via
# OC_RSYNC_LOCKFILE_SYNC_WORKFLOW, and asserts the expected exit code for
# each case. This catches regressions in the gate's matcher (e.g., a
# future refactor that silently stops requiring the diff-detection step,
# or one that mis-classifies the offline validation step as the refresh
# step).
#
# Each fixture starts from the real workflow and strips a single load-
# bearing piece so we exercise one assertion failure at a time. A fixture
# that fails to trigger the corresponding failure means the gate is no
# longer enforcing that piece.

set -uo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
target="${script_dir}/../check_cargo_lockfile_sync.sh"
repo_root=$(cd "${script_dir}/../../.." && pwd)
real_workflow="${repo_root}/.github/workflows/cargo-lockfile-sync.yml"

if [[ ! -r "${target}" ]]; then
    echo "FAIL: cannot locate check_cargo_lockfile_sync.sh at ${target}" >&2
    exit 2
fi

if [[ ! -r "${real_workflow}" ]]; then
    echo "FAIL: cannot locate workflow at ${real_workflow}" >&2
    exit 2
fi

if ! command -v python3 >/dev/null 2>&1; then
    echo "SKIP: python3 unavailable" >&2
    exit 0
fi

if ! python3 -c "import yaml" >/dev/null 2>&1; then
    echo "SKIP: python3 PyYAML unavailable" >&2
    exit 0
fi

tmp=$(mktemp -d)
trap 'rm -rf "${tmp}"' EXIT

failures=0
case_count=0

# Run the gate against a fixture workflow file and assert exit code.
#
# $1 - human-readable label
# $2 - expected exit code (0 = pass, 1 = violation)
# $3 - fixture workflow path
run_case() {
    local label="$1" expected="$2" fixture="$3"
    case_count=$((case_count + 1))
    OC_RSYNC_LOCKFILE_SYNC_WORKFLOW="${fixture}" \
        bash "${target}" >"${tmp}/out" 2>"${tmp}/err"
    local actual=$?
    if [[ "${actual}" -ne "${expected}" ]]; then
        echo "FAIL ${label}: expected exit ${expected}, got ${actual}" >&2
        sed 's/^/  out: /' "${tmp}/out" >&2
        sed 's/^/  err: /' "${tmp}/err" >&2
        failures=$((failures + 1))
    else
        echo "ok ${label} (exit ${actual})"
    fi
}

# Copy the real workflow into the fixture tempdir and apply a `sed`
# mutation. The python helper makes it easy to surgically drop a key from
# the YAML without having to maintain a separate fixture file per case.
mutate_workflow() {
    local out="$1"
    shift
    local script="$*"
    python3 - "${real_workflow}" "${out}" <<PY
import sys
import yaml

src, dst = sys.argv[1], sys.argv[2]
with open(src, "r", encoding="utf-8") as fh:
    doc = yaml.safe_load(fh)

${script}

with open(dst, "w", encoding="utf-8") as fh:
    yaml.safe_dump(doc, fh, sort_keys=False)
PY
}

# -------- Case 1: the unmodified workflow passes.
cp "${real_workflow}" "${tmp}/case1.yml"
run_case "real workflow passes" 0 "${tmp}/case1.yml"

# -------- Case 2: dropping the Cargo.toml pull_request path fails.
mutate_workflow "${tmp}/case2.yml" "
on_block = doc.get('on', doc.get(True))
on_block['pull_request']['paths'] = [
    p for p in on_block['pull_request']['paths']
    if p not in ('Cargo.toml', 'crates/**/Cargo.toml')
]
"
run_case "missing Cargo.toml + crates/** triggers fail" 1 "${tmp}/case2.yml"

# -------- Case 3: dropping contents: write permission fails.
mutate_workflow "${tmp}/case3.yml" "
doc['permissions'] = {'contents': 'read'}
"
run_case "permissions.contents != 'write' fails" 1 "${tmp}/case3.yml"

# -------- Case 4: removing the cargo update --workspace refresh fails.
mutate_workflow "${tmp}/case4.yml" "
job = next(iter(doc['jobs'].values()))
job['steps'] = [
    s for s in job['steps']
    if not (
        isinstance(s, dict)
        and 'run' in s
        and 'cargo update --workspace' in s['run']
        and '--offline' not in s['run']
    )
]
"
run_case "removing cargo update --workspace refresh fails" 1 "${tmp}/case4.yml"

# -------- Case 5: removing the offline validation step fails.
mutate_workflow "${tmp}/case5.yml" "
job = next(iter(doc['jobs'].values()))
job['steps'] = [
    s for s in job['steps']
    if not (
        isinstance(s, dict)
        and 'run' in s
        and 'cargo update' in s['run']
        and '--offline' in s['run']
    )
]
"
run_case "removing cargo update --offline fails" 1 "${tmp}/case5.yml"

# -------- Case 6: removing the diff-detection step fails.
mutate_workflow "${tmp}/case6.yml" "
job = next(iter(doc['jobs'].values()))
job['steps'] = [
    s for s in job['steps']
    if not (
        isinstance(s, dict)
        and 'run' in s
        and 'git diff --quiet' in s['run']
        and 'Cargo.lock' in s['run']
    )
]
"
run_case "removing git diff detection fails" 1 "${tmp}/case6.yml"

# -------- Case 7: removing the conditional push step fails.
mutate_workflow "${tmp}/case7.yml" "
job = next(iter(doc['jobs'].values()))
job['steps'] = [
    s for s in job['steps']
    if not (
        isinstance(s, dict)
        and 'run' in s
        and 'git push' in s['run']
    )
]
"
run_case "removing conditional git push fails" 1 "${tmp}/case7.yml"

# -------- Case 8: dropping the first-party PR guard fails.
mutate_workflow "${tmp}/case8.yml" "
job = next(iter(doc['jobs'].values()))
job.pop('if', None)
"
run_case "dropping first-party PR + bot-author guards fails" 1 "${tmp}/case8.yml"

# -------- Case 9: a malformed YAML file is reported as a parse failure.
echo ': : :' >"${tmp}/case9.yml"
run_case "malformed YAML is rejected" 1 "${tmp}/case9.yml"

# -------- Case 10: a missing workflow file exits with environment error.
case_count=$((case_count + 1))
OC_RSYNC_LOCKFILE_SYNC_WORKFLOW="${tmp}/does-not-exist.yml" \
    bash "${target}" >"${tmp}/out" 2>"${tmp}/err"
actual=$?
if [[ "${actual}" -ne 2 ]]; then
    echo "FAIL missing workflow should exit 2: got ${actual}" >&2
    sed 's/^/  err: /' "${tmp}/err" >&2
    failures=$((failures + 1))
else
    echo "ok missing workflow exits 2 (env error)"
fi

echo
echo "ran ${case_count} case(s); ${failures} failure(s)"
if [[ "${failures}" -ne 0 ]]; then
    exit 1
fi
exit 0
