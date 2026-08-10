//! Criterion micro-benchmark: parallel `compute_extras` scaling.
//!
//! # Why this exists
//!
//! Task DDP-I1 (#2282). `engine::delete::compute_extras` is phase 1 of
//! the parallel-deterministic-delete pipeline: per directory, list the
//! destination, subtract the segment's basenames, classify the survivors.
//! Each call is independent across directories, so the pipeline runs
//! them through a rayon scope. This bench measures how that scaling
//! actually looks on a realistic shape (1000 dirs, 100 files per dir,
//! 50% extras per dir) at 1/4/8/16 threads, and pins the single-thread
//! baseline next to the sweep so the speedup is unambiguous.
//!
//! Together with `delete_emitter_unlink.rs` (DDP-I2, #2283) and
//! `delete_end_to_end.rs` (DDP-I3, #2284), this feeds the DDP-F3
//! decision: if the end-to-end pipeline stays within 5% of the legacy
//! batched sweep, the legacy `handle_post_transfer_deletions` code path
//! can be removed.
//!
//! # Workloads (Criterion group `delete_plan_compute`)
//!
//! - `parallel_compute_extras_100k_files_1k_dirs/1_threads`
//! - `parallel_compute_extras_100k_files_1k_dirs/4_threads`
//! - `parallel_compute_extras_100k_files_1k_dirs/8_threads`
//! - `parallel_compute_extras_100k_files_1k_dirs/16_threads`
//! - `serial_compute_extras_baseline`
//!
//! Each iteration walks every dir, runs `compute_extras`, and discards
//! the result. The fixture is built once per `Criterion::bench_function`
//! call (held in a `TempDir`) so the timed loop only does work the
//! pipeline would do.
//!
//! Run: `cargo bench -p engine --bench delete_plan_compute`

#![deny(unsafe_code)]
#![cfg(unix)]

use std::fs::File;
use std::path::PathBuf;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rayon::ThreadPoolBuilder;
use rayon::prelude::*;
use tempfile::TempDir;

use engine::delete::compute_extras;
use protocol::flist::FileEntry;

/// Number of synthetic directories built by the fixture.
const DIR_COUNT: usize = 1_000;

/// Number of entries created per directory. Each dir ends up with this
/// many files on disk.
const FILES_PER_DIR: usize = 100;

/// Fraction of on-disk entries (per directory) that are *not* present in
/// the segment, i.e. extras. With `FILES_PER_DIR = 100` and a 50%
/// extras ratio the fixture exposes 50 extras per dir, ~50k extras
/// total across the 1000 dirs.
const EXTRAS_PERCENT: usize = 50;

/// Thread counts swept for the parallel compute_extras workload.
const THREAD_COUNTS: &[usize] = &[1, 4, 8, 16];

/// One pre-built per-directory work item: the dest dir path and the
/// matching flist segment (the files that should *not* be deleted).
struct DirWorkItem {
    dest: PathBuf,
    segment: Vec<FileEntry>,
}

/// Builds a synthetic fixture rooted at `root_dir`.
///
/// For each of `DIR_COUNT` subdirectories we create `FILES_PER_DIR`
/// regular files on disk and emit a segment listing the
/// `(100 - EXTRAS_PERCENT)%` files that should be retained. The
/// resulting `DirWorkItem` slice is exactly what the segment-dispatch
/// phase of the pipeline would feed `compute_extras`.
fn build_fixture(root_dir: &std::path::Path) -> Vec<DirWorkItem> {
    let mut items = Vec::with_capacity(DIR_COUNT);
    let keep_per_dir = FILES_PER_DIR * (100 - EXTRAS_PERCENT) / 100;
    for d in 0..DIR_COUNT {
        let dest = root_dir.join(format!("d{d:04}"));
        std::fs::create_dir(&dest).expect("create dest dir");
        let mut segment = Vec::with_capacity(keep_per_dir);
        for f in 0..FILES_PER_DIR {
            let name = format!("f{f:03}.dat");
            File::create(dest.join(&name)).expect("create file");
            if f < keep_per_dir {
                segment.push(FileEntry::new_file(PathBuf::from(&name), 0, 0o644));
            }
        }
        items.push(DirWorkItem { dest, segment });
    }
    items
}

/// Runs `compute_extras` across every dir in the fixture sequentially.
fn run_serial(items: &[DirWorkItem]) {
    for item in items {
        let extras = compute_extras(&item.dest, &item.segment).expect("compute_extras");
        // Touch the result so the optimizer cannot elide the work.
        std::hint::black_box(extras);
    }
}

/// Runs `compute_extras` across every dir in the fixture in parallel
/// via the supplied rayon pool.
fn run_parallel(pool: &rayon::ThreadPool, items: &[DirWorkItem]) {
    pool.install(|| {
        items.par_iter().for_each(|item| {
            let extras = compute_extras(&item.dest, &item.segment).expect("compute_extras");
            std::hint::black_box(extras);
        });
    });
}

fn bench_serial_baseline(c: &mut Criterion) {
    let mut group = c.benchmark_group("delete_plan_compute");
    let total_files = (DIR_COUNT * FILES_PER_DIR) as u64;
    group.throughput(Throughput::Elements(total_files));

    group.bench_function("serial_compute_extras_baseline", |b| {
        let tmp = TempDir::new().expect("tempdir");
        let items = build_fixture(tmp.path());
        b.iter(|| run_serial(&items));
    });

    group.finish();
}

fn bench_parallel_sweep(c: &mut Criterion) {
    let mut group = c.benchmark_group("delete_plan_compute");
    let total_files = (DIR_COUNT * FILES_PER_DIR) as u64;
    group.throughput(Throughput::Elements(total_files));

    for &threads in THREAD_COUNTS {
        let id = BenchmarkId::new(
            "parallel_compute_extras_100k_files_1k_dirs",
            format!("{threads}_threads"),
        );
        group.bench_function(id, |b| {
            let pool = ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .expect("rayon pool");
            let tmp = TempDir::new().expect("tempdir");
            let items = build_fixture(tmp.path());
            b.iter(|| run_parallel(&pool, &items));
            // Keep `tmp` alive across the iter loop above; drop here so
            // the fixture is torn down before the next bench cell runs.
            drop(items);
            drop(tmp);
        });
    }

    group.finish();
}

criterion_group!(benches, bench_serial_baseline, bench_parallel_sweep);
criterion_main!(benches);
