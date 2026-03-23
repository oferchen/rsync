#!/bin/bash
# 1GB single-file benchmark with phase breakdown.
#
# Measures initial sync, no-change sync, and delta sync (1% scattered changes)
# for both oc-rsync and upstream rsync. Reports wall-clock time, throughput
# (MB/s), and peak RSS for each phase.
#
# Usage:
#   ./scripts/benchmark_1gb.sh
#
# Environment variables:
#   OC_RSYNC         Path to oc-rsync binary (default: target/release/oc-rsync)
#   UPSTREAM_RSYNC   Path to upstream rsync binary (default: auto-detected)
#   RUNS             Number of runs per benchmark (default: 3)
#   BLOCK_SIZE       rsync --block-size value in bytes (default: 131072 = 128KB)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

OC_RSYNC="${OC_RSYNC:-${PROJECT_ROOT}/target/release/oc-rsync}"
UPSTREAM_RSYNC="${UPSTREAM_RSYNC:-${PROJECT_ROOT}/target/interop/upstream-install/3.4.1/bin/rsync}"
RUNS="${RUNS:-3}"
BLOCK_SIZE="${BLOCK_SIZE:-131072}"

FILE_SIZE_BYTES=$((1024 * 1024 * 1024))  # 1 GB
FILE_SIZE_MB=1024
DELTA_PATCH_PERCENT=1  # 1% = ~10MB scattered changes
DELTA_PATCH_COUNT=100  # number of scattered patches
DELTA_PATCH_SIZE=$((FILE_SIZE_BYTES * DELTA_PATCH_PERCENT / 100 / DELTA_PATCH_COUNT))  # ~100KB each

WORKDIR=""

# Cleanup on exit
cleanup() {
    if [[ -n "$WORKDIR" && -d "$WORKDIR" ]]; then
        rm -rf "$WORKDIR"
    fi
}
trap cleanup EXIT

# Detect platform-specific peak RSS measurement
setup_time_cmd() {
    if [[ "$(uname)" == "Darwin" ]]; then
        # macOS: /usr/bin/time -l reports peak RSS in bytes
        TIME_CMD="/usr/bin/time -l"
        RSS_UNIT="bytes"
    else
        # Linux: /usr/bin/time -v reports peak RSS in KB
        TIME_CMD="/usr/bin/time -v"
        RSS_UNIT="kb"
    fi
}

# Extract peak RSS from time output (stderr capture)
# Args: $1 = file containing time stderr output
extract_peak_rss_kb() {
    local timefile="$1"
    if [[ "$RSS_UNIT" == "bytes" ]]; then
        # macOS: "maximum resident set size" line, value in bytes
        local bytes
        bytes=$(grep "maximum resident set size" "$timefile" | awk '{print $NF}')
        echo $(( bytes / 1024 ))
    else
        # Linux: "Maximum resident set size (kbytes):" line
        grep "Maximum resident set size" "$timefile" | awk '{print $NF}'
    fi
}

# Run a single timed benchmark.
# Args: $1 = label, $2... = command to run
# Outputs: TIME_S, THROUGHPUT_MBS, PEAK_RSS_KB as global vars
run_once() {
    local label="$1"
    shift

    local time_stderr
    time_stderr=$(mktemp)

    local start end duration_ns duration_s
    start=$(date +%s%N 2>/dev/null || python3 -c "import time; print(int(time.time()*1e9))")
    $TIME_CMD "$@" >/dev/null 2>"$time_stderr" || true
    end=$(date +%s%N 2>/dev/null || python3 -c "import time; print(int(time.time()*1e9))")

    duration_ns=$((end - start))
    TIME_S=$(awk "BEGIN {printf \"%.3f\", $duration_ns / 1000000000.0}")
    THROUGHPUT_MBS=$(awk "BEGIN {if ($duration_ns > 0) printf \"%.1f\", $FILE_SIZE_MB / ($duration_ns / 1000000000.0); else print \"inf\"}")
    PEAK_RSS_KB=$(extract_peak_rss_kb "$time_stderr")

    rm -f "$time_stderr"
}

