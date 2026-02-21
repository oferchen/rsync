#!/bin/bash
# Run benchmark inside a podman/docker container.
#
# Usage: ./scripts/run_full_benchmark_container.sh [--runs N] [--json]
#
# Builds upstream rsync 3.4.1 + oc-rsync (current HEAD) inside
# an Arch Linux container, then runs the full benchmark matrix.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

RUNS=5
JSON_FLAG=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --runs)  RUNS="$2"; shift 2 ;;
        --json)  JSON_FLAG="--json"; shift ;;
        *)       echo "Usage: $0 [--runs N] [--json]" >&2; exit 1 ;;
    esac
done

CONTAINER_ENGINE="${CONTAINER_ENGINE:-podman}"
IMAGE_NAME="oc-rsync-bench-full"

echo "=== oc-rsync Benchmark: upstream rsync vs oc-rsync ==="
echo "Container engine: $CONTAINER_ENGINE"
echo "Runs per test: $RUNS"
echo ""

echo "Building container image (this may take a while)..."
"$CONTAINER_ENGINE" build \
    -t "$IMAGE_NAME" \
    -f "$REPO_ROOT/scripts/Containerfile.benchmark-full" \
    "$REPO_ROOT"

echo ""
echo "Running benchmarks inside container..."
echo ""

"$CONTAINER_ENGINE" run --rm \
    -e BENCH_RUNS="$RUNS" \
    "$IMAGE_NAME" \
    "/usr/sbin/sshd && python3 /usr/local/bin/run_full_benchmark.py --runs ${RUNS} ${JSON_FLAG}"
