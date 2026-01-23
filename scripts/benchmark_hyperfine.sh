#!/bin/bash
# Hyperfine benchmark script for statistically rigorous performance comparison.
#
# Compares oc-rsync against upstream rsync using hyperfine's statistical analysis.
# Supports multiple scenarios and exports results in JSON/markdown formats.
#
# Usage:
#   ./scripts/benchmark_hyperfine.sh [OPTIONS]
#
# Options:
#   --warmup N     Number of warmup runs (default: 3)
#   --runs N       Number of benchmark runs (default: 10)
#   --export-json  Export results to JSON file
#   --export-md    Export results to markdown file
#   --scenario S   Run specific scenario (small_files|large_file|mixed_tree|local_copy)
#
# Requirements:
#   - hyperfine: cargo install hyperfine
#   - Upstream rsync built in target/interop/upstream-install/3.4.1/
#   - oc-rsync built with: cargo build --release

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
UPSTREAM="${PROJECT_ROOT}/target/interop/upstream-install/3.4.1/bin/rsync"
OC_RSYNC="${PROJECT_ROOT}/target/release/oc-rsync"

# Defaults
WARMUP=3
RUNS=10
EXPORT_JSON=false
EXPORT_MD=false
SCENARIO="all"

usage() {
    cat << EOF
Usage: $(basename "$0") [OPTIONS]

Options:
  -w, --warmup N       Number of warmup runs (default: 3)
  -n, --runs N         Number of benchmark runs (default: 10)
  -j, --export-json    Export results to JSON file
  -m, --export-md      Export results to markdown file
  -s, --scenario S     Run specific scenario: small_files|large_file|mixed_tree|local_copy|all
  -h, --help           Show this help message

Requirements:
  - hyperfine: cargo install hyperfine
  - Upstream rsync built in target/interop/upstream-install/3.4.1/
  - oc-rsync built with: cargo build --release
EOF
    exit "${1:-0}"
}

# Parse options using getopts for short options, manual for long options
while [[ $# -gt 0 ]]; do
    case $1 in
        -w|--warmup) WARMUP="$2"; shift 2 ;;
        -n|--runs) RUNS="$2"; shift 2 ;;
        -j|--export-json) EXPORT_JSON=true; shift ;;
        -m|--export-md) EXPORT_MD=true; shift ;;
        -s|--scenario) SCENARIO="$2"; shift 2 ;;
        -h|--help) usage 0 ;;
        --) shift; break ;;
        -*) echo "Unknown option: $1"; usage 1 ;;
        *) break ;;
    esac
done

# Check prerequisites
check_prereqs() {
    if ! command -v hyperfine &>/dev/null; then
        echo "ERROR: hyperfine not found. Install with: cargo install hyperfine"
        exit 1
    fi

    if [[ ! -x "$UPSTREAM" ]]; then
        echo "ERROR: Upstream rsync not found at: $UPSTREAM"
        echo "Run: ./scripts/build_upstream.sh"
        exit 1
    fi

    if [[ ! -x "$OC_RSYNC" ]]; then
        echo "ERROR: oc-rsync not found at: $OC_RSYNC"
        echo "Run: cargo build --release"
        exit 1
    fi
}

# Setup test data
setup_small_files() {
    local dir="$1"
    for i in $(seq 1 1000); do
        dd if=/dev/urandom of="$dir/file_$i.dat" bs=1024 count=1 2>/dev/null
    done
}

setup_large_file() {
    local dir="$1"
    dd if=/dev/urandom of="$dir/large.dat" bs=1M count=100 2>/dev/null
}

setup_mixed_tree() {
    local dir="$1"
    for d in $(seq 1 20); do
        mkdir -p "$dir/dir_$d"
        for f in $(seq 1 50); do
            echo "Content for dir $d file $f" > "$dir/dir_$d/file_$f.txt"
        done
    done
}

