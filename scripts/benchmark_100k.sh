#!/bin/bash
# 100K files benchmark with phase breakdown.
#
# Creates 100,000 small files across 1,000 directories and benchmarks
# initial sync, no-change sync, and incremental sync (10% modified)
# for both oc-rsync and upstream rsync.
#
# Reports wall-clock time, peak RSS, and file count for each phase.
# Each benchmark runs 3 times; the median is reported.
#
# Usage:
#   ./scripts/benchmark_100k.sh
#
# Environment variables:
#   OC_RSYNC         Path to oc-rsync binary (default: target/release/oc-rsync)
#   UPSTREAM_RSYNC    Path to upstream rsync binary (default: rsync)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

OC_RSYNC="${OC_RSYNC:-${PROJECT_ROOT}/target/release/oc-rsync}"
UPSTREAM_RSYNC="${UPSTREAM_RSYNC:-rsync}"

NUM_DIRS=1000
FILES_PER_DIR=100
TOTAL_FILES=$((NUM_DIRS * FILES_PER_DIR))
RUNS=3

# Detect platform-appropriate time command for peak RSS measurement.
# GNU /usr/bin/time -v reports "Maximum resident set size (kbytes)".
# macOS uses /usr/bin/time -l which reports bytes on "maximum resident set size".
detect_time_cmd() {
    if [[ "$(uname)" == "Darwin" ]]; then
        TIME_CMD="/usr/bin/time -l"
        RSS_PARSE="parse_rss_macos"
    elif [[ -x /usr/bin/time ]]; then
        TIME_CMD="/usr/bin/time -v"
        RSS_PARSE="parse_rss_linux"
    else
        echo "ERROR: /usr/bin/time not found. Cannot measure peak RSS."
        exit 1
    fi
}

parse_rss_linux() {
    # GNU time -v outputs: "Maximum resident set size (kbytes): NNN"
    local stderr_file="$1"
    grep "Maximum resident set size" "$stderr_file" | awk '{print $NF}'
}

parse_rss_macos() {
    # macOS time -l outputs: "NNN  maximum resident set size" (bytes)
    local stderr_file="$1"
    grep "maximum resident set size" "$stderr_file" | awk '{print int($1 / 1024)}'
}

format_rss() {
    local kb="$1"
    if (( kb >= 1024 )); then
        echo "$(echo "scale=1; $kb / 1024" | bc) MB"
    else
        echo "${kb} KB"
    fi
}

# Check prerequisites
check_prereqs() {
    if [[ ! -x "$OC_RSYNC" ]]; then
        echo "ERROR: oc-rsync not found at: $OC_RSYNC"
        echo "Build with: cargo build --release"
        exit 1
    fi

    if ! command -v "$UPSTREAM_RSYNC" &>/dev/null && [[ ! -x "$UPSTREAM_RSYNC" ]]; then
        echo "ERROR: upstream rsync not found at: $UPSTREAM_RSYNC"
        exit 1
    fi
}

# Create 100K files: 1000 dirs x 100 files, 1-4KB each
create_test_data() {
    local dest="$1"
    echo "Creating $TOTAL_FILES files across $NUM_DIRS directories..."
    local start
    start=$(date +%s)

    for d in $(seq 0 $((NUM_DIRS - 1))); do
        local dir_path="$dest/dir_$(printf '%04d' "$d")"
        mkdir -p "$dir_path"
        for f in $(seq 0 $((FILES_PER_DIR - 1))); do
            # Random size between 1024 and 4096 bytes
            local size=$(( (RANDOM % 4 + 1) * 1024 ))
            dd if=/dev/urandom of="$dir_path/file_$(printf '%03d' "$f").dat" \
                bs="$size" count=1 2>/dev/null
        done
    done

    local end
    end=$(date +%s)
    local total_size
    total_size=$(du -sh "$dest" | cut -f1)
    echo "  Created $TOTAL_FILES files ($total_size) in $((end - start))s"
}

