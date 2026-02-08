//! Comprehensive benchmarks for Phase 1 I/O optimizations.
//!
//! This benchmark suite measures the performance improvements from:
//! - Vectored I/O (writev)
//! - Adaptive buffer sizing
//! - io_uring support (Linux 5.6+)
//! - Memory-mapped I/O for large files
//! - Metadata syscall batching
//!
//! Run with: `cargo bench -p fast_io -- io_optimizations`

use std::fs::File;
use std::io::{BufWriter, IoSlice, Read, Write};

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use tempfile::{NamedTempFile, tempdir};

#[cfg(all(unix, not(all(target_os = "linux", feature = "io_uring"))))]
use fast_io::MmapReader;

#[cfg(all(target_os = "linux", feature = "io_uring"))]
use fast_io::{IoUringConfig, IoUringReader, IoUringWriter, is_io_uring_available};

// ============================================================================
// Test Data Generation
// ============================================================================

fn create_test_file(size: usize) -> NamedTempFile {
    let mut file = NamedTempFile::new().expect("Failed to create temp file");
    let mut data = vec![0u8; size];
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = (i % 256) as u8;
    }
    file.write_all(&data).expect("Failed to write test data");
    file.flush().expect("Failed to flush");
    file
}

// ============================================================================
// Benchmark: Vectored I/O (writev) vs Sequential Writes
// ============================================================================

