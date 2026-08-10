#!/usr/bin/env bash
# zsync-style large-dataset benchmark (#2082).
#
# Simulates a real-world zsync workload: a 10 GB sparse VM disk image with
# ~1% of bytes modified between basis and target. Measures the cost of
# delta-updating the destination from the source via oc-rsync (and,
# optionally, upstream rsync) so release qualification can sanity-check
# the match-index + rolling-hash pipeline at scale.
#
# This benchmark is intentionally gated behind OC_RSYNC_LARGE_BENCH=1:
# generating the 10 GB corpus and running both binaries takes several
# minutes and ~22 GB of scratch space, which is too heavy for default
# CI runs. The companion harness `tools/ci/run_zsync_large_bench.sh`
# wraps this script for release qualification jobs.
#
# Usage:
#   OC_RSYNC_LARGE_BENCH=1 ./scripts/zsync_bench_large_dataset.sh
#
# Environment variables (all optional):
#   OC_RSYNC_LARGE_BENCH   Required: must be set to "1" or the script
#                          exits 0 with a skip message.
#   OC_RSYNC               Path to oc-rsync binary.
#                          Default: target/release/oc-rsync
#   UPSTREAM_RSYNC         Path to upstream rsync. Auto-detected from
#                          target/interop/upstream-install/{3.4.2,3.4.1}.
#                          Set to "skip" to omit the upstream comparison.
#   BENCH_SIZE_GB          Basis file size in GiB. Default: 10.
#   BENCH_MODIFY_PCT       Percentage of bytes to flip. Default: 1.
#   BENCH_SECTOR_KB        Sector size for the sparse layout (KiB).
#                          Default: 64. Smaller sectors stress rolling-
#                          hash throughput; larger sectors increase the
#                          run-of-zeros density.
#   BENCH_FILL_RATIO       Fraction of sectors filled with random data
#                          versus left as zero runs. Default: 0.35
#                          (mirrors a moderately used VM image).
#   BLOCK_SIZE             rsync --block-size value in bytes.
#                          Default: 131072 (128 KiB).
#   RESULTS_DIR            Where to write the result JSON/markdown.
#                          Default: target/benchmarks
#
# Output:
#   - Wall-clock time per binary
#   - Bytes transferred (parsed from --stats)
#   - Peak RSS (`/usr/bin/time -v` or -l on macOS)
#   - JSON + Markdown summary in $RESULTS_DIR
#
# Scratch space is created under ${TMPDIR:-/tmp}/oc-rsync-zsync-large.$$
# and removed on exit. The cleanup trap only removes that one directory
# (no variable-expanded rm -rf into bind-mounted paths).

set -euo pipefail

# --- Gate -------------------------------------------------------------------

if [[ "${OC_RSYNC_LARGE_BENCH:-0}" != "1" ]]; then
    cat <<'MSG'
[zsync_bench_large_dataset] skipped: OC_RSYNC_LARGE_BENCH is not set to 1.
This benchmark generates a 10 GB sparse VM image (and modifies ~1% of it),
which takes several minutes and ~22 GB of scratch space. It is intended
for release qualification, not default CI runs.

To enable, run:
    OC_RSYNC_LARGE_BENCH=1 ./scripts/zsync_bench_large_dataset.sh
MSG
    exit 0
fi

# --- Configuration ----------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

OC_RSYNC="${OC_RSYNC:-${PROJECT_ROOT}/target/release/oc-rsync}"

if [[ -z "${UPSTREAM_RSYNC:-}" ]]; then
    for candidate in \
        "${PROJECT_ROOT}/target/interop/upstream-install/3.4.2/bin/rsync" \
        "${PROJECT_ROOT}/target/interop/upstream-install/3.4.1/bin/rsync" \
        "/usr/bin/rsync"; do
        if [[ -x "$candidate" ]]; then
            UPSTREAM_RSYNC="$candidate"
            break
        fi
    done
fi
UPSTREAM_RSYNC="${UPSTREAM_RSYNC:-skip}"

BENCH_SIZE_GB="${BENCH_SIZE_GB:-10}"
BENCH_MODIFY_PCT="${BENCH_MODIFY_PCT:-1}"
BENCH_SECTOR_KB="${BENCH_SECTOR_KB:-64}"
BENCH_FILL_RATIO="${BENCH_FILL_RATIO:-0.35}"
BLOCK_SIZE="${BLOCK_SIZE:-131072}"
RESULTS_DIR="${RESULTS_DIR:-${PROJECT_ROOT}/target/benchmarks}"

BENCH_SIZE_BYTES=$(( BENCH_SIZE_GB * 1024 * 1024 * 1024 ))
SECTOR_SIZE_BYTES=$(( BENCH_SECTOR_KB * 1024 ))
TOTAL_SECTORS=$(( BENCH_SIZE_BYTES / SECTOR_SIZE_BYTES ))
MODIFY_BYTES=$(( BENCH_SIZE_BYTES * BENCH_MODIFY_PCT / 100 ))
MODIFY_PATCH_COUNT="${MODIFY_PATCH_COUNT:-1000}"
MODIFY_PATCH_SIZE=$(( MODIFY_BYTES / MODIFY_PATCH_COUNT ))

