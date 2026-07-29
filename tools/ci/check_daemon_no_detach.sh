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

# Lower this when a migration slice converts call sites to the guarded helpers.
# It must never be raised.
PENDING_CEILING=77

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

if [ "${violations}" -ne 0 ]; then
    printf '\n%s violation(s) found.\n' "${violations}" >&2
    exit 1
fi

printf '\nNo violations found.\n'