fn bench_vectored_io(c: &mut Criterion) {
    let mut group = c.benchmark_group("vectored_io");
    group.sample_size(20); // Reduce sample size to avoid disk quota

    // Use a shared temp directory to avoid quota issues
    let bench_dir = tempdir().unwrap();

    // Test with different chunk counts
    for num_chunks in [4, 8, 16] {
        let chunk_size = 4096;
        let total_size = num_chunks * chunk_size;

        // Prepare test data
        let chunks: Vec<Vec<u8>> = (0..num_chunks)
            .map(|i| vec![(i % 256) as u8; chunk_size])
            .collect();

        group.throughput(Throughput::Bytes(total_size as u64));

        // Baseline: Sequential write() calls
        group.bench_with_input(
            BenchmarkId::new("sequential_writes", num_chunks),
            &chunks,
            |b, chunks| {
                let path = bench_dir.path().join("test_seq.bin");
                b.iter(|| {
                    let mut file = File::create(&path).unwrap();

                    for chunk in chunks {
                        file.write_all(chunk).unwrap();
                    }
                    file.sync_all().unwrap();
                });
                let _ = std::fs::remove_file(&path);
            },
        );

        // Optimized: Vectored I/O with write_vectored()
        group.bench_with_input(
            BenchmarkId::new("write_vectored", num_chunks),
            &chunks,
            |b, chunks| {
                let path = bench_dir.path().join("test_vec.bin");
                b.iter(|| {
                    let mut file = File::create(&path).unwrap();

                    let io_slices: Vec<IoSlice> = chunks.iter().map(|c| IoSlice::new(c)).collect();

                    // Single vectored write call - reduces syscalls
                    let _ = file.write_vectored(&io_slices).unwrap();
                    file.sync_all().unwrap();
                });
                let _ = std::fs::remove_file(&path);
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark: Adaptive Buffer Sizing
// ============================================================================

fn bench_adaptive_buffers(c: &mut Criterion) {
    let mut group = c.benchmark_group("adaptive_buffers");

    // Test files of different sizes
    let test_cases = [
        ("small_4kb", 4 * 1024, 4 * 1024),
        ("medium_100kb", 100 * 1024, 64 * 1024),
        ("large_5mb", 5 * 1024 * 1024, 256 * 1024),
    ];

    for (name, file_size, optimal_buffer) in test_cases {
        let file = create_test_file(file_size);
        let path = file.path();

        group.throughput(Throughput::Bytes(file_size as u64));

        // Fixed small buffer (4KB - suboptimal for large files)
        group.bench_with_input(BenchmarkId::new("fixed_4kb", name), &path, |b, path| {
            b.iter(|| {
                let mut f = File::open(path).unwrap();
                let mut buf = vec![0u8; 4 * 1024];
                let mut total = 0;
                loop {
                    let n = f.read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    black_box(&buf[..n]);
                    total += n;
                }
                total
            });
        });

        // Adaptive buffer (optimal for file size)
        group.bench_with_input(
            BenchmarkId::new("adaptive", name),
            &(path, optimal_buffer),
            |b, (path, buffer_size)| {
                b.iter(|| {
                    let mut f = File::open(path).unwrap();
                    let mut buf = vec![0u8; *buffer_size];
                    let mut total = 0;
                    loop {
                        let n = f.read(&mut buf).unwrap();
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
    }

    group.finish();
}

// ============================================================================
// Benchmark: io_uring vs Standard I/O
// ============================================================================

#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn bench_io_uring(c: &mut Criterion) {
    if !is_io_uring_available() {
        eprintln!("Skipping io_uring benchmarks: not available on this system");
        return;
    }

    let mut group = c.benchmark_group("io_uring");

    let test_sizes = [
        ("64kb", 64 * 1024),
        ("1mb", 1024 * 1024),
        ("10mb", 10 * 1024 * 1024),
    ];

    for (name, size) in test_sizes {
        let file = create_test_file(size);
        let path = file.path();

        group.throughput(Throughput::Bytes(size as u64));

        // Standard I/O baseline
        group.bench_with_input(BenchmarkId::new("standard_io", name), &path, |b, path| {
            b.iter(|| {
                let mut f = File::open(path).unwrap();
                let mut buf = vec![0u8; 64 * 1024];
                let mut total = 0;
                loop {
                    let n = f.read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    black_box(&buf[..n]);
                    total += n;
                }
                total
            });
        });

        // io_uring optimized I/O
        group.bench_with_input(BenchmarkId::new("io_uring", name), &path, |b, path| {
            b.iter(|| {
                let config = IoUringConfig::default();
                let mut reader = IoUringReader::open(path, &config).unwrap();
                let data = reader.read_all_batched().unwrap();
                black_box(&data);
                data.len()
            });
        });
    }

    group.finish();
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn bench_io_uring_writes(c: &mut Criterion) {
    if !is_io_uring_available() {
        eprintln!("Skipping io_uring write benchmarks: not available");
        return;
    }

    let mut group = c.benchmark_group("io_uring_writes");

    let test_sizes = [("64kb", 64 * 1024), ("1mb", 1024 * 1024)];

    for (name, size) in test_sizes {
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

        group.throughput(Throughput::Bytes(size as u64));

        // Standard I/O baseline
        group.bench_with_input(BenchmarkId::new("standard_io", name), &data, |b, data| {
            b.iter(|| {
                let dir = tempdir().unwrap();
                let path = dir.path().join("test.bin");
                let file = File::create(&path).unwrap();
                let mut writer = BufWriter::new(file);
                writer.write_all(data).unwrap();
                writer.flush().unwrap();
            });
        });

        // io_uring optimized writes
        group.bench_with_input(BenchmarkId::new("io_uring", name), &data, |b, data| {
            b.iter(|| {
                let dir = tempdir().unwrap();
                let path = dir.path().join("test.bin");
                let config = IoUringConfig::default();
                let mut writer = IoUringWriter::create(&path, &config).unwrap();
                writer.write_all(data).unwrap();
                writer.flush().unwrap();
            });
        });
    }

    group.finish();
}

// ============================================================================
// Benchmark: Memory-Mapped I/O vs Standard I/O
// ============================================================================

#[cfg(all(unix, not(all(target_os = "linux", feature = "io_uring"))))]
fn bench_mmap_io(c: &mut Criterion) {
    let mut group = c.benchmark_group("mmap_io");

    let test_sizes = [
        ("256kb", 256 * 1024),
        ("1mb", 1024 * 1024),
        ("10mb", 10 * 1024 * 1024),
    ];

    for (name, size) in test_sizes {
        let file = create_test_file(size);
        let path = file.path();

        group.throughput(Throughput::Bytes(size as u64));

        // Standard read() baseline
        group.bench_with_input(BenchmarkId::new("standard_read", name), &path, |b, path| {
            b.iter(|| {
                let mut f = File::open(path).unwrap();
                let mut buf = vec![0u8; size];
                f.read_exact(&mut buf).unwrap();
                black_box(&buf);
            });
        });

        // Memory-mapped I/O
        group.bench_with_input(BenchmarkId::new("mmap", name), &path, |b, path| {
            b.iter(|| {
                let reader = MmapReader::open(path).unwrap();
                let data = reader.as_slice();
                black_box(data);
            });
        });
    }

    group.finish();
}

// ============================================================================
// Benchmark: Buffered vs Unbuffered Writes
// ============================================================================

fn bench_buffered_writes(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffered_writes");

    let chunk_sizes = [("4kb", 4096), ("64kb", 65536)];
    let num_chunks = 256; // Total: 1MB or 16MB

    for (name, chunk_size) in chunk_sizes {
        let total_size = chunk_size * num_chunks;
        let data = vec![0xAA_u8; chunk_size];

        group.throughput(Throughput::Bytes(total_size as u64));

        // Unbuffered writes (direct File::write)
        group.bench_with_input(BenchmarkId::new("unbuffered", name), &data, |b, data| {
            b.iter(|| {
                let dir = tempdir().unwrap();
                let path = dir.path().join("test.bin");
                let mut file = File::create(&path).unwrap();

                for _ in 0..num_chunks {
                    file.write_all(data).unwrap();
                }
                file.sync_all().unwrap();
            });
        });

        // Buffered writes with optimal buffer
        group.bench_with_input(
            BenchmarkId::new("buffered_256kb", name),
            &data,
            |b, data| {
                b.iter(|| {
                    let dir = tempdir().unwrap();
                    let path = dir.path().join("test.bin");
                    let file = File::create(&path).unwrap();
                    let mut writer = BufWriter::with_capacity(256 * 1024, file);

                    for _ in 0..num_chunks {
                        writer.write_all(data).unwrap();
                    }
                    writer.flush().unwrap();
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Benchmark Groups
// ============================================================================

#[cfg(all(target_os = "linux", feature = "io_uring"))]
criterion_group!(
    io_optimizations,
    bench_vectored_io,
    bench_adaptive_buffers,
    bench_io_uring,
    bench_io_uring_writes,
    bench_buffered_writes,
);

#[cfg(all(unix, not(all(target_os = "linux", feature = "io_uring"))))]
criterion_group!(
    io_optimizations,
    bench_vectored_io,
    bench_adaptive_buffers,
    bench_mmap_io,
    bench_buffered_writes,
);

#[cfg(all(not(unix), not(all(target_os = "linux", feature = "io_uring"))))]
criterion_group!(
    io_optimizations,
    bench_vectored_io,
    bench_adaptive_buffers,
    bench_buffered_writes,
);

criterion_main!(io_optimizations);
