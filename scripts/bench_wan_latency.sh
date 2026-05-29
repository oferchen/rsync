#!/usr/bin/env bash
# MIF-7: WAN latency benchmark - measures transfer time improvement
# from MSG_INFO frame coalescing at simulated RTT levels.
#
# Runs inside the rsync-profile container with tc netem on loopback.
# Compares upstream rsync client vs oc-rsync client pushing to
# an upstream rsync daemon. This isolates the client-side framing
# improvement from the MIF-5 coalescing.

set -euo pipefail

UPSTREAM_RSYNC="/usr/bin/rsync"
OC_RSYNC="/workspace/target/release/oc-rsync"
DAEMON_PORT=18895
NUM_FILES=1000
RUNS=7
FIXTURE_DIR="/tmp/mif7-fixture"
DEST_DIR="/tmp/mif7-dest"
CONF_FILE="/tmp/mif7-rsyncd.conf"
RESULTS_FILE="/tmp/mif7-results.csv"

cleanup() {
    tc qdisc del dev lo root 2>/dev/null || true
    pkill -f "mif7-rsyncd.conf" 2>/dev/null || true
    sleep 0.3
}

trap cleanup EXIT

echo "=== MIF-7: WAN Latency Coalescing Benchmark ==="
echo "Date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "Upstream rsync: $($UPSTREAM_RSYNC --version 2>&1 | head -1)"
echo "OC-rsync: $($OC_RSYNC --version 2>&1 | head -1)"
echo "Files: $NUM_FILES, Runs per config: $RUNS"
echo ""

# --- Create test fixture ---
echo "Creating $NUM_FILES-file test fixture..."
rm -rf "$FIXTURE_DIR"
mkdir -p "$FIXTURE_DIR"
for i in $(seq 1 $NUM_FILES); do
    size=$(( 100 + (i * 1900 / NUM_FILES) ))
    dd if=/dev/urandom of="$FIXTURE_DIR/file_$(printf '%04d' $i).dat" \
       bs=1 count=$size 2>/dev/null
done
TOTAL_SIZE=$(du -sh "$FIXTURE_DIR" | cut -f1)
echo "Fixture: $NUM_FILES files, $TOTAL_SIZE total"

# --- Configure upstream rsync daemon ---
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

# --- Header for CSV ---
echo "rtt_ms,client,run,elapsed_s" > "$RESULTS_FILE"

start_daemon() {
    pkill -f "mif7-rsyncd.conf" 2>/dev/null || true
    sleep 0.3
    rm -rf "$DEST_DIR" && mkdir -p "$DEST_DIR" && chmod 777 "$DEST_DIR"
    $UPSTREAM_RSYNC --daemon --config="$CONF_FILE" --no-detach &
    DAEMON_PID=$!
    sleep 0.5
    if ! ss -tlnp | grep -q "$DAEMON_PORT"; then
        echo "ERROR: daemon not listening on port $DAEMON_PORT"
        exit 1
    fi
}

# --- Benchmark function ---
run_bench() {
    local rtt_ms=$1
    local client_label=$2
    local client_bin=$3
    local run_num=$4

    rm -rf "$DEST_DIR"/*
    sync

    local start end elapsed rc=0
    start=$(date +%s%N)
    "$client_bin" -a --itemize-changes \
        "$FIXTURE_DIR/" "rsync://127.0.0.1:${DAEMON_PORT}/bench/" \
        >/dev/null 2>&1 || rc=$?
    end=$(date +%s%N)
    elapsed=$(awk "BEGIN{printf \"%.6f\", ($end - $start) / 1000000000}")

    if [ "$rc" -ne 0 ]; then
        printf "  [WARN] %s exit=%d at RTT=%dms run=%d\n" \
            "$client_label" "$rc" "$rtt_ms" "$run_num" >&2
    fi

    echo "$rtt_ms,$client_label,$run_num,$elapsed" >> "$RESULTS_FILE"
    printf "  RTT=%3dms  %-16s  run=%d  time=%ss\n" \
        "$rtt_ms" "$client_label" "$run_num" "$elapsed"
}

# --- Run benchmarks for each RTT ---
for RTT in 0 50 100 200; do
    echo "--- RTT = ${RTT}ms ---"

    # Fresh daemon for each RTT level
    start_daemon

    if [ "$RTT" -gt 0 ]; then
        tc qdisc add dev lo root netem delay $((RTT / 2))ms
        sleep 0.2
    fi

    for RUN in $(seq 1 $RUNS); do
        run_bench "$RTT" "upstream" "$UPSTREAM_RSYNC" "$RUN"
        run_bench "$RTT" "oc-rsync" "$OC_RSYNC" "$RUN"
    done

    if [ "$RTT" -gt 0 ]; then
        tc qdisc del dev lo root
        sleep 0.2
    fi

    kill $DAEMON_PID 2>/dev/null || true
    wait $DAEMON_PID 2>/dev/null || true
    echo ""
done

echo "=== Raw Results ==="
cat "$RESULTS_FILE"
echo ""

echo "=== Summary (median of $RUNS runs) ==="
echo ""
printf "%-7s  %-16s  %10s\n" "RTT" "Client" "Median(s)"
printf "%-7s  %-16s  %10s\n" "-------" "----------------" "----------"

MEDIAN_ROW=$(( (RUNS + 1) / 2 ))

for RTT in 0 50 100 200; do
    for CLIENT in "upstream" "oc-rsync"; do
        median=$(grep "^${RTT},${CLIENT}," "$RESULTS_FILE" \
                 | cut -d',' -f4 | sort -n \
                 | awk "NR==${MEDIAN_ROW}{print}")
        printf "%-7s  %-16s  %10s\n" "${RTT}ms" "$CLIENT" "$median"
    done
done

echo ""
echo "=== Delta (positive = oc-rsync faster) ==="
echo ""

for RTT in 0 50 100 200; do
    up_med=$(grep "^${RTT},upstream," "$RESULTS_FILE" \
             | cut -d',' -f4 | sort -n \
             | awk "NR==${MEDIAN_ROW}{print}")
    oc_med=$(grep "^${RTT},oc-rsync," "$RESULTS_FILE" \
             | cut -d',' -f4 | sort -n \
             | awk "NR==${MEDIAN_ROW}{print}")
    if [ -n "$up_med" ] && [ -n "$oc_med" ]; then
        pct=$(awk "BEGIN{printf \"%.1f\", (($up_med - $oc_med) / $up_med) * 100}")
        printf "RTT=%3dms:  upstream=%ss  oc-rsync=%ss  delta=%s%%\n" \
            "$RTT" "$up_med" "$oc_med" "$pct"
    fi
done

echo ""
echo "Done. CSV at $RESULTS_FILE"
