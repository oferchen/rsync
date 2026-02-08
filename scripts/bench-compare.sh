#!/bin/bash
# =============================================================================
# bench-compare.sh  --  Compare upstream rsync vs oc-rsync performance
# =============================================================================
#
# Runs side-by-side benchmarks of upstream rsync and oc-rsync across several
# workload patterns: initial sync, incremental sync, dry-run, and delete mode.
#
# This script complements the xtask benchmark command:
#   - xtask benchmark: version-to-version comparison of oc-rsync releases
#     against remote mirrors or local rsync daemon (Rust, uses git worktrees)
#   - bench-compare.sh: upstream rsync vs oc-rsync feature comparison
#     using synthetic local test data (shell, self-contained)
#
# Usage: ./scripts/bench-compare.sh [OPTIONS]
#
# Options:
#   --help          Show this help message
#   --runs N        Number of runs per test (default: 3)
#   --small-count N Number of small files to create (default: 1000)
#   --skip-large    Skip the 100MB large file test
#   --keep-data     Do not remove temp directory on exit
#   --data-dir DIR  Use DIR for test data (default: auto tmpdir)
#   --oc-rsync PATH Path to oc-rsync binary
#   --rsync PATH    Path to upstream rsync binary
#
# Environment variables:
#   OC_RSYNC        Path to oc-rsync binary (overridden by --oc-rsync)
#   UPSTREAM_RSYNC  Path to upstream rsync binary (overridden by --rsync)

set -euo pipefail

# =============================================================================
# Defaults
# =============================================================================
RUNS=3
SMALL_COUNT=1000
SKIP_LARGE=0
KEEP_DATA=0
DATA_DIR=""
OC_RSYNC_BIN="${OC_RSYNC:-}"
UPSTREAM_RSYNC_BIN="${UPSTREAM_RSYNC:-}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# =============================================================================
# Help
# =============================================================================
show_help() {
    # Print the comment block at the top of this script as help text.
    local line
    local started=0
    while IFS= read -r line; do
        case "$line" in
            "#!/bin/bash") continue ;;
            "# "*)
                started=1
                echo "${line#\# }"
                ;;
            "#")
                if (( started )); then echo ""; fi
                ;;
            *)
                if (( started )); then break; fi
                ;;
        esac
    done < "${BASH_SOURCE[0]}"
    exit 0
}

# =============================================================================
# Argument parsing
# =============================================================================
while [[ $# -gt 0 ]]; do
    case "$1" in
        --help|-h)
            show_help
            ;;
        --runs)
            RUNS="$2"; shift 2
            ;;
        --small-count)
            SMALL_COUNT="$2"; shift 2
            ;;
        --skip-large)
            SKIP_LARGE=1; shift
            ;;
        --keep-data)
            KEEP_DATA=1; shift
            ;;
        --data-dir)
            DATA_DIR="$2"; shift 2
            ;;
        --oc-rsync)
            OC_RSYNC_BIN="$2"; shift 2
            ;;
        --rsync)
            UPSTREAM_RSYNC_BIN="$2"; shift 2
            ;;
        *)
            echo "Unknown option: $1" >&2
            echo "Run with --help for usage." >&2
            exit 1
            ;;
    esac
done

# =============================================================================
# Utility functions
# =============================================================================

log_info() {
    echo "[INFO] $*"
}

log_error() {
    echo "[ERROR] $*" >&2
}

die() {
    log_error "$@"
    exit 1
}

