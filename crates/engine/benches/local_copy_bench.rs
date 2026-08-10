//! Benchmarks for the local copy path.
//!
//! Run with: `cargo bench -p engine --bench local_copy_bench`
//!
//! Measures end-to-end local copy throughput across file sizes and directory
//! structures using `LocalCopyPlan::execute`.

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::Path;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use tempfile::TempDir;

use engine::local_copy::{LocalCopyPlan, LocalCopySummary};

/// Creates a file filled with a deterministic byte pattern.
fn create_file_with_size(path: &Path, size: usize) {
    let mut file = fs::File::create(path).expect("create file");
    // Write in 64 KB chunks to avoid a single large allocation for big files.
    let chunk_size = 64 * 1024;
    let chunk: Vec<u8> = (0..chunk_size).map(|i| (i % 251) as u8).collect();
    let mut remaining = size;
    while remaining > 0 {
        let n = remaining.min(chunk_size);
        file.write_all(&chunk[..n]).expect("write chunk");
        remaining -= n;
    }
    file.flush().expect("flush");
}

/// Builds a `LocalCopyPlan` from source and destination paths.
fn plan_copy(src: &Path, dst: &Path) -> LocalCopyPlan {
    let operands = vec![
        OsString::from(src.as_os_str()),
        OsString::from(dst.as_os_str()),
    ];
    LocalCopyPlan::from_operands(&operands).expect("plan")
}

/// Executes the plan and returns the summary, panicking on error.
fn run_copy(plan: &LocalCopyPlan) -> LocalCopySummary {
    plan.execute().expect("copy succeeds")
}

// ---------------------------------------------------------------------------
// Single-file benchmarks
// ---------------------------------------------------------------------------

/// Benchmarks copying a single file at various sizes.
fn bench_single_file_copy(c: &mut Criterion) {
    let mut group = c.benchmark_group("local_copy_single_file");

    let sizes: &[(&str, usize)] = &[
        ("1KB", 1_024),
        ("1MB", 1_024 * 1_024),
        ("100MB", 100 * 1_024 * 1_024),
    ];

    for &(label, size) in sizes {
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::new("copy", label), &size, |b, &size| {
            // Use iter_batched so temp-dir setup/teardown is excluded from timing.
            b.iter_batched(
                || {
                    let tmp = TempDir::new().expect("tempdir");
                    let src = tmp.path().join("source.dat");
                    let dst = tmp.path().join("dest.dat");
                    create_file_with_size(&src, size);
                    let plan = plan_copy(&src, &dst);
                    (tmp, plan)
                },
                |(_tmp, plan)| {
                    let summary = run_copy(&plan);
                    black_box(summary)
                },
                criterion::BatchSize::PerIteration,
            );
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Many small files benchmark
// ---------------------------------------------------------------------------

/// Benchmarks copying 100 x 1 KB files from a single directory.
fn bench_many_small_files(c: &mut Criterion) {
    let mut group = c.benchmark_group("local_copy_many_small_files");
    let file_count: usize = 100;
    let file_size: usize = 1_024;
    let total_bytes = (file_count * file_size) as u64;

    group.throughput(Throughput::Bytes(total_bytes));

    group.bench_function("100x1KB", |b| {
        b.iter_batched(
            || {
                let tmp = TempDir::new().expect("tempdir");
                let src_dir = tmp.path().join("src");
                let dst_dir = tmp.path().join("dst");
                fs::create_dir_all(&src_dir).expect("create src dir");
                // Destination directory must exist for rsync-style copy.
                fs::create_dir_all(&dst_dir).expect("create dst dir");

                for i in 0..file_count {
                    let name = format!("file_{i:04}.dat");
                    create_file_with_size(&src_dir.join(&name), file_size);
                }

                // Trailing slash on source to copy contents into dst.
                let src_operand = format!("{}/", src_dir.display());
                let operands = vec![
                    OsString::from(&src_operand),
                    OsString::from(dst_dir.as_os_str()),
                ];
                let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
                (tmp, plan)
            },
            |(_tmp, plan)| {
                let summary = run_copy(&plan);
                black_box(summary)
            },
            criterion::BatchSize::PerIteration,
        );
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Directory tree benchmark
// ---------------------------------------------------------------------------

/// Builds a nested directory tree with files at every level.
///
/// Structure: `depth` levels, `breadth` subdirs per level, one 1 KB file per dir.
fn create_dir_tree(root: &Path, depth: usize, breadth: usize, file_size: usize) -> usize {
    let mut file_count = 0;

    fn recurse(dir: &Path, depth: usize, breadth: usize, file_size: usize, count: &mut usize) {
        fs::create_dir_all(dir).expect("mkdir");
        let file_path = dir.join("data.dat");
        create_file_with_size(&file_path, file_size);
        *count += 1;

        if depth > 0 {
            for i in 0..breadth {
                let child = dir.join(format!("d{i}"));
                recurse(&child, depth - 1, breadth, file_size, count);
            }
        }
    }

    recurse(root, depth, breadth, file_size, &mut file_count);
    file_count
}

/// Benchmarks copying a directory tree (3 levels deep, 3 branches, 1 KB files).
///
/// Total files: 1 + 3 + 9 + 27 = 40 files across 40 directories.
fn bench_directory_tree(c: &mut Criterion) {
    let mut group = c.benchmark_group("local_copy_directory_tree");

    let depth = 3;
    let breadth = 3;
    let file_size = 1_024;
    // 3^0 + 3^1 + 3^2 + 3^3 = 1 + 3 + 9 + 27 = 40
    let expected_files = 40;
    let total_bytes = (expected_files * file_size) as u64;

    group.throughput(Throughput::Bytes(total_bytes));

    group.bench_function("3_deep_3_wide_1KB", |b| {
        b.iter_batched(
            || {
                let tmp = TempDir::new().expect("tempdir");
                let src_dir = tmp.path().join("src");
                let dst_dir = tmp.path().join("dst");
                fs::create_dir_all(&dst_dir).expect("create dst dir");

                let count = create_dir_tree(&src_dir, depth, breadth, file_size);
                assert_eq!(count, expected_files);

                let src_operand = format!("{}/", src_dir.display());
                let operands = vec![
                    OsString::from(&src_operand),
                    OsString::from(dst_dir.as_os_str()),
                ];
                let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
                (tmp, plan)
            },
            |(_tmp, plan)| {
                let summary = run_copy(&plan);
                black_box(summary)
            },
            criterion::BatchSize::PerIteration,
        );
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Criterion harness
// ---------------------------------------------------------------------------

criterion_group!(
    name = local_copy_benches;
    config = Criterion::default()
        .sample_size(10)
        .measurement_time(std::time::Duration::from_secs(5));
    targets =
        bench_single_file_copy,
        bench_many_small_files,
        bench_directory_tree
);

criterion_main!(local_copy_benches);
