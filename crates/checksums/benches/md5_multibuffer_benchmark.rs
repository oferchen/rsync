//! crates/checksums/benches/md5_multibuffer_benchmark.rs
//!
//! Focused micro-bench comparing the scalar single-stream MD5 path against
//! the multi-buffer SIMD batch path (`md5_digest_batch`) at a sweep of batch
//! widths N = {1, 4, 8, 16, 64}.
//!
//! # Why this bench exists
//!
//! Strong-checksum work for protocol >= 27 uses MD5; the signature generator
//! accumulates per-block work and hands it to `md5_digest_batch`, which
//! dispatches at runtime to the widest available SIMD backend (AVX-512 16-way,
//! AVX2 8-way, SSE4.1 / SSSE3 / SSE2 / NEON / WASM SIMD 4-way, scalar 1-way).
//! The framing bench in `framing_overhead_benchmark.rs` exercises one fixed
//! batch window (16 blocks) folded into a wider framing analysis; this bench
//! instead isolates the SIMD lane-width payoff at the granularity each
//! backend actually pivots on, mirroring `md4_multibuffer_benchmark.rs`.
//!
//! - **N = 1** - degenerate batch; pays SIMD setup with no parallelism. Shows
//!   the worst-case crossover point vs scalar.
//! - **N = 4** - exact fill for SSE2/SSE4.1/SSSE3 and NEON/WASM; AVX2 /
//!   AVX-512 run partial.
//! - **N = 8** - exact fill for AVX2; AVX-512 runs partial.
//! - **N = 16** - exact fill for AVX-512; matches the generator's batch
//!   window (`BATCH_SIZE = 16` in `framing_overhead_benchmark.rs`).
//! - **N = 64** - several full passes per backend; amortizes any per-call
//!   overhead and shows steady-state throughput.
//!
//! # Block sizes
//!
//! Block sizes match the signature generator's growth curve: 700 B is
//! upstream's `DEFAULT_BLOCK_SIZE` for small files, 4 KiB / 32 KiB / 128 KiB
//! cover the range up to `MAX_BLOCK_SIZE_V30`. Throughput is reported as
//! total bytes hashed per call (N * block_size).
//!
//! Run with: `cargo bench -p checksums --bench md5_multibuffer_benchmark`

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

use checksums::strong::{Md5, md5_digest_batch};

/// Batch widths swept by this bench. Chosen to land exactly on the lane
/// counts of each supported SIMD backend (SSE2/SSE4.1/SSSE3/NEON/WASM = 4,
/// AVX2 = 8, AVX-512 = 16), plus N = 1 (worst case) and N = 64 (steady state).
const BATCH_WIDTHS: &[usize] = &[1, 4, 8, 16, 64];

/// Block sizes the signature generator picks across the file-size range.
const BLOCK_SIZES: &[usize] = &[700, 4_096, 32_768, 131_072];

/// Builds a deterministic pseudo-random buffer so re-runs are comparable
/// without depending on the host RNG. Uses xorshift64 - cheap and produces
/// data the hash backends cannot specialize on.
fn build_buffer(size: usize) -> Vec<u8> {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut out = vec![0u8; size];
    let mut i = 0;
    while i + 8 <= size {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out[i..i + 8].copy_from_slice(&state.to_le_bytes());
        i += 8;
    }
    while i < size {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out[i] = state as u8;
        i += 1;
    }
    out
}

/// Splits `buf` into `n` contiguous block_size slices.
fn make_blocks(buf: &[u8], n: usize, block_size: usize) -> Vec<&[u8]> {
    (0..n)
        .map(|i| {
            let start = i * block_size;
            &buf[start..start + block_size]
        })
        .collect()
}

/// Keep wall time bearable for the larger (N=64, 128 KiB) configurations
/// while giving Criterion enough samples for narrow confidence intervals.
fn configured_criterion() -> Criterion {
    Criterion::default()
        .sample_size(20)
        .warm_up_time(std::time::Duration::from_millis(500))
        .measurement_time(std::time::Duration::from_secs(3))
}

/// Bench scalar vs multi-buffer batch at each (N, block_size) pair.
fn bench_md5_multibuffer(c: &mut Criterion) {
    let mut group = c.benchmark_group("md5_multibuffer");

    for &block_size in BLOCK_SIZES {
        for &n in BATCH_WIDTHS {
            let total_bytes = n * block_size;
            let buf = build_buffer(total_bytes);
            let blocks = make_blocks(&buf, n, block_size);
            let id = format!("n{n}_block{block_size}");

            group.throughput(Throughput::Bytes(total_bytes as u64));

            // Scalar baseline: hash each block one at a time via the public
            // `Md5` API. Mirrors what callers do before opting into the batch
            // helper.
            group.bench_with_input(BenchmarkId::new("scalar", &id), &blocks, |b, blocks| {
                b.iter(|| {
                    for block in blocks {
                        black_box(Md5::digest(black_box(block)));
                    }
                });
            });

            // Multi-buffer batch: hands all N blocks to the SIMD dispatcher in
            // a single call. The dispatcher picks AVX-512 / AVX2 / SSE4.1 /
            // SSSE3 / SSE2 / NEON / WASM / scalar based on the cached
            // `is_x86_feature_detected!` probe (or arch-equivalent).
            group.bench_with_input(BenchmarkId::new("batch", &id), &blocks, |b, blocks| {
                b.iter(|| {
                    black_box(md5_digest_batch(black_box(blocks)));
                });
            });
        }
    }

    group.finish();
}

criterion_group! {
    name = benches;
    config = configured_criterion();
    targets = bench_md5_multibuffer,
}

criterion_main!(benches);
