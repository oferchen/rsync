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
#
# Drilldown mode (env-gated, OC_RSYNC_BENCH_DRILLDOWN=1):
#   Adds three sub-scenarios that isolate the individual IOCP hotspots
#   identified in docs/audits/iocp-sync-blocking-audit.md so future Windows
#   improvements can be attributed to specific changes:
#     - write_only_iocp:     forces full-file writes via --whole-file
#                            --inplace, isolating the IocpWriter per-IO
#                            blocking drain (audit rows #1, #4, #13).
#                            Control: native std::fs::copy via `cp`.
#     - read_only_iocp:      runs oc-rsync with --dry-run against the same
#                            1 GiB fixture so the file is mapped/read but
#                            never written, isolating the IocpReader
#                            blocking drain (audit rows #2, #3).
#     - network_only_loopback: pushes a 1 GiB file through two oc-rsync
#                            daemons over loopback rsync://, isolating the
#                            IocpSocketWriter / Reader hot paths (audit
#                            rows #8-#11) from disk completion costs.
#   Optional knobs:
#     BENCH_DAEMON_PORT      TCP port for the loopback daemon
#                            (default: 18730)

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
OC_RSYNC_BENCH_DRILLDOWN="${OC_RSYNC_BENCH_DRILLDOWN:-0}"
BENCH_DAEMON_PORT="${BENCH_DAEMON_PORT:-18730}"

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

# ----------------------------------------------------------------------------
# Drilldown mode: per-hotspot isolation (env-gated).
#
# Each sub-scenario keeps the same hyperfine harness, but swaps the
# command pair to neutralise everything except the hotspot under test.
# See docs/audits/iocp-sync-blocking-audit.md for the mapping from
# scenario name to audit row.
# ----------------------------------------------------------------------------
if [ "$OC_RSYNC_BENCH_DRILLDOWN" = "1" ]; then
    log "drilldown mode: OC_RSYNC_BENCH_DRILLDOWN=1"

    if ! command -v cp >/dev/null 2>&1; then
        skip "drilldown requires cp (MSYS2 coreutils); not on PATH"
    fi

    DRILL_ROOT="$WORKROOT/drilldown"
    mkdir -p "$DRILL_ROOT"

    # ------------------------------------------------------------------
    # write_only_iocp
    #   --whole-file forces a full sender->receiver byte stream (no
    #   delta), --inplace skips the temp-file + rename so every byte
    #   lands via IocpWriter. Control is std::fs::copy via `cp`, which
    #   bypasses oc-rsync entirely and exercises only NTFS write
    #   bandwidth. The delta between the two is the IOCP write-path
    #   overhead (audit rows #1, #4, #13).
    # ------------------------------------------------------------------
    WRITE_SRC="$DRILL_ROOT/write/src"
    WRITE_DST_OC="$DRILL_ROOT/write/dst_oc"
    WRITE_DST_CP="$DRILL_ROOT/write/dst_cp"
    mkdir -p "$WRITE_SRC" "$WRITE_DST_OC" "$WRITE_DST_CP"
    cp "$LARGE_SRC/large.bin" "$WRITE_SRC/large.bin"

    write_only_out="$BENCH_OUT_DIR/write_only_iocp.json"
    log "running scenario: write_only_iocp -> $write_only_out"
    hyperfine \
        --warmup "$BENCH_WARMUP" \
        --runs "$BENCH_RUNS" \
        --export-json "$write_only_out" \
        --command-name "oc-rsync-write" \
            --prepare "rm -rf '$WRITE_DST_OC'/* '$WRITE_DST_OC'/.[!.]* 2>/dev/null || true" \
            "'$OC_RSYNC' --whole-file --inplace -a '$WRITE_SRC/' '$WRITE_DST_OC/'" \
        --command-name "fs-copy-control" \
            --prepare "rm -rf '$WRITE_DST_CP'/* '$WRITE_DST_CP'/.[!.]* 2>/dev/null || true" \
            "cp '$WRITE_SRC/large.bin' '$WRITE_DST_CP/large.bin'"

    # ------------------------------------------------------------------
    # read_only_iocp
    #   --dry-run walks and reads the source but never writes the
    #   destination, isolating IocpReader's per-IO blocking drain
    #   (audit rows #2, #3). Control is upstream rsync with the same
    #   flag, so any delta reflects oc-rsync's read-side completion
    #   handling rather than the dry-run bookkeeping itself.
    # ------------------------------------------------------------------
    READ_DST_OC="$DRILL_ROOT/read/dst_oc"
    READ_DST_UP="$DRILL_ROOT/read/dst_up"
    mkdir -p "$READ_DST_OC" "$READ_DST_UP"

    read_only_out="$BENCH_OUT_DIR/read_only_iocp.json"
    log "running scenario: read_only_iocp -> $read_only_out"
    hyperfine \
        --warmup "$BENCH_WARMUP" \
        --runs "$BENCH_RUNS" \
        --export-json "$read_only_out" \
        --command-name "oc-rsync-read" \
            "'$OC_RSYNC' -a --dry-run '$LARGE_SRC/' '$READ_DST_OC/'" \
        --command-name "upstream-rsync-read" \
            "'$UPSTREAM_RSYNC' -a --dry-run '$LARGE_SRC/' '$READ_DST_UP/'"

    # ------------------------------------------------------------------
    # network_only_loopback
    #   Spawns two short-lived oc-rsync daemons on loopback ports and
    #   measures a push from one to the other. Source and destination
    #   are on the same disk, so disk bandwidth is symmetrical; the
    #   variable under test is the IocpSocket send/recv path (audit
    #   rows #8-#11). Control is upstream rsync running its own
    #   loopback daemon under the same shape.
    # ------------------------------------------------------------------
    NET_ROOT="$DRILL_ROOT/network"
    NET_SRC="$NET_ROOT/src"
    NET_DST_OC="$NET_ROOT/dst_oc"
    NET_DST_UP="$NET_ROOT/dst_up"
    NET_CONF_OC="$NET_ROOT/oc-rsyncd.conf"
    NET_CONF_UP="$NET_ROOT/upstream-rsyncd.conf"
    NET_PID_OC="$NET_ROOT/oc-rsyncd.pid"
    NET_PID_UP="$NET_ROOT/upstream-rsyncd.pid"
    NET_LOG_OC="$NET_ROOT/oc-rsyncd.log"
    NET_LOG_UP="$NET_ROOT/upstream-rsyncd.log"
    mkdir -p "$NET_SRC" "$NET_DST_OC" "$NET_DST_UP"
    cp "$LARGE_SRC/large.bin" "$NET_SRC/large.bin"

    OC_PORT="$BENCH_DAEMON_PORT"
    UP_PORT=$(( BENCH_DAEMON_PORT + 1 ))

    cat >"$NET_CONF_OC" <<EOF
