#!/bin/bash
# Remote rsync:// benchmark comparing multiple versions.
#
# Compares:
#   - Upstream rsync 3.4.1
#   - oc-rsync v0.5.2
#   - oc-rsync dev (current codebase)
#
# Tests against public rsync mirrors with 50-100MB of data.
#
# Usage:
#   ./scripts/benchmark_remote.sh [OPTIONS]
#
# Options:
#   --warmup N     Number of warmup runs (default: 2)
#   --runs N       Number of benchmark runs (default: 5)
#   --perf         Generate perf profiles for oc-rsync dev
#   --export-json  Export results to JSON
#
# Requirements:
#   - hyperfine: cargo install hyperfine
#   - Upstream rsync 3.4.1
#   - oc-rsync builds

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Binary paths
UPSTREAM="${PROJECT_ROOT}/target/interop/upstream-install/3.4.1/bin/rsync"
OC_V052="${PROJECT_ROOT}/target/oc-rsync-v0.5.2"
OC_DEV="${PROJECT_ROOT}/target/release/oc-rsync"
OC_DEV_DEBUG="${PROJECT_ROOT}/target/release-with-debug/oc-rsync"

# Defaults
WARMUP=2
RUNS=5
USE_PERF=false
EXPORT_JSON=false

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() { echo -e "${GREEN}[INFO]${NC} $1"; }
log_section() { echo -e "\n${BLUE}=== $1 ===${NC}"; }

usage() {
    cat << EOF
Usage: $(basename "$0") [OPTIONS]

Options:
  -w, --warmup N     Number of warmup runs (default: 2)
  -n, --runs N       Number of benchmark runs (default: 5)
  -p, --perf         Generate perf profiles for oc-rsync dev
  -j, --export-json  Export results to JSON
  -h, --help         Show this help
EOF
    exit "${1:-0}"
}

while [[ $# -gt 0 ]]; do
    case $1 in
        -w|--warmup) WARMUP="$2"; shift 2 ;;
        -n|--runs) RUNS="$2"; shift 2 ;;
        -p|--perf) USE_PERF=true; shift ;;
        -j|--export-json) EXPORT_JSON=true; shift ;;
        -h|--help) usage 0 ;;
        *) echo "Unknown option: $1"; usage 1 ;;
    esac
done

check_prereqs() {
    local missing=false

    if ! command -v hyperfine &>/dev/null; then
        echo -e "${RED}ERROR:${NC} hyperfine not found. Install: cargo install hyperfine"
        missing=true
    fi

    if [[ ! -x "$UPSTREAM" ]]; then
        echo -e "${RED}ERROR:${NC} Upstream rsync not found at: $UPSTREAM"
        missing=true
    fi

    if [[ ! -x "$OC_V052" ]]; then
        echo -e "${YELLOW}WARN:${NC} oc-rsync v0.5.2 not found at: $OC_V052"
        echo "  Will skip v0.5.2 comparison"
        OC_V052=""
    fi

    if [[ ! -x "$OC_DEV" ]]; then
        echo -e "${RED}ERROR:${NC} oc-rsync dev not found at: $OC_DEV"
        missing=true
    fi

    # Use debug version for perf profiling if available
    if [[ ! -x "$OC_DEV_DEBUG" ]]; then
        OC_DEV_DEBUG="$OC_DEV"
    fi

    if $missing; then
        exit 1
    fi
}

# Test server connectivity
test_server() {
    local url="$1"
    local name="$2"

    echo -n "  Testing $name... "
    if timeout 10 "$UPSTREAM" --list-only "$url" &>/dev/null; then
        echo "OK"
        return 0
    else
        echo "FAILED"
        return 1
    fi
}

