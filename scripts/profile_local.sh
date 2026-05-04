#!/bin/bash
# Local rsync daemon profiling script
# Runs benchmarks against localhost rsyncd with Linux kernel source

set -euo pipefail

# Configuration
BENCH_DIR="/tmp/rsync-bench"
KERNEL_SRC="$BENCH_DIR/kernel-src"
DEST_DIR="$BENCH_DIR/dest"
RESULTS_DIR="$BENCH_DIR/results"
PROFILES_DIR="$BENCH_DIR/profiles"
RSYNCD_CONF="$BENCH_DIR/rsyncd.conf"
RSYNCD_PID="$BENCH_DIR/rsyncd.pid"
PORT=8873

# Binaries
UPSTREAM_RSYNC="${UPSTREAM_RSYNC:-rsync}"
OC_RSYNC="${OC_RSYNC:-/home/ofer/rsync/target/release/oc-rsync}"

# Benchmark settings
WARMUP_RUNS=5
TIMED_RUNS=10

usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Options:
    -w NUM    Warmup runs (default: $WARMUP_RUNS)
    -n NUM    Timed runs (default: $TIMED_RUNS)
    -p        Enable perf profiling
    -f        Generate flamegraph
    -s        Syscall trace (strace -c)
    -h        Show this help

Environment:
    UPSTREAM_RSYNC    Path to upstream rsync binary
    OC_RSYNC          Path to oc-rsync binary
EOF
    exit 0
}

PERF_PROFILE=false
FLAMEGRAPH=false
STRACE=false

while getopts "w:n:pfsh" opt; do
    case $opt in
        w) WARMUP_RUNS="$OPTARG" ;;
        n) TIMED_RUNS="$OPTARG" ;;
        p) PERF_PROFILE=true ;;
        f) FLAMEGRAPH=true ;;
        s) STRACE=true ;;
        h) usage ;;
        *) usage ;;
    esac
done

log() {
    echo "[$(date '+%H:%M:%S')] $*"
}

error() {
    echo "[ERROR] $*" >&2
    exit 1
}

check_prereqs() {
    command -v "$UPSTREAM_RSYNC" >/dev/null || error "upstream rsync not found"
    [[ -x "$OC_RSYNC" ]] || error "oc-rsync not found at $OC_RSYNC"
    [[ -d "$KERNEL_SRC" ]] || error "kernel source not found at $KERNEL_SRC"

    if $PERF_PROFILE; then
        command -v perf >/dev/null || error "perf not found"
    fi
    if $FLAMEGRAPH; then
        command -v flamegraph >/dev/null || error "flamegraph not found (cargo install flamegraph)"
    fi
    if $STRACE; then
        command -v strace >/dev/null || error "strace not found"
    fi
}

start_daemon() {
    if [[ -f "$RSYNCD_PID" ]] && kill -0 "$(cat "$RSYNCD_PID")" 2>/dev/null; then
        log "rsyncd already running (pid $(cat "$RSYNCD_PID"))"
        return 0
    fi

    log "Starting rsyncd on port $PORT..."
    "$UPSTREAM_RSYNC" --daemon --config="$RSYNCD_CONF" --port="$PORT" --no-detach &
    echo $! > "$RSYNCD_PID"
    sleep 1

    if ! kill -0 "$(cat "$RSYNCD_PID")" 2>/dev/null; then
        error "Failed to start rsyncd"
    fi
    log "rsyncd started (pid $(cat "$RSYNCD_PID"))"
}

stop_daemon() {
    if [[ -f "$RSYNCD_PID" ]]; then
        log "Stopping rsyncd..."
        kill "$(cat "$RSYNCD_PID")" 2>/dev/null || true
        rm -f "$RSYNCD_PID"
    fi
}

