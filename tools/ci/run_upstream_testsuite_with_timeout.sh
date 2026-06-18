#!/usr/bin/env bash
# run_upstream_testsuite_with_timeout.sh - run upstream's runtests.py with a
# per-test timeout wrapper.
#
# Upstream's runtests.py does not enforce a per-test timeout, so a single
# hanging test (we have seen `daemon`, `files-from`, `hardlinks` hang for
# 300s+) blocks the entire suite. This wrapper invokes runtests.py once
# per test name via `runtests.py --name <test>` under coreutils
# `timeout(1)`, classifies each outcome (PASS / FAIL / SKIP / TIMEOUT),
# and exits non-zero if any test FAILS or TIMES OUT.
#
# Per-test timeout resolution (highest precedence first):
#   1. PER_TEST_TIMEOUTS entry in upstream_testsuite_timeouts.conf
#   2. $OC_RSYNC_PER_TEST_TIMEOUT environment variable (applies to every
#      test that does not have a per-test override)
#   3. DEFAULT_TIMEOUT from upstream_testsuite_timeouts.conf (120s)
#
# Environment overrides:
#   OC_RSYNC_PER_TEST_TIMEOUT   default timeout in seconds (overrides
#                               DEFAULT_TIMEOUT for unlisted tests)
#   UPSTREAM_VERSION            upstream rsync version (default 3.5.0dev,
#                               falls back to 3.4.4)
#   UPSTREAM_SRC_DIR            absolute path to upstream rsync source tree
#                               (overrides version-derived path)
#   OC_RSYNC_BIN                path to oc-rsync binary
#   UPSTREAM_RSYNC_BIN          path to upstream rsync binary (defaults
#                               to $UPSTREAM_SRC_DIR/rsync)
#   RSYNC_TESTTMP               directory upstream uses for per-test
#                               scratch + logs (default ~/rsync/testtmp,
#                               matches upstream runtests.py)
#   TEST_NAMES                  whitespace-separated list of test names
#                               to run; default = every *.test plus
#                               *_test.py in the testsuite
#   TIMEOUT_CONFIG              path to upstream_testsuite_timeouts.conf
#                               (defaults to alongside this script)
#
# Exit status:
#   0 if every test PASSES (XFAIL/SKIP also OK)
#   1 if any test FAILS or TIMES OUT
#   2 on harness/precondition error

set -euo pipefail

# --- Resolve paths -----------------------------------------------------------

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
workspace_root=$(cd "${script_dir}/../.." && pwd)

upstream_version="${UPSTREAM_VERSION:-3.5.0dev}"
upstream_src_root="${workspace_root}/target/interop/upstream-src"
upstream_src_dir="${UPSTREAM_SRC_DIR:-}"
if [[ -z "$upstream_src_dir" ]]; then
    upstream_src_dir="${upstream_src_root}/rsync-${upstream_version}"
    # Fall back to the released branch if 3.5.0dev is not checked out.
    if [[ ! -d "$upstream_src_dir" ]]; then
        upstream_src_dir="${upstream_src_root}/rsync-3.4.4"
    fi
fi

