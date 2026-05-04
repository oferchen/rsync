//! crates/checksums/benches/pipelined_benchmark.rs
//!
//! Benchmarks comparing pipelined vs sequential checksum computation.
//!
//! Run with: `cargo bench -p checksums -- pipelined`

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use rand::Rng;
use std::io::{Cursor, Read};

use checksums::RollingDigest;
use checksums::pipelined::{DoubleBufferedReader, PipelineConfig, compute_checksums_pipelined};
use checksums::strong::{Md4, Md5, Sha256, StrongDigest, Xxh3};

/// Generate random data of the specified size.
fn generate_random_data(size: usize) -> Vec<u8> {
    let mut rng = rand::thread_rng();
    let mut data = vec![0u8; size];
    rng.fill(&mut data[..]);
    data
}

/// Sequential (non-pipelined) checksum computation for comparison.
fn compute_checksums_sequential<D: StrongDigest>(
    data: &[u8],
    block_size: usize,
) -> Vec<(RollingDigest, D::Digest)>
where
    D::Seed: Default,
{
    let mut results = Vec::new();
    let mut offset = 0;

    while offset < data.len() {
        let end = (offset + block_size).min(data.len());
        let block = &data[offset..end];

        let rolling = RollingDigest::from_bytes(block);
        let strong = D::digest(block);
        results.push((rolling, strong));

        offset = end;
    }

    results
}

/// Benchmark pipelined vs sequential with MD5 (CPU-intensive).
fn bench_pipelined_md5(c: &mut Criterion) {
    let mut group = c.benchmark_group("pipelined_md5");

    for file_size in [256 * 1024, 1024 * 1024, 4 * 1024 * 1024] {
        let data = generate_random_data(file_size);
        let block_size = 64 * 1024;

        group.throughput(Throughput::Bytes(file_size as u64));

        // Sequential baseline
        group.bench_with_input(
            BenchmarkId::new("sequential", file_size),
            &data,
            |b, data| {
                b.iter(|| {
                    let result = compute_checksums_sequential::<Md5>(black_box(data), block_size);
                    black_box(result)
                });
            },
        );

        // Pipelined
        group.bench_with_input(
            BenchmarkId::new("pipelined", file_size),
            &data,
            |b, data| {
                b.iter(|| {
                    let config = PipelineConfig::default()
                        .with_block_size(block_size)
                        .with_min_file_size(0);
                    let result = compute_checksums_pipelined::<Md5, _>(
                        Cursor::new(black_box(data.clone())),
                        config,
                        Some(file_size as u64),
                    )
                    .unwrap();
                    black_box(result)
                });
            },
        );
    }

    group.finish();
}

/// Benchmark pipelined vs sequential with MD4 (CPU-intensive).
fn bench_pipelined_md4(c: &mut Criterion) {
    let mut group = c.benchmark_group("pipelined_md4");

    for file_size in [256 * 1024, 1024 * 1024, 4 * 1024 * 1024] {
        let data = generate_random_data(file_size);
        let block_size = 64 * 1024;

        group.throughput(Throughput::Bytes(file_size as u64));

        // Sequential baseline
        group.bench_with_input(
            BenchmarkId::new("sequential", file_size),
            &data,
            |b, data| {
                b.iter(|| {
                    let result = compute_checksums_sequential::<Md4>(black_box(data), block_size);
                    black_box(result)
                });
            },
        );

        // Pipelined
        group.bench_with_input(
            BenchmarkId::new("pipelined", file_size),
            &data,
            |b, data| {
                b.iter(|| {
                    let config = PipelineConfig::default()
                        .with_block_size(block_size)
                        .with_min_file_size(0);
                    let result = compute_checksums_pipelined::<Md4, _>(
                        Cursor::new(black_box(data.clone())),
                        config,
                        Some(file_size as u64),
                    )
                    .unwrap();
                    black_box(result)
                });
            },
        );
    }

    group.finish();
}

