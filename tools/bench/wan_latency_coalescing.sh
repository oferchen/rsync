#!/usr/bin/env bash
# MIF-7: WAN latency benchmark for MSG_INFO frame coalescing.
#
# Measures wall-clock transfer time improvement from frame coalescing
# under simulated WAN conditions using tc netem on loopback.
#
# Prerequisites:
#   - Linux with tc (iproute2) and netem kernel module
#   - NET_ADMIN capability if running in a container
#   - Upstream rsync at UPSTREAM_RSYNC path
#   - oc-rsync release binary at OC_RSYNC path
#
# Usage:
#   bash tools/bench/wan_latency_coalescing.sh
#
# Inside rsync-profile container:
#   podman exec -it --cap-add NET_ADMIN rsync-profile \
#       bash /workspace/tools/bench/wan_latency_coalescing.sh
#
# Environment variables:
#   UPSTREAM_RSYNC  - path to upstream rsync (default: /usr/bin/rsync)
#   OC_RSYNC        - path to oc-rsync binary (default: /workspace/target/release/oc-rsync)
#   NUM_FILES       - number of test files (default: 10000)
#   RUNS            - runs per configuration (default: 7)
#   DAEMON_PORT     - rsync daemon port (default: 18895)

set -euo pipefail

# --- Configuration ---
UPSTREAM_RSYNC="${UPSTREAM_RSYNC:-/usr/bin/rsync}"
OC_RSYNC="${OC_RSYNC:-/workspace/target/release/oc-rsync}"
DAEMON_PORT="${DAEMON_PORT:-18895}"
NUM_FILES="${NUM_FILES:-10000}"
RUNS="${RUNS:-7}"
RTT_LEVELS=(0 50 100 200)
PACKET_LOSS_PCT=1

FIXTURE_DIR="/tmp/mif7-coalescing-fixture"
DEST_DIR="/tmp/mif7-coalescing-dest"
CONF_FILE="/tmp/mif7-coalescing-rsyncd.conf"
RESULTS_FILE="/tmp/mif7-coalescing-results.csv"

# --- Preflight checks ---
check_prerequisites() {
    local failed=0

    if ! command -v tc &>/dev/null; then
        echo "ERROR: tc (iproute2) not found. Install iproute2." >&2
        failed=1
    fi

    if ! modprobe -n sch_netem 2>/dev/null; then
        echo "WARNING: netem kernel module may not be available." >&2
    fi

    if [ ! -x "$UPSTREAM_RSYNC" ]; then
        echo "ERROR: upstream rsync not found at $UPSTREAM_RSYNC" >&2
        failed=1
    fi

    if [ ! -x "$OC_RSYNC" ]; then
        echo "ERROR: oc-rsync not found at $OC_RSYNC" >&2
        failed=1
    fi

    if [ "$(id -u)" -ne 0 ]; then
        echo "ERROR: tc netem requires root or NET_ADMIN capability." >&2
        failed=1
    fi

    if [ "$failed" -ne 0 ]; then
        echo "Aborting due to failed prerequisites." >&2
        exit 1
    fi
}

# --- Cleanup handler ---
cleanup() {
    tc qdisc del dev lo root 2>/dev/null || true
    pkill -f "$CONF_FILE" 2>/dev/null || true
    sleep 0.3
}

trap cleanup EXIT

# --- Daemon lifecycle ---
start_daemon() {
    pkill -f "$CONF_FILE" 2>/dev/null || true
    sleep 0.3
    rm -rf "$DEST_DIR" && mkdir -p "$DEST_DIR" && chmod 777 "$DEST_DIR"
    "$UPSTREAM_RSYNC" --daemon --config="$CONF_FILE" --no-detach &
    DAEMON_PID=$!
    sleep 0.5

    if ! ss -tlnp 2>/dev/null | grep -q ":${DAEMON_PORT}" && \
       ! netstat -tlnp 2>/dev/null | grep -q ":${DAEMON_PORT}"; then
        echo "ERROR: daemon not listening on port $DAEMON_PORT" >&2
        exit 1
    fi
}

stop_daemon() {
    if [ -n "${DAEMON_PID:-}" ]; then
        kill "$DAEMON_PID" 2>/dev/null || true
        wait "$DAEMON_PID" 2>/dev/null || true
    fi
}

# --- Test fixture creation ---
create_fixture() {
    echo "Creating $NUM_FILES-file test fixture (1KB each)..."
    rm -rf "$FIXTURE_DIR"
    mkdir -p "$FIXTURE_DIR"

    # Generate files in batches for speed. Each file is 1024 bytes of
    # random content to defeat deduplication and compression.
    local i
    for i in $(seq 1 "$NUM_FILES"); do
        dd if=/dev/urandom of="$FIXTURE_DIR/file_$(printf '%05d' "$i").dat" \
            bs=1024 count=1 2>/dev/null
    done

    local total_size
    total_size=$(du -sh "$FIXTURE_DIR" | cut -f1)
    echo "Fixture ready: $NUM_FILES files, $total_size total"
}

# --- Daemon configuration ---
write_daemon_config() {
    cat > "$CONF_FILE" <<RCONF
port = $DAEMON_PORT
use chroot = false
read only = false
uid = root
gid = root
[bench]
    path = $DEST_DIR
    read only = false
RCONF
}

