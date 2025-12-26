//! crates/checksums/benches/checksums_benchmark.rs
//!
//! Benchmarks for checksum computation performance.
//!
//! Run with: `cargo bench -p checksums`

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use rand::Rng;

use checksums::RollingChecksum;
use checksums::strong::{Md4, Md5, Sha1, Sha256, Sha512, Xxh3, Xxh64};

/// Generate random data of the specified size.
fn generate_random_data(size: usize) -> Vec<u8> {
    let mut rng = rand::thread_rng();
    let mut data = vec![0u8; size];
    rng.fill(&mut data[..]);
    data
}

/// Benchmark rolling checksum computation for different block sizes.
fn bench_rolling_checksum(c: &mut Criterion) {
    let mut group = c.benchmark_group("rolling_checksum");

    for size in [512, 1024, 4096, 8192, 32768, 131072] {
        let data = generate_random_data(size);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("update", size), &data, |b, data| {
            b.iter(|| {
                let mut checksum = RollingChecksum::new();
                checksum.update(black_box(data));
                black_box(checksum.value())
            });
        });
    }

    group.finish();
}

/// Benchmark rolling checksum sliding window (roll) operation.
fn bench_rolling_checksum_roll(c: &mut Criterion) {
    let mut group = c.benchmark_group("rolling_checksum_roll");

    let block_size = 8192;
    let data = generate_random_data(block_size * 2);

    // Initialize checksum with first block
    let mut base_checksum = RollingChecksum::new();
    base_checksum.update(&data[..block_size]);

    group.bench_function("single_roll", |b| {
        b.iter(|| {
            let mut checksum = base_checksum.clone();
            // Roll single byte
            checksum
                .roll(black_box(data[0]), black_box(data[block_size]))
                .unwrap();
            black_box(checksum.value())
        });
    });

    group.bench_function("roll_many_128", |b| {
        b.iter(|| {
            let mut checksum = base_checksum.clone();
            let roll_count = 128;
            checksum
                .roll_many(
                    black_box(&data[..roll_count]),
                    black_box(&data[block_size..block_size + roll_count]),
                )
                .unwrap();
            black_box(checksum.value())
        });
    });

    group.finish();
}

/// Benchmark MD5 digest computation.
fn bench_md5_digest(c: &mut Criterion) {
    let mut group = c.benchmark_group("md5_digest");

    for size in [512, 1024, 4096, 32768, 131072] {
        let data = generate_random_data(size);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("digest", size), &data, |b, data| {
            b.iter(|| black_box(Md5::digest(black_box(data))));
        });
    }

    group.finish();
}

/// Benchmark MD4 digest computation.
fn bench_md4_digest(c: &mut Criterion) {
    let mut group = c.benchmark_group("md4_digest");

    for size in [512, 1024, 4096, 32768, 131072] {
        let data = generate_random_data(size);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("digest", size), &data, |b, data| {
            b.iter(|| black_box(Md4::digest(black_box(data))));
        });
    }

    group.finish();
}

/// Benchmark SHA-1 digest computation.
fn bench_sha1_digest(c: &mut Criterion) {
    let mut group = c.benchmark_group("sha1_digest");

    for size in [512, 1024, 4096, 32768, 131072] {
        let data = generate_random_data(size);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("digest", size), &data, |b, data| {
            b.iter(|| black_box(Sha1::digest(black_box(data))));
        });
    }

    group.finish();
}

/// Benchmark SHA-256 digest computation.
fn bench_sha256_digest(c: &mut Criterion) {
    let mut group = c.benchmark_group("sha256_digest");

    for size in [512, 1024, 4096, 32768, 131072] {
        let data = generate_random_data(size);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("digest", size), &data, |b, data| {
            b.iter(|| black_box(Sha256::digest(black_box(data))));
        });
    }

    group.finish();
}

