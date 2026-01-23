//! Benchmarks for TokenBuffer vs per-token allocation.
//!
//! Run with: `cargo bench -p transfer -- token_buffer`

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use transfer::token_buffer::TokenBuffer;

/// Benchmark per-token Vec allocation vs TokenBuffer reuse.
fn bench_token_allocation(c: &mut Criterion) {
    let mut group = c.benchmark_group("token_allocation");

    // Typical token sizes in rsync delta transfer
    for token_size in [700, 4096, 32768, 131072] {
        group.throughput(Throughput::Bytes(token_size as u64));

        // Per-token Vec allocation (baseline)
        group.bench_with_input(
            BenchmarkId::new("vec_alloc", format!("{}B", token_size)),
            &token_size,
            |b, &size| {
                b.iter(|| {
                    let mut buf = Vec::with_capacity(size);
                    buf.resize(size, 0u8);
                    // Simulate writing data
                    for i in 0..size.min(64) {
                        buf[i] = (i % 256) as u8;
                    }
                    black_box(&buf);
                });
            },
        );

        // TokenBuffer reuse
        let mut token_buf = TokenBuffer::new();
        group.bench_with_input(
            BenchmarkId::new("token_buffer", format!("{}B", token_size)),
            &token_size,
            |b, &size| {
                b.iter(|| {
                    token_buf.resize_for(size);
                    let slice = token_buf.as_mut_slice();
                    // Simulate writing data
                    for i in 0..size.min(64) {
                        slice[i] = (i % 256) as u8;
                    }
                    black_box(token_buf.as_slice());
                });
            },
        );
    }

    group.finish();
}

/// Benchmark sequential token processing (simulating delta application).
fn bench_sequential_tokens(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequential_tokens");

    for token_count in [100, 1000, 10000] {
        // Mix of token sizes typical in delta transfers
        let token_sizes: Vec<usize> = (0..token_count)
            .map(|i| match i % 4 {
                0 => 700,    // Block size
                1 => 4096,   // Page size
                2 => 32768,  // Chunk size
                _ => 1024,   // Random
            })
            .collect();

        let total_bytes: usize = token_sizes.iter().sum();
        group.throughput(Throughput::Bytes(total_bytes as u64));

        // Per-token allocation
        group.bench_with_input(
            BenchmarkId::new("vec_per_token", format!("{}_tokens", token_count)),
            &token_sizes,
            |b, sizes| {
                b.iter(|| {
                    for &size in sizes {
                        let mut buf = Vec::with_capacity(size);
                        buf.resize(size, 0u8);
                        buf[0] = 42;
                        black_box(&buf);
                    }
                });
            },
        );

        // TokenBuffer reuse
        group.bench_with_input(
            BenchmarkId::new("token_buffer_reuse", format!("{}_tokens", token_count)),
            &token_sizes,
            |b, sizes| {
                let mut token_buf = TokenBuffer::new();
                b.iter(|| {
                    for &size in sizes {
                        token_buf.resize_for(size);
                        token_buf.as_mut_slice()[0] = 42;
                        black_box(token_buf.as_slice());
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark varying size patterns (grow and shrink).
fn bench_varying_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("varying_sizes");

    // Pattern: grow from small to large, then back to small
    let sizes: Vec<usize> = (0..1000)
        .map(|i| {
            if i < 500 {
                700 + i * 100 // Grow
            } else {
                700 + (1000 - i) * 100 // Shrink
            }
        })
        .collect();

    let total_bytes: usize = sizes.iter().sum();
    group.throughput(Throughput::Bytes(total_bytes as u64));

    // Vec allocation (must allocate for each grow)
    group.bench_function("vec_varying", |b| {
        b.iter(|| {
            for &size in &sizes {
                let mut buf = Vec::with_capacity(size);
                buf.resize(size, 0u8);
                buf[0] = 42;
                black_box(&buf);
            }
        });
    });

    // TokenBuffer (grows once, never shrinks internal capacity)
    group.bench_function("token_buffer_varying", |b| {
        let mut token_buf = TokenBuffer::new();
        b.iter(|| {
            for &size in &sizes {
                token_buf.resize_for(size);
                token_buf.as_mut_slice()[0] = 42;
                black_box(token_buf.as_slice());
            }
        });
    });

    group.finish();
}

/// Benchmark TokenBuffer capacity behavior.
fn bench_capacity_tracking(c: &mut Criterion) {
    let mut group = c.benchmark_group("capacity_tracking");

    group.bench_function("token_buffer_capacity_stable", |b| {
        let mut token_buf = TokenBuffer::new();
        // Pre-grow to max size
        token_buf.resize_for(131072);

        b.iter(|| {
            // Alternate between sizes - capacity stays at max
            for i in 0..100 {
                let size = if i % 2 == 0 { 700 } else { 32768 };
                token_buf.resize_for(size);
                black_box(token_buf.as_slice());
            }
            // Verify capacity didn't shrink
            assert!(token_buf.capacity() >= 131072);
        });
    });

    group.finish();
}

criterion_group!(
    token_buffer_benchmarks,
    bench_token_allocation,
    bench_sequential_tokens,
    bench_varying_sizes,
    bench_capacity_tracking
);

criterion_main!(token_buffer_benchmarks);
