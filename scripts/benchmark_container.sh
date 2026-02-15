#!/bin/bash
# Run performance benchmark inside a podman/docker Linux container.
#
# Usage: ./scripts/benchmark_container.sh [--runs N]
#
# Builds oc-rsync (release) and upstream rsync 3.4.1 inside a Debian container,
# then runs the benchmark suite and prints the report.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RUNS="${1:-5}"

# Accept --runs N
if [[ "${1:-}" == "--runs" ]]; then
    RUNS="${2:-5}"
fi

CONTAINER_ENGINE="${CONTAINER_ENGINE:-podman}"
IMAGE_NAME="oc-rsync-bench"
CONTAINER_NAME="oc-rsync-bench-run"

echo "=== oc-rsync vs upstream rsync 3.4.1 — Linux Container Benchmark ==="
echo "Container engine: $CONTAINER_ENGINE"
echo "Runs per test: $RUNS"
echo ""

# Build the benchmark container image
echo "Building container image..."
"$CONTAINER_ENGINE" build \
    -t "$IMAGE_NAME" \
    -f "$REPO_ROOT/scripts/Containerfile.benchmark" \
    "$REPO_ROOT"

echo ""
echo "Running benchmarks inside container..."
echo ""

# Run the benchmark
"$CONTAINER_ENGINE" run --rm \
    --name "$CONTAINER_NAME" \
    -e BENCH_RUNS="$RUNS" \
    "$IMAGE_NAME"