# Scratch directory (PID-suffixed, never bind-mounted)
SCRATCH="${TMPDIR:-/tmp}/oc-rsync-zsync-large.$$"

cleanup() {
    # Only ever remove the specific scratch directory we created above.
    if [[ -n "${SCRATCH:-}" && -d "$SCRATCH" && "$SCRATCH" == *"/oc-rsync-zsync-large."* ]]; then
        rm -rf "$SCRATCH"
    fi
}
trap cleanup EXIT INT TERM

# --- Platform helpers -------------------------------------------------------

setup_time_cmd() {
    if [[ "$(uname)" == "Darwin" ]]; then
        TIME_CMD=(/usr/bin/time -l)
        RSS_UNIT="bytes"
    else
        TIME_CMD=(/usr/bin/time -v)
        RSS_UNIT="kb"
    fi
}

extract_peak_rss_kb() {
    local timefile="$1"
    if [[ "$RSS_UNIT" == "bytes" ]]; then
        local bytes
        bytes=$(grep -E "maximum resident set size" "$timefile" | awk '{print $1}')
        echo $(( bytes / 1024 ))
    else
        grep -E "Maximum resident set size" "$timefile" | awk '{print $NF}'
    fi
}

extract_transferred_bytes() {
    # Parse rsync --stats output: "Total bytes sent: N" lines.
    # Returns sent + received (the wire bytes that crossed the delta).
    local statsfile="$1"
    local sent recv
    sent=$(grep -E "Total bytes sent" "$statsfile" | head -1 \
        | sed -E 's/[^0-9]//g' || echo 0)
    recv=$(grep -E "Total bytes received" "$statsfile" | head -1 \
        | sed -E 's/[^0-9]//g' || echo 0)
    sent=${sent:-0}
    recv=${recv:-0}
    echo $(( sent + recv ))
}

# --- Corpus generation ------------------------------------------------------

# Generates a sparse "VM disk image" style file:
#   - Allocates BENCH_SIZE_BYTES of logical space by seeking to EOF.
#   - Writes random sectors at deterministic offsets covering
#     BENCH_FILL_RATIO of the address space; the remainder stays as
#     filesystem-level holes (sparse zero regions).
# Output is reproducible across runs because offsets come from a linear
# congruential walk seeded from a fixed constant.
generate_basis() {
    local path="$1"
    echo "[basis] generating ${BENCH_SIZE_GB} GiB sparse VM-like image at $path"
    : > "$path"

    # Reserve logical size via a single seek+write of one byte at EOF.
    dd if=/dev/zero of="$path" bs=1 count=1 \
        seek=$(( BENCH_SIZE_BYTES - 1 )) conv=notrunc 2>/dev/null

    local filled
    filled=$(awk "BEGIN {printf \"%d\", $TOTAL_SECTORS * $BENCH_FILL_RATIO}")
    echo "[basis] writing $filled / $TOTAL_SECTORS sectors (${BENCH_SECTOR_KB} KiB each)"

    # Use awk to emit deterministic offsets without invoking dd 35000+ times.
    # Each iteration writes one sector of random data.
    local seed=2654435761
    local mod=$TOTAL_SECTORS
    local i offset
    for (( i = 0; i < filled; i++ )); do
        # Linear congruential walk modulo TOTAL_SECTORS
        seed=$(( (seed * 1103515245 + 12345) & 2147483647 ))
        offset=$(( (seed % mod) * SECTOR_SIZE_BYTES ))
        dd if=/dev/urandom of="$path" bs="$SECTOR_SIZE_BYTES" count=1 \
            seek="$offset" conv=notrunc oflag=seek_bytes 2>/dev/null
        if (( i % 1000 == 0 )); then
            printf "  [basis] %d / %d sectors written\r" "$i" "$filled" >&2
        fi
    done
    printf "\n" >&2
    echo "[basis] done. logical=$(du -h --apparent-size "$path" 2>/dev/null | cut -f1 || du -h "$path" | cut -f1), allocated=$(du -h "$path" | cut -f1)"
}

# Creates a "1% modified" target by copying the basis (preserving sparseness
# where possible) and patching MODIFY_PATCH_COUNT scattered offsets with
# random bytes.
generate_target() {
    local basis="$1"
    local target="$2"
    echo "[target] cloning basis -> target (preserves sparseness on supported FS)"
    cp --sparse=always "$basis" "$target" 2>/dev/null || cp "$basis" "$target"

    echo "[target] applying $MODIFY_PATCH_COUNT patches (~$MODIFY_PATCH_SIZE B each, ~${BENCH_MODIFY_PCT}% total)"
    local max_offset=$(( BENCH_SIZE_BYTES - MODIFY_PATCH_SIZE ))
    local seed=1442695041
    local mod=$max_offset
    local i offset
    for (( i = 0; i < MODIFY_PATCH_COUNT; i++ )); do
        seed=$(( (seed * 1103515245 + 12345) & 2147483647 ))
        offset=$(( seed % mod ))
        dd if=/dev/urandom of="$target" bs="$MODIFY_PATCH_SIZE" count=1 \
            seek="$offset" conv=notrunc oflag=seek_bytes 2>/dev/null
    done
    touch "$target"
    echo "[target] done. allocated=$(du -h "$target" | cut -f1)"
}