oc_rsync_bin="${OC_RSYNC_BIN:-${workspace_root}/target/release/oc-rsync}"
if [[ "$oc_rsync_bin" != /* ]]; then
    oc_rsync_bin="${workspace_root}/${oc_rsync_bin}"
fi
upstream_rsync_bin="${UPSTREAM_RSYNC_BIN:-${upstream_src_dir}/rsync}"

testtmp_root="${RSYNC_TESTTMP:-${HOME}/rsync/testtmp}"
log_root="${workspace_root}/target/interop/upstream-testsuite-timeout"
timeout_config="${TIMEOUT_CONFIG:-${script_dir}/upstream_testsuite_timeouts.conf}"

# --- Preconditions -----------------------------------------------------------

die() {
    echo "ERROR: $*" >&2
    exit 2
}

if ! command -v timeout >/dev/null 2>&1; then
    die "coreutils 'timeout' is required; install coreutils (Linux only)."
fi

if [[ ! -d "$upstream_src_dir" ]]; then
    die "upstream source tree not found at: $upstream_src_dir
    Run tools/ci/run_upstream_testsuite.sh once to fetch and configure it,
    or set UPSTREAM_SRC_DIR to an existing tree."
fi

if [[ ! -d "${upstream_src_dir}/testsuite" ]]; then
    die "testsuite/ directory missing under: $upstream_src_dir"
fi

runtests_py="${upstream_src_dir}/runtests.py"
if [[ ! -f "$runtests_py" ]]; then
    die "runtests.py not found at: $runtests_py
    This wrapper targets the newer Python-driven upstream test framework.
    For the *.test-only shell harness, use run_upstream_testsuite.sh."
fi

if [[ ! -x "$oc_rsync_bin" ]]; then
    die "oc-rsync binary not found or not executable: $oc_rsync_bin
    Build with: cargo build --locked --release --bin oc-rsync"
fi

if [[ ! -x "$upstream_rsync_bin" ]]; then
    die "upstream rsync binary not found at: $upstream_rsync_bin
    Run tools/ci/run_upstream_testsuite.sh once to build it."
fi

# --- Load per-test timeout config -------------------------------------------

DEFAULT_TIMEOUT=120
PER_TEST_TIMEOUTS=()
if [[ -f "$timeout_config" ]]; then
    # shellcheck source=/dev/null
    source "$timeout_config"
else
    echo "warning: timeout config not found ($timeout_config); using defaults" >&2
fi

# OC_RSYNC_PER_TEST_TIMEOUT overrides DEFAULT_TIMEOUT but leaves per-test
# entries authoritative.
default_timeout="${OC_RSYNC_PER_TEST_TIMEOUT:-${DEFAULT_TIMEOUT}}"

case "$default_timeout" in
    ''|*[!0-9]*)
        die "invalid default timeout '$default_timeout'; must be a positive integer (seconds)"
        ;;
esac
if (( default_timeout <= 0 )); then
    die "default timeout must be > 0 (got $default_timeout)"
fi

resolve_timeout() {
    local name=$1
    local entry key value
    for entry in "${PER_TEST_TIMEOUTS[@]}"; do
        key=${entry%%=*}
        value=${entry#*=}
        if [[ "$key" == "$name" ]]; then
            case "$value" in
                ''|*[!0-9]*)
                    echo "warning: ignoring non-numeric per-test timeout '$entry'" >&2
                    echo "$default_timeout"
                    return
                    ;;
            esac
            echo "$value"
            return
        fi
    done
    echo "$default_timeout"
}

# --- Discover tests ----------------------------------------------------------

discover_tests() {
    local suite="${upstream_src_dir}/testsuite"
    local f base
    for f in "$suite"/*.test; do
        [[ -e "$f" ]] || continue
        base=$(basename "$f" .test)
        printf '%s\n' "$base"
    done
    for f in "$suite"/*_test.py; do
        [[ -e "$f" ]] || continue
        base=$(basename "$f" _test.py)
        printf '%s\n' "$base"
    done
}

if [[ -n "${TEST_NAMES:-}" ]]; then
    # shellcheck disable=SC2206  # intentional word-split on whitespace
    test_names=( ${TEST_NAMES} )
else
    mapfile -t test_names < <(discover_tests | sort -u)
fi

if (( ${#test_names[@]} == 0 )); then
    die "no tests discovered in ${upstream_src_dir}/testsuite"
fi

# --- Run -------------------------------------------------------------------

rm -rf "$log_root"
mkdir -p "$log_root"

passed=0
failed=0
skipped=0
xfailed=0
timed_out=0
failed_tests=()
timed_out_tests=()

run_one_test() {
    local name=$1
    local budget log_file rc test_log
    budget=$(resolve_timeout "$name")
    log_file="${log_root}/${name}.runtests.log"

    printf '==> %-40s  timeout=%ss\n' "$name" "$budget"

    # NB: do NOT pass --preserve-status: we rely on timeout(1) returning
    # 124 when the budget is exceeded so the wrapper can distinguish a
    # genuine test failure from a TIMEOUT. --kill-after gives the child
    # a 10s grace window before SIGKILL; if SIGKILL fires we see 137.
    set +e
    timeout --kill-after=10s "${budget}s" \
        python3 "$runtests_py" \
            --rsync-path="$oc_rsync_bin" \
            --name "$name" \
            >"$log_file" 2>&1
    rc=$?
    set -e

    # Surface the upstream per-test log if present. runtests.py writes
    # logs to $RSYNC_TESTTMP/<name>/test.log; we tee the harness output
    # plus the per-test log so CI users see both.
    test_log="${testtmp_root}/${name}/test.log"
    if [[ -f "$test_log" ]]; then
        {
            echo
            echo "----- per-test log: ${test_log} -----"
            cat "$test_log"
        } >>"$log_file"
    fi

    case "$rc" in
        0)
            echo "    PASS    $name"
            passed=$((passed + 1))
            ;;
        77)
            echo "    SKIP    $name"
            skipped=$((skipped + 1))
            ;;
        78)
            echo "    XFAIL   $name"
            xfailed=$((xfailed + 1))
            ;;
        124)
            # coreutils timeout: exceeded the budget
            echo "    TIMEOUT $name  (after ${budget}s; log: $log_file)"
            timed_out=$((timed_out + 1))
            timed_out_tests+=("$name")
            ;;
        137)
            # SIGKILL after --kill-after grace expired
            echo "    TIMEOUT $name  (SIGKILL after grace; log: $log_file)"
            timed_out=$((timed_out + 1))
            timed_out_tests+=("$name")
            ;;
        *)
            echo "    FAIL    $name  (exit $rc; log: $log_file)"
            failed=$((failed + 1))
            failed_tests+=("$name")
            ;;
    esac
}

echo "==> upstream testsuite (per-test timeout wrapper)"
echo "    upstream source: $upstream_src_dir"
echo "    oc-rsync:        $oc_rsync_bin"
echo "    upstream rsync:  $upstream_rsync_bin"
echo "    testtmp:         $testtmp_root"
echo "    default timeout: ${default_timeout}s"
echo "    tests to run:    ${#test_names[@]}"
echo

for name in "${test_names[@]}"; do
    run_one_test "$name"
done

# --- Summary ---------------------------------------------------------------

total=${#test_names[@]}
echo
echo "------------------------------------------------------------"
echo "  total:    $total"
echo "  passed:   $passed"
echo "  failed:   $failed"
echo "  timeout:  $timed_out"
echo "  xfail:    $xfailed"
echo "  skipped:  $skipped"
if (( ${#failed_tests[@]} )); then
    echo "  failures:"
    for t in "${failed_tests[@]}"; do
        echo "    - $t (log: ${log_root}/${t}.runtests.log)"
    done
fi
if (( ${#timed_out_tests[@]} )); then
    echo "  timeouts:"
    for t in "${timed_out_tests[@]}"; do
        echo "    - $t (log: ${log_root}/${t}.runtests.log)"
    done
fi

if (( failed > 0 || timed_out > 0 )); then
    exit 1
fi
exit 0
