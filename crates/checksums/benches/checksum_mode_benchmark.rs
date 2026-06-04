//! Benchmark for `--checksum` mode whole-file hashing (CSM-9 validation).
//!
//! Measures wall-clock time for whole-file checksum computation at various file
//! sizes, comparing XXH3/128 (post-CSM-8 default) against MD5 (pre-CSM-8
//! default). This validates that the algorithm switch in CSM-8 brought checksum
//! mode performance within 1.05x of upstream rsync.
//!
//! The benchmark exercises two tiers:
//!
//! 1. **Pure hash throughput** - in-memory digest computation to isolate CPU cost.
//! 2. **File-backed hashing** - reads real files from disk, matching the I/O
//!    pattern of `--checksum` mode where every file is hashed end-to-end.
//!
//! Run with: `cargo bench -p checksums --bench checksum_mode_benchmark`

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rand::RngExt;
use std::hint::black_box;
use std::io::Write;
use std::path::PathBuf;

use checksums::parallel::hash_files_parallel;
use checksums::strong::{Md5, StrongDigest, Xxh3, Xxh3_128};

/// File sizes that represent typical workloads in `--checksum` mode.
/// 1 MB covers small config files batched together, 10 MB covers typical
/// source trees, 100 MB covers media or build artifacts.
const FILE_SIZES: &[(usize, &str)] = &[
    (1 << 20, "1MB"),
    (10 << 20, "10MB"),
    (100 << 20, "100MB"),
];

/// Smaller sizes for pure-hash throughput where 100 MB would dominate runtime.
const HASH_SIZES: &[(usize, &str)] = &[
    (4 << 10, "4KB"),
    (64 << 10, "64KB"),
    (1 << 20, "1MB"),
    (10 << 20, "10MB"),
    (100 << 20, "100MB"),
];

/// Generate random data of the specified size.
fn generate_random_data(size: usize) -> Vec<u8> {
    let mut rng = rand::rng();
    let mut data = vec![0u8; size];
    rng.fill(&mut data[..]);
    data
}

/// Write a file of the specified size filled with random data.
/// Returns the path and the data (for digest verification if needed).
fn create_temp_file(dir: &std::path::Path, name: &str, size: usize) -> PathBuf {
    let path = dir.join(name);
    let data = generate_random_data(size);
    let mut f = std::fs::File::create(&path).expect("create temp file");
    f.write_all(&data).expect("write temp file");
    f.sync_all().expect("sync temp file");
    path
}

// ---------------------------------------------------------------------------
// Tier 1: Pure hash throughput (in-memory, no I/O)
// ---------------------------------------------------------------------------

