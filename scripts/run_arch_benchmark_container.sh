#!/bin/bash
# Run benchmark + profiling inside an Arch Linux container.
#
# Usage:
#   ./scripts/run_arch_benchmark_container.sh [--runs N] [--json] [--profile]
#
# Builds upstream rsync 3.4.1 + oc-rsync v0.5.8 + oc-rsync HEAD inside
# an Arch Linux container, then runs the full benchmark matrix.
#
# Modes:
#   (default)   Run benchmark: 3 binaries × 5 modes × 3 scenarios
#   --profile   Run profiling: flamegraphs, strace, perf stat

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

RUNS=5
JSON_FLAG=""
PROFILE_MODE=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --runs)     RUNS="$2"; shift 2 ;;
        --json)     JSON_FLAG="--json"; shift ;;
        --profile)  PROFILE_MODE=true; shift ;;
        -h|--help)
            echo "Usage: $0 [--runs N] [--json] [--profile]"
            echo ""
            echo "Options:"
            echo "  --runs N     Number of benchmark runs per test (default: 5)"
            echo "  --json       Output JSON only"
            echo "  --profile    Run profiling (flamegraphs, strace, perf stat)"
            exit 0
            ;;
        *)  echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

CONTAINER_ENGINE="${CONTAINER_ENGINE:-podman}"
IMAGE_NAME="oc-rsync-bench-arch"
RESULTS_DIR="$REPO_ROOT/benchmark-results"

echo "=== oc-rsync Benchmark: Arch Linux Container ==="
echo "Container engine: $CONTAINER_ENGINE"
echo "Runs per test: $RUNS"
echo "Profile mode: $PROFILE_MODE"
echo ""

echo "Building container image (this may take a while)..."
"$CONTAINER_ENGINE" build \
    -t "$IMAGE_NAME" \
    -f "$REPO_ROOT/scripts/Containerfile.benchmark-arch" \
    "$REPO_ROOT"

echo ""

mkdir -p "$RESULTS_DIR"

if $PROFILE_MODE; then
    echo "Running profiling inside container..."
    echo ""
    "$CONTAINER_ENGINE" run --rm --privileged \
        -v "$RESULTS_DIR:/results:Z" \
        "$IMAGE_NAME" \
        "/usr/sbin/sshd && bash /usr/local/bin/profile_hotpaths.sh"
    echo ""
    echo "Profiling results saved to: $RESULTS_DIR/"
else
    echo "Running benchmarks inside container..."
    echo ""
    "$CONTAINER_ENGINE" run --rm --privileged \
        -v "$RESULTS_DIR:/results:Z" \
        -e BENCH_RUNS="$RUNS" \
        "$IMAGE_NAME" \
        "/usr/sbin/sshd && python3 /usr/local/bin/run_arch_benchmark.py --runs ${RUNS} ${JSON_FLAG}"
fi
