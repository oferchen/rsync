#!/usr/bin/env bash
# Regression guard against daemon tests that silently pass without running.
#
# Background: `RuntimeOptions::detach` defaults to true on Unix, so a daemon
# started from a test with the usual arguments calls `become_daemon()`, which
# forks. The parent of that fork is the test process, and it exits with status
# 0. The test binary therefore terminates successfully before any assertion
# runs and the harness records a pass - an unconditional `assert!(false)` in
# such a test still reports PASSED.
#
# `crates/daemon/src/tests/support.rs` closes this by appending `--no-detach`
# after the caller's arguments in `force_no_detach()`; detach flags are applied
# in argument order and the last one wins, so no caller can opt back out.
#
# What it checks:
#   1. `force_no_detach()` still appends `--no-detach` in the test support
#      module - the guard itself has not been deleted or defeated.
#   2. Daemon tests do not spawn `run_daemon` on a thread directly, which
#      bypasses the support helpers entirely.
#   3. The population of call sites still using the temporary
#      `*_pending_no_detach` helpers only ever shrinks. Those tests predate the
#      foreground requirement and are migrated in separate slices; the ceiling
#      below must be lowered as they are, never raised.
#   4. Root-level integration tests that run the daemon in-process pass
#      `--no-detach`. They live outside the daemon crate and cannot reach its
#      test-support helpers, so checks 1-3 are blind to them - and the fork
#      kills the whole test binary there just the same.
#
# Usage:
#   tools/ci/check_daemon_no_detach.sh
#
# Exit codes:
#   0 - No violations found.
#   1 - Violation detected.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

TESTS_DIR="${REPO_ROOT}/crates/daemon/src/tests"
SUPPORT="${TESTS_DIR}/support.rs"
ROOT_TESTS_DIR="${REPO_ROOT}/tests"

# Lower this when a migration slice converts call sites to the guarded helpers.
# It must never be raised.
PENDING_CEILING=77

# Root-level integration tests that call run_daemon() without --no-detach. Each
# one is vacuous on Unix: the daemon forks, this test binary is the parent, and
# it exits 0 before any assertion runs. Lower this as they are fixed; it must
# never be raised.
#
# The one remaining file is tests/integration_daemon_max_connections_cap.rs.
ROOT_DETACHING_CEILING=1

violations=0

printf '=== daemon no-detach guard ===\n'

if [ ! -f "${SUPPORT}" ]; then
    printf 'FAIL: cannot locate %s\n' "${SUPPORT}" >&2
    exit 1
fi

# 1. The guard itself must still be in place.
if ! grep -q 'arguments.push(OsString::from("--no-detach"));' "${SUPPORT}"; then
    printf 'FAIL: force_no_detach() no longer appends --no-detach in %s\n' \
        "crates/daemon/src/tests/support.rs" >&2
    printf '      Without it every daemon test forks and exits 0 before asserting.\n' >&2
    violations=$((violations + 1))
else
    printf 'OK: force_no_detach() appends --no-detach\n'
fi

# 2. No test may spawn run_daemon on a thread directly. `support.rs` is the one
#    sanctioned spawn point: every helper there routes through force_no_detach()
#    or the explicitly named temporary escape hatch.
bypasses="$(grep -rn 'thread::spawn(.*[^_]run_daemon(' "${TESTS_DIR}/chunks" \
    --include='*.rs' || true)"
if [ -n "${bypasses}" ]; then
    printf 'FAIL: daemon tests spawn run_daemon directly, bypassing the guard:\n' >&2
    printf '%s\n' "${bypasses}" >&2
    printf '      Use spawn_daemon() from crates/daemon/src/tests/support.rs.\n' >&2
    violations=$((violations + 1))
else
    printf 'OK: no direct run_daemon thread spawns in daemon tests\n'
fi

# 3. The pending-migration population may only shrink.
pending="$(grep -ro '\(start\|spawn\)_daemon_pending_no_detach(' "${TESTS_DIR}/chunks" \
    --include='*.rs' | wc -l | tr -d ' ')"
printf 'Pending-migration call sites: %s (ceiling %s)\n' "${pending}" "${PENDING_CEILING}"
if [ "${pending}" -gt "${PENDING_CEILING}" ]; then
    printf 'FAIL: %s call sites still start a detaching daemon, ceiling is %s.\n' \
        "${pending}" "${PENDING_CEILING}" >&2
    printf '      New daemon tests must use start_daemon()/spawn_daemon().\n' >&2
    violations=$((violations + 1))
fi

# 4. Root-level integration tests cannot reach the daemon crate's helpers, so
#    they carry the flag themselves or they are vacuous.
detaching=()
while IFS= read -r file; do
    [ -n "${file}" ] || continue
    grep -q -- '--no-detach' "${file}" || detaching+=("${file#"${REPO_ROOT}/"}")
done < <(grep -rl 'run_daemon(' "${ROOT_TESTS_DIR}" --include='*.rs' | sort)

printf 'Root tests running a detaching daemon: %s (ceiling %s)\n' \
    "${#detaching[@]}" "${ROOT_DETACHING_CEILING}"
if [ "${#detaching[@]}" -gt "${ROOT_DETACHING_CEILING}" ]; then
    printf 'FAIL: %s root integration test(s) call run_daemon() without --no-detach, ceiling is %s:\n' \
        "${#detaching[@]}" "${ROOT_DETACHING_CEILING}" >&2
    printf '  %s\n' "${detaching[@]}" >&2
    printf '      The daemon forks and the test binary - its parent - exits 0\n' >&2
    printf '      before any assertion runs, so the test reports a pass without\n' >&2
    printf '      proving anything. Pass --no-detach in the daemon arguments.\n' >&2
    violations=$((violations + 1))
fi

if [ "${violations}" -ne 0 ]; then
    printf '\n%s violation(s) found.\n' "${violations}" >&2
    exit 1
fi

printf '\nNo violations found.\n'
