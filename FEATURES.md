# Feature Flags

This document describes all compile-time feature flags available in the `oc-rsync` workspace.

## Table of Contents

- [Quick Start](#quick-start)
- [Default Features](#default-features)
- [Performance Optimization Features](#performance-optimization-features)
  - [SIMD Acceleration](#simd-acceleration)
  - [Parallel Processing](#parallel-processing)
  - [io_uring](#io_uring)
  - [copy_file_range](#copy_file_range)
  - [Memory-mapped I/O](#memory-mapped-io)
  - [Batched fsync](#batched-fsync)
- [Compression Features](#compression-features)
- [Metadata Features](#metadata-features)
- [Runtime Features](#runtime-features)
- [Build Examples](#build-examples)
- [Performance Tuning](#performance-tuning)

## Quick Start

```bash
# Default build (optimized for modern systems)
cargo build --release

# Build with all features
cargo build --release --all-features

# Minimal build (smallest binary, no optimizations)
cargo build --release --no-default-features

# Linux-specific optimizations
cargo build --release --features io_uring,copy_file_range
```

## Default Features

The following features are enabled by default for optimal performance on modern systems:

| Feature | Description | Overhead |
|---------|-------------|----------|
| `simd` | SIMD-accelerated checksums | ~50KB |
| `parallel` | Multi-core file operations | Minimal |
| `copy_file_range` | Zero-copy local transfers (Linux) | None |
| `mmap` | Memory-mapped I/O | None |
| `batch_sync` | Batched fsync operations | None |
| `zstd` | Zstandard compression | ~200KB |
| `lz4` | LZ4 compression | ~50KB |
| `acl` | ACL support (Unix) | ~30KB |
| `xattr` | Extended attributes (Unix) | ~20KB |
| `iconv` | Character encoding conversion | ~100KB |

**Total binary size overhead**: ~450KB for maximum performance

To build without default features:
```bash
cargo build --release --no-default-features
```

## Performance Optimization Features

### SIMD Acceleration

**Feature**: `simd`

**Enables**: `checksums/md5-simd`, `checksums/xxh3-simd`

**Description**: SIMD-accelerated checksum computation using AVX2 (x86_64) or NEON (ARM).

**Requirements**:
- x86_64 CPU with AVX2 support (most CPUs since 2013)
- ARM CPU with NEON support (all 64-bit ARM CPUs)
- Runtime detection automatically falls back to scalar code

**Performance Impact**:
- 2-4x faster MD5 checksums
- 2-3x faster XXH3 checksums
- Most beneficial for large files or checksum-heavy workloads

**Binary Size**: +50KB

**Example**:
```bash
# Build with SIMD optimizations
cargo build --release --features simd

# Disable SIMD (for compatibility)
cargo build --release --no-default-features --features parallel,mmap
```

**When to disable**: Only disable if targeting very old CPUs or debugging checksum issues.

---

### Parallel Processing

**Feature**: `parallel`

**Enables**: `cli/parallel`, `checksums/parallel`

**Description**: Multi-threaded file operations using rayon thread pools.

**Requirements**: None (pure Rust)

**Performance Impact**:
- 2-8x faster on multi-core systems
- Scales with CPU core count
- Minimal overhead on single-core systems

**Binary Size**: Minimal (<10KB)

**Use Cases**:
- Batch checksum computation
- Directory tree traversal
- Large file list processing

**Example**:
```bash
# Build with parallel support
cargo build --release --features parallel

# Use in rsync operations
oc-rsync -r --checksum --parallel /source /dest
```

**When to disable**: Only for embedded systems with single-core CPUs.

---

### io_uring

**Feature**: `io_uring`

**Enables**: `transfer/io_uring`, `fast_io/io_uring`

**Description**: Linux io_uring for batched async I/O syscalls.

**Requirements**:
- Linux kernel 5.6+ (February 2020)
- Automatic runtime fallback to standard I/O on older kernels
- Linux-only (no effect on other platforms)

**Performance Impact**:
- 20-40% faster I/O throughput
- Reduces syscall overhead by batching operations
- Most beneficial for high-IOPS workloads

**Binary Size**: +100KB (liburing dependency)

**Example**:
```bash
# Build with io_uring support
cargo build --release --features io_uring

# Runtime detection automatically uses io_uring if available
oc-rsync /source /dest
```

**Platform Support**:
- **Linux 5.6+**: Full support
- **Linux <5.6**: Automatic fallback to standard I/O
- **macOS/Windows**: Feature has no effect

**When to enable**:
- Linux servers with kernel 5.6+
- High-throughput transfer scenarios
- Storage systems with high IOPS

**When to disable**:
- Cross-platform builds targeting older Linux
- Embedded systems with older kernels
- Windows/macOS-only deployments

---

### copy_file_range

**Feature**: `copy_file_range`

**Enables**: `fast_io/copy_file_range`

**Description**: Zero-copy file transfers using the `copy_file_range` syscall.

**Requirements**:
- Linux kernel 4.5+ for same-filesystem copies
- Linux kernel 5.3+ for cross-filesystem copies
- Automatic fallback to read/write on unsupported systems

**Performance Impact**:
- 30-50% faster local copies
- Eliminates userspace buffer copies
- Most beneficial for large files on local filesystems

**Binary Size**: None

**Example**:
```bash
# Build with copy_file_range support
cargo build --release --features copy_file_range

# Use for local copies
oc-rsync /source/file /dest/file
```

**Platform Support**:
- **Linux 4.5+**: Same-filesystem copies
- **Linux 5.3+**: Cross-filesystem copies
- **Other platforms**: Automatic fallback to standard I/O

**When to enable**:
- Linux deployments
- Local filesystem copies
- Large file transfers

**When to disable**:
- Non-Linux platforms (already no-op)
- Network transfers (not applicable)

---

### Memory-mapped I/O

**Feature**: `mmap`

**Enables**: `fast_io/mmap`, `transfer/mmap`

**Description**: Memory-mapped I/O for large file reads.

**Requirements**: None (cross-platform via memmap2)

**Performance Impact**:
- Zero-copy file reading
- Better CPU cache utilization
- Most beneficial for large files (>1MB)

**Binary Size**: Minimal

**Trade-offs**:
- May cause issues on 32-bit systems with very large files (>4GB)
- Can trigger OOM on systems with limited virtual memory

**Example**:
```bash
# Build with mmap support (enabled by default)
cargo build --release

# Disable mmap for 32-bit builds
cargo build --release --no-default-features --features simd,parallel
```

**When to disable**:
- 32-bit platforms
- Very large files (>4GB) on constrained systems
- Embedded systems with limited virtual memory

---

### Batched fsync

**Feature**: `batch_sync`

**Enables**: `engine/batch-sync`

**Description**: Batches `fsync()` calls to improve write throughput.

**Requirements**: None

**Performance Impact**:
- 10-30% faster writes
- Reduces metadata update overhead
- Still maintains crash safety guarantees

**Binary Size**: None

**Safety**:
- All data is written before the transfer completes
- fsync is called at strategic points (e.g., end of batch)
- Still crash-safe (data committed before acknowledgment)

**Example**:
```bash
# Build with batched fsync (enabled by default)
cargo build --release

# Disable for maximum durability
cargo build --release --no-default-features --features simd,parallel,mmap
```

**When to disable**:
- Maximum durability requirements
- Systems with unreliable power
- SSD write amplification concerns

---

## Compression Features

### zstd

**Feature**: `zstd`

**Description**: Zstandard compression (modern, fast, high ratio)

**Binary Size**: +200KB

**Performance**: ~3x faster than gzip with better compression

**Recommended**: Yes (modern standard)

### lz4

**Feature**: `lz4`

**Description**: LZ4 compression (extremely fast, lower ratio)

**Binary Size**: +50KB

**Performance**: ~10x faster than gzip, lower compression

**Recommended**: For speed-critical workloads

---

## Metadata Features

### ACL Support

**Feature**: `acl`

**Description**: POSIX Access Control Lists

**Platform**: Unix-only

**Binary Size**: +30KB

### Extended Attributes

**Feature**: `xattr`

**Description**: Extended file attributes

**Platform**: Unix-only

**Binary Size**: +20KB

### Character Encoding

**Feature**: `iconv`

**Description**: Filename encoding conversion (iconv)

**Binary Size**: +100KB

**Use Case**: Cross-platform filename compatibility

---

## Runtime Features

### Async Runtime

**Feature**: `async`

**Description**: Tokio-based async I/O

**Use Case**: Daemon mode, concurrent connections

**Not enabled by default** (optional for specific use cases)

### systemd Integration

**Feature**: `sd-notify`

**Description**: systemd service notification

**Use Case**: Linux service management

**Not enabled by default**

### Heap Profiling

**Feature**: `dhat-heap`

**Description**: dhat heap profiler integration

**Use Case**: Memory allocation debugging

**Not enabled by default** (development only)

---

## Build Examples

### Minimal Build

Smallest binary, pure Rust, no optimizations:

```bash
cargo build --release --no-default-features
```

**Binary size**: ~2-3MB (depending on platform)

### Standard Build

Default configuration (recommended):

```bash
cargo build --release
```

**Features**: All defaults (simd, parallel, copy_file_range, mmap, batch_sync, compression, metadata)

**Binary size**: ~4-5MB

### Maximum Performance Build

All optimizations enabled:

```bash
cargo build --release --all-features
```

**Includes**: All performance features + async runtime + debugging tools

**Binary size**: ~6-7MB

### Linux Server Build

Optimized for Linux servers:

```bash
cargo build --release \
  --features simd,parallel,io_uring,copy_file_range,mmap,batch_sync,zstd,lz4
```

### Embedded/Cross-Platform Build

Compatible with older systems:

```bash
cargo build --release \
  --no-default-features \
  --features parallel,mmap,zstd
```

### macOS/FreeBSD Build

Platform-appropriate optimizations:

```bash
cargo build --release \
  --features simd,parallel,mmap,batch_sync,zstd,lz4,acl,xattr
```

---

## Performance Tuning

### Recommended Configurations

**For maximum performance (Linux 5.6+)**:
```bash
cargo build --release --features simd,parallel,io_uring,copy_file_range,mmap,batch_sync
```

**For maximum compatibility**:
```bash
cargo build --release --features simd,parallel,mmap
```

**For smallest binary**:
```bash
cargo build --release --no-default-features
```

### Feature Selection Guide

| Workload | Recommended Features |
|----------|---------------------|
| Large files (GB+) | `simd,mmap,copy_file_range` |
| Many small files | `parallel,batch_sync` |
| Network transfers | `simd,parallel,zstd` |
| Local copies | `copy_file_range,mmap,batch_sync` |
| Checksum-heavy | `simd,parallel` |
| Embedded systems | `parallel` only |

### Performance Expectations

**Baseline** (no optimizations):
- Checksum: 500 MB/s
- Local copy: 1 GB/s
- Network: Limited by bandwidth

**With SIMD** (`simd`):
- Checksum: 1500-2000 MB/s (3-4x faster)

**With Parallel** (`parallel`):
- Multi-file: 4-8x faster (scales with cores)

**With io_uring** (`io_uring` on Linux 5.6+):
- I/O throughput: +20-40%

**With copy_file_range** (`copy_file_range` on Linux):
- Local copy: 1.5-2.5 GB/s (1.5-2.5x faster)

**With mmap** (`mmap`):
- Large file read: +15-25%

**With batch_sync** (`batch_sync`):
- Write throughput: +10-30%

---

## Feature Dependencies

```
simd
├── checksums/md5-simd
└── checksums/xxh3-simd

parallel
├── cli/parallel
└── checksums/parallel
    └── engine/parallel
        └── engine/lazy-metadata
        └── engine/batch-sync

io_uring
├── transfer/io_uring
└── fast_io/io_uring

copy_file_range
└── fast_io/copy_file_range

mmap
├── fast_io/mmap
└── transfer/mmap

batch_sync
└── engine/batch-sync
```

---

## Testing Features

```bash
# Test each feature individually
cargo test --no-default-features --features simd
cargo test --no-default-features --features parallel
cargo test --no-default-features --features io_uring
cargo test --no-default-features --features copy_file_range
cargo test --no-default-features --features mmap
cargo test --no-default-features --features batch_sync

# Test all features together
cargo test --all-features

# Test without any features
cargo test --no-default-features
```

---

## Troubleshooting

### Build Errors

**Error**: `error: usage of an 'unsafe' block`
- **Cause**: Feature gates not properly configured
- **Fix**: Ensure `batch-sync` and `acl` features are enabled if using unsafe code

**Error**: `io-uring not found`
- **Cause**: Missing liburing on Linux
- **Fix**: Install liburing-dev or disable `io_uring` feature

### Runtime Issues

**Issue**: No performance improvement with `simd`
- **Check**: CPU supports AVX2 (x86_64) or NEON (ARM)
- **Verify**: Feature is actually enabled: `oc-rsync --version` should show features

**Issue**: `io_uring` not working
- **Check**: Kernel version 5.6+ (`uname -r`)
- **Note**: Automatic fallback to standard I/O on older kernels

**Issue**: Crashes with `mmap`
- **Cause**: Very large files on 32-bit systems
- **Fix**: Disable `mmap` feature or use 64-bit build

---

## Summary

- **Default build**: Optimized for modern systems (all recommended features)
- **Minimal build**: `--no-default-features` for smallest binary
- **Maximum performance**: `--all-features` for all optimizations
- **Platform-specific**: Use `io_uring` and `copy_file_range` on Linux 5.6+

For most users, the default configuration provides the best balance of performance, compatibility, and binary size.
