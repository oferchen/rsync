#!/bin/bash
# Startup overhead benchmark - measures pure invocation cost of oc-rsync vs upstream rsync.
#
# Compares --version, --help, empty dry-run, and small-directory dry-run latencies.
# Uses hyperfine when available, falls back to bash time loops.
#
# Usage:
#   ./scripts/benchmark_startup.sh [OPTIONS]
#
# Options:
#   -n, --iterations N   Number of iterations (default: 100, hyperfine default: auto)
#   -w, --warmup N       Number of warmup iterations (default: 10)
#   -j, --json FILE      Write machine-readable JSON results to FILE
#   -h, --help           Show this help message
#
# Environment:
#   OC_RSYNC          Path to oc-rsync binary (default: target/release/oc-rsync)
#   UPSTREAM_RSYNC    Path to upstream rsync binary (default: target/interop/upstream-install/3.4.1/bin/rsync)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

OC_RSYNC="${OC_RSYNC:-${PROJECT_ROOT}/target/release/oc-rsync}"
UPSTREAM_RSYNC="${UPSTREAM_RSYNC:-${PROJECT_ROOT}/target/interop/upstream-install/3.4.1/bin/rsync}"

ITERATIONS=100
WARMUP=10
JSON_FILE=""

usage() {
    sed -n '2,/^$/s/^# //p' "$0"
    exit 0
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -n|--iterations) ITERATIONS="$2"; shift 2 ;;
        -w|--warmup) WARMUP="$2"; shift 2 ;;
        -j|--json) JSON_FILE="$2"; shift 2 ;;
        -h|--help) usage ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

# Validate binaries exist
for bin_var in OC_RSYNC UPSTREAM_RSYNC; do
    bin_path="${!bin_var}"
    if [[ ! -x "$bin_path" ]]; then
        echo "Error: $bin_var not found or not executable: $bin_path" >&2
        echo "Set $bin_var env var or build with: cargo build --release" >&2
        exit 1
    fi
done

echo "=== Startup Overhead Benchmark ==="
echo "oc-rsync:       $OC_RSYNC"
echo "upstream rsync: $UPSTREAM_RSYNC"
echo "iterations:     $ITERATIONS"
echo "warmup:         $WARMUP"
echo ""

# Create temp directories with cleanup
TMPDIR_ROOT="$(mktemp -d)"
EMPTY_SRC="${TMPDIR_ROOT}/empty_src"
EMPTY_DST="${TMPDIR_ROOT}/empty_dst"
SMALL_SRC="${TMPDIR_ROOT}/small_src"
SMALL_DST="${TMPDIR_ROOT}/small_dst"

cleanup() {
    rm -rf "$TMPDIR_ROOT"
}
trap cleanup EXIT

mkdir -p "$EMPTY_SRC" "$EMPTY_DST" "$SMALL_SRC" "$SMALL_DST"

# Populate small directory with 10 files of varying sizes
for i in $(seq 1 10); do
    dd if=/dev/urandom of="${SMALL_SRC}/file_${i}.dat" bs=1024 count=$((i * 10)) 2>/dev/null
done

# ---------------------------------------------------------------------------
# Hyperfine path
# ---------------------------------------------------------------------------

if command -v hyperfine &>/dev/null; then
    echo "Using hyperfine for statistical benchmarking"
    echo ""

    RESULTS_DIR="${TMPDIR_ROOT}/results"
    mkdir -p "$RESULTS_DIR"

    run_hyperfine() {
        local label="$1"
        local oc_cmd="$2"
        local up_cmd="$3"
        local json_out="${RESULTS_DIR}/${label}.json"

        echo "--- $label ---"
        hyperfine \
            --warmup "$WARMUP" \
            --min-runs "$ITERATIONS" \
            --export-json "$json_out" \
            --command-name "oc-rsync" "$oc_cmd" \
            --command-name "upstream" "$up_cmd"
        echo ""
    }

    run_hyperfine "version" \
        "$OC_RSYNC --version" \
        "$UPSTREAM_RSYNC --version"

    run_hyperfine "help" \
        "$OC_RSYNC --help" \
        "$UPSTREAM_RSYNC --help"

    run_hyperfine "dry_run_empty" \
        "$OC_RSYNC --dry-run -r ${EMPTY_SRC}/ ${EMPTY_DST}/" \
        "$UPSTREAM_RSYNC --dry-run -r ${EMPTY_SRC}/ ${EMPTY_DST}/"

    run_hyperfine "dry_run_small" \
        "$OC_RSYNC --dry-run -r ${SMALL_SRC}/ ${SMALL_DST}/" \
        "$UPSTREAM_RSYNC --dry-run -r ${SMALL_SRC}/ ${SMALL_DST}/"

    # Merge all JSON results into a single file
    if [[ -n "$JSON_FILE" ]]; then
        python3 -c "
