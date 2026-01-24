//! Benchmark comparing sequential vs parallel checksum computation.

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use tempfile::tempdir;

use checksums::strong::{Md5, Md5Seed, StrongDigest};

const FILE_SIZE: usize = 1024 * 1024; // 1 MB per file

fn create_test_files(count: usize) -> (tempfile::TempDir, Vec<PathBuf>) {
    let dir = tempdir().unwrap();
    let mut paths = Vec::with_capacity(count);

    for i in 0..count {
        let path = dir.path().join(format!("file_{i}.bin"));
        let mut file = fs::File::create(&path).unwrap();
        // Create predictable content
        let content: Vec<u8> = (0..FILE_SIZE).map(|j| ((i + j) % 256) as u8).collect();
        file.write_all(&content).unwrap();
        paths.push(path);
    }

    (dir, paths)
}

fn hash_file_sequential(path: &PathBuf) -> [u8; 16] {
    let mut file = fs::File::open(path).unwrap();
    let mut hasher = Md5::with_seed(Md5Seed::none());
    let mut buffer = vec![0u8; 64 * 1024];

    loop {
        let n = file.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    hasher.finalize()
}

fn benchmark_checksum_sequential(paths: &[PathBuf]) -> Vec<[u8; 16]> {
    paths.iter().map(hash_file_sequential).collect()
}

fn benchmark_checksum_parallel(paths: &[PathBuf]) -> Vec<[u8; 16]> {
    use rayon::prelude::*;
    paths.par_iter().map(hash_file_sequential).collect()
}

fn checksum_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("checksum_comparison");

    for file_count in [8, 16, 32, 64] {
        let (dir, paths) = create_test_files(file_count);
        let total_bytes = (file_count * FILE_SIZE) as u64;

        group.throughput(Throughput::Bytes(total_bytes));

        group.bench_with_input(
            BenchmarkId::new("sequential", file_count),
            &paths,
            |b, paths| {
                b.iter(|| black_box(benchmark_checksum_sequential(paths)));
            },
        );

        group.bench_with_input(
            BenchmarkId::new("parallel", file_count),
            &paths,
            |b, paths| {
                b.iter(|| black_box(benchmark_checksum_parallel(paths)));
            },
        );

        drop(dir);
    }

    group.finish();
}

criterion_group!(benches, checksum_benchmarks);
criterion_main!(benches);