# --- Single transfer measurement ---
# Runs one transfer and appends the result to the CSV.
#
# Arguments:
#   $1 - RTT in milliseconds
#   $2 - client label ("upstream" or "oc-rsync")
#   $3 - client binary path
#   $4 - run number
run_transfer() {
    local rtt_ms=$1
    local client_label=$2
    local client_bin=$3
    local run_num=$4

    # Clean destination for a fresh initial sync each run.
    rm -rf "${DEST_DIR:?}"/*
    sync

    local start_ns end_ns elapsed_s rc=0
    start_ns=$(date +%s%N)
    "$client_bin" -a --itemize-changes \
        "$FIXTURE_DIR/" "rsync://127.0.0.1:${DAEMON_PORT}/bench/" \
        >/dev/null 2>&1 || rc=$?
    end_ns=$(date +%s%N)
    elapsed_s=$(awk "BEGIN{printf \"%.6f\", ($end_ns - $start_ns) / 1000000000}")

    if [ "$rc" -ne 0 ]; then
        printf "  [WARN] %s exit=%d at RTT=%dms run=%d\n" \
            "$client_label" "$rc" "$rtt_ms" "$run_num" >&2
    fi

    echo "$rtt_ms,$client_label,$run_num,$elapsed_s" >> "$RESULTS_FILE"
    printf "  RTT=%3dms  %-10s  run=%d/%d  time=%ss\n" \
        "$rtt_ms" "$client_label" "$run_num" "$RUNS" "$elapsed_s"
}

# --- Netem helpers ---
apply_netem() {
    local rtt_ms=$1
    local half_rtt=$(( rtt_ms / 2 ))
    tc qdisc add dev lo root netem delay "${half_rtt}ms" loss "${PACKET_LOSS_PCT}%"
    sleep 0.2
}

remove_netem() {
    tc qdisc del dev lo root 2>/dev/null || true
    sleep 0.2
}

# --- Summary reporting ---
# Extracts the median from a sorted column of numbers.
# Argument: newline-separated list of numbers on stdin.
median_of() {
    sort -n | awk -v n="$RUNS" 'NR == int((n+1)/2) { print }'
}

print_summary() {
    echo ""
    echo "=== Summary (median of $RUNS runs) ==="
    echo ""
    printf "%-7s  %-10s  %10s\n" "RTT" "Client" "Median(s)"
    printf "%-7s  %-10s  %10s\n" "-------" "----------" "----------"

    local rtt client med
    for rtt in "${RTT_LEVELS[@]}"; do
        for client in "upstream" "oc-rsync"; do
            med=$(grep "^${rtt},${client}," "$RESULTS_FILE" \
                  | cut -d',' -f4 | median_of)
            printf "%-7s  %-10s  %10s\n" "${rtt}ms" "$client" "$med"
        done
    done

    echo ""
    echo "=== Delta (positive = oc-rsync faster) ==="
    echo ""

    local up_med oc_med pct
    for rtt in "${RTT_LEVELS[@]}"; do
        up_med=$(grep "^${rtt},upstream," "$RESULTS_FILE" \
                 | cut -d',' -f4 | median_of)
        oc_med=$(grep "^${rtt},oc-rsync," "$RESULTS_FILE" \
                 | cut -d',' -f4 | median_of)
        if [ -n "$up_med" ] && [ -n "$oc_med" ]; then
            pct=$(awk "BEGIN{printf \"%.1f\", (($up_med - $oc_med) / $up_med) * 100}")
            printf "RTT=%3dms:  upstream=%ss  oc-rsync=%ss  delta=%s%%\n" \
                "$rtt" "$up_med" "$oc_med" "$pct"
        fi
    done
}

# --- Main ---
main() {
    check_prerequisites

    echo "=== MIF-7: WAN Latency Coalescing Benchmark ==="
    echo "Date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "Upstream rsync: $("$UPSTREAM_RSYNC" --version 2>&1 | head -1)"
    echo "oc-rsync: $("$OC_RSYNC" --version 2>&1 | head -1)"
    echo "Files: $NUM_FILES x 1KB, Runs per config: $RUNS"
    echo "RTT levels: ${RTT_LEVELS[*]}ms, Packet loss: ${PACKET_LOSS_PCT}%"
    echo ""

    create_fixture
    write_daemon_config

    # CSV header
    echo "rtt_ms,client,run,elapsed_s" > "$RESULTS_FILE"

    local rtt run
    for rtt in "${RTT_LEVELS[@]}"; do
        echo ""
        echo "--- RTT = ${rtt}ms ---"

        start_daemon

        if [ "$rtt" -gt 0 ]; then
            apply_netem "$rtt"
        fi

        for run in $(seq 1 "$RUNS"); do
            run_transfer "$rtt" "upstream" "$UPSTREAM_RSYNC" "$run"
            run_transfer "$rtt" "oc-rsync" "$OC_RSYNC" "$run"
        done

        if [ "$rtt" -gt 0 ]; then
            remove_netem
        fi

        stop_daemon
    done

    echo ""
    echo "=== Raw Results ==="
    cat "$RESULTS_FILE"

    print_summary

    echo ""
    echo "Results CSV: $RESULTS_FILE"
    echo "Done."
}

main "$@"
