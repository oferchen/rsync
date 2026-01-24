# md5-simd: SIMD-Accelerated Parallel MD5 Hashing

## Overview

A standalone Rust crate providing SIMD-accelerated parallel MD5 hashing with rayon integration. Designed to replace sequential MD5 computation in rsync where profiling shows 96% of CPU time spent in `md5_compress` during checksum sync.

## Goals

- **4-8x throughput improvement** for parallel MD5 workloads
- **Cross-platform SIMD**: AVX-512 (16 lanes), AVX2 (8 lanes), NEON (4 lanes)
- **Adaptive performance**: Runtime detection with scalar fallback
- **Rayon integration**: Natural parallel iterator API
- **Standalone publishability**: Independent crate for crates.io

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    md5-simd crate                       │
├─────────────────────────────────────────────────────────┤
│  Public API (rayon-integrated)                          │
│  - digest_batch(&[impl AsRef<[u8]>]) -> Vec<[u8;16]>   │
│  - digest_files(&[Path]) -> Vec<io::Result<[u8;16]>>   │
│  - ParallelMd5 trait for par_iter integration          │
├─────────────────────────────────────────────────────────┤
│  Adaptive Dispatcher (runtime CPU detection)            │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌────────────┐ │
│  │ AVX-512  │ │  AVX2    │ │  NEON    │ │  Scalar    │ │
│  │ 16 lanes │ │ 8 lanes  │ │ 4 lanes  │ │  1 lane    │ │
│  └──────────┘ └──────────┘ └──────────┘ └────────────┘ │
├─────────────────────────────────────────────────────────┤
│  Core MD5 Engine (transposed state, SIMD operations)    │
└─────────────────────────────────────────────────────────┘
```

### SIMD Strategy

MD5 processes 64-byte blocks. We transpose N inputs so that byte 0 of all inputs occupies one SIMD register, byte 1 another, etc. MD5 rounds then operate on all lanes simultaneously.

**Register layout per backend**:

| Backend | Register Width | Lanes | State Registers |
|---------|---------------|-------|-----------------|
| AVX-512 | 512-bit (ZMM) | 16 | 4 × __m512i |
| AVX2 | 256-bit (YMM) | 8 | 4 × __m256i |
| NEON | 128-bit | 4 | 4 × uint32x4_t |
| Scalar | 32-bit | 1 | 4 × u32 |

**MD5 operations map to SIMD**:
- `F(B,C,D) = (B & C) | (!B & D)` → `vpternlogd` (AVX-512) or `vpand/vpor`
- `ROTL(x, n)` → `vprold` (AVX-512) or shift+or sequence
- Addition → `vpaddd`

### Adaptive Dispatch

Runtime CPU detection selects optimal backend:

```rust
impl Dispatcher {
    pub fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx512f") {
                return Self { backend: Backend::Avx512 };
            }
            if is_x86_feature_detected!("avx2") {
                return Self { backend: Backend::Avx2 };
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            return Self { backend: Backend::Neon };
        }
        Self { backend: Backend::Scalar }
    }
}
```

**Workload-based decisions**:
- 1 hash: Scalar (cold path, no SIMD overhead)
- 2-3 hashes: SIMD with padded lanes (still ~2x faster)
- 4+ hashes: Full SIMD utilization

## Public API

```rust
// Batch hashing
pub fn digest_batch<T: AsRef<[u8]>>(inputs: &[T]) -> Vec<[u8; 16]>;

// File hashing with I/O
pub fn digest_files(paths: &[impl AsRef<Path>]) -> Vec<io::Result<[u8; 16]>>;

// Rayon trait extension
pub trait ParallelMd5: ParallelIterator {
    fn md5_digest(self) -> Vec<[u8; 16]>;
}
```

**Usage**:
```rust
use md5_simd::ParallelMd5;

// Hash files in parallel
let checksums = md5_simd::digest_files(&paths);

// Integrate with existing parallel iterators
let checksums: Vec<[u8; 16]> = data_chunks
    .par_iter()
    .md5_digest();
```

## rsync Integration

### Integration Points

1. **Checksum sync (`-c` flag)** - `crates/engine/src/local_copy/`
   - Current: 96% CPU in sequential `md5_compress`
   - New: Batch file checksums via `digest_files`

2. **Block signatures** - `crates/transfer/src/generator.rs`
   - Current: Sequential block MD5 (30% of delta transfer CPU)
   - New: Batch block checksums via `digest_batch`

3. **Post-transfer verification** - `crates/transfer/src/receiver.rs`
   - Current: Sequential file verification
   - New: Parallel verification via `digest_files`

### Feature Integration

```toml
# crates/checksums/Cargo.toml
[features]
simd = ["md5-simd"]

[dependencies]
md5-simd = { path = "../md5-simd", optional = true }
```

## Crate Structure

```
crates/md5-simd/
├── Cargo.toml
├── src/
│   ├── lib.rs              # Public API, re-exports
│   ├── digest.rs           # Core digest types
│   ├── dispatcher.rs       # Runtime CPU detection
│   ├── scalar.rs           # Fallback implementation
│   ├── simd/
│   │   ├── mod.rs          # Common SIMD utilities
│   │   ├── avx2.rs         # 8-lane AVX2
│   │   ├── avx512.rs       # 16-lane AVX-512
│   │   └── neon.rs         # 4-lane ARM NEON
│   └── transpose.rs        # Data transposition
├── benches/
│   └── throughput.rs
└── tests/
    ├── correctness.rs      # RFC 1321 vectors
    └── parallel.rs         # Multi-lane consistency
```

## Testing Strategy

**Correctness**:
- RFC 1321 test vectors for all backends
- Cross-backend consistency (parallel == sequential)
- Edge cases: empty input, partial blocks, large files

**Performance**:
- Criterion benchmarks comparing backends
- Throughput measurement in GiB/s
- Comparison against md-5 crate baseline

## Expected Performance

| Backend | Lanes | Expected Throughput | Speedup |
|---------|-------|---------------------|---------|
| Scalar | 1 | ~550 MiB/s | 1x |
| NEON | 4 | ~1.5 GiB/s | ~2.5x |
| AVX2 | 8 | ~3 GiB/s | ~5x |
| AVX-512 | 16 | ~5 GiB/s | ~9x |

Note: Speedup is for parallel workloads. Single-hash performance matches or slightly trails scalar due to transposition overhead.

## Implementation Order

1. Scaffold crate with public API stubs
2. Implement scalar backend (correctness baseline)
3. Implement AVX2 backend (most common target)
4. Add benchmarks, verify performance
5. Implement AVX-512 backend
6. Implement NEON backend
7. Integrate into checksums crate
8. Update rsync integration points
9. Performance validation with rsync benchmarks

## References

- [minio/md5-simd](https://github.com/minio/md5-simd) - Go implementation
- [RFC 1321](https://www.ietf.org/rfc/rfc1321.txt) - MD5 specification
- Intel Intrinsics Guide for AVX2/AVX-512
- ARM NEON Intrinsics Reference
