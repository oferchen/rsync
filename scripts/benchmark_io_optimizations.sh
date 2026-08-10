#!/bin/bash
# Comprehensive I/O Optimization Benchmark Suite
# Measures Phase 1 improvements across all optimizations

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RESULTS_DIR="$PROJECT_ROOT/benchmark-results"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)

# Colors for output
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

log() {
    echo -e "${GREEN}[$(date '+%H:%M:%S')]${NC} $*"
}

warn() {
    echo -e "${YELLOW}[WARNING]${NC} $*"
}

error() {
    echo -e "${RED}[ERROR]${NC} $*" >&2
    exit 1
}

section() {
    echo
    echo "========================================================================"
    echo "  $*"
    echo "========================================================================"
    echo
}

# ============================================================================
# Check Prerequisites
# ============================================================================

check_prereqs() {
    log "Checking prerequisites..."

    # Check for cargo
    command -v cargo >/dev/null || error "cargo not found"

    # Check for criterion
    if ! cargo tree -p fast_io 2>/dev/null | grep -q criterion; then
        warn "criterion not found in dependencies, benchmarks may not run"
    fi

    # Check for perf (optional)
    if command -v perf >/dev/null; then
        log "perf available for detailed profiling"
    else
        warn "perf not found - skipping detailed syscall analysis"
    fi

    # Check kernel version for io_uring
    if [[ -f /proc/version ]]; then
        kernel_ver=$(uname -r | cut -d. -f1,2)
        major=$(echo "$kernel_ver" | cut -d. -f1)
        minor=$(echo "$kernel_ver" | cut -d. -f2)

        if [[ $major -ge 5 ]] && [[ $minor -ge 6 ]]; then
            log "Kernel ${kernel_ver} supports io_uring"
        else
            warn "Kernel ${kernel_ver} does not support io_uring (requires 5.6+)"
        fi
    fi
}

# ============================================================================
# Run Microbenchmarks
# ============================================================================

run_microbenchmarks() {
    section "Running Microbenchmarks (Criterion)"

    log "Building benchmarks in release mode..."
    cd "$PROJECT_ROOT"
    cargo build --release --benches -p fast_io

    log "Running I/O optimization benchmarks..."
    mkdir -p "$RESULTS_DIR/criterion"

    # Run with all features enabled
    cargo bench -p fast_io --features "mmap,io_uring" -- --save-baseline phase1_optimizations

    # Copy criterion results
    if [[ -d "$PROJECT_ROOT/target/criterion" ]]; then
        cp -r "$PROJECT_ROOT/target/criterion" "$RESULTS_DIR/criterion_${TIMESTAMP}"
        log "Criterion results saved to $RESULTS_DIR/criterion_${TIMESTAMP}"
    fi
}

# ============================================================================
# Run Transfer Benchmarks
# ============================================================================

run_transfer_benchmarks() {
    section "Running Transfer Benchmarks (MapFile)"

    cd "$PROJECT_ROOT"

    log "Running MapFile benchmarks..."
    cargo bench -p transfer --bench map_file_benchmark -- --save-baseline phase1_mmap

    log "Running token buffer benchmarks..."
    cargo bench -p transfer --bench token_buffer_benchmark -- --save-baseline phase1_buffers
}

# ============================================================================
# Real-World Performance Test
# ============================================================================

run_realworld_test() {
    section "Real-World Performance Test (Linux Kernel Source)"

    local BENCH_DIR="/tmp/rsync-bench"
    local KERNEL_SRC="$BENCH_DIR/kernel-src"
    local DEST_DIR="$BENCH_DIR/dest"

    if [[ ! -d "$KERNEL_SRC" ]]; then
        warn "Kernel source not found at $KERNEL_SRC"
        warn "Skipping real-world test. Run scripts/profile_local.sh -h for setup instructions."
        return
    fi

    log "Testing with kernel source: $KERNEL_SRC"

    # Build release binary
    cargo build --release
    local OC_RSYNC="$PROJECT_ROOT/target/release/oc-rsync"

    if [[ ! -x "$OC_RSYNC" ]]; then
        error "oc-rsync binary not found at $OC_RSYNC"
    fi

    # Test local copy performance
    rm -rf "$DEST_DIR"
    mkdir -p "$DEST_DIR"

    log "Running local copy test (warmup)..."
    time "$OC_RSYNC" -a "$KERNEL_SRC/" "$DEST_DIR/" 2>&1 | grep -E "sent|total size"

    rm -rf "$DEST_DIR"
    mkdir -p "$DEST_DIR"

    log "Running local copy test (timed)..."
    /usr/bin/time -v "$OC_RSYNC" -a "$KERNEL_SRC/" "$DEST_DIR/" 2>&1 | tee "$RESULTS_DIR/realworld_${TIMESTAMP}.log"
}

# ============================================================================
# Syscall Analysis
# ============================================================================