# Run benchmark with hyperfine
run_benchmark() {
    local name="$1"
    local source="$2"
    local dest_base="$3"
    shift 3
    local binaries=("$@")

    local export_args=""
    if $EXPORT_JSON; then
        export_args="--export-json ${PROJECT_ROOT}/benchmark_${name}.json"
    fi

    log_section "Benchmark: $name"
    echo "Source: $source"
    echo ""

    # Build hyperfine command
    local cmd="hyperfine --warmup $WARMUP --runs $RUNS"

    for bin in "${binaries[@]}"; do
        local bin_name=$(basename "$bin")
        local dest="${dest_base}_${bin_name}"
        mkdir -p "$dest"
        cmd="$cmd --command-name '$bin_name' 'rm -rf $dest/* && $bin -a $source $dest/'"
    done

    if [[ -n "$export_args" ]]; then
        cmd="$cmd $export_args"
    fi

    eval "$cmd"
}

# Run perf profile
run_perf_profile() {
    local name="$1"
    local source="$2"
    local dest="$3"

    if ! $USE_PERF; then
        return
    fi

    log_info "Generating perf profile for $name..."
    mkdir -p "$dest"
    rm -rf "$dest"/*

    perf record -g -o "${PROJECT_ROOT}/perf_${name}.data" -- \
        "$OC_DEV_DEBUG" -a "$source" "$dest/" 2>&1 || true

    log_info "Perf data: perf_${name}.data"
    log_info "Analyze with: perf report -i perf_${name}.data"
}

main() {
    check_prereqs

    echo "=============================================="
    echo "  Remote rsync:// Performance Benchmark"
    echo "=============================================="
    echo ""
    echo "Warmup: $WARMUP runs"
    echo "Benchmark: $RUNS runs"
    echo ""
    echo "Binaries:"
    echo "  upstream-rsync: $UPSTREAM"
    [[ -n "$OC_V052" ]] && echo "  oc-rsync-v0.5.2: $OC_V052"
    echo "  oc-rsync-dev: $OC_DEV"
    echo ""

    # Build list of binaries to test
    local binaries=("$UPSTREAM")
    [[ -n "$OC_V052" ]] && binaries+=("$OC_V052")
    binaries+=("$OC_DEV")

    # Create temp directory
    local workdir=$(mktemp -d)
    trap "rm -rf '$workdir'" EXIT

    log_section "Testing Server Connectivity"

    # Test servers
    local servers=()

    if test_server "rsync://rsync.kernel.org/pub/site/" "kernel.org"; then
        servers+=("rsync://rsync.kernel.org/pub/site/|kernel.org")
    fi

    if test_server "rsync://archive.ubuntu.com/ubuntu/pool/main/p/python3.12/" "ubuntu"; then
        servers+=("rsync://archive.ubuntu.com/ubuntu/pool/main/p/python3.12/|ubuntu")
    fi

    if [[ ${#servers[@]} -eq 0 ]]; then
        echo -e "${RED}ERROR:${NC} No servers available"
        exit 1
    fi

    # Benchmark each server
    for server_info in "${servers[@]}"; do
        IFS='|' read -r url name <<< "$server_info"

        # Determine what to download based on server
        local source=""
        local size_desc=""

        case "$name" in
            kernel.org)
                # Download keyring files (~8MB total)
                source="$url"
                size_desc="~8MB (keyring files)"
                ;;
            ubuntu)
                # Download python3.12-dbg package (~48MB)
                source="${url}python3.12-dbg_3.12.3-1_amd64.deb"
                size_desc="~48MB (python3.12-dbg)"
                ;;
        esac

        echo ""
        log_section "Server: $name"
        echo "URL: $source"
        echo "Size: $size_desc"

        run_benchmark \
            "remote_${name}" \
            "$source" \
            "$workdir/dest_${name}" \
            "${binaries[@]}"

        # Run perf profile on dev version
        run_perf_profile "remote_${name}" "$source" "$workdir/perf_dest_${name}"
    done

    # Summary
    log_section "Summary"
    echo ""
    echo "Benchmark complete."

    if $EXPORT_JSON; then
        echo "JSON results: benchmark_remote_*.json"
    fi

    if $USE_PERF; then
        echo "Perf profiles: perf_remote_*.data"
        echo ""
        echo "To analyze bottlenecks:"
        echo "  perf report -i perf_remote_kernel.org.data"
        echo "  perf report -i perf_remote_ubuntu.data"
    fi
}

main "$@"
