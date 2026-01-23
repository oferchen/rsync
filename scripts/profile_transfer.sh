#!/bin/bash
# Profile rsync transfers to identify performance bottlenecks.
#
# This script:
# 1. Starts an upstream rsync daemon
# 2. Runs transfers with both upstream and oc-rsync
# 3. Collects timing data for various scenarios
# 4. Optionally runs with perf or flamegraph
#
# Usage:
#   ./scripts/profile_transfer.sh [--perf] [--flamegraph]
#
# Requirements:
#   - Upstream rsync built in target/interop/upstream-install/3.4.1/
#   - oc-rsync built with: cargo build --release
#   - For perf: linux-tools-common
#   - For flamegraph: cargo install flamegraph

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Paths
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
UPSTREAM="${PROJECT_ROOT}/target/interop/upstream-install/3.4.1/bin/rsync"
OC_RSYNC="${PROJECT_ROOT}/target/release/oc-rsync"

# Options
USE_PERF=false
USE_FLAMEGRAPH=false

for arg in "$@"; do
    case $arg in
        --perf) USE_PERF=true ;;
        --flamegraph) USE_FLAMEGRAPH=true ;;
        *) echo "Unknown option: $arg"; exit 1 ;;
    esac
done

log_info() { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_section() { echo -e "\n${BLUE}=== $1 ===${NC}"; }

# Check prerequisites
check_prereqs() {
    if [[ ! -x "$UPSTREAM" ]]; then
        echo -e "${RED}[ERROR]${NC} Upstream rsync not found at: $UPSTREAM"
        echo "Run: ./scripts/build_upstream.sh"
        exit 1
    fi

    if [[ ! -x "$OC_RSYNC" ]]; then
        echo -e "${RED}[ERROR]${NC} oc-rsync not found at: $OC_RSYNC"
        echo "Run: cargo build --release"
        exit 1
    fi

    if $USE_PERF && ! command -v perf &>/dev/null; then
        echo -e "${RED}[ERROR]${NC} perf not found. Install linux-tools-common"
        exit 1
    fi

    if $USE_FLAMEGRAPH && ! command -v flamegraph &>/dev/null; then
        echo -e "${RED}[ERROR]${NC} flamegraph not found. Run: cargo install flamegraph"
        exit 1
    fi
}

# Create test data
setup_test_data() {
    local dir="$1"
    local scenario="$2"

    case "$scenario" in
        small_files)
            log_info "Creating 1000 x 1KB files..."
            for i in $(seq 1 1000); do
                dd if=/dev/urandom of="$dir/file_$i.dat" bs=1024 count=1 2>/dev/null
            done
            ;;
        large_file)
            log_info "Creating 100MB file..."
            dd if=/dev/urandom of="$dir/large.dat" bs=1M count=100 2>/dev/null
            ;;
        mixed_tree)
            log_info "Creating mixed directory tree (20 dirs x 50 files)..."
            for d in $(seq 1 20); do
                mkdir -p "$dir/dir_$d"
                for f in $(seq 1 50); do
                    echo "Content for dir $d file $f" > "$dir/dir_$d/file_$f.txt"
                done
            done
            ;;
        deep_tree)
            log_info "Creating deep directory tree (20 levels)..."
            local path="$dir"
            for i in $(seq 1 20); do
                path="$path/level_$i"
                mkdir -p "$path"
                echo "Depth $i" > "$path/file.txt"
            done
            ;;
    esac
}

# Start daemon
start_daemon() {
    local port="$1"
    local module_path="$2"
    local config_file="$3"
    local pid_file="$4"

    cat > "$config_file" << EOF
pid file = $pid_file
port = $port
use chroot = false
numeric ids = yes

[bench]
    path = $module_path
    read only = false
EOF

    "$UPSTREAM" --daemon --config "$config_file" --no-detach &
    DAEMON_PID=$!

    # Wait for daemon to be ready by trying rsync list command
    for i in $(seq 1 50); do
        if "$UPSTREAM" "rsync://127.0.0.1:$port/" >/dev/null 2>&1; then
            log_info "Daemon started on port $port (PID: $DAEMON_PID)"
            return 0
        fi
        sleep 0.2
    done

    log_warn "Daemon failed to start"
    return 1
}

