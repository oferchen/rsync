#!/bin/bash
# Simple remote rsync:// benchmark without hyperfine dependency.
#
# Compares:
#   - Upstream rsync (system)
#   - oc-rsync v0.5.2
#   - oc-rsync v0.5.3
#   - oc-rsync dev (current codebase)
#
# Tests against public rsync mirrors (GNU, Debian - not kernel.org).
#
# Usage:
#   ./scripts/benchmark_simple.sh [OPTIONS]
#
# Options:
#   -n N    Number of benchmark runs (default: 3)
#   -s S    Server to test: all, debian, gnu-bash, gnu-coreutils (default: all)
#
# Requirements:
#   - System rsync
#   - oc-rsync builds in target/

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Binary paths
UPSTREAM="/usr/bin/rsync"
OC_V052="${PROJECT_ROOT}/target/oc-rsync-v0.5.2"
OC_V053="${PROJECT_ROOT}/target/oc-rsync-v0.5.3"
OC_DEV="${PROJECT_ROOT}/target/release/oc-rsync"

# Defaults
RUNS=3
SERVER="all"

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
  -n N    Number of benchmark runs (default: 3)
  -s S    Server to test: all, debian, gnu-bash, gnu-coreutils (default: all)
  -h      Show this help
EOF
    exit "${1:-0}"
}

while getopts "n:s:h" opt; do
    case $opt in
        n) RUNS="$OPTARG" ;;
        s) SERVER="$OPTARG" ;;
        h) usage 0 ;;
        *) usage 1 ;;
    esac
done
shift $((OPTIND - 1))

check_prereqs() {
    local missing=false

    if [[ ! -x "$UPSTREAM" ]]; then
        echo -e "${RED}ERROR:${NC} System rsync not found at: $UPSTREAM"
        missing=true
    fi

    if [[ ! -x "$OC_V052" ]]; then
        echo -e "${YELLOW}WARN:${NC} oc-rsync v0.5.2 not found at: $OC_V052"
        OC_V052=""
    fi

    if [[ ! -x "$OC_V053" ]]; then
        echo -e "${YELLOW}WARN:${NC} oc-rsync v0.5.3 not found at: $OC_V053"
        OC_V053=""
    fi

    if [[ ! -x "$OC_DEV" ]]; then
        echo -e "${RED}ERROR:${NC} oc-rsync dev not found at: $OC_DEV"
        missing=true
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

# Time a single run and extract stats
time_run() {
    local binary="$1"
    local source="$2"
    local dest="$3"

    rm -rf "$dest"/*
    mkdir -p "$dest"

    local start_time end_time elapsed
    start_time=$(date +%s.%N)

    if "$binary" -a "$source" "$dest/" &>/dev/null; then
        end_time=$(date +%s.%N)
        elapsed=$(echo "$end_time - $start_time" | bc)
        echo "$elapsed"
    else
        echo "FAILED"
    fi
}

# Run benchmark for a single binary
run_binary_benchmark() {
    local name="$1"
    local binary="$2"
    local source="$3"
    local dest="$4"

    local times=()
    local failed=false

    for ((i=1; i<=RUNS; i++)); do
        local result
        result=$(time_run "$binary" "$source" "$dest")
        if [[ "$result" == "FAILED" ]]; then
            failed=true
            break
        fi
        times+=("$result")
    done

    if $failed; then
        printf "  %-20s FAILED\n" "$name:"
        return
    fi

    # Calculate average
    local sum=0
    for t in "${times[@]}"; do
        sum=$(echo "$sum + $t" | bc)
    done
    local avg=$(echo "scale=2; $sum / ${#times[@]}" | bc)

    # Find min and max
    local min="${times[0]}"
    local max="${times[0]}"
    for t in "${times[@]}"; do
        if (( $(echo "$t < $min" | bc -l) )); then min="$t"; fi
        if (( $(echo "$t > $max" | bc -l) )); then max="$t"; fi
    done

    printf "  %-20s avg: %6.2fs  min: %6.2fs  max: %6.2fs\n" "$name:" "$avg" "$min" "$max"
}

main() {
    check_prereqs

    echo "=============================================="
    echo "  Simple Remote rsync:// Benchmark"
    echo "=============================================="
    echo ""
    echo "Runs per test: $RUNS"
    echo ""
    echo "Binaries:"
    echo "  upstream: $($UPSTREAM --version | head -1)"
    [[ -n "$OC_V052" ]] && echo "  oc-rsync-v0.5.2: $($OC_V052 --version | head -1)"
    [[ -n "$OC_V053" ]] && echo "  oc-rsync-v0.5.3: $($OC_V053 --version | head -1)"
    echo "  oc-rsync-dev: $($OC_DEV --version | head -1)"
    echo ""

    # Create temp directory
    local workdir=$(mktemp -d)
    trap "rm -rf '$workdir'" EXIT

    log_section "Testing Server Connectivity"

    # Test servers
    declare -A servers

    if [[ "$SERVER" == "all" || "$SERVER" == "gnu-bash" ]]; then
        if test_server "rsync://ftp.gnu.org/gnu/bash/" "gnu-bash"; then
            servers["gnu-bash"]="rsync://ftp.gnu.org/gnu/bash/"
        fi
    fi

    if [[ "$SERVER" == "all" || "$SERVER" == "gnu-coreutils" ]]; then
        if test_server "rsync://ftp.gnu.org/gnu/coreutils/" "gnu-coreutils"; then
            servers["gnu-coreutils"]="rsync://ftp.gnu.org/gnu/coreutils/"
        fi
    fi

    if [[ "$SERVER" == "all" || "$SERVER" == "debian" ]]; then
        if test_server "rsync://mirror.clarkson.edu/debian/doc/" "debian-doc"; then
            servers["debian-doc"]="rsync://mirror.clarkson.edu/debian/doc/"
        fi
    fi

    if [[ ${#servers[@]} -eq 0 ]]; then
        echo -e "${RED}ERROR:${NC} No servers available"
        exit 1
    fi

    # Benchmark each server
    for name in "${!servers[@]}"; do
        url="${servers[$name]}"

        log_section "Benchmark: $name"
        echo "Source: $url"
        echo ""

        run_binary_benchmark "upstream" "$UPSTREAM" "$url" "$workdir/dest_upstream"

        if [[ -n "$OC_V052" ]]; then
            run_binary_benchmark "oc-rsync-v0.5.2" "$OC_V052" "$url" "$workdir/dest_v052"
        fi

        if [[ -n "$OC_V053" ]]; then
            run_binary_benchmark "oc-rsync-v0.5.3" "$OC_V053" "$url" "$workdir/dest_v053"
        fi

        run_binary_benchmark "oc-rsync-dev" "$OC_DEV" "$url" "$workdir/dest_dev"
    done

    log_section "Summary"
    echo ""
    echo "Benchmark complete."
    echo "Lower times are better."
}

main "$@"
