#!/bin/bash
# Full file list vs incremental flist (INC_RECURSE) memory benchmark.
#
# Measures peak RSS of oc-rsync and upstream rsync when pushing directory
# trees of 100K and 1M files under three flist modes:
#
#   Mode A  full flist            (--no-inc-recursive)
#   Mode B  receiver INC_RECURSE  (default; sender always sends full list)
#   Mode C  sender INC_RECURSE    (pending opt-in flag, see #1862; skipped)
#
# At large directory scales, INC_RECURSE keeps memory bounded by streaming
# file lists in segments rather than buffering the full list. This benchmark
# gives concrete numbers for the current code.
#
# Refs: #1864 flist memory benchmark, #966/#971 RSS gap context.
#
# Usage:
#   scripts/benchmark_flist_memory.sh [--scales 100k|1m|both] [--summary]
#
# Inside the rsync-profile container:
#   podman exec rsync-profile bash /workspace/scripts/benchmark_flist_memory.sh
#
# Environment variables:
#   OC_RSYNC         Path to oc-rsync binary (default: /usr/local/bin/oc-rsync-dev
#                     in container, else target/release/oc-rsync)
#   UPSTREAM_RSYNC   Path to upstream rsync binary (default: rsync)
#   BENCH_ROOT       Where to put fixtures and destinations (default:
#                     /tmp/oc-rsync-bench). MUST NOT be a bind mount.
#   RUNS             Iterations per mode (default: 3, median reported)
#
# Safety: fixture and destination paths live under BENCH_ROOT; cleanup
# refuses to rm anything outside that prefix.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

BENCH_ROOT="${BENCH_ROOT:-/tmp/oc-rsync-bench}"
RUNS="${RUNS:-3}"
SCALES_ARG="both"
EMIT_SUMMARY=0

# Default oc-rsync: prefer container path, then release build, then PATH lookup.
default_oc_rsync() {
    if [[ -x /usr/local/bin/oc-rsync-dev ]]; then
        echo /usr/local/bin/oc-rsync-dev
    elif [[ -x "${PROJECT_ROOT}/target/release/oc-rsync" ]]; then
        echo "${PROJECT_ROOT}/target/release/oc-rsync"
    else
        echo oc-rsync
    fi
}

OC_RSYNC="${OC_RSYNC:-$(default_oc_rsync)}"
UPSTREAM_RSYNC="${UPSTREAM_RSYNC:-rsync}"

usage() {
    sed -n '1,30p' "$0"
}

# Parse args
while (( $# > 0 )); do
    case "$1" in
        --scales)
            SCALES_ARG="${2:-}"
            shift 2 ;;
        --summary)
            EMIT_SUMMARY=1
            shift ;;
        -h|--help)
            usage
            exit 0 ;;
        *)
            echo "Unknown argument: $1" >&2
            usage >&2
            exit 2 ;;
    esac
done

case "$SCALES_ARG" in
    100k) SCALES=("100k") ;;
    1m)   SCALES=("1m") ;;
    both) SCALES=("100k" "1m") ;;
    *)
        echo "ERROR: --scales must be 100k, 1m, or both (got: $SCALES_ARG)" >&2
        exit 2 ;;
esac

# Detect time command. Linux GNU time -v exposes "Maximum resident set size".
detect_time_cmd() {
    if [[ "$(uname)" == "Darwin" ]]; then
        TIME_CMD=(/usr/bin/time -l)
        RSS_PARSE=parse_rss_macos
    elif [[ -x /usr/bin/time ]]; then
        TIME_CMD=(/usr/bin/time -v)
        RSS_PARSE=parse_rss_linux
    else
        echo "ERROR: /usr/bin/time not found. Cannot measure peak RSS." >&2
        exit 1
    fi
}

parse_rss_linux() {
    # Output: "Maximum resident set size (kbytes): NNN"
    grep "Maximum resident set size" "$1" | awk '{print $NF}'
}

parse_rss_macos() {
    # Output: "NNN  maximum resident set size" (bytes)
    grep "maximum resident set size" "$1" | awk '{print int($1 / 1024)}'
}

format_rss_mb() {
    # Round to 1 decimal MB.
    awk -v kb="$1" 'BEGIN { printf "%.1f", kb/1024 }'
}