/// Benchmark pipelined vs sequential with SHA256 (CPU-intensive).
fn bench_pipelined_sha256(c: &mut Criterion) {
    let mut group = c.benchmark_group("pipelined_sha256");

    for file_size in [256 * 1024, 1024 * 1024, 4 * 1024 * 1024] {
        let data = generate_random_data(file_size);
        let block_size = 64 * 1024;

        group.throughput(Throughput::Bytes(file_size as u64));

        // Sequential baseline
        group.bench_with_input(
            BenchmarkId::new("sequential", file_size),
            &data,
            |b, data| {
                b.iter(|| {
                    let result =
                        compute_checksums_sequential::<Sha256>(black_box(data), block_size);
                    black_box(result)
                });
            },
        );

        // Pipelined
        group.bench_with_input(
            BenchmarkId::new("pipelined", file_size),
            &data,
            |b, data| {
                b.iter(|| {
                    let config = PipelineConfig::default()
                        .with_block_size(block_size)
                        .with_min_file_size(0);
                    let result = compute_checksums_pipelined::<Sha256, _>(
                        Cursor::new(black_box(data.clone())),
                        config,
                        Some(file_size as u64),
                    )
                    .unwrap();
                    black_box(result)
                });
            },
        );
    }

    group.finish();
}

/// Benchmark pipelined vs sequential with XXH3 (fast, non-CPU-intensive).
/// This tests the overhead of pipelining - for fast hashes, sequential may be faster.
fn bench_pipelined_xxh3(c: &mut Criterion) {
    let mut group = c.benchmark_group("pipelined_xxh3");

    for file_size in [256 * 1024, 1024 * 1024, 4 * 1024 * 1024] {
        let data = generate_random_data(file_size);
        let block_size = 64 * 1024;

        group.throughput(Throughput::Bytes(file_size as u64));

        // Sequential baseline
        group.bench_with_input(
            BenchmarkId::new("sequential", file_size),
            &data,
            |b, data| {
                b.iter(|| {
                    let result = compute_checksums_sequential::<Xxh3>(black_box(data), block_size);
                    black_box(result)
                });
            },
        );

        // Pipelined
        group.bench_with_input(
            BenchmarkId::new("pipelined", file_size),
            &data,
            |b, data| {
                b.iter(|| {
                    let config = PipelineConfig::default()
                        .with_block_size(block_size)
                        .with_min_file_size(0);
                    let result = compute_checksums_pipelined::<Xxh3, _>(
                        Cursor::new(black_box(data.clone())),
                        config,
                        Some(file_size as u64),
                    )
                    .unwrap();
                    black_box(result)
                });
            },
        );
    }

    group.finish();
}

/// Benchmark the double-buffered reader itself.
fn bench_double_buffered_reader(c: &mut Criterion) {
    let mut group = c.benchmark_group("double_buffered_reader");

    let file_size = 4 * 1024 * 1024;
    let data = generate_random_data(file_size);
    let block_size = 64 * 1024;

    group.throughput(Throughput::Bytes(file_size as u64));

    // Direct read (no pipelining)
    group.bench_function("direct_read", |b| {
        b.iter(|| {
            let mut reader = Cursor::new(black_box(data.clone()));
            let mut buffer = vec![0u8; block_size];
            let mut total = 0;

            loop {
                let n = reader.read(&mut buffer).unwrap();
                if n == 0 {
                    break;
                }
                total += n;
            }

            black_box(total)
        });
    });

    // Pipelined read
    group.bench_function("pipelined_read", |b| {
        b.iter(|| {
            let config = PipelineConfig::default()
                .with_block_size(block_size)
                .with_min_file_size(0);
            let mut reader = DoubleBufferedReader::with_size_hint(
                Cursor::new(black_box(data.clone())),
                config,
                Some(file_size as u64),
            );

            let mut total = 0;
            while let Some(block) = reader.next_block().unwrap() {
                total += block.len();
            }

            black_box(total)
        });
    });

    group.finish();
}

/// Benchmark different block sizes for pipelining.
fn bench_block_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("pipelined_block_sizes");

    let file_size = 4 * 1024 * 1024;
    let data = generate_random_data(file_size);

    group.throughput(Throughput::Bytes(file_size as u64));

    for block_size in [16 * 1024, 32 * 1024, 64 * 1024, 128 * 1024, 256 * 1024] {
        group.bench_with_input(BenchmarkId::new("md5", block_size), &data, |b, data| {
            b.iter(|| {
                let config = PipelineConfig::default()
                    .with_block_size(block_size)
                    .with_min_file_size(0);
                let result = compute_checksums_pipelined::<Md5, _>(
                    Cursor::new(black_box(data.clone())),
                    config,
                    Some(file_size as u64),
                )
                .unwrap();
                black_box(result)
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_pipelined_md5,
    bench_pipelined_md4,
    bench_pipelined_sha256,
    bench_pipelined_xxh3,
    bench_double_buffered_reader,
    bench_block_sizes,
);
criterion_main!(benches);
