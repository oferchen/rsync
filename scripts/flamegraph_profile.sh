#!/bin/bash
# Generate flamegraph CPU profiles for oc-rsync.
#
# Creates SVG flamegraphs showing where CPU time is spent during transfers.
# Requires debug symbols (use release-with-debug profile).
#
# Usage:
#   ./scripts/flamegraph_profile.sh [OPTIONS]
#
# Options:
#   --scenario S   Scenario to profile (small_files|large_file|mixed_tree)
#   --output FILE  Output SVG file (default: flamegraph_<scenario>.svg)
#   --freq N       Sampling frequency in Hz (default: 999)
#
# Requirements:
#   - cargo-flamegraph: cargo install flamegraph
#   - perf: linux-tools-common (or equivalent)
#   - oc-rsync built with: cargo build --profile release-with-debug

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OC_RSYNC="${PROJECT_ROOT}/target/release-with-debug/oc-rsync"

# Defaults
SCENARIO="small_files"
OUTPUT=""
FREQ=999

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --scenario) SCENARIO="$2"; shift 2 ;;
        --output) OUTPUT="$2"; shift 2 ;;
        --freq) FREQ="$2"; shift 2 ;;
        --help|-h)
            head -18 "$0" | tail -n +2 | sed 's/^# //' | sed 's/^#//'
            exit 0
            ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

# Set default output
if [[ -z "$OUTPUT" ]]; then
    OUTPUT="${PROJECT_ROOT}/flamegraph_${SCENARIO}.svg"
fi

# Check prerequisites
check_prereqs() {
    if ! command -v flamegraph &>/dev/null; then
        echo "ERROR: flamegraph not found. Install with: cargo install flamegraph"
        exit 1
    fi

    if [[ ! -x "$OC_RSYNC" ]]; then
        echo "ERROR: oc-rsync with debug symbols not found at: $OC_RSYNC"
        echo "Build with: cargo build --profile release-with-debug"
        exit 1
    fi

    # Check if perf is available and has permissions
    if ! perf record -h &>/dev/null 2>&1; then
        echo "WARNING: perf may require elevated privileges."
        echo "Try: sudo sysctl -w kernel.perf_event_paranoid=1"
    fi
}

# Setup test data
setup_small_files() {
    local dir="$1"
    echo "Creating 1000 x 1KB files..."
    for i in $(seq 1 1000); do
        dd if=/dev/urandom of="$dir/file_$i.dat" bs=1024 count=1 2>/dev/null
    done
}

setup_large_file() {
    local dir="$1"
    echo "Creating 100MB file..."
    dd if=/dev/urandom of="$dir/large.dat" bs=1M count=100 2>/dev/null
}

setup_mixed_tree() {
    local dir="$1"
    echo "Creating 20 dirs x 50 files..."
    for d in $(seq 1 20); do
        mkdir -p "$dir/dir_$d"
        for f in $(seq 1 50); do
            echo "Content for dir $d file $f" > "$dir/dir_$d/file_$f.txt"
        done
    done
}

# Main
main() {
    check_prereqs

    local workdir=$(mktemp -d)
    local src="$workdir/src"
    local dest="$workdir/dest"
    mkdir -p "$src" "$dest"

    trap "rm -rf '$workdir'" EXIT

    echo "=============================================="
    echo "  Flamegraph Profiling: $SCENARIO"
    echo "=============================================="
    echo ""
    echo "Binary: $OC_RSYNC"
    echo "Output: $OUTPUT"
    echo "Frequency: ${FREQ} Hz"
    echo ""

    # Setup test data
    case "$SCENARIO" in
        small_files) setup_small_files "$src" ;;
        large_file) setup_large_file "$src" ;;
        mixed_tree) setup_mixed_tree "$src" ;;
        *)
            echo "Unknown scenario: $SCENARIO"
            echo "Valid: small_files, large_file, mixed_tree"
            exit 1
            ;;
    esac

    echo ""
    echo "Generating flamegraph..."

    # Run flamegraph
    flamegraph \
        --output "$OUTPUT" \
        --freq "$FREQ" \
        -- "$OC_RSYNC" -a "$src/" "$dest/"

    echo ""
    echo "Flamegraph saved to: $OUTPUT"
    echo ""
    echo "Open in browser to analyze CPU hotspots."
    echo "Look for tall towers = hot code paths."
}

main "$@"