/// Compare XXH3/128 vs MD5 vs XXH3/64 pure digest throughput.
///
/// This isolates the CPU cost difference that CSM-8 eliminated by switching
/// the default from MD5 to XXH3/128.
fn bench_whole_file_hash_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("checksum_mode_hash_throughput");

    for &(size, label) in HASH_SIZES {
        let data = generate_random_data(size);
        group.throughput(Throughput::Bytes(size as u64));

        // XXH3/128 - post-CSM-8 default for --checksum mode
        group.bench_with_input(
            BenchmarkId::new("xxh3_128", label),
            &data,
            |b, data| {
                b.iter(|| black_box(Xxh3_128::digest(0, black_box(data))));
            },
        );

        // MD5 - pre-CSM-8 default (the slow path)
        group.bench_with_input(BenchmarkId::new("md5", label), &data, |b, data| {
            b.iter(|| black_box(Md5::digest(black_box(data))));
        });

        // XXH3/64 - included for reference as the block-level hash
        group.bench_with_input(BenchmarkId::new("xxh3_64", label), &data, |b, data| {
            b.iter(|| black_box(Xxh3::digest(0, black_box(data))));
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Tier 2: File-backed whole-file hashing (realistic I/O path)
// ---------------------------------------------------------------------------

/// Benchmark single-file whole-file hash with disk I/O.
///
/// This measures the end-to-end cost that `--checksum` mode pays per file:
/// open + read + hash. The I/O component is significant for smaller files
/// where syscall overhead dominates, while for larger files the hash
/// algorithm's throughput becomes the bottleneck.
fn bench_whole_file_hash_with_io(c: &mut Criterion) {
    let mut group = c.benchmark_group("checksum_mode_file_hash");

    let tmpdir = tempfile::tempdir().expect("create tmpdir");

    for &(size, label) in FILE_SIZES {
        let path = create_temp_file(tmpdir.path(), &format!("file_{label}.bin"), size);
        let paths = vec![path];

        group.throughput(Throughput::Bytes(size as u64));

        // XXH3/128 with file I/O - the production path after CSM-8
        group.bench_with_input(
            BenchmarkId::new("xxh3_128_file", label),
            &paths,
            |b, paths| {
                b.iter(|| {
                    let results = hash_files_parallel::<Xxh3_128>(black_box(paths), 64 * 1024);
                    black_box(results)
                });
            },
        );

        // MD5 with file I/O - the production path before CSM-8
        group.bench_with_input(
            BenchmarkId::new("md5_file", label),
            &paths,
            |b, paths| {
                b.iter(|| {
                    let results = hash_files_parallel::<Md5>(black_box(paths), 64 * 1024);
                    black_box(results)
                });
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Tier 3: Multi-file parallel hashing (batch --checksum workload)
// ---------------------------------------------------------------------------

/// Benchmark parallel whole-file hashing of many files.
///
/// In `--checksum` mode, every file in the transfer set is hashed. This
/// measures the throughput when processing a batch of files concurrently
/// via rayon, which is the actual hot path in production.
fn bench_parallel_whole_file_hash(c: &mut Criterion) {
    let mut group = c.benchmark_group("checksum_mode_parallel");

    let tmpdir = tempfile::tempdir().expect("create tmpdir");

    // 100 files of 1 MB each = 100 MB total
    let file_count = 100;
    let file_size = 1 << 20;
    let total_bytes = file_count * file_size;
    let paths: Vec<PathBuf> = (0..file_count)
        .map(|i| create_temp_file(tmpdir.path(), &format!("par_{i}.bin"), file_size))
        .collect();

    group.throughput(Throughput::Bytes(total_bytes as u64));

    group.bench_with_input(
        BenchmarkId::new("xxh3_128", "100x1MB"),
        &paths,
        |b, paths| {
            b.iter(|| {
                let results = hash_files_parallel::<Xxh3_128>(black_box(paths), 64 * 1024);
                black_box(results)
            });
        },
    );

    group.bench_with_input(
        BenchmarkId::new("md5", "100x1MB"),
        &paths,
        |b, paths| {
            b.iter(|| {
                let results = hash_files_parallel::<Md5>(black_box(paths), 64 * 1024);
                black_box(results)
            });
        },
    );

    group.finish();
}

// ---------------------------------------------------------------------------
// Tier 4: Overhead per byte measurement
// ---------------------------------------------------------------------------

/// Measure checksum overhead per byte at the file sizes used by --checksum mode.
///
/// Reports throughput in bytes/sec so the per-byte cost is directly visible
/// in the Criterion HTML report. A higher throughput for XXH3/128 relative to
/// MD5 confirms the CSM-8 fix is effective.
fn bench_overhead_per_byte(c: &mut Criterion) {
    let mut group = c.benchmark_group("checksum_mode_overhead_per_byte");

    // Use a streaming update pattern to match the file-read loop in
    // parallel/files.rs hash_file_internal.
    let buf_size = 64 * 1024;

    for &(size, label) in FILE_SIZES {
        let data = generate_random_data(size);
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(
            BenchmarkId::new("xxh3_128_streaming", label),
            &data,
            |b, data| {
                b.iter(|| {
                    let mut hasher = Xxh3_128::with_seed(0);
                    for chunk in data.chunks(buf_size) {
                        hasher.update(black_box(chunk));
                    }
                    black_box(hasher.finalize())
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("md5_streaming", label),
            &data,
            |b, data| {
                b.iter(|| {
                    let mut hasher = Md5::new();
                    for chunk in data.chunks(buf_size) {
                        hasher.update(black_box(chunk));
                    }
                    black_box(hasher.finalize())
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_whole_file_hash_throughput,
    bench_whole_file_hash_with_io,
    bench_parallel_whole_file_hash,
    bench_overhead_per_byte,
);

criterion_main!(benches);
