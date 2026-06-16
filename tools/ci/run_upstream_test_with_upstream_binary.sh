#!/usr/bin/env bash
# run_upstream_test_with_upstream_binary.sh - validate whether a failing
# upstream-testsuite test is environmental (CI-only) by running the SAME
# test with the UPSTREAM rsync binary instead of oc-rsync.
#
# If upstream rsync also fails in this environment, the test is environmental
# and the failure is not an oc-rsync bug.
#
# Usage:
#   tools/ci/run_upstream_test_with_upstream_binary.sh daemon
#   tools/ci/run_upstream_test_with_upstream_binary.sh <test-name>
#
# Pre-conditions:
#   - run_upstream_testsuite.sh has been executed at least once so that
#     target/interop/upstream-src/rsync-3.4.4/rsync is built
#   - All CHECK_PROGS helpers are built (the harness's build_upstream_helpers
#     function handles this)

set -euo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
upstream_version="${UPSTREAM_VERSION:-3.4.4}"
upstream_src_dir="${workspace_root}/target/interop/upstream-src/rsync-${upstream_version}"
upstream_rsync_bin="${upstream_src_dir}/rsync"
testrun_timeout="${TESTRUN_TIMEOUT:-300}"

if [[ $# -ne 1 ]]; then
    echo "Usage: $0 <test-name>" >&2
    exit 2
fi
test_name=$1

if [[ ! -x "$upstream_rsync_bin" ]]; then
    echo "ERROR: upstream rsync binary not found at $upstream_rsync_bin" >&2
    echo "Run tools/ci/run_upstream_testsuite.sh first to build it." >&2
    exit 2
fi

test_script="${upstream_src_dir}/testsuite/${test_name}.test"
if [[ ! -f "$test_script" ]]; then
    echo "ERROR: test script not found: $test_script" >&2
    exit 2
fi

log_root="${workspace_root}/target/interop/upstream-testsuite-validation"
rm -rf "$log_root"
mkdir -p "$log_root"
scratchbase="${log_root}/scratch"
mkdir -p "$scratchbase"

scratchdir="${scratchbase}/${test_name}"
mkdir -p "$scratchdir"

# Mirror the env exports from run_upstream_testsuite.sh::run_one_test.
export TOOLDIR="$upstream_src_dir"
export srcdir="${upstream_src_dir}/testsuite"
export suitedir="${upstream_src_dir}/testsuite"
export scratchdir
export RSYNC="$upstream_rsync_bin"

echo "==> Validating upstream-testsuite test '${test_name}' with UPSTREAM rsync"
echo "==> RSYNC=$RSYNC"
echo "==> $($RSYNC --version | head -1)"
echo ""

set +e
timeout "$testrun_timeout" bash "$test_script" 2>&1 | tee "${log_root}/${test_name}.log"
status=${PIPESTATUS[0]}
set -e

echo ""
echo "==> Test '${test_name}' with upstream rsync EXIT CODE: ${status}"
case "$status" in
    0)
        echo "==> RESULT: upstream rsync PASSES → environment is OK; if oc-rsync fails here, it's our bug"
        ;;
    77)
        echo "==> RESULT: upstream rsync SKIPPED (env-precondition not met)"
        ;;
    124)
        echo "==> RESULT: upstream rsync TIMED OUT after ${testrun_timeout}s"
        ;;
    *)
        echo "==> RESULT: upstream rsync FAILS (exit ${status}) → ENVIRONMENTAL; oc-rsync failure is not a code bug"
        ;;
esac

exit "$status"