# Run benchmark N times and pick the median by wall-clock time.
# Args: $1 = label, $2 = phase description, $3... = command
# Sets: MEDIAN_TIME, MEDIAN_THROUGHPUT, MEDIAN_RSS
run_median() {
    local label="$1"
    local phase="$2"
    shift 2

    local -a times=()
    local -a throughputs=()
    local -a rss_values=()

    for ((i = 1; i <= RUNS; i++)); do
        run_once "$label" "$@"
        times+=("$TIME_S")
        throughputs+=("$THROUGHPUT_MBS")
        rss_values+=("$PEAK_RSS_KB")
    done

    # Sort by time and pick median index
    local sorted_indices
    sorted_indices=$(for i in "${!times[@]}"; do echo "$i ${times[$i]}"; done | sort -k2 -n | awk '{print $1}')
    local median_pos=$(( RUNS / 2 ))
    local median_idx
    median_idx=$(echo "$sorted_indices" | sed -n "$((median_pos + 1))p")

    MEDIAN_TIME="${times[$median_idx]}"
    MEDIAN_THROUGHPUT="${throughputs[$median_idx]}"
    MEDIAN_RSS="${rss_values[$median_idx]}"
}

# Create a 1GB file with pseudo-random data
create_1gb_file() {
    local path="$1"
    echo "Creating 1GB pseudo-random file..."
    dd if=/dev/urandom of="$path" bs=1M count=1024 2>/dev/null
    echo "  Done: $(du -h "$path" | cut -f1)"
}

# Apply scattered delta changes to a file (modify ~1% at random offsets)
apply_delta_changes() {
    local path="$1"
    echo "Applying ${DELTA_PATCH_COUNT} scattered patches (~${DELTA_PATCH_SIZE} bytes each)..."
    local max_offset=$((FILE_SIZE_BYTES - DELTA_PATCH_SIZE))

    for ((i = 0; i < DELTA_PATCH_COUNT; i++)); do
        # Deterministic but spread-out offsets
        local offset=$(( (i * FILE_SIZE_BYTES / DELTA_PATCH_COUNT) + (i * 7919 % (FILE_SIZE_BYTES / DELTA_PATCH_COUNT)) ))
        if (( offset > max_offset )); then
            offset=$max_offset
        fi
        dd if=/dev/urandom of="$path" bs="$DELTA_PATCH_SIZE" count=1 seek="$offset" conv=notrunc oflag=seek_bytes 2>/dev/null
    done
    echo "  Done: ~$((DELTA_PATCH_COUNT * DELTA_PATCH_SIZE / 1024 / 1024))MB modified"
}

# Print a formatted result row
# Args: $1=binary, $2=phase, $3=time, $4=throughput, $5=rss_kb
print_row() {
    local binary="$1" phase="$2" time_s="$3" tp="$4" rss_kb="$5"
    local rss_mb
    rss_mb=$(awk "BEGIN {printf \"%.1f\", $rss_kb / 1024.0}")
    printf "| %-15s | %-18s | %10s s | %10s MB/s | %10s MB |\n" \
        "$binary" "$phase" "$time_s" "$tp" "$rss_mb"
}

# Print separator
print_sep() {
    printf "| %-15s | %-18s | %12s | %14s | %13s |\n" \
        "---------------" "------------------" "------------" "--------------" "-------------"
}

# Print header
print_header() {
    printf "| %-15s | %-18s | %12s | %14s | %13s |\n" \
        "Binary" "Phase" "Wall Clock" "Throughput" "Peak RSS"
    print_sep
}