# --- Benchmark runner -------------------------------------------------------

# Run a single benchmark.
# Args: $1 = label, $2 = binary, $3 = src (basis side), $4 = dest (target side)
run_delta() {
    local label="$1" binary="$2" src="$3" dest="$4"

    local timefile statsfile
    timefile=$(mktemp "${SCRATCH}/time.XXXXXX")
    statsfile=$(mktemp "${SCRATCH}/stats.XXXXXX")

    local start end duration_ns duration_s
    start=$(date +%s%N 2>/dev/null || python3 -c "import time; print(int(time.time()*1e9))")
    "${TIME_CMD[@]}" "$binary" \
        --inplace \
        --no-whole-file \
        --stats \
        --block-size="$BLOCK_SIZE" \
        "$src" "$dest" >"$statsfile" 2>"$timefile" || true
    end=$(date +%s%N 2>/dev/null || python3 -c "import time; print(int(time.time()*1e9))")

    duration_ns=$(( end - start ))
    duration_s=$(awk "BEGIN {printf \"%.3f\", $duration_ns / 1000000000.0}")
    local peak_rss_kb transferred
    peak_rss_kb=$(extract_peak_rss_kb "$timefile")
    transferred=$(extract_transferred_bytes "$statsfile")

    echo "  [$label] wall=${duration_s}s rss=$(( peak_rss_kb / 1024 ))MB transferred=$(( transferred / 1024 / 1024 ))MB"
    printf '%s\t%s\t%s\t%s\n' "$label" "$duration_s" "$peak_rss_kb" "$transferred"
}

# --- Main -------------------------------------------------------------------

main() {
    setup_time_cmd
    mkdir -p "$SCRATCH" "$RESULTS_DIR"

    if [[ ! -x "$OC_RSYNC" ]]; then
        echo "ERROR: oc-rsync not found at: $OC_RSYNC" >&2
        echo "Build with: cargo build --release" >&2
        exit 1
    fi

    echo "=============================================="
    echo "  zsync-style large-dataset benchmark (#2082)"
    echo "=============================================="
    echo "Scratch dir:      $SCRATCH"
    echo "Basis size:       ${BENCH_SIZE_GB} GiB"
    echo "Modify fraction:  ${BENCH_MODIFY_PCT}%"
    echo "Sector size:      ${BENCH_SECTOR_KB} KiB"
    echo "Fill ratio:       $BENCH_FILL_RATIO"
    echo "Block size:       $BLOCK_SIZE bytes"
    echo "oc-rsync:         $OC_RSYNC ($("$OC_RSYNC" --version 2>/dev/null | head -1 || echo 'unknown'))"
    if [[ "$UPSTREAM_RSYNC" == "skip" || ! -x "$UPSTREAM_RSYNC" ]]; then
        echo "upstream rsync:   <skipped> (set UPSTREAM_RSYNC to compare)"
        UPSTREAM_RSYNC="skip"
    else
        echo "upstream rsync:   $UPSTREAM_RSYNC ($("$UPSTREAM_RSYNC" --version 2>/dev/null | head -1 || echo 'unknown'))"
    fi
    echo ""

    local basis="$SCRATCH/basis.img"
    local target_oc="$SCRATCH/target_oc.img"
    local target_up="$SCRATCH/target_up.img"

    generate_basis "$basis"
    generate_target "$basis" "$target_oc"
    if [[ "$UPSTREAM_RSYNC" != "skip" ]]; then
        cp --sparse=always "$target_oc" "$target_up" 2>/dev/null || cp "$target_oc" "$target_up"
    fi
    echo ""

    local result_tsv="$RESULTS_DIR/zsync_large_$(date +%Y%m%d_%H%M%S).tsv"
    {
        printf 'binary\twall_s\tpeak_rss_kb\ttransferred_bytes\n'
        echo "=== Phase: delta update (basis -> target, ~${BENCH_MODIFY_PCT}% modified) ==="
        echo "[oc-rsync] running delta sync..."
        run_delta "oc-rsync" "$OC_RSYNC" "$basis" "$target_oc"

        if [[ "$UPSTREAM_RSYNC" != "skip" ]]; then
            echo "[upstream] running delta sync..."
            run_delta "upstream" "$UPSTREAM_RSYNC" "$basis" "$target_up"
        fi
    } | tee "$result_tsv"

    echo ""
    echo "Results written to: $result_tsv"
}

main "$@"
