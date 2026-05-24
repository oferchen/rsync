#!/usr/bin/env sh
# run_russh_concurrency_bench.sh - stable CI entry point for the
# RUSSH-3 russh concurrency bench harness.
#
# Safe to invoke from any CI job:
#   - Default behavior is to skip with exit 0 unless OC_RSYNC_RUSSH_BENCH=1.
#   - Skips gracefully (exit 0) on non-Linux runners, when upstream rsync
#     is not on PATH, or when the oc-rsync release binary is missing.
#   - Set OC_RSYNC_RUSSH_BENCH_REQUIRED=1 to turn skips into hard
#     failures (used by the RUSSH-7 release-qualification cell where
#     prerequisites are guaranteed).
#
# Context:
#   - Underlying harness: scripts/benchmark_russh_concurrency.sh.
#   - Audit: docs/audit/russh-spawn-blocking-ceiling-inventory.md
#     (RUSSH-1 #2804, RUSSH-2 #2805) documents the default 512-slot
#     spawn_blocking pool and the two long-lived slots per session.
#   - Downstream RUSSH-4..7 will call this wrapper at N=64/128/256/512
#     to chart the saturation knee.
#
# Knobs honoured (all optional):
#   RUSSH_N           Parallel push session count. Default: 64.
#                     Use 128 / 256 / 512 for RUSSH-4..7 ceiling probes.
#   RUSSH_PORT        TCP port for the upstream rsyncd. Default: 28860.
#   FIXTURE_BYTES     Per-session payload size. Default: 1048576 (1 MiB).

set -eu

workspace_root=$(cd "$(dirname "$0")/../.." && pwd)
script="${workspace_root}/scripts/benchmark_russh_concurrency.sh"

skip_or_fail() {
    msg=$1
    printf '[run_russh_concurrency_bench] skipped: %s\n' "$msg"
    if [ "${OC_RSYNC_RUSSH_BENCH_REQUIRED:-0}" = "1" ]; then
        exit 2
    fi
    exit 0
}

if [ ! -f "$script" ]; then
    printf 'russh concurrency bench script missing: %s\n' "$script" >&2
    exit 2
fi

if [ "${OC_RSYNC_RUSSH_BENCH:-0}" != "1" ]; then
    skip_or_fail "OC_RSYNC_RUSSH_BENCH is not set to 1 (opt-in only)"
fi

uname_s=$(uname -s 2>/dev/null || echo unknown)
if [ "$uname_s" != "Linux" ]; then
    skip_or_fail "Linux-only (uname=${uname_s}); needs /proc/\$pid/status"
fi

if ! command -v rsync >/dev/null 2>&1; then
    skip_or_fail "upstream rsync not on PATH"
fi

oc_rsync_bin="${OC_RSYNC:-${workspace_root}/target/release/oc-rsync}"
if [ ! -x "$oc_rsync_bin" ]; then
    skip_or_fail "oc-rsync release binary missing at $oc_rsync_bin (run: cargo build --release)"
fi
OC_RSYNC="$oc_rsync_bin"
export OC_RSYNC

bench_args=""
if [ -n "${RUSSH_N:-}" ]; then
    bench_args="$bench_args --sessions ${RUSSH_N}"
fi
if [ -n "${RUSSH_PORT:-}" ]; then
    bench_args="$bench_args --port ${RUSSH_PORT}"
fi

printf '[run_russh_concurrency_bench] launching %s%s\n' "$script" "${bench_args:+ ${bench_args}}"
# shellcheck disable=SC2086
exec sh "$script" $bench_args