import json, glob, os, sys

results = {}
for f in sorted(glob.glob('${RESULTS_DIR}/*.json')):
    label = os.path.splitext(os.path.basename(f))[0]
    with open(f) as fh:
        results[label] = json.load(fh)

with open('${JSON_FILE}', 'w') as fh:
    json.dump(results, fh, indent=2)
print(f'JSON results written to ${JSON_FILE}')
" 2>/dev/null || echo "Warning: python3 not available, skipping JSON merge"
    fi

    # Print summary table
    echo ""
    echo "=== Summary Table ==="
    printf "%-20s %12s %12s %10s\n" "Scenario" "oc-rsync(ms)" "upstream(ms)" "ratio"
    printf "%-20s %12s %12s %10s\n" "--------" "------------" "------------" "-----"

    for label in version help dry_run_empty dry_run_small; do
        json_file="${RESULTS_DIR}/${label}.json"
        if [[ -f "$json_file" ]]; then
            python3 -c "
import json, sys
with open('${json_file}') as f:
    data = json.load(f)
cmds = data['results']
oc = next(c for c in cmds if c['command'] == 'oc-rsync')
up = next(c for c in cmds if c['command'] == 'upstream')
oc_ms = oc['mean'] * 1000
up_ms = up['mean'] * 1000
ratio = oc_ms / up_ms if up_ms > 0 else float('inf')
print(f'$label|{oc_ms:.2f}|{up_ms:.2f}|{ratio:.2f}x')
" 2>/dev/null | while IFS='|' read -r lbl oc_ms up_ms ratio; do
                printf "%-20s %12s %12s %10s\n" "$lbl" "$oc_ms" "$up_ms" "$ratio"
            done
        fi
    done

    exit 0
fi

# ---------------------------------------------------------------------------
# Bash fallback path - manual timing with /usr/bin/time or bash TIMEFORMAT
# ---------------------------------------------------------------------------

echo "hyperfine not found, using bash time loop fallback"
echo ""

# Collect raw timings in nanoseconds using perl for sub-ms precision.
# Falls back to date +%s%N on Linux or perl on macOS.
now_ns() {
    if [[ "$(uname)" == "Darwin" ]]; then
        perl -MTime::HiRes=time -e 'printf "%d\n", time * 1e9'
    else
        date +%s%N
    fi
}

# Run a command N times, collect wall-clock durations in microseconds.
# Results are appended to the global TIMINGS array.
declare -a TIMINGS=()

run_timed() {
    local cmd="$1"
    local n="$2"
    local warmup="$3"
    TIMINGS=()

    # Warmup runs
    for (( i = 0; i < warmup; i++ )); do
        eval "$cmd" >/dev/null 2>&1 || true
    done

    # Measured runs
    for (( i = 0; i < n; i++ )); do
        local start end
        start="$(now_ns)"
        eval "$cmd" >/dev/null 2>&1 || true
        end="$(now_ns)"
        TIMINGS+=( $(( (end - start) / 1000 )) )  # microseconds
    done
}