# Measure wall-clock time of a command in seconds (fractional).
# Prints elapsed seconds to stdout; command stdout/stderr go to /dev/null.
time_cmd() {
    local start end elapsed
    start=$(date +%s%N 2>/dev/null || date +%s)
    "$@" >/dev/null 2>&1 || true
    end=$(date +%s%N 2>/dev/null || date +%s)
    if [[ ${#start} -gt 10 ]]; then
        # Nanosecond precision available
        elapsed=$(echo "scale=6; ($end - $start) / 1000000000" | bc)
    else
        # Fall back to second precision
        elapsed=$(( end - start ))
    fi
    echo "$elapsed"
}

# Given a list of numbers as arguments, print the median.
median() {
    local -a sorted
    sorted=($(printf '%s\n' "$@" | sort -g))
    local n=${#sorted[@]}
    if (( n == 0 )); then
        echo "0"
        return
    fi
    local mid=$(( n / 2 ))
    if (( n % 2 == 1 )); then
        echo "${sorted[$mid]}"
    else
        echo "scale=6; (${sorted[$mid - 1]} + ${sorted[$mid]}) / 2" | bc
    fi
}

# Format seconds as a fixed-width string like "  1.234s".
fmt_time() {
    local t="$1"
    # Handle bc output that starts with "." (e.g. .123456)
    if [[ "$t" == .* ]]; then
        t="0$t"
    fi
    printf "%7.3fs" "$t"
}

# Compute speedup ratio: upstream_time / oc_time.
# >1.0 means oc-rsync is faster.
compute_speedup() {
    local t_up="$1"
    local t_oc="$2"
    # Guard against division by zero or negligible times
    local is_zero
    is_zero=$(echo "$t_oc <= 0.0005" | bc 2>/dev/null || echo "1")
    if [[ "$is_zero" == "1" ]]; then
        echo "N/A"
    else
        echo "scale=2; $t_up / $t_oc" | bc 2>/dev/null || echo "N/A"
    fi
}

# Print a formatted result row.
print_row() {
    local label="$1"
    local t_up="$2"
    local t_oc="$3"
    local speedup="$4"
    printf "  %-34s | %s | %s | %sx\n" \
        "$label" \
        "$(fmt_time "$t_up")" \
        "$(fmt_time "$t_oc")" \
        "$speedup"
}

# =============================================================================
# Detect binaries
# =============================================================================
detect_binaries() {
    log_info "Detecting binaries..."

    # Upstream rsync
    if [[ -z "$UPSTREAM_RSYNC_BIN" ]]; then
        UPSTREAM_RSYNC_BIN=$(which rsync 2>/dev/null || true)
    fi
    if [[ -z "$UPSTREAM_RSYNC_BIN" ]] || [[ ! -x "$UPSTREAM_RSYNC_BIN" ]]; then
        die "Cannot find upstream rsync. Install rsync or use --rsync PATH."
    fi

    # oc-rsync
    if [[ -z "$OC_RSYNC_BIN" ]]; then
        OC_RSYNC_BIN="$PROJECT_ROOT/target/release/oc-rsync"
    fi
    if [[ ! -x "$OC_RSYNC_BIN" ]]; then
        log_info "oc-rsync not found at $OC_RSYNC_BIN; building release..."
        (cd "$PROJECT_ROOT" && cargo build --release) \
            || die "Failed to build oc-rsync with 'cargo build --release'."
    fi
    if [[ ! -x "$OC_RSYNC_BIN" ]]; then
        die "oc-rsync binary not found at $OC_RSYNC_BIN"
    fi

    echo ""
    echo "  Upstream rsync : $UPSTREAM_RSYNC_BIN"
    echo "    $("$UPSTREAM_RSYNC_BIN" --version 2>&1 | head -1 || echo '(unknown version)')"
    echo ""
    echo "  oc-rsync       : $OC_RSYNC_BIN"
    echo "    $("$OC_RSYNC_BIN" --version 2>&1 | head -1 || echo '(unknown version)')"
    echo ""
}

# =============================================================================
# Create test data
# =============================================================================
create_test_data() {
    local base="$1"
    local src="$base/src"

    log_info "Creating test data under $src ..."

    mkdir -p "$src"

    # -- Small files: SMALL_COUNT x 1KB in nested directories --
    log_info "  $SMALL_COUNT small files (1KB each) in nested dirs..."
    local dirs_count=$(( SMALL_COUNT / 10 ))
    if (( dirs_count < 1 )); then dirs_count=1; fi

    local i
    for i in $(seq 1 "$dirs_count"); do
        mkdir -p "$src/small/dir_${i}"
    done
    for i in $(seq 1 "$SMALL_COUNT"); do
        local dir_idx=$(( (i % dirs_count) + 1 ))
        dd if=/dev/urandom of="$src/small/dir_${dir_idx}/file_${i}.dat" \
            bs=1024 count=1 2>/dev/null
    done

    # -- Medium files: 10 x 1MB --
    log_info "  10 medium files (1MB each)..."
    mkdir -p "$src/medium"
    for i in $(seq 1 10); do
        dd if=/dev/urandom of="$src/medium/medium_${i}.dat" \
            bs=1048576 count=1 2>/dev/null
    done

    # -- Large file: 1 x 100MB --
    if (( SKIP_LARGE == 0 )); then
        log_info "  1 large file (100MB)..."
        mkdir -p "$src/large"
        dd if=/dev/urandom of="$src/large/large_file.dat" \
            bs=1048576 count=100 2>/dev/null
    fi

    log_info "Test data created."
}

# Create a modified copy of the source with ~10% of files changed.
create_modified_data() {
    local base="$1"
    local src="$base/src"
    local mod_src="$base/src-modified"

    log_info "Creating modified copy (~10%% of files changed)..."

    # Start with an exact copy
    cp -a "$src" "$mod_src"

    # Modify ~10% of small files
    local change_count=$(( SMALL_COUNT / 10 ))
    if (( change_count < 1 )); then change_count=1; fi
    local dirs_count=$(( SMALL_COUNT / 10 ))
    if (( dirs_count < 1 )); then dirs_count=1; fi

    local i
    for i in $(seq 1 "$change_count"); do
        local dir_idx=$(( (i % dirs_count) + 1 ))
        local fpath="$mod_src/small/dir_${dir_idx}/file_${i}.dat"
        if [[ -f "$fpath" ]]; then
            dd if=/dev/urandom of="$fpath" bs=1024 count=1 2>/dev/null
        fi
    done

    # Modify 1 medium file
    if [[ -f "$mod_src/medium/medium_1.dat" ]]; then
        dd if=/dev/urandom of="$mod_src/medium/medium_1.dat" \
            bs=1048576 count=1 2>/dev/null
    fi

    log_info "Modified copy created."
}

# =============================================================================
# Benchmark runners
# =============================================================================

# Run a single benchmark scenario for one binary.
# Usage: run_single_bench <binary> <runs> <src> <dst> [rsync_args...]
# Prints the median time.
run_single_bench() {
    local binary="$1"; shift
    local num_runs="$1"; shift
    local src="$1"; shift
    local dst="$1"; shift
    local -a extra_args=("$@")

    local -a times=()
    local run
    for run in $(seq 1 "$num_runs"); do
        rm -rf "$dst"
        mkdir -p "$dst"
        local t
        t=$(time_cmd "$binary" -a "${extra_args[@]}" "$src" "$dst/")
        times+=("$t")
    done
    median "${times[@]}"
}

# Run a benchmark that requires pre-populated destination (no wipe between runs).
# Usage: run_prepopulated_bench <binary> <runs> <seed_src> <sync_src> <dst> [rsync_args...]
# The destination is seeded from seed_src before each run, then sync_src is synced to it.
run_prepopulated_bench() {
    local binary="$1"; shift
    local num_runs="$1"; shift
    local seed_src="$1"; shift
    local sync_src="$1"; shift
    local dst="$1"; shift
    local -a extra_args=("$@")

    local -a times=()
    local run
    for run in $(seq 1 "$num_runs"); do
        rm -rf "$dst"
        mkdir -p "$dst"
        cp -a "$seed_src"/* "$dst/" 2>/dev/null || true
        local t
        t=$(time_cmd "$binary" -a "${extra_args[@]}" "$sync_src" "$dst/")
        times+=("$t")
    done
    median "${times[@]}"
}

# Run a delete-mode benchmark with extra files in destination.
# Usage: run_delete_bench <binary> <runs> <src> <dst>
run_delete_bench() {
    local binary="$1"; shift
    local num_runs="$1"; shift
    local src="$1"; shift
    local dst="$1"; shift

    local -a times=()
    local run
    for run in $(seq 1 "$num_runs"); do
        rm -rf "$dst"
        mkdir -p "$dst"
        # Seed with source data
        cp -a "$src"/* "$dst/" 2>/dev/null || true
        # Add extra files that --delete should remove
        mkdir -p "$dst/extra_stale_dir"
        local j
        for j in $(seq 1 50); do
            dd if=/dev/urandom of="$dst/extra_stale_dir/extra_${j}.dat" \
                bs=1024 count=1 2>/dev/null
        done
        local t
        t=$(time_cmd "$binary" -a --delete "$src" "$dst/")
        times+=("$t")
    done
    median "${times[@]}"
}

# =============================================================================
# Main
# =============================================================================
main() {
    echo "============================================================"
    echo "  bench-compare.sh  --  upstream rsync vs oc-rsync"
    echo "============================================================"
    echo ""

    detect_binaries

    # Set up temp directory
    if [[ -z "$DATA_DIR" ]]; then
        DATA_DIR=$(mktemp -d "${TMPDIR:-/tmp}/bench-compare.XXXXXX")
    else
        mkdir -p "$DATA_DIR"
    fi

    log_info "Data directory : $DATA_DIR"
    log_info "Runs per test  : $RUNS"
    log_info "Small files    : $SMALL_COUNT"
    if (( SKIP_LARGE )); then
        log_info "Large file     : skipped"
    fi
    echo ""

    # Cleanup trap
    if (( KEEP_DATA == 0 )); then
        trap 'log_info "Cleaning up $DATA_DIR ..."; rm -rf "$DATA_DIR"' EXIT
    else
        trap 'log_info "Keeping test data in $DATA_DIR"' EXIT
    fi

    # Create test data
    create_test_data "$DATA_DIR"
    create_modified_data "$DATA_DIR"

    local src="$DATA_DIR/src/"
    local mod_src="$DATA_DIR/src-modified/"
    local dst_base="$DATA_DIR/dst"
    mkdir -p "$dst_base"

    echo ""
    echo "============================================================"
    echo "  Results  (median of $RUNS runs)"
    echo "============================================================"
    echo ""
    printf "  %-34s | %9s | %9s | %s\n" \
        "Test" "upstream" "oc-rsync" "speedup"
    printf "  %s\n" "$(printf -- '-%.0s' $(seq 1 76))"

    # --- 1. Initial sync (empty destination) ---
    local t_up t_oc spd
    t_up=$(run_single_bench "$UPSTREAM_RSYNC_BIN" "$RUNS" "$src" "$dst_base/init_up")
    t_oc=$(run_single_bench "$OC_RSYNC_BIN" "$RUNS" "$src" "$dst_base/init_oc")
    spd=$(compute_speedup "$t_up" "$t_oc")
    print_row "Initial sync (full)" "$t_up" "$t_oc" "$spd"

    # --- 2. Incremental sync (10% changed) ---
    t_up=$(run_prepopulated_bench "$UPSTREAM_RSYNC_BIN" "$RUNS" \
        "$DATA_DIR/src" "$mod_src" "$dst_base/incr_up")
    t_oc=$(run_prepopulated_bench "$OC_RSYNC_BIN" "$RUNS" \
        "$DATA_DIR/src" "$mod_src" "$dst_base/incr_oc")
    spd=$(compute_speedup "$t_up" "$t_oc")
    print_row "Incremental sync (10% changed)" "$t_up" "$t_oc" "$spd"

    # --- 3. Dry-run ---
    t_up=$(run_single_bench "$UPSTREAM_RSYNC_BIN" "$RUNS" "$src" "$dst_base/dry_up" "-n")
    t_oc=$(run_single_bench "$OC_RSYNC_BIN" "$RUNS" "$src" "$dst_base/dry_oc" "-n")
    spd=$(compute_speedup "$t_up" "$t_oc")
    print_row "Dry-run (-n)" "$t_up" "$t_oc" "$spd"

    # --- 4. Delete mode ---
    t_up=$(run_delete_bench "$UPSTREAM_RSYNC_BIN" "$RUNS" \
        "$DATA_DIR/src" "$dst_base/del_up")
    t_oc=$(run_delete_bench "$OC_RSYNC_BIN" "$RUNS" \
        "$DATA_DIR/src" "$dst_base/del_oc")
    spd=$(compute_speedup "$t_up" "$t_oc")
    print_row "Delete mode (--delete)" "$t_up" "$t_oc" "$spd"

    echo ""
    printf "  %s\n" "$(printf -- '-%.0s' $(seq 1 76))"
    echo ""
    echo "  Speedup > 1.00x  =>  oc-rsync is faster than upstream rsync"
    echo "  Speedup < 1.00x  =>  upstream rsync is faster than oc-rsync"
    echo ""
    echo "  Each test ran $RUNS time(s); the median time is reported."
    echo ""
    log_info "Done."
}

main
