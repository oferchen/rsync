#!/usr/bin/env sh
# windows_throughput_bench.sh
#
# Windows throughput benchmark for oc-rsync vs upstream MSYS2 rsync.
# Runs under MSYS2 bash on a Windows GitHub runner (or any MSYS2 shell).
#
# Generates a fixture (1 GiB single file + 10000 small files) and drives
# hyperfine to measure wall-clock time for a full local push from src/ to
# dst/ with both binaries. Output is a JSON report per scenario that
# downstream tooling can ingest.
#
# This script is intentionally best-effort: it skips with exit 0 (and a
# clear log line) if any required tool is missing so that we never block
# CI on Windows runner provisioning quirks.
#
# Required environment / tooling:
#   - bash (POSIX sh compatible; runs under MSYS2 on Windows)
#   - hyperfine (https://github.com/sharkdp/hyperfine)
#   - oc-rsync.exe (built with --release; iocp is on by default)
#   - upstream rsync (MSYS2 package `rsync`)
#
# Optional environment overrides:
#   OC_RSYNC          Path to oc-rsync.exe (default: target/release/oc-rsync.exe)
#   UPSTREAM_RSYNC    Path or name of upstream rsync (default: rsync)
#   BENCH_OUT_DIR     Directory for JSON reports (default: bench-out)
#   BENCH_WARMUP      Hyperfine warmup runs (default: 1)
#   BENCH_RUNS        Hyperfine measured runs (default: 3)
#   BENCH_LARGE_MIB   Size of the single large file in MiB (default: 1024)
#   BENCH_SMALL_COUNT Number of small files (default: 10000)
#   BENCH_SMALL_KIB   Size of each small file in KiB (default: 4)

set -eu

log() {
    printf '[windows-throughput-bench] %s\n' "$*"
}

skip() {
    log "SKIP: $*"
    exit 0
}

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

OC_RSYNC="${OC_RSYNC:-${PROJECT_ROOT}/target/release/oc-rsync.exe}"
UPSTREAM_RSYNC="${UPSTREAM_RSYNC:-rsync}"
BENCH_OUT_DIR="${BENCH_OUT_DIR:-${PROJECT_ROOT}/bench-out}"
BENCH_WARMUP="${BENCH_WARMUP:-1}"
BENCH_RUNS="${BENCH_RUNS:-3}"
BENCH_LARGE_MIB="${BENCH_LARGE_MIB:-1024}"
BENCH_SMALL_COUNT="${BENCH_SMALL_COUNT:-10000}"
BENCH_SMALL_KIB="${BENCH_SMALL_KIB:-4}"

# Tool checks (skip rather than fail to keep CI green on missing deps).
if ! command -v hyperfine >/dev/null 2>&1; then
    skip "hyperfine not on PATH; install via MSYS2 (pacman -S hyperfine) or cargo install hyperfine"
fi
if ! command -v dd >/dev/null 2>&1; then
    skip "dd not on PATH; install MSYS2 coreutils"
fi
if [ ! -x "$OC_RSYNC" ] && ! command -v "$OC_RSYNC" >/dev/null 2>&1; then
    skip "oc-rsync not found at: $OC_RSYNC"
fi
if ! command -v "$UPSTREAM_RSYNC" >/dev/null 2>&1; then
    skip "upstream rsync not on PATH ($UPSTREAM_RSYNC); install MSYS2 rsync package"
fi

mkdir -p "$BENCH_OUT_DIR"

log "oc-rsync      : $OC_RSYNC"
log "upstream rsync: $UPSTREAM_RSYNC"
log "warmup runs   : $BENCH_WARMUP"
log "measured runs : $BENCH_RUNS"
log "output dir    : $BENCH_OUT_DIR"

WORKROOT="$(mktemp -d 2>/dev/null || mktemp -d -t 'win-throughput')"
trap 'rm -rf "$WORKROOT"' EXIT INT TERM

# ----------------------------------------------------------------------------
# Fixture 1: single large file (default 1 GiB).
# ----------------------------------------------------------------------------
LARGE_SRC="$WORKROOT/large/src"
LARGE_DST_OC="$WORKROOT/large/dst_oc"
LARGE_DST_UP="$WORKROOT/large/dst_up"
mkdir -p "$LARGE_SRC" "$LARGE_DST_OC" "$LARGE_DST_UP"

log "generating large fixture: ${BENCH_LARGE_MIB} MiB"
dd if=/dev/urandom of="$LARGE_SRC/large.bin" bs=1048576 count="$BENCH_LARGE_MIB" status=none

# ----------------------------------------------------------------------------
# Fixture 2: many small files (default 10000 x 4 KiB).
# ----------------------------------------------------------------------------
SMALL_SRC="$WORKROOT/small/src"
SMALL_DST_OC="$WORKROOT/small/dst_oc"
SMALL_DST_UP="$WORKROOT/small/dst_up"
mkdir -p "$SMALL_SRC" "$SMALL_DST_OC" "$SMALL_DST_UP"

log "generating small-files fixture: ${BENCH_SMALL_COUNT} x ${BENCH_SMALL_KIB} KiB"
i=1
while [ "$i" -le "$BENCH_SMALL_COUNT" ]; do
    # Bucket into 100 sub-dirs to avoid one huge directory hot-spot.
    bucket=$(( i % 100 ))
    bdir="$SMALL_SRC/d$bucket"
    [ -d "$bdir" ] || mkdir -p "$bdir"
    dd if=/dev/urandom of="$bdir/f$i.bin" bs=1024 count="$BENCH_SMALL_KIB" status=none
    i=$(( i + 1 ))
done

run_scenario() {
    scenario="$1"
    src="$2"
    dst_oc="$3"
    dst_up="$4"
    out_json="$BENCH_OUT_DIR/${scenario}.json"

    log "running scenario: $scenario -> $out_json"

    # `--prepare` runs before every measured iteration, ensuring a clean
    # destination so we measure full-copy throughput, not no-op quick-check.
    # `--ignore-failure` is omitted: any non-zero exit should fail the bench.
    hyperfine \
        --warmup "$BENCH_WARMUP" \
        --runs "$BENCH_RUNS" \
        --export-json "$out_json" \
        --command-name "oc-rsync" \
            --prepare "rm -rf '$dst_oc'/* '$dst_oc'/.[!.]* 2>/dev/null || true" \
            "'$OC_RSYNC' -a '$src/' '$dst_oc/'" \
        --command-name "upstream-rsync" \
            --prepare "rm -rf '$dst_up'/* '$dst_up'/.[!.]* 2>/dev/null || true" \
            "'$UPSTREAM_RSYNC' -a '$src/' '$dst_up/'"
}

run_scenario "large_1gib"   "$LARGE_SRC" "$LARGE_DST_OC" "$LARGE_DST_UP"
run_scenario "small_10000"  "$SMALL_SRC" "$SMALL_DST_OC" "$SMALL_DST_UP"

log "done. reports in: $BENCH_OUT_DIR"
ls -la "$BENCH_OUT_DIR"
