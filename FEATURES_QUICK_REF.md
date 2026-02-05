# Feature Flags Quick Reference

## TL;DR

```bash
# Default (recommended)
cargo build --release

# Minimal
cargo build --release --no-default-features

# Maximum performance
cargo build --release --all-features
```

## Feature Categories

### Performance (Default ✓)
- `simd` - SIMD checksums (2-4x faster)
- `parallel` - Multi-core (2-8x faster)
- `copy_file_range` - Linux zero-copy (30-50% faster)
- `mmap` - Memory-mapped I/O (15-25% faster)
- `batch_sync` - Batched fsync (10-30% faster)

### Performance (Opt-in)
- `io_uring` - Linux 5.6+ async I/O (20-40% faster)

### Compression (Default ✓)
- `zstd` - Zstandard compression
- `lz4` - LZ4 compression

### Metadata (Default ✓)
- `acl` - POSIX ACLs
- `xattr` - Extended attributes
- `iconv` - Encoding conversion

### Runtime (Opt-in)
- `async` - Tokio runtime
- `sd-notify` - systemd integration
- `dhat-heap` - Heap profiling

## Common Builds

```bash
# Linux server (5.6+)
cargo build --release --features simd,parallel,io_uring,copy_file_range,mmap,batch_sync

# Cross-platform
cargo build --release --features simd,parallel,mmap

# Embedded
cargo build --release --no-default-features --features parallel

# Debug performance
cargo build --release --all-features
```

## Feature Flags Syntax

```bash
# Single feature
cargo build --features simd

# Multiple features
cargo build --features simd,parallel,mmap

# No defaults + specific features
cargo build --no-default-features --features simd,parallel

# All features
cargo build --all-features
```

## Performance Matrix

| Workload | Features |
|----------|----------|
| Large files | `simd,mmap,copy_file_range` |
| Many small files | `parallel,batch_sync` |
| Network | `simd,parallel,zstd` |
| Local copy | `copy_file_range,mmap,batch_sync` |
| Checksums | `simd,parallel` |

## Default = All Recommended

The default build enables all recommended optimizations for modern systems:
- All performance features except `io_uring` (for compatibility)
- Both compression algorithms
- All metadata features
- ~450KB binary size overhead for maximum performance
