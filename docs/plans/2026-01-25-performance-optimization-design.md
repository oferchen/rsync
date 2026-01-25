# Performance Optimization Design: Exceed Upstream rsync by 10%

**Date**: 2026-01-25
**Status**: Approved
**Target**: 10% faster than upstream rsync 3.4.1 on Linux kernel source transfer

## Overview

Optimize oc-rsync daemon protocol transfers to exceed upstream rsync performance by 10%, measured using Linux kernel source (~80K files, ~1.5GB) as the benchmark dataset.

## Benchmark Environment

### Test Infrastructure
- **Test Data**: Linux kernel 6.x source (~1.5GB, ~80K files)
- **Local Daemon**: rsyncd on localhost with `[kernel]` module
- **Measurement**: 5 warmup runs, 10 timed runs
- **Metrics**: wall time, CPU time, syscall counts

### Directory Structure
```
/tmp/rsync-bench/
├── kernel-src/          # Linux kernel source (test data)
├── dest/                # Transfer destination
├── profiles/            # perf.data and flamegraphs
└── results/             # Benchmark CSV results
```

## Optimization Strategy

### Phase 1: I/O Optimization (Priority 1)

#### 1.1 Vectored I/O (writev/readv)
Combine multiple small writes into single syscall. Target protocol crate multiplex writer and file list encoding. Expected gain: 5-15% reduction in write syscalls.

#### 1.2 Adaptive Buffer Sizing
Match buffer size to transfer characteristics:
- Small files: smaller buffers reduce memory waste
- Large files: 256KB+ buffers improve throughput
- Use file size hints from file list

#### 1.3 io_uring Integration (Linux 5.6+)
Async I/O without syscall overhead. Feature-gated behind `io-uring` flag. Target file reads during delta generation with fallback to standard I/O on older kernels.

#### 1.4 Memory-mapped I/O for Large Files
Avoid copy between kernel/userspace. Threshold: files > 1MB use mmap. Benefits delta transfer with random access pattern.

#### 1.5 Syscall Batching
Reduce metadata syscalls. Batch `fstat` calls where possible. Cache file metadata during directory traversal.

### Phase 2: Protocol Optimization (Priority 2)

#### 2.1 File List Batching
Stream entire file list in single batch with compression. Reference upstream `flist.c` batching logic.

#### 2.2 Incremental File List
Enable `--inc-recursive` style behavior. Begin generator phase while receiver still building list. Reduces perceived latency for large directories.

#### 2.3 Checksum Pipelining
Overlap checksum computation with I/O. Send file data while computing next file's checksum. Use double-buffering for producer/consumer pattern.

#### 2.4 Reduced Acknowledgment Overhead
Group multiple file completions into single ACK message. Reduces protocol chattiness on high-latency links.

#### 2.5 Compression Tuning
Match compression level to data:
- Already-compressed files (jpg, zip): skip compression
- Text files: zstd level 3 (fast)
- Use file extension hints from file list

### Phase 3: Parallelization (Priority 3)

#### 3.1 Parallel Checksum Computation
Use `md5-simd` crate for batch SIMD hashing. Parallel signature generation for multiple files. Target: 4x speedup on 4+ core systems.

#### 3.2 Parallel File List Generation
Use `jwalk` for parallel directory traversal. Collect metadata in parallel, sort for protocol order.

#### 3.3 Concurrent Delta Generation
Generator computes signatures while receiver applies deltas. Use bounded channels to prevent memory bloat.

#### 3.4 Parallel File Transfers
For many-small-files scenario. Configurable concurrency limit (default: 4). Maintain ordering for deterministic output.

#### 3.5 SIMD-Accelerated Rolling Checksum
AVX2/NEON acceleration for rolling checksum. Target: 2-3x faster block matching.

## Testing Strategy

### Coverage Target: 95%

1. **Benchmark Test Suite**: Compare against upstream rsync output byte-for-byte
2. **Unit Tests**: Per optimization with correctness verification
3. **Integration Tests**: Local daemon, interop with upstream
4. **Property-Based Tests**: Arbitrary file trees with proptest
5. **Performance Tests**: Criterion benchmarks with regression tracking

## Success Criteria

- Each phase shows measurable improvement before proceeding
- Re-profile after each major change
- Final result: 10%+ faster than upstream rsync on kernel source transfer
- All tests pass, 95% coverage maintained

## Reference

- Upstream rsync source: `target/interop/upstream-src/rsync-3.4.1`
- Existing profiling: NSS lookups (12%), clock_gettime (5%), I/O overhead
