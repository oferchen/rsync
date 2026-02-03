#!/bin/bash
# Performance benchmark comparing oc-rsync vs upstream rsync
#
# Usage: ./scripts/benchmark.sh [--quick|--full]
#
# This script creates test data and measures transfer performance
# for both oc-rsync and upstream rsync.

set -e

# Configuration
OC_RSYNC="${OC_RSYNC:-target/release/oc-rsync}"
UPSTREAM_RSYNC="${UPSTREAM_RSYNC:-/usr/bin/rsync}"
BENCHMARK_DIR="${BENCHMARK_DIR:-/tmp/rsync-benchmark}"
RESULTS_FILE="${RESULTS_FILE:-benchmark-results.txt}"

# Test parameters
QUICK_FILES=1000
QUICK_FILE_SIZE=1024
FULL_FILES=10000
FULL_FILE_SIZE=10240

# Parse arguments
MODE="quick"
if [[ "$1" == "--full" ]]; then
    MODE="full"
    NUM_FILES=$FULL_FILES
    FILE_SIZE=$FULL_FILE_SIZE
else
    NUM_FILES=$QUICK_FILES
    FILE_SIZE=$QUICK_FILE_SIZE
fi

echo "=== oc-rsync vs upstream rsync Benchmark ==="
echo "Mode: $MODE"
echo "Files: $NUM_FILES, Size: $FILE_SIZE bytes each"
echo ""

# Check binaries exist
if [[ ! -x "$OC_RSYNC" ]]; then
    echo "Building oc-rsync..."
    cargo build --release --quiet
fi

if [[ ! -x "$UPSTREAM_RSYNC" ]]; then
    echo "Error: upstream rsync not found at $UPSTREAM_RSYNC"
    exit 1
fi

# Show versions
echo "Versions:"
echo "  oc-rsync: $("$OC_RSYNC" --version 2>/dev/null | head -1 || echo "unknown")"
echo "  rsync:    $("$UPSTREAM_RSYNC" --version | head -1)"
echo ""

# Create test directories
rm -rf "$BENCHMARK_DIR"
mkdir -p "$BENCHMARK_DIR/src"
mkdir -p "$BENCHMARK_DIR/dest-oc"
mkdir -p "$BENCHMARK_DIR/dest-upstream"

# Generate test data
echo "Generating $NUM_FILES test files..."
for i in $(seq 1 $NUM_FILES); do
    dd if=/dev/urandom of="$BENCHMARK_DIR/src/file$i.dat" bs=$FILE_SIZE count=1 2>/dev/null
done
TOTAL_SIZE=$(du -sh "$BENCHMARK_DIR/src" | cut -f1)
echo "Total test data: $TOTAL_SIZE"
echo ""

# Benchmark function
benchmark() {
    local name="$1"
    local cmd="$2"
    local dest="$3"

    # Clear destination
    rm -rf "$dest"/*

    # Run and time
    echo "Running $name..."
    local start=$(date +%s.%N)
    eval "$cmd" >/dev/null 2>&1
    local end=$(date +%s.%N)

    local duration=$(echo "$end - $start" | bc)
    echo "  Time: ${duration}s"

    # Calculate throughput
    local bytes=$(du -sb "$BENCHMARK_DIR/src" | cut -f1)
    local mbps=$(echo "scale=2; $bytes / $duration / 1048576" | bc)
    echo "  Throughput: ${mbps} MB/s"
    echo ""

    echo "$name: ${duration}s (${mbps} MB/s)" >> "$RESULTS_FILE"
}

# Clear results
echo "=== Benchmark Results $(date) ===" > "$RESULTS_FILE"
echo "Mode: $MODE, Files: $NUM_FILES, Size: $FILE_SIZE" >> "$RESULTS_FILE"
echo "" >> "$RESULTS_FILE"

# Test 1: Initial sync (whole file transfer)
echo "=== Test 1: Initial Sync (whole file) ==="
benchmark "oc-rsync" \
    "\"$OC_RSYNC\" -a \"$BENCHMARK_DIR/src/\" \"$BENCHMARK_DIR/dest-oc/\"" \
    "$BENCHMARK_DIR/dest-oc"

benchmark "upstream rsync" \
    "\"$UPSTREAM_RSYNC\" -a \"$BENCHMARK_DIR/src/\" \"$BENCHMARK_DIR/dest-upstream/\"" \
    "$BENCHMARK_DIR/dest-upstream"

# Test 2: No-change sync (should be fast)
echo "=== Test 2: No-change Sync ==="
benchmark "oc-rsync (no-change)" \
    "\"$OC_RSYNC\" -a \"$BENCHMARK_DIR/src/\" \"$BENCHMARK_DIR/dest-oc/\"" \
    "$BENCHMARK_DIR/dest-oc"

benchmark "upstream rsync (no-change)" \
    "\"$UPSTREAM_RSYNC\" -a \"$BENCHMARK_DIR/src/\" \"$BENCHMARK_DIR/dest-upstream/\"" \
    "$BENCHMARK_DIR/dest-upstream"

# Test 3: Incremental sync (modify some files)
echo "=== Test 3: Incremental Sync (10% modified) ==="
NUM_MODIFIED=$((NUM_FILES / 10))
for i in $(seq 1 $NUM_MODIFIED); do
    dd if=/dev/urandom of="$BENCHMARK_DIR/src/file$i.dat" bs=$FILE_SIZE count=1 2>/dev/null
done

benchmark "oc-rsync (incremental)" \
    "\"$OC_RSYNC\" -a \"$BENCHMARK_DIR/src/\" \"$BENCHMARK_DIR/dest-oc/\"" \
    "$BENCHMARK_DIR/dest-oc"

benchmark "upstream rsync (incremental)" \
    "\"$UPSTREAM_RSYNC\" -a \"$BENCHMARK_DIR/src/\" \"$BENCHMARK_DIR/dest-upstream/\"" \
    "$BENCHMARK_DIR/dest-upstream"

# Test 4: With compression
echo "=== Test 4: Compressed Transfer ==="
rm -rf "$BENCHMARK_DIR/dest-oc"/* "$BENCHMARK_DIR/dest-upstream"/*

benchmark "oc-rsync (compressed)" \
    "\"$OC_RSYNC\" -az \"$BENCHMARK_DIR/src/\" \"$BENCHMARK_DIR/dest-oc/\"" \
    "$BENCHMARK_DIR/dest-oc"

benchmark "upstream rsync (compressed)" \
    "\"$UPSTREAM_RSYNC\" -az \"$BENCHMARK_DIR/src/\" \"$BENCHMARK_DIR/dest-upstream/\"" \
    "$BENCHMARK_DIR/dest-upstream"

# Cleanup
rm -rf "$BENCHMARK_DIR"

echo "=== Benchmark Complete ==="
echo "Results saved to $RESULTS_FILE"
cat "$RESULTS_FILE"
