//! Throughput benchmarks for md5-simd.
//!
//! Run with: cargo bench -p md5-simd

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use md5_simd::{active_backend, digest, digest_batch};
use rand::{rngs::StdRng, Rng, SeedableRng};

/// Generate random data of specified size.
fn random_data(size: usize, seed: u64) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(seed);
    (0..size).map(|_| rng.gen()).collect()
}

/// Benchmark single digest at various input sizes.
fn bench_single_digest(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_digest");

    for size in [64, 1_024, 64 * 1_024, 1_024 * 1_024] {
        let data = random_data(size, 42);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &data, |b, data| {
            b.iter(|| digest(black_box(data)));
        });
    }

    group.finish();
}

/// Benchmark batch digest at various batch sizes.
fn bench_batch_digest(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_digest");

    let input_size = 1_024; // 1KB per input
    let data = random_data(input_size, 42);

    for batch_size in [8, 64, 256] {
        let inputs: Vec<&[u8]> = (0..batch_size).map(|_| data.as_slice()).collect();
        let total_bytes = (input_size * batch_size) as u64;

        group.throughput(Throughput::Bytes(total_bytes));
        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            &inputs,
            |b, inputs| {
                b.iter(|| digest_batch(black_box(inputs)));
            },
        );
    }

    group.finish();
}

/// Compare md5-simd against md-5 crate (reference implementation).
fn bench_vs_reference(c: &mut Criterion) {
    use md5::{Digest, Md5};

    let mut group = c.benchmark_group("vs_reference");

    let size = 64 * 1_024; // 64KB
    let data = random_data(size, 42);

    group.throughput(Throughput::Bytes(size as u64));

    group.bench_function("md5_simd", |b| {
        b.iter(|| digest(black_box(&data)));
    });

    group.bench_function("md5_crate", |b| {
        b.iter(|| {
            let mut hasher = Md5::new();
            hasher.update(black_box(&data));
            hasher.finalize()
        });
    });

    group.finish();
}

/// Benchmark batch with varying input lengths (real-world scenario).
fn bench_mixed_lengths(c: &mut Criterion) {
    let mut group = c.benchmark_group("mixed_lengths");

    // Simulate real rsync file checksums: varying file sizes
    let sizes = [64, 128, 256, 512, 1_024, 2_048, 4_096, 8_192];
    let inputs: Vec<Vec<u8>> = sizes.iter().map(|&s| random_data(s, s as u64)).collect();
    let refs: Vec<&[u8]> = inputs.iter().map(|v| v.as_slice()).collect();
    let total_bytes: u64 = sizes.iter().sum::<usize>() as u64;

    group.throughput(Throughput::Bytes(total_bytes));

    group.bench_function("batch_8_mixed", |b| {
        b.iter(|| digest_batch(black_box(&refs)));
    });

    group.finish();
}

/// Report the active backend.
fn bench_report_backend(c: &mut Criterion) {
    let backend = active_backend();
    println!("\nActive backend: {:?} ({} lanes)", backend, backend.lanes());

    c.bench_function("backend_query", |b| {
        b.iter(active_backend);
    });
}

criterion_group!(
    benches,
    bench_report_backend,
    bench_single_digest,
    bench_batch_digest,
    bench_vs_reference,
    bench_mixed_lengths,
);
criterion_main!(benches);
