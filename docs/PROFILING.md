# Performance Profiling Guide

This document describes the profiling tools and workflows for analyzing oc-rsync performance.

## Quick Start

```bash
# Build with release optimizations
cargo build --release

# Run hyperfine benchmark (statistical comparison)
./scripts/benchmark_hyperfine.sh

# Generate flamegraph (CPU hotspots)
cargo build --profile release-with-debug
./scripts/flamegraph_profile.sh --scenario small_files

# Heap allocation analysis
cargo run --profile dhat --features dhat-heap --bin dhat-profile
```

## Profiling Workflow

Follow this sequence to identify and fix performance bottlenecks:

### 1. Hyperfine Benchmarking

First, establish a baseline with statistical rigor:

```bash
./scripts/benchmark_hyperfine.sh --warmup 3 --runs 10 --export-json --export-md
```

Options:
- `--scenario <name>`: Run specific scenario (small_files, large_file, mixed_tree, local_copy)
- `--export-json`: Export results to JSON
- `--export-md`: Export results to markdown

### 2. Flamegraph Analysis

Identify CPU hotspots with flamegraphs:

```bash
# Build with debug symbols
cargo build --profile release-with-debug

# Generate flamegraph
./scripts/flamegraph_profile.sh --scenario small_files --freq 999

# View the SVG in browser
firefox flamegraph_small_files.svg
```

Look for:
- Tall towers = hot code paths consuming CPU time
- Wide bases = frequently called functions
- Unexpected functions taking significant time

### 3. Heap Profiling with Dhat

Analyze memory allocations:

```bash
# Build with dhat feature
cargo build --profile dhat --features dhat-heap

# Run profiler
cargo run --profile dhat --features dhat-heap --bin dhat-profile

# Analyze results
# Open https://nicholass.github.io/nicholasses-dhat-viewer/
# Load the generated dhat-heap.json file
```

Look for:
- High allocation counts in hot paths
- Large allocations that could be pooled
- Short-lived allocations that could be stack-allocated

### 4. Syscall Analysis

For I/O bottlenecks, use strace:

```bash
# Count syscalls
strace -c ./target/release/oc-rsync -a src/ dest/

# Trace specific syscalls
strace -e openat,read,write ./target/release/oc-rsync -a src/ dest/
```

## Build Profiles

### release-with-debug

Release optimizations with debug symbols for profiling:

```toml
[profile.release-with-debug]
inherits = "release"
debug = true
strip = false
```

Use for: Flamegraphs, perf profiling

### dhat

Release optimizations for heap profiling:

```toml
[profile.dhat]
inherits = "release"
debug = true
strip = false
```

Use for: Dhat heap analysis

## Benchmark Scenarios

| Scenario | Description | Measures |
|----------|-------------|----------|
| small_files | 1000 x 1KB files | Per-file overhead, metadata handling |
| large_file | 100MB file | Throughput, delta algorithm |
| mixed_tree | 20 dirs x 50 files | Directory traversal, recursion |
| local_copy | 1000 files, no compression | Pure copy performance |

## Common Bottlenecks

### UID/GID Lookups

**Symptom**: High timerfd_create, getdents64, openat to /etc/passwd
**Cause**: Uncached NSS lookups per file
**Fix**: Implemented in `crates/metadata/src/id_lookup.rs` with HashMap cache

### Atomic File Writes

**Symptom**: High rename syscall count
**Cause**: Write-to-temp-then-rename pattern for crash safety
**Trade-off**: Acceptable for data integrity

### Delta Token Allocation

**Symptom**: High allocation count in delta_apply
**Fix**: Consider buffer pooling or pre-allocation

## Comparing with Upstream

Always benchmark against upstream rsync for reference:

```bash
# Ensure upstream is built
./scripts/build_upstream.sh

# Run comparison
./scripts/profile_transfer.sh
```

## Profiling Tips

1. **Isolate the bottleneck**: Use specific scenarios to isolate issues
2. **Warm up caches**: Run multiple iterations before measuring
3. **Control for variance**: Use hyperfine for statistical significance
4. **Profile release builds**: Debug builds have different characteristics
5. **Check syscalls**: I/O patterns often matter more than CPU time