# Compute statistics from TIMINGS array (values in microseconds).
# Sets STAT_MEAN, STAT_MEDIAN, STAT_P99 (all in milliseconds).
compute_stats() {
    local sorted
    sorted=($(printf '%s\n' "${TIMINGS[@]}" | sort -n))
    local count=${#sorted[@]}

    # Mean
    local sum=0
    for v in "${sorted[@]}"; do
        sum=$((sum + v))
    done
    STAT_MEAN=$(perl -e "printf '%.3f', $sum / $count / 1000")

    # Median
    local mid=$((count / 2))
    if (( count % 2 == 0 )); then
        STAT_MEDIAN=$(perl -e "printf '%.3f', (${sorted[$mid - 1]} + ${sorted[$mid]}) / 2 / 1000")
    else
        STAT_MEDIAN=$(perl -e "printf '%.3f', ${sorted[$mid]} / 1000")
    fi

    # P99
    local p99_idx=$(( (count * 99 + 99) / 100 - 1 ))
    if (( p99_idx >= count )); then
        p99_idx=$((count - 1))
    fi
    STAT_P99=$(perl -e "printf '%.3f', ${sorted[$p99_idx]} / 1000")
}

# Store results for final table and JSON
declare -a RESULT_LABELS=()
declare -a RESULT_OC_MEAN=()
declare -a RESULT_OC_MEDIAN=()
declare -a RESULT_OC_P99=()
declare -a RESULT_UP_MEAN=()
declare -a RESULT_UP_MEDIAN=()
declare -a RESULT_UP_P99=()

bench_scenario() {
    local label="$1"
    local oc_cmd="$2"
    local up_cmd="$3"

    echo "--- $label ---"

    echo "  oc-rsync..."
    run_timed "$oc_cmd" "$ITERATIONS" "$WARMUP"
    compute_stats
    local oc_mean="$STAT_MEAN" oc_median="$STAT_MEDIAN" oc_p99="$STAT_P99"
    echo "    mean=${oc_mean}ms  median=${oc_median}ms  p99=${oc_p99}ms"

    echo "  upstream..."
    run_timed "$up_cmd" "$ITERATIONS" "$WARMUP"
    compute_stats
    local up_mean="$STAT_MEAN" up_median="$STAT_MEDIAN" up_p99="$STAT_P99"
    echo "    mean=${up_mean}ms  median=${up_median}ms  p99=${up_p99}ms"

    RESULT_LABELS+=("$label")
    RESULT_OC_MEAN+=("$oc_mean")
    RESULT_OC_MEDIAN+=("$oc_median")
    RESULT_OC_P99+=("$oc_p99")
    RESULT_UP_MEAN+=("$up_mean")
    RESULT_UP_MEDIAN+=("$up_median")
    RESULT_UP_P99+=("$up_p99")
    echo ""
}

bench_scenario "version" \
    "$OC_RSYNC --version" \
    "$UPSTREAM_RSYNC --version"

bench_scenario "help" \
    "$OC_RSYNC --help" \
    "$UPSTREAM_RSYNC --help"

bench_scenario "dry_run_empty" \
    "$OC_RSYNC --dry-run -r ${EMPTY_SRC}/ ${EMPTY_DST}/" \
    "$UPSTREAM_RSYNC --dry-run -r ${EMPTY_SRC}/ ${EMPTY_DST}/"

bench_scenario "dry_run_small" \
    "$OC_RSYNC --dry-run -r ${SMALL_SRC}/ ${SMALL_DST}/" \
    "$UPSTREAM_RSYNC --dry-run -r ${SMALL_SRC}/ ${SMALL_DST}/"

# Print summary table
echo "=== Summary Table ==="
printf "%-20s %12s %12s %10s %12s %12s %10s\n" \
    "Scenario" "oc mean(ms)" "up mean(ms)" "ratio" "oc p99(ms)" "up p99(ms)" "p99 ratio"
printf "%-20s %12s %12s %10s %12s %12s %10s\n" \
    "--------" "-----------" "-----------" "-----" "----------" "----------" "---------"

for (( i = 0; i < ${#RESULT_LABELS[@]}; i++ )); do
    ratio=$(perl -e "printf '%.2f', ${RESULT_OC_MEAN[$i]} / ${RESULT_UP_MEAN[$i]}") 2>/dev/null || ratio="N/A"
    p99_ratio=$(perl -e "printf '%.2f', ${RESULT_OC_P99[$i]} / ${RESULT_UP_P99[$i]}") 2>/dev/null || p99_ratio="N/A"
    printf "%-20s %12s %12s %9sx %12s %12s %8sx\n" \
        "${RESULT_LABELS[$i]}" \
        "${RESULT_OC_MEAN[$i]}" "${RESULT_UP_MEAN[$i]}" "$ratio" \
        "${RESULT_OC_P99[$i]}" "${RESULT_UP_P99[$i]}" "$p99_ratio"
done

# Write JSON output
if [[ -n "$JSON_FILE" ]]; then
    {
        echo "{"
        for (( i = 0; i < ${#RESULT_LABELS[@]}; i++ )); do
            [[ $i -gt 0 ]] && echo ","
            cat <<ENTRY
  "${RESULT_LABELS[$i]}": {
    "oc_rsync": {
      "mean_ms": ${RESULT_OC_MEAN[$i]},
      "median_ms": ${RESULT_OC_MEDIAN[$i]},
      "p99_ms": ${RESULT_OC_P99[$i]},
      "iterations": $ITERATIONS
    },
    "upstream": {
      "mean_ms": ${RESULT_UP_MEAN[$i]},
      "median_ms": ${RESULT_UP_MEDIAN[$i]},
      "p99_ms": ${RESULT_UP_P99[$i]},
      "iterations": $ITERATIONS
    }
  }
ENTRY
        done
        echo ""
        echo "}"
    } > "$JSON_FILE"
    echo ""
    echo "JSON results written to $JSON_FILE"
fi