# Run hyperfine benchmark
run_hyperfine() {
    local name="$1"
    local setup_cmd="$2"
    local upstream_cmd="$3"
    local oc_cmd="$4"

    local export_args=""
    if $EXPORT_JSON; then
        export_args="$export_args --export-json ${PROJECT_ROOT}/benchmark_${name}.json"
    fi
    if $EXPORT_MD; then
        export_args="$export_args --export-markdown ${PROJECT_ROOT}/benchmark_${name}.md"
    fi

    echo ""
    echo "=== Benchmark: $name ==="
    echo ""

    hyperfine \
        --warmup "$WARMUP" \
        --runs "$RUNS" \
        --setup "$setup_cmd" \
        --command-name "upstream-rsync" "$upstream_cmd" \
        --command-name "oc-rsync" "$oc_cmd" \
        $export_args
}

# Benchmark scenarios
benchmark_small_files() {
    local workdir=$(mktemp -d)
    local src="$workdir/src"
    local dest_up="$workdir/dest_up"
    local dest_oc="$workdir/dest_oc"
    mkdir -p "$src" "$dest_up" "$dest_oc"

    echo "Setting up 1000 x 1KB files..."
    setup_small_files "$src"

    run_hyperfine \
        "small_files" \
        "rm -rf $dest_up/* $dest_oc/*" \
        "$UPSTREAM -a $src/ $dest_up/" \
        "$OC_RSYNC -a $src/ $dest_oc/"

    rm -rf "$workdir"
}

benchmark_large_file() {
    local workdir=$(mktemp -d)
    local src="$workdir/src"
    local dest_up="$workdir/dest_up"
    local dest_oc="$workdir/dest_oc"
    mkdir -p "$src" "$dest_up" "$dest_oc"

    echo "Setting up 100MB file..."
    setup_large_file "$src"

    run_hyperfine \
        "large_file" \
        "rm -rf $dest_up/* $dest_oc/*" \
        "$UPSTREAM -a $src/ $dest_up/" \
        "$OC_RSYNC -a $src/ $dest_oc/"

    rm -rf "$workdir"
}

benchmark_mixed_tree() {
    local workdir=$(mktemp -d)
    local src="$workdir/src"
    local dest_up="$workdir/dest_up"
    local dest_oc="$workdir/dest_oc"
    mkdir -p "$src" "$dest_up" "$dest_oc"

    echo "Setting up 20 dirs x 50 files..."
    setup_mixed_tree "$src"

    run_hyperfine \
        "mixed_tree" \
        "rm -rf $dest_up/* $dest_oc/*" \
        "$UPSTREAM -a $src/ $dest_up/" \
        "$OC_RSYNC -a $src/ $dest_oc/"

    rm -rf "$workdir"
}

benchmark_local_copy() {
    local workdir=$(mktemp -d)
    local src="$workdir/src"
    local dest_up="$workdir/dest_up"
    local dest_oc="$workdir/dest_oc"
    mkdir -p "$src" "$dest_up" "$dest_oc"

    echo "Setting up local copy test (1000 files)..."
    setup_small_files "$src"

    run_hyperfine \
        "local_copy" \
        "rm -rf $dest_up/* $dest_oc/*" \
        "$UPSTREAM -a --no-compress $src/ $dest_up/" \
        "$OC_RSYNC -a --no-compress $src/ $dest_oc/"

    rm -rf "$workdir"
}

# Main
main() {
    check_prereqs

    echo "=============================================="
    echo "  Hyperfine Benchmark: oc-rsync vs upstream"
    echo "=============================================="
    echo ""
    echo "Warmup runs: $WARMUP"
    echo "Benchmark runs: $RUNS"
    echo "Upstream: $UPSTREAM"
    echo "oc-rsync: $OC_RSYNC"

    case "$SCENARIO" in
        all)
            benchmark_small_files
            benchmark_large_file
            benchmark_mixed_tree
            benchmark_local_copy
            ;;
        small_files) benchmark_small_files ;;
        large_file) benchmark_large_file ;;
        mixed_tree) benchmark_mixed_tree ;;
        local_copy) benchmark_local_copy ;;
        *)
            echo "Unknown scenario: $SCENARIO"
            echo "Valid: small_files, large_file, mixed_tree, local_copy, all"
            exit 1
            ;;
    esac

    echo ""
    echo "Benchmark complete."
    if $EXPORT_JSON; then
        echo "JSON results: benchmark_*.json"
    fi
    if $EXPORT_MD; then
        echo "Markdown results: benchmark_*.md"
    fi
}

main "$@"
