//! Criterion benchmarks for cross-platform fast copy paths.
//!
//! Measures throughput of each copy mechanism available on the current platform:
//! - Linux: io_uring, copy_file_range, FICLONE, standard copy
//! - macOS: clonefile, fcopyfile, standard copy
//! - Windows: CopyFileExW, ReFS reflink, standard copy
//! - All platforms: standard buffered copy fallback
//!
//! Run with: `cargo bench -p fast_io -- platform_copy`

use std::fs;
use std::hint::black_box;
use std::io::Write;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tempfile::TempDir;

use fast_io::platform_copy::{DefaultPlatformCopy, PlatformCopy};

/// File sizes to benchmark: 4KB, 64KB, 1MB, 16MB.
const SIZES: &[(&str, usize)] = &[
    ("4KB", 4 * 1024),
    ("64KB", 64 * 1024),
    ("1MB", 1024 * 1024),
    ("16MB", 16 * 1024 * 1024),
];

/// Creates a temp file filled with a deterministic byte pattern.
fn create_source_file(dir: &TempDir, size: usize) -> std::path::PathBuf {
    let path = dir.path().join("source.bin");
    let mut file = fs::File::create(&path).expect("create source file");
    // Write in 64KB chunks to avoid large stack allocations
    let chunk_size = 64 * 1024;
    let chunk: Vec<u8> = (0..chunk_size).map(|i| (i % 251) as u8).collect();
    let mut remaining = size;
    while remaining > 0 {
        let n = remaining.min(chunk_size);
        file.write_all(&chunk[..n]).expect("write source data");
        remaining -= n;
    }
    file.flush().expect("flush source");
    path
}

/// Benchmarks `DefaultPlatformCopy::copy_file` which auto-selects the best
/// mechanism for the current platform (clonefile/fcopyfile on macOS,
/// FICLONE/copy_file_range on Linux, CopyFileExW on Windows, std::fs::copy
/// as fallback).
fn bench_platform_copy_dispatch(c: &mut Criterion) {
    let mut group = c.benchmark_group("platform_copy/dispatch");
    group.sample_size(20);

    let copier = DefaultPlatformCopy::new();

    for &(label, size) in SIZES {
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::from_parameter(label), &size, |b, &size| {
            let dir = TempDir::new().unwrap();
            let src = create_source_file(&dir, size);
            let dst = dir.path().join("dest.bin");

            b.iter(|| {
                // Remove destination so clonefile (which requires non-existent target) works
                let _ = fs::remove_file(&dst);
                let result = copier.copy_file(&src, &dst, size as u64).unwrap();
                black_box(result)
            });
        });
    }

    group.finish();
}

/// Benchmarks `std::fs::copy` as the portable baseline for comparison.
fn bench_std_fs_copy(c: &mut Criterion) {
    let mut group = c.benchmark_group("platform_copy/std_fs_copy");
    group.sample_size(20);

    for &(label, size) in SIZES {
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::from_parameter(label), &size, |b, &size| {
            let dir = TempDir::new().unwrap();
            let src = create_source_file(&dir, size);
            let dst = dir.path().join("dest.bin");

            b.iter(|| {
                let bytes = fs::copy(&src, &dst).unwrap();
                black_box(bytes)
            });
        });
    }

    group.finish();
}

