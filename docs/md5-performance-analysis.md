# MD5 Performance Analysis: SIMD vs Assembly vs Rayon

## Performance Model Comparison

### Architecture 1: Current (Rayon + Single Hash)
```rust
// Current pattern in checksums/src/parallel.rs
pub fn compute_digests_parallel<D, T>(blocks: &[T]) -> Vec<D::Digest>
where
    D: StrongDigest + Send,
    T: AsRef<[u8]> + Sync,
{
    blocks
        .par_iter()
        .map(|block| D::digest(block.as_ref()))
        .collect()
}
```

**Performance**:
- Parallelism: `N_THREADS` (typically 8-16 cores)
- Per-thread: 1 hash at a time
- Total throughput: `N_THREADS × single_hash_speed`

**Pros**:
- Simple, idiomatic Rust
- Scales with CPU cores
- Works for any digest algorithm

**Cons**:
- No SIMD exploitation within each thread
- Thread overhead for very small inputs

### Architecture 2: SIMD Batch (md5-simd)
```rust
// Using md5-simd batch API
pub fn compute_digests_batch<T>(blocks: &[T]) -> Vec<[u8; 16]>
where
    T: AsRef<[u8]>,
{
    md5_simd::digest_batch(blocks)
}
```

**Performance**:
- Parallelism: 4-16 SIMD lanes (depending on CPU)
- Per-batch: 4-16 hashes simultaneously
- Total throughput: `SIMD_LANES × single_hash_speed`

**Pros**:
- No thread synchronization overhead
- Excellent for small inputs (< 4KB)
- Single-threaded efficiency

**Cons**:
- Doesn't scale beyond SIMD width
- Requires inputs ready in batch
- Only works for MD4/MD5

### Architecture 3: Hybrid (Rayon + SIMD)
```rust
// Theoretical best: combine both approaches
pub fn compute_digests_hybrid<T>(blocks: &[T]) -> Vec<[u8; 16]>
where
    T: AsRef<[u8]> + Sync,
{
    const BATCH_SIZE: usize = 16; // AVX-512 lanes

    blocks
        .par_chunks(BATCH_SIZE)
        .flat_map(|chunk| md5_simd::digest_batch(chunk))
        .collect()
}
```

**Performance**:
- Parallelism: `N_THREADS × SIMD_LANES`
- Per-thread: 4-16 hashes simultaneously
- Total throughput: `N_THREADS × SIMD_LANES × single_hash_speed`

**Theoretical speedup**: 8 threads × 16 SIMD lanes = 128x parallelism

**Pros**:
- Maximum throughput
- Scales with cores AND SIMD
- Best for large batch jobs

**Cons**:
- Most complex
- Diminishing returns beyond certain scale
- Only beneficial for very large datasets

## Workload Characterization

### Typical rsync MD5 Use Cases

#### 1. File List Checksums
**Pattern**: Hash metadata for 1000s of files during sync
```rust
// Example: file paths for whole-file checksums
let file_data: Vec<Vec<u8>> = files.iter().map(|f| f.metadata_bytes()).collect();
let checksums = md5_digest_batch(&file_data);
```

**Input characteristics**:
- Count: 100 - 100,000 files
- Size: 32 - 1024 bytes per entry
- Pattern: Many small independent inputs

**Best approach**: **SIMD batch** or **Hybrid**
- Rayon overhead dominates for small inputs
- SIMD processes 4-16 at once with minimal overhead

#### 2. Block-Level Checksums (Delta Sync)
**Pattern**: Hash fixed-size blocks during delta generation
```rust
// Example: 700KB file with 1KB blocks = 700 checksums
let blocks: Vec<&[u8]> = file_data.chunks(BLOCK_SIZE).collect();
let signatures = compute_digests_parallel::<Md5, _>(&blocks);
```

**Input characteristics**:
- Count: 10 - 10,000 blocks
- Size: 512 bytes - 8KB per block
- Pattern: Uniform size, sequential access

**Best approach**: **Rayon** or **Hybrid**
- Enough work to justify threads
- Could benefit from SIMD within threads

#### 3. Whole-File Checksums
**Pattern**: Stream hash of entire file content
```rust
// Example: verify 100MB file transfer
let mut hasher = Md5::new();
for chunk in file.chunks(64 * 1024) {
    hasher.update(chunk);
}
let digest = hasher.finalize();
```

**Input characteristics**:
- Count: 1 file
- Size: 1MB - 1GB+
- Pattern: Streaming

**Best approach**: **RustCrypto md-5 with asm**
- Single-stream performance matters
- No batching opportunity
- Assembly-optimized compression

## Benchmarking Script

To make a data-driven decision, run this benchmark suite:

```rust
// benches/md5_strategies.rs
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use checksums::strong::{Md5, StrongDigest};
use rayon::prelude::*;

fn generate_test_data(count: usize, size: usize) -> Vec<Vec<u8>> {
    (0..count).map(|i| vec![i as u8; size]).collect()
}

fn bench_strategies(c: &mut Criterion) {
    let scenarios = [
        ("file_list", 1000, 128),     // 1000 files, 128 bytes each
        ("small_blocks", 1000, 512),   // 1000 blocks, 512 bytes each
        ("medium_blocks", 500, 4096),  // 500 blocks, 4KB each
        ("large_blocks", 100, 65536),  // 100 blocks, 64KB each
    ];

    for (name, count, size) in scenarios {
        let mut group = c.benchmark_group(format!("md5_{}", name));
        let data = generate_test_data(count, size);
        let total_bytes = (count * size) as u64;
        group.throughput(Throughput::Bytes(total_bytes));

        // Strategy 1: Sequential
        group.bench_function("sequential", |b| {
            b.iter(|| {
                data.iter()
                    .map(|d| Md5::digest(black_box(d)))
                    .collect::<Vec<_>>()
            });
        });

        // Strategy 2: Rayon parallel
        group.bench_function("rayon", |b| {
            b.iter(|| {
                data.par_iter()
                    .map(|d| Md5::digest(black_box(d)))
                    .collect::<Vec<_>>()
            });
        });

        // Strategy 3: SIMD batch (if available)
        #[cfg(feature = "md5-simd")]
        group.bench_function("simd_batch", |b| {
            b.iter(|| {
                checksums::strong::md5_digest_batch(black_box(&data))
            });
        });

        // Strategy 4: Hybrid (Rayon + SIMD)
        #[cfg(feature = "md5-simd")]
        group.bench_function("hybrid", |b| {
            b.iter(|| {
                data.par_chunks(16)
                    .flat_map(|chunk| checksums::strong::md5_digest_batch(chunk))
                    .collect::<Vec<_>>()
            });
        });

        group.finish();
    }
}

criterion_group!(benches, bench_strategies);
criterion_main!(benches);
```

**Run with**:
```bash
cargo bench --bench md5_strategies -- --save-baseline current
```

**Expected results**:
- **File list (small inputs)**: SIMD batch fastest, Rayon has overhead
- **Medium blocks**: Rayon competitive, hybrid slightly better
- **Large blocks**: Rayon sufficient, SIMD overhead not worth it

## Real-World Profiling

### Instrument Production Code

Add performance counters to measure actual usage:

```rust
// In checksums/src/strong/md5.rs
use std::sync::atomic::{AtomicU64, Ordering};

static DIGEST_CALLS: AtomicU64 = AtomicU64::new(0);
static DIGEST_BYTES: AtomicU64 = AtomicU64::new(0);
static BATCH_CALLS: AtomicU64 = AtomicU64::new(0);
static BATCH_INPUTS: AtomicU64 = AtomicU64::new(0);

impl StrongDigest for Md5 {
    fn digest(data: &[u8]) -> Self::Digest {
        DIGEST_CALLS.fetch_add(1, Ordering::Relaxed);
        DIGEST_BYTES.fetch_add(data.len() as u64, Ordering::Relaxed);
        // ... existing implementation
    }
}

pub fn digest_batch<T: AsRef<[u8]>>(inputs: &[T]) -> Vec<[u8; 16]> {
    BATCH_CALLS.fetch_add(1, Ordering::Relaxed);
    BATCH_INPUTS.fetch_add(inputs.len() as u64, Ordering::Relaxed);
    // ... existing implementation
}

pub fn print_stats() {
    eprintln!("MD5 Stats:");
    eprintln!("  Single digest calls: {}", DIGEST_CALLS.load(Ordering::Relaxed));
    eprintln!("  Total bytes hashed: {}", DIGEST_BYTES.load(Ordering::Relaxed));
    eprintln!("  Batch calls: {}", BATCH_CALLS.load(Ordering::Relaxed));
    eprintln!("  Avg batch size: {:.1}",
        BATCH_INPUTS.load(Ordering::Relaxed) as f64 /
        BATCH_CALLS.load(Ordering::Relaxed).max(1) as f64);
}
```

Run a typical rsync workload and check the stats:
```bash
cargo build --release
./target/release/rsync -avz source/ dest/
# Check logs for stats output
```

## Performance Estimation

### Hardware Reference: AMD Ryzen 9 / Intel Core i9