clean_dest() {
    rm -rf "$DEST_DIR"/*
}

run_transfer() {
    local binary="$1"
    local name="$2"
    local run_num="$3"

    clean_dest

    local start end duration
    start=$(date +%s.%N)
    "$binary" -a "rsync://localhost:$PORT/kernel/" "$DEST_DIR/" >/dev/null 2>&1
    end=$(date +%s.%N)
    duration=$(echo "$end - $start" | bc)

    echo "$duration"
}

run_perf_transfer() {
    local binary="$1"
    local name="$2"
    local output="$PROFILES_DIR/${name}.perf.data"

    clean_dest
    log "Recording perf profile for $name..."

    perf record -g -o "$output" -- \
        "$binary" -a "rsync://localhost:$PORT/kernel/" "$DEST_DIR/" >/dev/null 2>&1

    perf report -i "$output" --stdio > "$PROFILES_DIR/${name}.perf.txt" 2>/dev/null
    log "Profile saved to $PROFILES_DIR/${name}.perf.txt"
}

run_flamegraph_transfer() {
    local binary="$1"
    local name="$2"
    local output="$PROFILES_DIR/${name}.svg"

    clean_dest
    log "Generating flamegraph for $name..."

    flamegraph -o "$output" -- \
        "$binary" -a "rsync://localhost:$PORT/kernel/" "$DEST_DIR/" >/dev/null 2>&1

    log "Flamegraph saved to $output"
}

run_strace_transfer() {
    local binary="$1"
    local name="$2"
    local output="$PROFILES_DIR/${name}.strace.txt"

    clean_dest
    log "Running strace for $name..."

    strace -c -o "$output" -- \
        "$binary" -a "rsync://localhost:$PORT/kernel/" "$DEST_DIR/" >/dev/null 2>&1

    log "Syscall summary saved to $output"
}

benchmark() {
    local binary="$1"
    local name="$2"
    local results_file="$RESULTS_DIR/${name}.csv"

    log "Benchmarking $name..."

    # Warmup
    log "  Warmup ($WARMUP_RUNS runs)..."
    for ((i=1; i<=WARMUP_RUNS; i++)); do
        run_transfer "$binary" "$name" "warmup-$i" >/dev/null
    done

    # Timed runs
    log "  Timed runs ($TIMED_RUNS runs)..."
    echo "run,duration_seconds" > "$results_file"

    local total=0
    for ((i=1; i<=TIMED_RUNS; i++)); do
        duration=$(run_transfer "$binary" "$name" "$i")
        echo "$i,$duration" >> "$results_file"
        total=$(echo "$total + $duration" | bc)
        printf "    Run %2d: %.3fs\n" "$i" "$duration"
    done

    local avg=$(echo "scale=3; $total / $TIMED_RUNS" | bc)
    log "  Average: ${avg}s"

    # Optional profiling
    if $PERF_PROFILE; then
        run_perf_transfer "$binary" "$name"
    fi
    if $FLAMEGRAPH; then
        run_flamegraph_transfer "$binary" "$name"
    fi
    if $STRACE; then
        run_strace_transfer "$binary" "$name"
    fi

    echo "$avg"
}

main() {
    check_prereqs

    mkdir -p "$RESULTS_DIR" "$PROFILES_DIR"

    log "=== Local rsync Benchmark ==="
    log "Kernel source: $KERNEL_SRC"
    log "Warmup: $WARMUP_RUNS, Timed: $TIMED_RUNS"
    echo

    start_daemon
    trap stop_daemon EXIT

    # Get versions
    local upstream_ver=$("$UPSTREAM_RSYNC" --version | head -1)
    local oc_ver=$("$OC_RSYNC" --version | head -1)

    log "Upstream: $upstream_ver"
    log "oc-rsync: $oc_ver"
    echo

    # Run benchmarks
    upstream_avg=$(benchmark "$UPSTREAM_RSYNC" "upstream")
    echo
    oc_avg=$(benchmark "$OC_RSYNC" "oc-rsync")
    echo

    # Calculate improvement
    local diff=$(echo "scale=3; $upstream_avg - $oc_avg" | bc)
    local pct=$(echo "scale=1; ($diff / $upstream_avg) * 100" | bc)

    log "=== Results ==="
    log "Upstream rsync: ${upstream_avg}s"
    log "oc-rsync:       ${oc_avg}s"
    log "Difference:     ${diff}s (${pct}%)"

    if (( $(echo "$pct > 0" | bc -l) )); then
        log "oc-rsync is ${pct}% FASTER"
    else
        local neg_pct=$(echo "scale=1; -1 * $pct" | bc)
        log "oc-rsync is ${neg_pct}% slower"
    fi

    # Save summary
    cat > "$RESULTS_DIR/summary.csv" <<EOF
binary,avg_seconds
upstream,$upstream_avg
oc-rsync,$oc_avg
EOF

    log "Results saved to $RESULTS_DIR/"
}

main "$@"
