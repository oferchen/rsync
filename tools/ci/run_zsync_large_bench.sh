#!/usr/bin/env sh
# run_zsync_large_bench.sh - release-qualification wrapper for the
# 10 GB sparse-VM zsync benchmark (#2082).
#
# Safe to invoke from any CI job:
#   - Default behavior is to skip with exit 0 unless OC_RSYNC_LARGE_BENCH=1.
#   - If invoked on a platform without /usr/bin/time, dd with
#     oflag=seek_bytes, or the oc-rsync release binary, the wrapper skips
#     gracefully (exit 0) rather than failing the pipeline.
#   - Set ZSYNC_LARGE_BENCH_REQUIRED=1 to turn skips into hard failures
#     (used by the release-qualification matrix where prerequisites are
#     guaranteed).
#
# This wrapper exists so the release pipeline can call a single, stable
# entry point. The underlying benchmark logic lives in
# scripts/zsync_bench_large_dataset.sh.

set -eu

workspace_root=$(cd "$(dirname "$0")/../.." && pwd)
script="${workspace_root}/scripts/zsync_bench_large_dataset.sh"

skip_or_fail() {
    msg="$1"
    printf '[run_zsync_large_bench] skipped: %s\n' "$msg"
    if [ "${ZSYNC_LARGE_BENCH_REQUIRED:-0}" = "1" ]; then
        exit 2
    fi
    exit 0
}

if [ ! -f "$script" ]; then
    printf 'zsync large-bench script missing: %s\n' "$script" >&2
    exit 2
fi

if [ "${OC_RSYNC_LARGE_BENCH:-0}" != "1" ]; then
    skip_or_fail "OC_RSYNC_LARGE_BENCH is not set to 1 (release-qualification only)"
fi

if ! command -v /usr/bin/time >/dev/null 2>&1; then
    skip_or_fail "/usr/bin/time not available; needed for peak-RSS capture"
fi

if ! command -v dd >/dev/null 2>&1; then
    skip_or_fail "dd not available; needed for sparse corpus generation"
fi

# macOS dd does not support oflag=seek_bytes, which the corpus generator
# relies on for byte-precise offsets. The release qualification matrix
# pins this benchmark to Linux runners, so we simply skip on macOS.
uname_s=$(uname -s 2>/dev/null || echo unknown)
if [ "$uname_s" = "Darwin" ]; then
    if ! dd if=/dev/zero of=/dev/null bs=1 count=0 oflag=seek_bytes 2>/dev/null; then
        skip_or_fail "macOS dd lacks oflag=seek_bytes (use Linux runner or install GNU coreutils)"
    fi
fi

oc_rsync_bin="${OC_RSYNC:-${workspace_root}/target/release/oc-rsync}"
if [ ! -x "$oc_rsync_bin" ]; then
    skip_or_fail "oc-rsync release binary missing at $oc_rsync_bin (run: cargo build --release)"
fi
OC_RSYNC="$oc_rsync_bin"
export OC_RSYNC

printf '[run_zsync_large_bench] launching %s\n' "$script"
exec sh "$script" "$@"