**Single-core MD5 throughput** (estimated):
- Pure Rust (RustCrypto soft): ~500 MB/s
- Assembly (md5-asm): ~700 MB/s
- SIMD 4-lane (SSE2): ~1500 MB/s (4 × 400 MB/s with overhead)
- SIMD 8-lane (AVX2): ~2800 MB/s (8 × 400 MB/s with overhead)
- SIMD 16-lane (AVX-512): ~5000 MB/s (16 × 400 MB/s with overhead)

**Rayon multi-core** (8 threads):
- Assembly: 8 × 700 MB/s = ~5600 MB/s
- Pure Rust: 8 × 500 MB/s = ~4000 MB/s

**Hybrid** (8 threads × 16 SIMD lanes):
- Theoretical: 8 × 5000 MB/s = ~40 GB/s
- Realistic: Memory bandwidth limited (~20 GB/s)

### Break-Even Analysis

**When does SIMD beat Rayon?**

Rayon overhead per task: ~1-5 µs (thread scheduling)
SIMD setup cost: ~0.1 µs (lane shuffling)

For a 512-byte input:
- Single hash time: ~1 µs
- Rayon total: 1 µs + 2 µs overhead = **3 µs**
- SIMD batch (8 inputs): 8 µs + 0.1 µs = **8.1 µs ÷ 8 = 1.01 µs each**

**SIMD wins when**:
- Input size < 4KB
- Batch size ≥ 8 inputs
- Single-threaded or thread pool saturated

**Rayon wins when**:
- Input size > 16KB
- High CPU core count available
- Inputs arrive over time (streaming)

## Feature Comparison: md5-asm vs md5-simd

### RustCrypto md-5 with "asm" feature

**Implementation**: Hand-coded x86/x64 assembly (md5-asm)
```asm
; x64.S (simplified)
md5_compress:
    mov     eax, [rdi]      ; Load state
    mov     ebx, [rdi+4]
    ; ... unrolled MD5 rounds ...
    add     [rdi], eax      ; Update state
    ret
```

**Characteristics**:
- **Platform**: x86/x64 only (Linux/macOS, not Windows)
- **Optimization**: Register scheduling, instruction pairing
- **Throughput**: ~700 MB/s single-stream on modern CPUs
- **Latency**: Minimal overhead, tight loops
- **Use case**: Best for large single files

### Custom md5-simd

**Implementation**: Rust intrinsics (SIMD)
```rust
// Simplified AVX2 example
use std::arch::x86_64::*;

pub unsafe fn digest_x8(inputs: &[&[u8]; 8]) -> [[u8; 16]; 8] {
    let mut states = [_mm256_set_epi32(...); 4]; // 8-wide state

    for block in interleave_blocks(inputs) {
        md5_round(&mut states, &block); // Process 8 blocks at once
    }

    deinterleave_states(states)
}
```

**Characteristics**:
- **Platform**: x86_64, aarch64, wasm32 (multi-platform)
- **Optimization**: Data-level parallelism, 4-16 inputs simultaneously
- **Throughput**: ~2-5 GB/s for batches on modern CPUs
- **Latency**: Higher per-input overhead due to setup
- **Use case**: Best for many small inputs

## Decision Matrix

| Workload | Input Size | Input Count | Best Strategy | Speedup |
|----------|-----------|-------------|---------------|---------|
| File list metadata | 32-512 B | 1,000+ | SIMD batch | 8-12x |
| Delta block sigs | 512 B - 4 KB | 100-10,000 | Hybrid | 4-8x |
| Medium files | 4-64 KB | 10-1,000 | Rayon | 6-8x |
| Large files | 64 KB+ | 1-100 | Rayon + asm | 8x |
| Streaming transfer | 1 MB+ | 1 | RustCrypto asm | 1.4x |

## Recommendation Summary

### Use md5-simd when:
1. **Batch hashing is common** in profiled workloads
2. **Many small inputs** (< 4KB) need checksums
3. **Single-threaded efficiency** matters (embedded, constrained envs)

### Use RustCrypto md-5 when:
1. **Streaming use cases** dominate
2. **Simplicity** and **maintenance** are priorities
3. **Large single files** are the common pattern

### Use Hybrid approach:
1. Keep RustCrypto for streaming API
2. Keep md5-simd only for batch functions
3. Let caller choose based on use case

### Immediate action:
1. **Run the benchmark suite** above
2. **Profile production rsync runs** with instrumentation
3. **Measure actual batch opportunities** in real workloads
4. **Make data-driven decision** based on results

## Expected Outcome

Based on rsync's typical workload (many small-to-medium files):
- **File sync**: SIMD batch likely wins (file list checksums)
- **Large transfers**: RustCrypto asm sufficient (whole-file hashes)
- **Delta sync**: Rayon already good (block signatures)

**Likely conclusion**: Keep both, use SIMD for batch operations where applicable, RustCrypto for streaming.
