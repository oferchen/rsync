//! Benchmarks for MapFile vs direct File reads.
//!
//! Run with: `cargo bench -p transfer -- map_file`

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use tempfile::NamedTempFile;
use transfer::map_file::MapFile;

/// Create a test file with random-ish data of the specified size.
fn create_test_file(size: usize) -> NamedTempFile {
    let mut file = NamedTempFile::new().expect("Failed to create temp file");
    let mut data = vec![0u8; size];
    // Fill with predictable pattern
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = (i % 256) as u8;
    }
    file.write_all(&data).expect("Failed to write test data");
    file.flush().expect("Failed to flush");
    file
}

/// Benchmark sequential reads with direct File::open vs MapFile.
fn bench_sequential_reads(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequential_reads");

    for size_kb in [64, 256, 1024, 4096] {
        let size = size_kb * 1024;
        let file = create_test_file(size);
        let path = file.path();

        group.throughput(Throughput::Bytes(size as u64));

        // Direct File reads in 32KB chunks
        group.bench_with_input(
            BenchmarkId::new("direct_file", format!("{size_kb}KB")),
            &size,
            |b, &size| {
                b.iter(|| {
                    let mut f = File::open(path).expect("Failed to open");
                    let mut buf = vec![0u8; 32 * 1024];
                    let mut total = 0;
                    while total < size {
                        let n = f.read(&mut buf).expect("Failed to read");
                        if n == 0 {
                            break;
                        }
                        black_box(&buf[..n]);
                        total += n;
                    }
                    total
                });
            },
        );

        // MapFile with BufferedMap (256KB window)
        group.bench_with_input(
            BenchmarkId::new("map_file", format!("{size_kb}KB")),
            &size,
            |b, &size| {
                b.iter(|| {
                    let mut map_file = MapFile::open(path).expect("Failed to create MapFile");
                    let mut total = 0;
                    while total < size {
                        let chunk_size = (32 * 1024).min(size - total);
                        let data = map_file
                            .map_ptr(total as u64, chunk_size)
                            .expect("Failed to map");
                        black_box(data);
                        total += chunk_size;
                    }
                    total
                });
            },
        );
    }

    group.finish();
}

/// Benchmark random access patterns with direct File vs MapFile.
fn bench_random_access(c: &mut Criterion) {
    let mut group = c.benchmark_group("random_access");

    // Use 4MB file for random access tests
    let size = 4 * 1024 * 1024;
    let file = create_test_file(size);
    let path = file.path();

    // Generate deterministic "random" offsets
    let offsets: Vec<u64> = (0..100)
        .map(|i| ((i * 7919) % (size / 4096)) as u64 * 4096) // 4KB aligned
        .collect();

    group.throughput(Throughput::Elements(offsets.len() as u64));

    // Direct File seeks and reads
    group.bench_function("direct_file_100_seeks", |b| {
        b.iter(|| {
            let mut f = File::open(path).expect("Failed to open");
            let mut buf = vec![0u8; 4096];
            for &offset in &offsets {
                f.seek(SeekFrom::Start(offset)).expect("Failed to seek");
                f.read_exact(&mut buf).expect("Failed to read");
                black_box(&buf);
            }
        });
    });

    // MapFile random access (benefits from 256KB window)
    group.bench_function("map_file_100_seeks", |b| {
        b.iter(|| {
            let mut map_file = MapFile::open(path).expect("Failed to create MapFile");
            for &offset in &offsets {
                let data = map_file.map_ptr(offset, 4096).expect("Failed to map");
                black_box(data);
            }
        });
    });

    group.finish();
}

/// Benchmark repeated access to the same region (cache hit scenario).
fn bench_cached_access(c: &mut Criterion) {
    let mut group = c.benchmark_group("cached_access");

    let size = 1024 * 1024;
    let file = create_test_file(size);
    let path = file.path();

    group.throughput(Throughput::Elements(1000));

    // Direct File - must seek each time
    group.bench_function("direct_file_1000_same_region", |b| {
        b.iter(|| {
            let mut f = File::open(path).expect("Failed to open");
            let mut buf = vec![0u8; 4096];
            for _ in 0..1000 {
                f.seek(SeekFrom::Start(512 * 1024)).expect("Failed to seek");
                f.read_exact(&mut buf).expect("Failed to read");
                black_box(&buf);
            }
        });
    });

    // MapFile - window stays cached
    group.bench_function("map_file_1000_same_region", |b| {
        b.iter(|| {
            let mut map_file = MapFile::open(path).expect("Failed to create MapFile");
            for _ in 0..1000 {
                let data = map_file.map_ptr(512 * 1024, 4096).expect("Failed to map");
                black_box(data);
            }
        });
    });

    group.finish();
}

criterion_group!(
    map_file_benchmarks,
    bench_sequential_reads,
    bench_random_access,
    bench_cached_access
);

criterion_main!(map_file_benchmarks);