# Run benchmark
run_benchmark() {
    local name="$1"
    local binary="$2"
    local source="$3"
    local dest="$4"
    local runs="${5:-5}"
    local extra_args="${6:-}"

    local times=()

    for i in $(seq 1 "$runs"); do
        rm -rf "$dest"/*

        local start_ns=$(date +%s%N)

        if $USE_FLAMEGRAPH && [[ "$binary" == "$OC_RSYNC" ]]; then
            flamegraph -o "flamegraph_${name}_run${i}.svg" -- \
                "$binary" -av $extra_args "$source" "$dest" >/dev/null 2>&1
        elif $USE_PERF && [[ "$binary" == "$OC_RSYNC" ]]; then
            perf record -g -o "perf_${name}_run${i}.data" -- \
                "$binary" -av $extra_args "$source" "$dest" >/dev/null 2>&1
        else
            "$binary" -av $extra_args "$source" "$dest" >/dev/null 2>&1
        fi

        local end_ns=$(date +%s%N)
        local elapsed_ms=$(( (end_ns - start_ns) / 1000000 ))
        times+=("$elapsed_ms")
    done

    # Calculate statistics
    local sum=0
    local min=${times[0]}
    local max=${times[0]}

    for t in "${times[@]}"; do
        sum=$((sum + t))
        ((t < min)) && min=$t
        ((t > max)) && max=$t
    done

    local avg=$((sum / runs))
    printf "  %-12s avg: %6d ms  min: %6d ms  max: %6d ms\n" "$name" "$avg" "$min" "$max" >&2

    # Return average for comparison (to stdout for capture)
    echo "$avg"
}

# Main
main() {
    check_prereqs

    local workdir=$(mktemp -d)
    DAEMON_PID=""
    trap "rm -rf '$workdir'; [[ -n \${DAEMON_PID:-} ]] && kill \$DAEMON_PID 2>/dev/null || true" EXIT

    local port=15000
    local module_path="$workdir/module"
    local config_file="$workdir/rsyncd.conf"
    local pid_file="$workdir/rsyncd.pid"
    local dest_up="$workdir/dest_upstream"
    local dest_oc="$workdir/dest_oc"

    mkdir -p "$module_path" "$dest_up" "$dest_oc"

    start_daemon "$port" "$module_path" "$config_file" "$pid_file"

    local rsync_url="rsync://127.0.0.1:$port/bench/"

    echo ""
    echo "=============================================="
    echo "  rsync:// Transfer Performance Comparison"
    echo "=============================================="
    echo ""
    echo "Upstream: $UPSTREAM"
    echo "oc-rsync: $OC_RSYNC"
    echo "URL:      $rsync_url"
    echo ""

    local results_file="$workdir/results.csv"
    echo "scenario,upstream_ms,oc_rsync_ms,ratio" > "$results_file"

    # Scenario 1: Small files
    log_section "Scenario 1: 1000 x 1KB files (initial sync)"
    rm -rf "$module_path"/*
    setup_test_data "$module_path" small_files

    up_time=$(run_benchmark "upstream" "$UPSTREAM" "$rsync_url" "$dest_up")
    oc_time=$(run_benchmark "oc-rsync" "$OC_RSYNC" "$rsync_url" "$dest_oc")
    ratio=$(awk "BEGIN {printf \"%.2f\", $oc_time / $up_time}")
    echo "  Ratio: ${ratio}x"
    echo "small_files_initial,$up_time,$oc_time,$ratio" >> "$results_file"

    # Scenario 2: Small files (no change)
    log_section "Scenario 2: 1000 x 1KB files (no change sync)"

    up_time=$(run_benchmark "upstream" "$UPSTREAM" "$rsync_url" "$dest_up")
    oc_time=$(run_benchmark "oc-rsync" "$OC_RSYNC" "$rsync_url" "$dest_oc")
    ratio=$(awk "BEGIN {printf \"%.2f\", $oc_time / $up_time}")
    echo "  Ratio: ${ratio}x"
    echo "small_files_nochange,$up_time,$oc_time,$ratio" >> "$results_file"

    # Scenario 3: Large file
    log_section "Scenario 3: 100MB file (initial sync)"
    rm -rf "$module_path"/* "$dest_up"/* "$dest_oc"/*
    setup_test_data "$module_path" large_file

    up_time=$(run_benchmark "upstream" "$UPSTREAM" "$rsync_url" "$dest_up" 3)
    oc_time=$(run_benchmark "oc-rsync" "$OC_RSYNC" "$rsync_url" "$dest_oc" 3)
    ratio=$(awk "BEGIN {printf \"%.2f\", $oc_time / $up_time}")
    echo "  Ratio: ${ratio}x"
    echo "large_file,$up_time,$oc_time,$ratio" >> "$results_file"

    # Scenario 4: Mixed tree
    log_section "Scenario 4: Mixed tree (20 dirs x 50 files)"
    rm -rf "$module_path"/* "$dest_up"/* "$dest_oc"/*
    setup_test_data "$module_path" mixed_tree

    up_time=$(run_benchmark "upstream" "$UPSTREAM" "$rsync_url" "$dest_up")
    oc_time=$(run_benchmark "oc-rsync" "$OC_RSYNC" "$rsync_url" "$dest_oc")
    ratio=$(awk "BEGIN {printf \"%.2f\", $oc_time / $up_time}")
    echo "  Ratio: ${ratio}x"
    echo "mixed_tree,$up_time,$oc_time,$ratio" >> "$results_file"

    # Scenario 5: Deep tree
    log_section "Scenario 5: Deep directory tree (20 levels)"
    rm -rf "$module_path"/* "$dest_up"/* "$dest_oc"/*
    setup_test_data "$module_path" deep_tree

    up_time=$(run_benchmark "upstream" "$UPSTREAM" "$rsync_url" "$dest_up")
    oc_time=$(run_benchmark "oc-rsync" "$OC_RSYNC" "$rsync_url" "$dest_oc")
    ratio=$(awk "BEGIN {printf \"%.2f\", $oc_time / $up_time}")
    echo "  Ratio: ${ratio}x"
    echo "deep_tree,$up_time,$oc_time,$ratio" >> "$results_file"

    # Summary
    log_section "Summary"
    echo ""
    cat "$results_file" | column -t -s,
    echo ""

    if $USE_PERF; then
        log_info "Perf data saved to perf_*.data files"
        log_info "Analyze with: perf report -i perf_<scenario>_run1.data"
    fi

    if $USE_FLAMEGRAPH; then
        log_info "Flamegraphs saved to flamegraph_*.svg files"
    fi

    # Copy results to project root
    cp "$results_file" "$PROJECT_ROOT/benchmark_results.csv"
    log_info "Results saved to benchmark_results.csv"
}

main "$@"