check_prereqs() {
    if [[ ! -x "$OC_RSYNC" ]] && ! command -v "$OC_RSYNC" >/dev/null 2>&1; then
        echo "ERROR: oc-rsync not found at: $OC_RSYNC" >&2
        exit 1
    fi
    if ! command -v "$UPSTREAM_RSYNC" >/dev/null 2>&1 && [[ ! -x "$UPSTREAM_RSYNC" ]]; then
        echo "ERROR: upstream rsync not found at: $UPSTREAM_RSYNC" >&2
        exit 1
    fi
    if [[ "$BENCH_ROOT" != /tmp/oc-rsync-bench && "$BENCH_ROOT" != /var/tmp/oc-rsync-bench ]]; then
        # Refuse anywhere unusual to prevent rm -rf accidents on bind mounts.
        case "$BENCH_ROOT" in
            /tmp/*|/var/tmp/*) ;;
            *)
                echo "ERROR: BENCH_ROOT must live under /tmp or /var/tmp (got: $BENCH_ROOT)" >&2
                echo "       Refusing to operate to prevent accidental rm -rf on bind mounts." >&2
                exit 1 ;;
        esac
    fi
    mkdir -p "$BENCH_ROOT"
}

# Safe rmdir: only allows paths beneath BENCH_ROOT. Never use rm -rf with
# variable expansion outside this guard.
safe_rm_under_root() {
    local path="$1"
    case "$path" in
        "$BENCH_ROOT"/*)
            rm -rf -- "$path" ;;
        *)
            echo "REFUSING to rm path outside BENCH_ROOT: $path" >&2
            return 1 ;;
    esac
}

# Generate a directory tree of NUM_DIRS x FILES_PER_DIR empty files at $1.
# Empty files keep the focus on file-list memory rather than transfer bytes.
generate_fixture() {
    local root="$1"
    local num_dirs="$2"
    local files_per_dir="$3"
    local total=$((num_dirs * files_per_dir))

    if [[ -d "$root" && -f "$root/.fixture-ok" ]]; then
        local existing
        existing=$(grep -c "^" "$root/.fixture-ok" 2>/dev/null || echo 0)
        if [[ "$existing" == "$total" ]]; then
            echo "  Reusing existing fixture: $root ($total files)"
            return 0
        fi
    fi

    safe_rm_under_root "$root" || true
    mkdir -p "$root"

    echo "  Generating $total files ($num_dirs dirs x $files_per_dir files) at $root..."
    local start
    start=$(date +%s)

    # Use a Python helper for speed; mkdir -p + touch loops are too slow at 1M.
    python3 - "$root" "$num_dirs" "$files_per_dir" <<'PY'
import os, sys, pathlib
root, num_dirs, files_per_dir = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
for d in range(num_dirs):
    dpath = pathlib.Path(root) / f"dir_{d:05d}"
    dpath.mkdir(parents=True, exist_ok=True)
    for f in range(files_per_dir):
        fpath = dpath / f"file_{f:05d}.dat"
        fpath.touch()
PY

    local marker="$root/.fixture-ok"
    : > "$marker"
    local i
    for ((i=0; i<total; i++)); do echo >> "$marker"; done

    local end
    end=$(date +%s)
    echo "    Done in $((end - start))s"
}

# Run rsync once with given args, capture wall-clock and peak RSS of the
# top-level rsync process. For local push the parent is the sender role;
# the receiver/generator children fork off and their RSS is not captured here.
# Local push is still useful for showing total memory footprint scaling with
# file-list size.
run_one() {
    local binary="$1"
    local src="$2"
    local dest="$3"
    shift 3
    local extra_args=("$@")

    safe_rm_under_root "$dest"
    mkdir -p "$dest"

    local stderr_file
    stderr_file=$(mktemp)

    local start end
    start=$(date +%s.%N)
    "${TIME_CMD[@]}" "$binary" -a "${extra_args[@]}" "$src/" "$dest/" \
        >/dev/null 2>"$stderr_file" || true
    end=$(date +%s.%N)

    local wall rss
    wall=$(awk -v s="$start" -v e="$end" 'BEGIN { printf "%.3f", e - s }')
    rss=$($RSS_PARSE "$stderr_file")
    rm -f "$stderr_file"

    if [[ -z "$rss" ]]; then
        rss=0
    fi
    echo "$wall $rss"
}

# Median of N runs. Outputs "median_wall median_rss_kb".
median_run() {
    local label="$1"; shift
    local -a walls=() rsses=()
    local i
    for ((i=0; i<RUNS; i++)); do
        local result
        result=$(run_one "$@")
        walls+=("$(echo "$result" | awk '{print $1}')")
        rsses+=("$(echo "$result" | awk '{print $2}')")
        echo "    [$((i+1))/$RUNS] $label  wall=${walls[$i]}s  rss=$(format_rss_mb "${rsses[$i]}")MB" >&2
    done

    local sorted_w sorted_r mid
    sorted_w=$(printf '%s\n' "${walls[@]}" | sort -n)
    sorted_r=$(printf '%s\n' "${rsses[@]}" | sort -n)
    mid=$(( (RUNS + 1) / 2 ))
    echo "$(echo "$sorted_w" | sed -n "${mid}p") $(echo "$sorted_r" | sed -n "${mid}p")"
}

# Parallel result arrays
declare -a RES_MODE=() RES_BIN=() RES_SCALE=() RES_FILES=() RES_WALL=() RES_RSS=()

record() {
    RES_MODE+=("$1"); RES_BIN+=("$2"); RES_SCALE+=("$3")
    RES_FILES+=("$4"); RES_WALL+=("$5"); RES_RSS+=("$6")
}

# Run all modes against a given binary and scale.
bench_binary() {
    local binary="$1"
    local label="$2"
    local scale="$3"
    local total_files="$4"
    local src="$5"

    local dest_a dest_b dest_c
    dest_a="$BENCH_ROOT/dest-${label}-${scale}-modeA"
    dest_b="$BENCH_ROOT/dest-${label}-${scale}-modeB"
    dest_c="$BENCH_ROOT/dest-${label}-${scale}-modeC"

    echo "  $label / Mode A (full flist, --no-inc-recursive):"
    local r_a
    r_a=$(median_run "$label-A" "$binary" "$src" "$dest_a" --no-inc-recursive)
    record "A_full_flist" "$label" "$scale" "$total_files" \
        "$(echo "$r_a" | awk '{print $1}')" "$(echo "$r_a" | awk '{print $2}')"

    echo "  $label / Mode B (default, receiver INC_RECURSE):"
    local r_b
    r_b=$(median_run "$label-B" "$binary" "$src" "$dest_b")
    record "B_default" "$label" "$scale" "$total_files" \
        "$(echo "$r_b" | awk '{print $1}')" "$(echo "$r_b" | awk '{print $2}')"

    echo "  $label / Mode C (sender INC_RECURSE): SKIPPED, awaiting #1862 opt-in flag"
    record "C_sender_inc_recurse" "$label" "$scale" "$total_files" "n/a" "0"

    safe_rm_under_root "$dest_a" || true
    safe_rm_under_root "$dest_b" || true
    safe_rm_under_root "$dest_c" || true
}

write_tsv() {
    local out="$1"
    {
        printf "mode\tbinary\tscale\tfiles\twall_s\tpeak_rss_mb\n"
        local i
        for ((i=0; i<${#RES_MODE[@]}; i++)); do
            local rss_mb
            if [[ "${RES_RSS[$i]}" == "0" ]]; then
                rss_mb="n/a"
            else
                rss_mb=$(format_rss_mb "${RES_RSS[$i]}")
            fi
            printf "%s\t%s\t%s\t%s\t%s\t%s\n" \
                "${RES_MODE[$i]}" "${RES_BIN[$i]}" "${RES_SCALE[$i]}" \
                "${RES_FILES[$i]}" "${RES_WALL[$i]}" "$rss_mb"
        done
    } > "$out"
}

emit_markdown_summary() {
    echo ""
    echo "## flist memory benchmark"
    echo ""
    echo "| Mode | Binary | Scale | Files | Wall (s) | Peak RSS (MB) |"
    echo "|------|--------|-------|-------|----------|---------------|"
    local i
    for ((i=0; i<${#RES_MODE[@]}; i++)); do
        local rss_mb
        if [[ "${RES_RSS[$i]}" == "0" ]]; then
            rss_mb="n/a"
        else
            rss_mb=$(format_rss_mb "${RES_RSS[$i]}")
        fi
        echo "| ${RES_MODE[$i]} | ${RES_BIN[$i]} | ${RES_SCALE[$i]} | ${RES_FILES[$i]} | ${RES_WALL[$i]} | $rss_mb |"
    done
    echo ""
    echo "Mode A = full flist (--no-inc-recursive); Mode B = default (receiver INC_RECURSE);"
    echo "Mode C = sender INC_RECURSE (pending #1862 opt-in flag)."
}

main() {
    detect_time_cmd
    check_prereqs

    echo "================================================"
    echo "  Full vs Incremental flist memory benchmark"
    echo "================================================"
    echo "  oc-rsync:  $OC_RSYNC"
    echo "  upstream:  $UPSTREAM_RSYNC"
    echo "  BENCH_ROOT: $BENCH_ROOT"
    echo "  Scales:    ${SCALES[*]}"
    echo "  Runs:      $RUNS"
    echo ""
    "$OC_RSYNC" --version 2>/dev/null | head -1 || true
    "$UPSTREAM_RSYNC" --version 2>/dev/null | head -1 || true
    echo ""

    local timestamp
    timestamp=$(date +%Y%m%d-%H%M%S)
    local out_dir="${PROJECT_ROOT}/target/benchmarks"
    mkdir -p "$out_dir"
    local tsv="${out_dir}/flist_memory_${timestamp}.tsv"

    local scale
    for scale in "${SCALES[@]}"; do
        local num_dirs files_per_dir total
        case "$scale" in
            100k)  num_dirs=100;  files_per_dir=1000;  total=100000  ;;
            1m)    num_dirs=1000; files_per_dir=1000;  total=1000000 ;;
        esac

        echo "=== Scale $scale ($total files = $num_dirs dirs x $files_per_dir files) ==="
        local src="$BENCH_ROOT/src-$scale"
        generate_fixture "$src" "$num_dirs" "$files_per_dir"
        echo ""

        bench_binary "$OC_RSYNC" "oc-rsync" "$scale" "$total" "$src"
        bench_binary "$UPSTREAM_RSYNC" "upstream" "$scale" "$total" "$src"
        echo ""
    done

    write_tsv "$tsv"
    echo "Wrote: $tsv"

    if (( EMIT_SUMMARY )); then
        emit_markdown_summary | tee "${tsv%.tsv}.md"
    fi

    echo "Done."
}

main "$@"