use chroot = no
port = $OC_PORT
pid file = $NET_PID_OC
log file = $NET_LOG_OC
[bench]
    path = $NET_DST_OC
    read only = false
    uid = 0
    gid = 0
EOF

    cat >"$NET_CONF_UP" <<EOF
use chroot = no
port = $UP_PORT
pid file = $NET_PID_UP
log file = $NET_LOG_UP
[bench]
    path = $NET_DST_UP
    read only = false
    uid = 0
    gid = 0
EOF

    # Ensure no stale PID files survive a previous interrupted run.
    rm -f "$NET_PID_OC" "$NET_PID_UP"

    log "starting loopback oc-rsync daemon on 127.0.0.1:$OC_PORT"
    "$OC_RSYNC" --daemon --no-detach --config="$NET_CONF_OC" >"$NET_LOG_OC" 2>&1 &
    OC_DAEMON_PID=$!
    log "starting loopback upstream rsync daemon on 127.0.0.1:$UP_PORT"
    "$UPSTREAM_RSYNC" --daemon --no-detach --config="$NET_CONF_UP" >"$NET_LOG_UP" 2>&1 &
    UP_DAEMON_PID=$!

    stop_daemons() {
        if [ -n "${OC_DAEMON_PID:-}" ]; then
            kill "$OC_DAEMON_PID" 2>/dev/null || true
            wait "$OC_DAEMON_PID" 2>/dev/null || true
        fi
        if [ -n "${UP_DAEMON_PID:-}" ]; then
            kill "$UP_DAEMON_PID" 2>/dev/null || true
            wait "$UP_DAEMON_PID" 2>/dev/null || true
        fi
    }
    trap 'stop_daemons; rm -rf "$WORKROOT"' EXIT INT TERM

    # Poll briefly for each daemon's listening port (avoid fixed sleep).
    wait_for_port() {
        port="$1"
        attempts=0
        while [ "$attempts" -lt 50 ]; do
            if (echo >/dev/tcp/127.0.0.1/"$port") >/dev/null 2>&1; then
                return 0
            fi
            attempts=$(( attempts + 1 ))
            sleep 0.1
        done
        return 1
    }
    if ! wait_for_port "$OC_PORT"; then
        log "oc-rsync daemon failed to bind 127.0.0.1:$OC_PORT; see $NET_LOG_OC"
        stop_daemons
        exit 1
    fi
    if ! wait_for_port "$UP_PORT"; then
        log "upstream rsync daemon failed to bind 127.0.0.1:$UP_PORT; see $NET_LOG_UP"
        stop_daemons
        exit 1
    fi

    network_only_out="$BENCH_OUT_DIR/network_only_loopback.json"
    log "running scenario: network_only_loopback -> $network_only_out"
    hyperfine \
        --warmup "$BENCH_WARMUP" \
        --runs "$BENCH_RUNS" \
        --export-json "$network_only_out" \
        --command-name "oc-rsync-loopback" \
            --prepare "rm -rf '$NET_DST_OC'/* '$NET_DST_OC'/.[!.]* 2>/dev/null || true" \
            "'$OC_RSYNC' -a '$NET_SRC/' 'rsync://127.0.0.1:$OC_PORT/bench/'" \
        --command-name "upstream-rsync-loopback" \
            --prepare "rm -rf '$NET_DST_UP'/* '$NET_DST_UP'/.[!.]* 2>/dev/null || true" \
            "'$UPSTREAM_RSYNC' -a '$NET_SRC/' 'rsync://127.0.0.1:$UP_PORT/bench/'"

    stop_daemons
    trap 'rm -rf "$WORKROOT"' EXIT INT TERM
fi

log "done. reports in: $BENCH_OUT_DIR"
ls -la "$BENCH_OUT_DIR"