# Modify 10% of files for incremental benchmark
modify_files() {
    local src="$1"
    local count=$((TOTAL_FILES / 10))
    echo "Modifying $count files (10%)..."

    # Modify the first file in the first 10000 directories, or spread across dirs
    local modified=0
    for d in $(seq 0 $((NUM_DIRS - 1))); do
        if (( modified >= count )); then
            break
        fi
        local dir_path="$src/dir_$(printf '%04d' "$d")"
        # Modify every 10th file to spread changes
        for f in $(seq 0 9); do
            if (( modified >= count )); then
                break
            fi
            local file_path="$dir_path/file_$(printf '%03d' "$((f * 10)")).dat"
            if [[ -f "$file_path" ]]; then
                local size=$(( (RANDOM % 4 + 1) * 1024 ))
                dd if=/dev/urandom of="$file_path" bs="$size" count=1 2>/dev/null
                modified=$((modified + 1))
            fi
        done
    done
    echo "  Modified $modified files"
}

# Run a single timed rsync invocation. Captures wall-clock time and peak RSS.
# Outputs: "<wall_secs> <peak_rss_kb>" to stdout.
run_timed() {
    local binary="$1"
    local src="$2"
    local dest="$3"
    local stderr_file
    stderr_file=$(mktemp)

    local start end
    start=$(date +%s.%N)
    $TIME_CMD "$binary" -a "$src/" "$dest/" >/dev/null 2>"$stderr_file" || true
    end=$(date +%s.%N)

    local wall
    wall=$(echo "$end - $start" | bc)
    local rss
    rss=$($RSS_PARSE "$stderr_file")
    rm -f "$stderr_file"

    echo "$wall $rss"
}

# Run a benchmark RUNS times, return median wall-clock time and median peak RSS.
# Outputs: "<median_wall> <median_rss_kb>"
run_benchmark() {
    local binary="$1"
    local src="$2"
    local dest="$3"
    local phase="$4"
    local prepare_cmd="${5:-}"  # Optional command to run before each iteration

    local -a wall_times=()
    local -a rss_values=()

    for run in $(seq 1 "$RUNS"); do
        if [[ -n "$prepare_cmd" ]]; then
            eval "$prepare_cmd"
        fi

        local result
        result=$(run_timed "$binary" "$src" "$dest")
        local w r
        w=$(echo "$result" | awk '{print $1}')
        r=$(echo "$result" | awk '{print $2}')
        wall_times+=("$w")
        rss_values+=("$r")
    done

    # Sort and pick median (index 1 of 0-indexed 3 elements)
    local sorted_wall sorted_rss
    sorted_wall=$(printf '%s\n' "${wall_times[@]}" | sort -n)
    sorted_rss=$(printf '%s\n' "${rss_values[@]}" | sort -n)

    local median_wall median_rss
    median_wall=$(echo "$sorted_wall" | sed -n '2p')
    median_rss=$(echo "$sorted_rss" | sed -n '2p')

    echo "$median_wall $median_rss"
}

# Storage for results - parallel arrays
declare -a RESULT_PHASE=()
declare -a RESULT_BINARY=()
declare -a RESULT_WALL=()
declare -a RESULT_RSS=()
declare -a RESULT_FILES=()

# Record a single result row
record_result() {
    local phase="$1" binary="$2" wall="$3" rss="$4" files="$5"
    RESULT_PHASE+=("$phase")
    RESULT_BINARY+=("$binary")
    RESULT_WALL+=("$wall")
    RESULT_RSS+=("$rss")
    RESULT_FILES+=("$files")
}

# Print final comparison table
print_results() {
    local sep="+-----------------------+----------------+------------+------------+--------+"
    echo ""
    echo "=== 100K Files Benchmark Results ==="
    echo ""
    printf "| %-21s | %-14s | %10s | %10s | %6s |\n" \
        "Phase" "Binary" "Wall (s)" "Peak RSS" "Files"
    echo "$sep"

    for i in "${!RESULT_PHASE[@]}"; do
        local rss_fmt
        rss_fmt=$(format_rss "${RESULT_RSS[$i]}")
        printf "| %-21s | %-14s | %10s | %10s | %6s |\n" \
            "${RESULT_PHASE[$i]}" \
            "${RESULT_BINARY[$i]}" \
            "${RESULT_WALL[$i]}" \
            "$rss_fmt" \
            "${RESULT_FILES[$i]}"
    done
    echo "$sep"
}

# Print results in a CI-friendly markdown format
print_ci_summary() {
    echo ""
    echo "### 100K Files Benchmark"
    echo ""
    echo "| Phase | Binary | Wall (s) | Peak RSS | Files |"
    echo "|-------|--------|----------|----------|-------|"

    for i in "${!RESULT_PHASE[@]}"; do
        local rss_fmt
        rss_fmt=$(format_rss "${RESULT_RSS[$i]}")
        echo "| ${RESULT_PHASE[$i]} | ${RESULT_BINARY[$i]} | ${RESULT_WALL[$i]} | $rss_fmt | ${RESULT_FILES[$i]} |"
    done

    echo ""

    # Compute speedup for each phase pair (oc-rsync vs upstream)
    echo "#### Speedup (upstream wall / oc-rsync wall)"
    echo ""
    local i=0
    while (( i < ${#RESULT_PHASE[@]} )); do
        # Expect pairs: oc-rsync then upstream for each phase
        if (( i + 1 < ${#RESULT_PHASE[@]} )); then
            local oc_wall="${RESULT_WALL[$i]}"
            local up_wall="${RESULT_WALL[$((i + 1))]}"
            local speedup
            speedup=$(echo "scale=2; $up_wall / $oc_wall" | bc 2>/dev/null || echo "N/A")
            echo "- **${RESULT_PHASE[$i]}**: ${speedup}x"
        fi
        i=$((i + 2))
    done
}

main() {
    detect_time_cmd
    check_prereqs

    echo "================================================"
    echo "  100K Files Benchmark - Phase Breakdown"
    echo "================================================"
    echo ""
    echo "Configuration:"
    echo "  Files:          $TOTAL_FILES ($NUM_DIRS dirs x $FILES_PER_DIR files)"
    echo "  File sizes:     1-4 KB (random)"
    echo "  Runs per bench: $RUNS (median reported)"
    echo "  oc-rsync:       $OC_RSYNC"
    echo "  upstream rsync: $UPSTREAM_RSYNC"
    echo ""

    # Show versions
    echo "Versions:"
    echo "  oc-rsync: $("$OC_RSYNC" --version 2>/dev/null | head -1 || echo "unknown")"
    echo "  rsync:    $("$UPSTREAM_RSYNC" --version 2>/dev/null | head -1 || echo "unknown")"
    echo ""

    # Create temp dirs with cleanup trap
    WORKDIR=$(mktemp -d)
    trap 'rm -rf "$WORKDIR"' EXIT

    SRC="$WORKDIR/src"
    DEST_OC="$WORKDIR/dest-oc"
    DEST_UP="$WORKDIR/dest-upstream"
    mkdir -p "$SRC" "$DEST_OC" "$DEST_UP"

    create_test_data "$SRC"
    echo ""

    # Phase 1: Initial sync (full transfer)
    echo "=== Phase 1: Initial Sync (full transfer) ==="
    echo "  Running oc-rsync ($RUNS runs)..."
    local oc_result
    oc_result=$(run_benchmark "$OC_RSYNC" "$SRC" "$DEST_OC" "initial" "rm -rf $DEST_OC && mkdir -p $DEST_OC")
    local oc_wall oc_rss
    oc_wall=$(echo "$oc_result" | awk '{print $1}')
    oc_rss=$(echo "$oc_result" | awk '{print $2}')
    record_result "Initial sync" "oc-rsync" "$oc_wall" "$oc_rss" "$TOTAL_FILES"
    echo "    wall=${oc_wall}s  rss=$(format_rss "$oc_rss")"

    echo "  Running upstream rsync ($RUNS runs)..."
    local up_result
    up_result=$(run_benchmark "$UPSTREAM_RSYNC" "$SRC" "$DEST_UP" "initial" "rm -rf $DEST_UP && mkdir -p $DEST_UP")
    local up_wall up_rss
    up_wall=$(echo "$up_result" | awk '{print $1}')
    up_rss=$(echo "$up_result" | awk '{print $2}')
    record_result "Initial sync" "upstream" "$up_wall" "$up_rss" "$TOTAL_FILES"
    echo "    wall=${up_wall}s  rss=$(format_rss "$up_rss")"
    echo ""

    # Ensure destinations are populated for no-change phase
    rm -rf "$DEST_OC" && mkdir -p "$DEST_OC"
    "$OC_RSYNC" -a "$SRC/" "$DEST_OC/" >/dev/null 2>&1
    rm -rf "$DEST_UP" && mkdir -p "$DEST_UP"
    "$UPSTREAM_RSYNC" -a "$SRC/" "$DEST_UP/" >/dev/null 2>&1

    # Phase 2: No-change sync (everything up to date)
    echo "=== Phase 2: No-change Sync ==="
    echo "  Running oc-rsync ($RUNS runs)..."
    oc_result=$(run_benchmark "$OC_RSYNC" "$SRC" "$DEST_OC" "no-change")
    oc_wall=$(echo "$oc_result" | awk '{print $1}')
    oc_rss=$(echo "$oc_result" | awk '{print $2}')
    record_result "No-change sync" "oc-rsync" "$oc_wall" "$oc_rss" "$TOTAL_FILES"
    echo "    wall=${oc_wall}s  rss=$(format_rss "$oc_rss")"

    echo "  Running upstream rsync ($RUNS runs)..."
    up_result=$(run_benchmark "$UPSTREAM_RSYNC" "$SRC" "$DEST_UP" "no-change")
    up_wall=$(echo "$up_result" | awk '{print $1}')
    up_rss=$(echo "$up_result" | awk '{print $2}')
    record_result "No-change sync" "upstream" "$up_wall" "$up_rss" "$TOTAL_FILES"
    echo "    wall=${up_wall}s  rss=$(format_rss "$up_rss")"
    echo ""

    # Phase 3: Incremental sync (10% modified)
    echo "=== Phase 3: Incremental Sync (10% modified) ==="
    modify_files "$SRC"

    echo "  Running oc-rsync ($RUNS runs)..."
    # Re-populate dest before each run since the sync will update it
    oc_result=$(run_benchmark "$OC_RSYNC" "$SRC" "$DEST_OC" "incremental")
    oc_wall=$(echo "$oc_result" | awk '{print $1}')
    oc_rss=$(echo "$oc_result" | awk '{print $2}')
    record_result "Incremental (10%)" "oc-rsync" "$oc_wall" "$oc_rss" "$TOTAL_FILES"
    echo "    wall=${oc_wall}s  rss=$(format_rss "$oc_rss")"

    echo "  Running upstream rsync ($RUNS runs)..."
    up_result=$(run_benchmark "$UPSTREAM_RSYNC" "$SRC" "$DEST_UP" "incremental")
    up_wall=$(echo "$up_result" | awk '{print $1}')
    up_rss=$(echo "$up_result" | awk '{print $2}')
    record_result "Incremental (10%)" "upstream" "$up_wall" "$up_rss" "$TOTAL_FILES"
    echo "    wall=${up_wall}s  rss=$(format_rss "$up_rss")"
    echo ""

    # Print results
    print_results
    print_ci_summary

    echo ""
    echo "Benchmark complete."
}

main "$@"