# Main
main() {
    setup_time_cmd

    # Validate binaries
    if [[ ! -x "$OC_RSYNC" ]]; then
        echo "ERROR: oc-rsync not found at: $OC_RSYNC"
        echo "Build with: cargo build --release"
        exit 1
    fi
    if [[ ! -x "$UPSTREAM_RSYNC" ]]; then
        echo "ERROR: upstream rsync not found at: $UPSTREAM_RSYNC"
        exit 1
    fi

    # Check macOS dd supports oflag=seek_bytes (needed for delta patching)
    if [[ "$(uname)" == "Darwin" ]]; then
        if ! dd if=/dev/zero of=/dev/null bs=1 count=0 oflag=seek_bytes 2>/dev/null; then
            echo "WARNING: macOS dd does not support oflag=seek_bytes."
            echo "Install GNU coreutils: brew install coreutils"
            echo "Then set PATH to include gdd or use: alias dd=gdd"
            exit 1
        fi
    fi

    WORKDIR=$(mktemp -d)
    local src_dir="$WORKDIR/src"
    local dest_oc="$WORKDIR/dest-oc"
    local dest_up="$WORKDIR/dest-up"
    mkdir -p "$src_dir" "$dest_oc" "$dest_up"

    echo "=============================================="
    echo "  1GB File Benchmark - Phase Breakdown"
    echo "=============================================="
    echo ""
    echo "Configuration:"
    echo "  oc-rsync:       $OC_RSYNC"
    echo "  upstream rsync: $UPSTREAM_RSYNC"
    echo "  File size:      ${FILE_SIZE_MB}MB"
    echo "  Block size:     ${BLOCK_SIZE} bytes"
    echo "  Runs per test:  ${RUNS} (median reported)"
    echo "  Work directory: $WORKDIR"
    echo ""
    echo "Versions:"
    echo "  oc-rsync: $("$OC_RSYNC" --version 2>/dev/null | head -1 || echo 'unknown')"
    echo "  rsync:    $("$UPSTREAM_RSYNC" --version 2>/dev/null | head -1 || echo 'unknown')"
    echo ""

    # Create test file
    create_1gb_file "$src_dir/data.bin"
    echo ""

    # Storage for results
    declare -a RESULTS=()

    # --- Phase 1: Initial sync ---
    echo "=== Phase 1: Initial Sync (1GB cold transfer) ==="

    rm -rf "$dest_oc"/* 2>/dev/null || true
    echo "  Benchmarking oc-rsync ($RUNS runs)..."
    run_median "oc-rsync" "initial" \
        "$OC_RSYNC" -a --block-size="$BLOCK_SIZE" "$src_dir/" "$dest_oc/"
    RESULTS+=("oc-rsync|Initial Sync|$MEDIAN_TIME|$MEDIAN_THROUGHPUT|$MEDIAN_RSS")
    echo "    Median: ${MEDIAN_TIME}s, ${MEDIAN_THROUGHPUT} MB/s, RSS: $((MEDIAN_RSS / 1024))MB"

    rm -rf "$dest_up"/* 2>/dev/null || true
    echo "  Benchmarking upstream rsync ($RUNS runs)..."
    run_median "upstream" "initial" \
        "$UPSTREAM_RSYNC" -a --block-size="$BLOCK_SIZE" "$src_dir/" "$dest_up/"
    RESULTS+=("upstream rsync|Initial Sync|$MEDIAN_TIME|$MEDIAN_THROUGHPUT|$MEDIAN_RSS")
    echo "    Median: ${MEDIAN_TIME}s, ${MEDIAN_THROUGHPUT} MB/s, RSS: $((MEDIAN_RSS / 1024))MB"
    echo ""

    # --- Phase 2: No-change sync ---
    echo "=== Phase 2: No-Change Sync (up-to-date) ==="

    # Ensure destinations are populated
    "$OC_RSYNC" -a "$src_dir/" "$dest_oc/" >/dev/null 2>&1
    "$UPSTREAM_RSYNC" -a "$src_dir/" "$dest_up/" >/dev/null 2>&1

    echo "  Benchmarking oc-rsync ($RUNS runs)..."
    run_median "oc-rsync" "no-change" \
        "$OC_RSYNC" -a --block-size="$BLOCK_SIZE" "$src_dir/" "$dest_oc/"
    RESULTS+=("oc-rsync|No-Change Sync|$MEDIAN_TIME|$MEDIAN_THROUGHPUT|$MEDIAN_RSS")
    echo "    Median: ${MEDIAN_TIME}s, ${MEDIAN_THROUGHPUT} MB/s, RSS: $((MEDIAN_RSS / 1024))MB"

    echo "  Benchmarking upstream rsync ($RUNS runs)..."
    run_median "upstream" "no-change" \
        "$UPSTREAM_RSYNC" -a --block-size="$BLOCK_SIZE" "$src_dir/" "$dest_up/"
    RESULTS+=("upstream rsync|No-Change Sync|$MEDIAN_TIME|$MEDIAN_THROUGHPUT|$MEDIAN_RSS")
    echo "    Median: ${MEDIAN_TIME}s, ${MEDIAN_THROUGHPUT} MB/s, RSS: $((MEDIAN_RSS / 1024))MB"
    echo ""

    # --- Phase 3: Delta sync (~1% scattered changes) ---
    echo "=== Phase 3: Delta Sync (~1% scattered modifications) ==="

    # Ensure destinations are populated from original
    "$OC_RSYNC" -a "$src_dir/" "$dest_oc/" >/dev/null 2>&1
    "$UPSTREAM_RSYNC" -a "$src_dir/" "$dest_up/" >/dev/null 2>&1

    # Apply delta changes to source
    apply_delta_changes "$src_dir/data.bin"
    # Touch the file to ensure mtime change triggers transfer
    touch "$src_dir/data.bin"
    echo ""

    echo "  Benchmarking oc-rsync ($RUNS runs)..."
    # For delta runs, re-populate dest from pre-delta state each time
    # We keep a pristine copy for this purpose
    cp "$dest_oc/data.bin" "$WORKDIR/pristine_oc.bin"
    cp "$dest_up/data.bin" "$WORKDIR/pristine_up.bin"

    local -a delta_oc_times=()
    local -a delta_oc_tp=()
    local -a delta_oc_rss=()
    for ((i = 1; i <= RUNS; i++)); do
        cp "$WORKDIR/pristine_oc.bin" "$dest_oc/data.bin"
        run_once "oc-rsync" "$OC_RSYNC" -a --block-size="$BLOCK_SIZE" "$src_dir/" "$dest_oc/"
        delta_oc_times+=("$TIME_S")
        delta_oc_tp+=("$THROUGHPUT_MBS")
        delta_oc_rss+=("$PEAK_RSS_KB")
    done
    # Find median
    local sorted_oc
    sorted_oc=$(for i in "${!delta_oc_times[@]}"; do echo "$i ${delta_oc_times[$i]}"; done | sort -k2 -n | awk '{print $1}')
    local mid_oc
    mid_oc=$(echo "$sorted_oc" | sed -n "$((RUNS / 2 + 1))p")
    RESULTS+=("oc-rsync|Delta Sync (1%)|${delta_oc_times[$mid_oc]}|${delta_oc_tp[$mid_oc]}|${delta_oc_rss[$mid_oc]}")
    echo "    Median: ${delta_oc_times[$mid_oc]}s, ${delta_oc_tp[$mid_oc]} MB/s, RSS: $((delta_oc_rss[$mid_oc] / 1024))MB"

    echo "  Benchmarking upstream rsync ($RUNS runs)..."
    local -a delta_up_times=()
    local -a delta_up_tp=()
    local -a delta_up_rss=()
    for ((i = 1; i <= RUNS; i++)); do
        cp "$WORKDIR/pristine_up.bin" "$dest_up/data.bin"
        run_once "upstream" "$UPSTREAM_RSYNC" -a --block-size="$BLOCK_SIZE" "$src_dir/" "$dest_up/"
        delta_up_times+=("$TIME_S")
        delta_up_tp+=("$THROUGHPUT_MBS")
        delta_up_rss+=("$PEAK_RSS_KB")
    done
    local sorted_up
    sorted_up=$(for i in "${!delta_up_times[@]}"; do echo "$i ${delta_up_times[$i]}"; done | sort -k2 -n | awk '{print $1}')
    local mid_up
    mid_up=$(echo "$sorted_up" | sed -n "$((RUNS / 2 + 1))p")
    RESULTS+=("upstream rsync|Delta Sync (1%)|${delta_up_times[$mid_up]}|${delta_up_tp[$mid_up]}|${delta_up_rss[$mid_up]}")
    echo "    Median: ${delta_up_times[$mid_up]}s, ${delta_up_tp[$mid_up]} MB/s, RSS: $((delta_up_rss[$mid_up] / 1024))MB"
    echo ""

    # --- Summary Table ---
    echo "=============================================="
    echo "  Results Summary (median of $RUNS runs)"
    echo "=============================================="
    echo ""
    print_header

    for row in "${RESULTS[@]}"; do
        IFS='|' read -r binary phase time_s tp rss_kb <<< "$row"
        print_row "$binary" "$phase" "$time_s" "$tp" "$rss_kb"
    done

    echo ""

    # --- Throughput Comparison ---
    echo "=============================================="
    echo "  Throughput Comparison (oc-rsync vs upstream)"
    echo "=============================================="
    echo ""

    local phases=("Initial Sync" "No-Change Sync" "Delta Sync (1%)")
    for phase in "${phases[@]}"; do
        local oc_tp="" up_tp=""
        for row in "${RESULTS[@]}"; do
            IFS='|' read -r binary rphase time_s tp rss_kb <<< "$row"
            if [[ "$rphase" == "$phase" ]]; then
                if [[ "$binary" == "oc-rsync" ]]; then
                    oc_tp="$tp"
                else
                    up_tp="$tp"
                fi
            fi
        done
        if [[ -n "$oc_tp" && -n "$up_tp" ]]; then
            local ratio
            ratio=$(awk "BEGIN {if ($up_tp > 0) printf \"%.2f\", $oc_tp / $up_tp; else print \"N/A\"}")
            printf "  %-20s  oc-rsync: %8s MB/s  upstream: %8s MB/s  ratio: %sx\n" \
                "$phase" "$oc_tp" "$up_tp" "$ratio"
        fi
    done

    echo ""
    echo "Benchmark complete. Work directory cleaned up on exit."
}

main "$@"
