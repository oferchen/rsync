#!/usr/bin/env sh
# run_daemon_concurrency_bench.sh - stable CI entry point for the
# D10K-1 daemon concurrency bench harness.
#
# Safe to invoke from any CI job:
#   - Default behavior is to skip with exit 0 unless OC_RSYNC_D10K_BENCH=1.
#   - Skips gracefully (exit 0) on non-Linux runners, when upstream rsync
#     is not on PATH, or when the oc-rsync release binary is missing.
#   - Set OC_RSYNC_D10K_BENCH_REQUIRED=1 to turn skips into hard failures
#     (used by the D10K-7 release-qualification cell where prerequisites
#     are guaranteed).
#
# Context:
#   - Underlying harness: scripts/benchmark_daemon_concurrency.sh.
#   - Memory note: project_daemon_10k_conn_ceiling (~10K concurrent-conn
#     ceiling on the thread-per-conn daemon model).
#   - Related cap test: DMC-2 (#2799, completed) - daemon admission-cap
#     integration test for --max-connections.
#
# Knobs honoured (all optional):
#   D10K_N            Parallel client count. Default: 1000.
#                     Use 5000 / 10000 for the D10K-2..4 ceiling probes.
#   D10K_MAX_CONNS    Daemon --max-connections cap. Default: N + 16.
#   D10K_PORT         TCP port for the daemon. Default: 28840.

set -eu

workspace_root=$(cd "$(dirname "$0")/../.." && pwd)
script="${workspace_root}/scripts/benchmark_daemon_concurrency.sh"

skip_or_fail() {
    msg=$1
    printf '[run_daemon_concurrency_bench] skipped: %s\n' "$msg"
    if [ "${OC_RSYNC_D10K_BENCH_REQUIRED:-0}" = "1" ]; then
        exit 2
    fi
    exit 0
}

if [ ! -f "$script" ]; then
    printf 'daemon concurrency bench script missing: %s\n' "$script" >&2
    exit 2
fi

if [ "${OC_RSYNC_D10K_BENCH:-0}" != "1" ]; then
    skip_or_fail "OC_RSYNC_D10K_BENCH is not set to 1 (opt-in only)"
fi

uname_s=$(uname -s 2>/dev/null || echo unknown)
if [ "$uname_s" != "Linux" ]; then
    skip_or_fail "Linux-only (uname=${uname_s}); needs /proc/\$pid/status"
fi

if ! command -v rsync >/dev/null 2>&1; then
    skip_or_fail "upstream rsync client not on PATH"
fi

oc_rsync_bin="${OC_RSYNC:-${workspace_root}/target/release/oc-rsync}"
if [ ! -x "$oc_rsync_bin" ]; then
    skip_or_fail "oc-rsync release binary missing at $oc_rsync_bin (run: cargo build --release)"
fi
OC_RSYNC="$oc_rsync_bin"
export OC_RSYNC

bench_args=""
if [ -n "${D10K_N:-}" ]; then
    bench_args="$bench_args --conns ${D10K_N}"
fi
if [ -n "${D10K_MAX_CONNS:-}" ]; then
    bench_args="$bench_args --max-conns ${D10K_MAX_CONNS}"
fi
if [ -n "${D10K_PORT:-}" ]; then
    bench_args="$bench_args --port ${D10K_PORT}"
fi

printf '[run_daemon_concurrency_bench] launching %s%s\n' "$script" "${bench_args:+ ${bench_args}}"
# shellcheck disable=SC2086
exec sh "$script" $bench_args