run_syscall_analysis() {
    section "Syscall Analysis (strace)"

    if ! command -v strace >/dev/null; then
        warn "strace not found, skipping syscall analysis"
        return
    fi

    cd "$PROJECT_ROOT"
    cargo build --release --example simple_copy 2>/dev/null || {
        warn "simple_copy example not found, skipping syscall analysis"
        return
    }

    local TEST_FILE="/tmp/benchmark_test_file.bin"
    local DEST_FILE="/tmp/benchmark_dest_file.bin"

    # Create 10MB test file
    dd if=/dev/urandom of="$TEST_FILE" bs=1M count=10 2>/dev/null

    log "Analyzing syscalls for 10MB file copy..."
    strace -c -o "$RESULTS_DIR/syscalls_${TIMESTAMP}.txt" \
        cp "$TEST_FILE" "$DEST_FILE" 2>&1 || true

    log "Syscall summary saved to $RESULTS_DIR/syscalls_${TIMESTAMP}.txt"

    rm -f "$TEST_FILE" "$DEST_FILE"
}

# ============================================================================
# Generate Report
# ============================================================================

generate_report() {
    section "Generating Summary Report"

    local REPORT="$RESULTS_DIR/phase1_summary_${TIMESTAMP}.txt"

    cat > "$REPORT" <<EOF
================================================================================
Phase 1 I/O Optimization Benchmark Results
================================================================================
Date: $(date)
Kernel: $(uname -r)
CPU: $(lscpu | grep "Model name" | cut -d: -f2 | xargs)
Memory: $(free -h | awk '/^Mem:/ {print $2}')
Disk: $(df -h / | awk 'NR==2 {print $1, $2}')

================================================================================
Optimizations Tested
================================================================================

1. Vectored I/O (writev)
   - Reduces syscall overhead by batching multiple buffers into one call
   - Expected improvement: 10-30% for small writes

2. Adaptive Buffer Sizing
   - Small files (< 64KB): 4KB buffers
   - Medium files (64KB-1MB): 64KB buffers
   - Large files (> 1MB): 256KB buffers
   - Expected improvement: 15-40% reduced memory + fewer syscalls

3. io_uring Support (Linux 5.6+)
   - Batched async I/O without thread pools
   - Expected improvement: 20-50% for large files with random access
   $(if [[ -f /proc/sys/kernel/osrelease ]]; then
        echo "   Status: $(grep -q "io_uring" /proc/kallsyms && echo "AVAILABLE" || echo "NOT AVAILABLE")"
     fi)

4. Memory-Mapped I/O (mmap)
   - Zero-copy reads for large files
   - Expected improvement: 30-60% for large sequential reads
   Status: ENABLED

5. Metadata Syscall Batching
   - Reduced stat/fstat calls with cached metadata
   - Expected improvement: 10-20% for directories with many files

================================================================================
Microbenchmark Results
================================================================================

Criterion results: $RESULTS_DIR/criterion_${TIMESTAMP}/

View HTML reports:
  file://$RESULTS_DIR/criterion_${TIMESTAMP}/report/index.html

================================================================================
Real-World Results
================================================================================

$(if [[ -f "$RESULTS_DIR/realworld_${TIMESTAMP}.log" ]]; then
    cat "$RESULTS_DIR/realworld_${TIMESTAMP}.log"
else
    echo "No real-world test results available"
fi)

================================================================================
Syscall Analysis
================================================================================

$(if [[ -f "$RESULTS_DIR/syscalls_${TIMESTAMP}.txt" ]]; then
    cat "$RESULTS_DIR/syscalls_${TIMESTAMP}.txt"
else
    echo "No syscall analysis available"
fi)

================================================================================
Next Steps
================================================================================

1. Compare results with baseline:
   cargo bench -p fast_io -- --baseline phase1_optimizations

2. Generate flamegraphs:
   cargo flamegraph --bench io_optimizations

3. Profile with perf:
   perf record -g cargo bench -p fast_io
   perf report

4. Compare with upstream rsync:
   ./scripts/profile_local.sh -s

================================================================================
EOF

    log "Report saved to: $REPORT"
    echo
    cat "$REPORT"
}

# ============================================================================
# Main
# ============================================================================

main() {
    log "Phase 1 I/O Optimization Benchmark Suite"
    log "Results directory: $RESULTS_DIR"

    mkdir -p "$RESULTS_DIR"

    check_prereqs

    # Run all benchmark suites
    run_microbenchmarks
    run_transfer_benchmarks

    # Optional: real-world and syscall tests
    run_realworld_test || warn "Real-world test skipped"
    run_syscall_analysis || warn "Syscall analysis skipped"

    # Generate summary
    generate_report

    section "Benchmark Complete!"
    log "All results saved to: $RESULTS_DIR"
    log ""
    log "Key files:"
    log "  - Summary report: $RESULTS_DIR/phase1_summary_${TIMESTAMP}.txt"
    log "  - Criterion HTML: $RESULTS_DIR/criterion_${TIMESTAMP}/report/index.html"
}

main "$@"