/// Benchmark SHA-512 digest computation.
fn bench_sha512_digest(c: &mut Criterion) {
    let mut group = c.benchmark_group("sha512_digest");

    for size in [512, 1024, 4096, 32768, 131072] {
        let data = generate_random_data(size);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("digest", size), &data, |b, data| {
            b.iter(|| black_box(Sha512::digest(black_box(data))));
        });
    }

    group.finish();
}

/// Benchmark XXH64 digest computation.
fn bench_xxh64_digest(c: &mut Criterion) {
    let mut group = c.benchmark_group("xxh64_digest");

    for size in [512, 1024, 4096, 32768, 131072] {
        let data = generate_random_data(size);
        let seed = 0u64;

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("digest", size), &data, |b, data| {
            b.iter(|| black_box(Xxh64::digest(black_box(seed), black_box(data))));
        });
    }

    group.finish();
}

/// Benchmark XXH3 (64-bit) digest computation.
fn bench_xxh3_digest(c: &mut Criterion) {
    let mut group = c.benchmark_group("xxh3_digest");

    for size in [512, 1024, 4096, 32768, 131072] {
        let data = generate_random_data(size);
        let seed = 0u64;

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("digest", size), &data, |b, data| {
            b.iter(|| black_box(Xxh3::digest(black_box(seed), black_box(data))));
        });
    }

    group.finish();
}

/// Compare all digest algorithms at the same block size.
fn bench_algorithm_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("algorithm_comparison");

    let size = 8192; // Typical rsync block size
    let data = generate_random_data(size);

    group.throughput(Throughput::Bytes(size as u64));

    group.bench_function("rolling_checksum", |b| {
        b.iter(|| {
            let mut checksum = RollingChecksum::new();
            checksum.update(black_box(&data));
            black_box(checksum.value())
        });
    });

    group.bench_function("md4", |b| {
        b.iter(|| black_box(Md4::digest(black_box(&data))));
    });

    group.bench_function("md5", |b| {
        b.iter(|| black_box(Md5::digest(black_box(&data))));
    });

    group.bench_function("sha1", |b| {
        b.iter(|| black_box(Sha1::digest(black_box(&data))));
    });

    group.bench_function("sha256", |b| {
        b.iter(|| black_box(Sha256::digest(black_box(&data))));
    });

    group.bench_function("sha512", |b| {
        b.iter(|| black_box(Sha512::digest(black_box(&data))));
    });

    group.bench_function("xxh64", |b| {
        b.iter(|| black_box(Xxh64::digest(0, black_box(&data))));
    });

    group.bench_function("xxh3", |b| {
        b.iter(|| black_box(Xxh3::digest(0, black_box(&data))));
    });

    group.finish();
}

/// Benchmark multiple block signature computation (sequential).
fn bench_block_signatures_sequential(c: &mut Criterion) {
    let mut group = c.benchmark_group("block_signatures_sequential");

    let block_size = 8192;

    for num_blocks in [10, 100, 1000] {
        let blocks: Vec<Vec<u8>> = (0..num_blocks)
            .map(|_| generate_random_data(block_size))
            .collect();

        let total_bytes = num_blocks * block_size;
        group.throughput(Throughput::Bytes(total_bytes as u64));

        group.bench_with_input(
            BenchmarkId::new("md5_signatures", num_blocks),
            &blocks,
            |b, blocks| {
                b.iter(|| {
                    let signatures: Vec<_> = blocks
                        .iter()
                        .map(|block| {
                            let mut rolling = RollingChecksum::new();
                            rolling.update(block);
                            (rolling.value(), Md5::digest(block))
                        })
                        .collect();
                    black_box(signatures)
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_rolling_checksum,
    bench_rolling_checksum_roll,
    bench_md4_digest,
    bench_md5_digest,
    bench_sha1_digest,
    bench_sha256_digest,
    bench_sha512_digest,
    bench_xxh64_digest,
    bench_xxh3_digest,
    bench_algorithm_comparison,
    bench_block_signatures_sequential,
);

criterion_main!(benches);
