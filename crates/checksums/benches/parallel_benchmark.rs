//! crates/checksums/benches/parallel_benchmark.rs
//!
//! Benchmarks comparing sequential vs parallel checksum computation.
//!
//! Run with: `cargo bench -p checksums --features parallel`

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use rand::Rng;

use checksums::parallel::{
    compute_block_signatures_parallel, compute_digests_parallel,
    compute_rolling_checksums_parallel,
};
use checksums::strong::{Md5, Sha256, Xxh3};
use checksums::RollingChecksum;

/// Generate random data of the specified size.
fn generate_random_data(size: usize) -> Vec<u8> {
    let mut rng = rand::thread_rng();
    let mut data = vec![0u8; size];
    rng.fill(&mut data[..]);
    data
}

/// Generate multiple blocks of random data.
fn generate_random_blocks(num_blocks: usize, block_size: usize) -> Vec<Vec<u8>> {
    (0..num_blocks)
        .map(|_| generate_random_data(block_size))
        .collect()
}

/// Compare sequential vs parallel MD5 digest computation.
fn bench_md5_sequential_vs_parallel(c: &mut Criterion) {
    let mut group = c.benchmark_group("md5_seq_vs_par");

    let block_size = 8192;

    for num_blocks in [10, 100, 500, 1000] {
        let blocks = generate_random_blocks(num_blocks, block_size);
        let total_bytes = num_blocks * block_size;

        group.throughput(Throughput::Bytes(total_bytes as u64));

        group.bench_with_input(
            BenchmarkId::new("sequential", num_blocks),
            &blocks,
            |b, blocks| {
                b.iter(|| {
                    let digests: Vec<_> = blocks
                        .iter()
                        .map(|block| Md5::digest(block.as_slice()))
                        .collect();
                    black_box(digests)
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("parallel", num_blocks),
            &blocks,
            |b, blocks| {
                b.iter(|| {
                    let digests = compute_digests_parallel::<Md5, _>(black_box(blocks));
                    black_box(digests)
                });
            },
        );
    }

    group.finish();
}

/// Compare sequential vs parallel SHA-256 digest computation.
fn bench_sha256_sequential_vs_parallel(c: &mut Criterion) {
    let mut group = c.benchmark_group("sha256_seq_vs_par");

    let block_size = 8192;

    for num_blocks in [10, 100, 500, 1000] {
        let blocks = generate_random_blocks(num_blocks, block_size);
        let total_bytes = num_blocks * block_size;

        group.throughput(Throughput::Bytes(total_bytes as u64));

        group.bench_with_input(
            BenchmarkId::new("sequential", num_blocks),
            &blocks,
            |b, blocks| {
                b.iter(|| {
                    let digests: Vec<_> = blocks
                        .iter()
                        .map(|block| Sha256::digest(block.as_slice()))
                        .collect();
                    black_box(digests)
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("parallel", num_blocks),
            &blocks,
            |b, blocks| {
                b.iter(|| {
                    let digests = compute_digests_parallel::<Sha256, _>(black_box(blocks));
                    black_box(digests)
                });
            },
        );
    }

    group.finish();
}

/// Compare sequential vs parallel XXH3 digest computation.
fn bench_xxh3_sequential_vs_parallel(c: &mut Criterion) {
    let mut group = c.benchmark_group("xxh3_seq_vs_par");

    let block_size = 8192;

    for num_blocks in [10, 100, 500, 1000] {
        let blocks = generate_random_blocks(num_blocks, block_size);
        let total_bytes = num_blocks * block_size;

        group.throughput(Throughput::Bytes(total_bytes as u64));

        group.bench_with_input(
            BenchmarkId::new("sequential", num_blocks),
            &blocks,
            |b, blocks| {
                b.iter(|| {
                    let digests: Vec<_> = blocks
                        .iter()
                        .map(|block| Xxh3::digest(0, block.as_slice()))
                        .collect();
                    black_box(digests)
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("parallel", num_blocks),
            &blocks,
            |b, blocks| {
                b.iter(|| {
                    let digests = compute_digests_parallel::<Xxh3, _>(black_box(blocks));
                    black_box(digests)
                });
            },
        );
    }

    group.finish();
}

/// Compare sequential vs parallel rolling checksum computation.
fn bench_rolling_checksum_sequential_vs_parallel(c: &mut Criterion) {
    let mut group = c.benchmark_group("rolling_seq_vs_par");

    let block_size = 8192;

    for num_blocks in [10, 100, 500, 1000] {
        let blocks = generate_random_blocks(num_blocks, block_size);
        let total_bytes = num_blocks * block_size;

        group.throughput(Throughput::Bytes(total_bytes as u64));

        group.bench_with_input(
            BenchmarkId::new("sequential", num_blocks),
            &blocks,
            |b, blocks| {
                b.iter(|| {
                    let checksums: Vec<_> = blocks
                        .iter()
                        .map(|block| {
                            let mut checksum = RollingChecksum::new();
                            checksum.update(block.as_slice());
                            checksum.value()
                        })
                        .collect();
                    black_box(checksums)
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("parallel", num_blocks),
            &blocks,
            |b, blocks| {
                b.iter(|| {
                    let checksums = compute_rolling_checksums_parallel(black_box(blocks));
                    black_box(checksums)
                });
            },
        );
    }

    group.finish();
}

/// Compare sequential vs parallel block signature computation.
fn bench_block_signatures_sequential_vs_parallel(c: &mut Criterion) {
    let mut group = c.benchmark_group("block_signatures_seq_vs_par");

    let block_size = 8192;

    for num_blocks in [10, 100, 500, 1000] {
        let blocks = generate_random_blocks(num_blocks, block_size);
        let total_bytes = num_blocks * block_size;

        group.throughput(Throughput::Bytes(total_bytes as u64));

        group.bench_with_input(
            BenchmarkId::new("sequential", num_blocks),
            &blocks,
            |b, blocks| {
                b.iter(|| {
                    let signatures: Vec<_> = blocks
                        .iter()
                        .map(|block| {
                            let mut rolling = RollingChecksum::new();
                            rolling.update(block.as_slice());
                            (rolling.value(), Md5::digest(block.as_slice()))
                        })
                        .collect();
                    black_box(signatures)
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("parallel", num_blocks),
            &blocks,
            |b, blocks| {
                b.iter(|| {
                    let signatures = compute_block_signatures_parallel::<Md5, _>(black_box(blocks));
                    black_box(signatures)
                });
            },
        );
    }

    group.finish();
}

/// Benchmark scaling with different block sizes.
fn bench_parallel_scaling_block_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel_scaling_block_size");

    let num_blocks = 100;

    for block_size in [512, 1024, 4096, 8192, 32768, 131072] {
        let blocks = generate_random_blocks(num_blocks, block_size);
        let total_bytes = num_blocks * block_size;

        group.throughput(Throughput::Bytes(total_bytes as u64));

        group.bench_with_input(
            BenchmarkId::new("parallel_md5", block_size),
            &blocks,
            |b, blocks| {
                b.iter(|| {
                    let digests = compute_digests_parallel::<Md5, _>(black_box(blocks));
                    black_box(digests)
                });
            },
        );
    }

    group.finish();
}

/// Benchmark parallel overhead for small workloads.
fn bench_parallel_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel_overhead");

    let block_size = 8192;

    // Very small block counts to measure overhead
    for num_blocks in [1, 2, 4, 8, 16, 32] {
        let blocks = generate_random_blocks(num_blocks, block_size);
        let total_bytes = num_blocks * block_size;

        group.throughput(Throughput::Bytes(total_bytes as u64));

        group.bench_with_input(
            BenchmarkId::new("sequential", num_blocks),
            &blocks,
            |b, blocks| {
                b.iter(|| {
                    let digests: Vec<_> = blocks
                        .iter()
                        .map(|block| Md5::digest(block.as_slice()))
                        .collect();
                    black_box(digests)
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("parallel", num_blocks),
            &blocks,
            |b, blocks| {
                b.iter(|| {
                    let digests = compute_digests_parallel::<Md5, _>(black_box(blocks));
                    black_box(digests)
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_md5_sequential_vs_parallel,
    bench_sha256_sequential_vs_parallel,
    bench_xxh3_sequential_vs_parallel,
    bench_rolling_checksum_sequential_vs_parallel,
    bench_block_signatures_sequential_vs_parallel,
    bench_parallel_scaling_block_size,
    bench_parallel_overhead,
);

criterion_main!(benches);