/// macOS-specific: benchmarks `clonefile` (CoW on APFS) and `fcopyfile`
/// (kernel-accelerated copy) individually.
#[cfg(target_os = "macos")]
fn bench_macos_paths(c: &mut Criterion) {
    // clonefile benchmark
    {
        let mut group = c.benchmark_group("platform_copy/clonefile");
        group.sample_size(20);

        for &(label, size) in SIZES {
            group.throughput(Throughput::Bytes(size as u64));

            group.bench_with_input(BenchmarkId::from_parameter(label), &size, |b, &size| {
                let dir = TempDir::new().unwrap();
                let src = create_source_file(&dir, size);
                let dst = dir.path().join("dest_clone.bin");

                b.iter(|| {
                    let _ = fs::remove_file(&dst);
                    match fast_io::try_clonefile(&src, &dst) {
                        Ok(()) => black_box(size as u64),
                        Err(_) => {
                            // Fallback if not on APFS - still measure the attempt cost
                            black_box(0u64)
                        }
                    }
                });
            });
        }

        group.finish();
    }

    // fcopyfile benchmark
    {
        let mut group = c.benchmark_group("platform_copy/fcopyfile");
        group.sample_size(20);

        for &(label, size) in SIZES {
            group.throughput(Throughput::Bytes(size as u64));

            group.bench_with_input(BenchmarkId::from_parameter(label), &size, |b, &size| {
                let dir = TempDir::new().unwrap();
                let src = create_source_file(&dir, size);
                let dst = dir.path().join("dest_fcopy.bin");

                b.iter(|| {
                    let _ = fs::remove_file(&dst);
                    match fast_io::try_fcopyfile(&src, &dst) {
                        Ok(()) => black_box(size as u64),
                        Err(_) => black_box(0u64),
                    }
                });
            });
        }

        group.finish();
    }
}

/// Linux-specific: benchmarks `copy_file_range` and io_uring copy paths
/// individually.
#[cfg(target_os = "linux")]
fn bench_linux_paths(c: &mut Criterion) {
    // copy_file_range benchmark via copy_file_contents
    {
        let mut group = c.benchmark_group("platform_copy/copy_file_range");
        group.sample_size(20);

        for &(label, size) in SIZES {
            group.throughput(Throughput::Bytes(size as u64));

            group.bench_with_input(BenchmarkId::from_parameter(label), &size, |b, &size| {
                let dir = TempDir::new().unwrap();
                let src = create_source_file(&dir, size);
                let dst_path = dir.path().join("dest_cfr.bin");

                b.iter(|| {
                    let source = fs::File::open(&src).unwrap();
                    let destination = fs::File::create(&dst_path).unwrap();
                    let copied = fast_io::copy_file_range::copy_file_contents(
                        &source,
                        &destination,
                        size as u64,
                    )
                    .unwrap();
                    black_box(copied)
                });
            });
        }

        group.finish();
    }

    // io_uring copy benchmark (only when feature is enabled and kernel supports it)
    #[cfg(feature = "io_uring")]
    {
        if fast_io::is_io_uring_available() {
            let mut group = c.benchmark_group("platform_copy/io_uring");
            group.sample_size(20);

            // io_uring only benefits files above the 256KB threshold
            let uring_sizes: &[(&str, usize)] = &[("1MB", 1024 * 1024), ("16MB", 16 * 1024 * 1024)];

            for &(label, size) in uring_sizes {
                group.throughput(Throughput::Bytes(size as u64));

                group.bench_with_input(BenchmarkId::from_parameter(label), &size, |b, &size| {
                    let dir = TempDir::new().unwrap();
                    let src = create_source_file(&dir, size);
                    let dst_path = dir.path().join("dest_uring.bin");

                    b.iter(|| {
                        let source = fs::File::open(&src).unwrap();
                        let destination = fs::File::create(&dst_path).unwrap();
                        let copied = fast_io::copy_file_range::copy_file_contents(
                            &source,
                            &destination,
                            size as u64,
                        )
                        .unwrap();
                        black_box(copied)
                    });
                });
            }

            group.finish();
        }
    }
}

// Compose the benchmark groups based on platform
#[cfg(target_os = "macos")]
criterion_group!(
    platform_copy_benches,
    bench_platform_copy_dispatch,
    bench_std_fs_copy,
    bench_macos_paths,
);

#[cfg(target_os = "linux")]
criterion_group!(
    platform_copy_benches,
    bench_platform_copy_dispatch,
    bench_std_fs_copy,
    bench_linux_paths,
);

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
criterion_group!(
    platform_copy_benches,
    bench_platform_copy_dispatch,
    bench_std_fs_copy,
);

criterion_main!(platform_copy_benches);
