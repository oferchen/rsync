//! Criterion bench: end-to-end parallel-deterministic delete pipeline
//! vs the legacy single-threaded batched sweep.
//!
//! # Why this exists
//!
//! Task DDP-I3 (#2284). Together with `delete_plan_compute.rs`
//! (DDP-I1, #2282) and `delete_emitter_unlink.rs` (DDP-I2, #2283),
//! this bench feeds the DDP-F3 (batched sweep removal) decision: if
//! the new pipeline lands within 5% of the legacy path on a realistic
//! 100K-file shape, the legacy `handle_post_transfer_deletions` code
//! in `crates/engine/src/local_copy/executor/directory/recursive/`
//! can be retired safely.
//!
//! # Two paths under test
//!
//! - **Parallel deterministic delete** (`parallel_deterministic_delete_during_100k_files`):
//!   phase 1 runs `engine::delete::compute_extras` across every
//!   directory in a rayon scope (parallel), builds one
//!   `DeletePlan` per dir, sorts it, and publishes it into a
//!   `DeletePlanMap`. Phase 2 runs the single-threaded
//!   `DeleteEmitter::emit_all`, which routes through `RealDeleteFs`
//!   and actually unlinks the extras.
//! - **Legacy batched sweep** (`legacy_batched_delete_during_100k_files`):
//!   a faithful reproduction of the wall-clock cost model of
//!   `crates/engine/src/local_copy/executor/directory/recursive/deletion.rs::handle_post_transfer_deletions`
//!   ->`delete_extraneous_entries` (in
//!   `crates/engine/src/local_copy/executor/cleanup.rs`). The legacy
//!   path is `pub(crate)` and threaded through a `CopyContext` that
//!   cannot be assembled from a bench crate, so this bench reproduces
//!   its observable algorithm: a single thread visits every
//!   directory, walks `fs::read_dir`, subtracts the keep set, and
//!   `fs::remove_file`s each extra in turn. That matches the
//!   syscalls the production path issues; the only thing missing is
//!   the `CopyContext` bookkeeping, which contributes a constant
//!   per-entry overhead independent of the parallel-vs-serial axis
//!   this bench is comparing.
//!
//! # Workload
//!
//! 100 directories with 1000 files each (100K files total). 10% of the
//! files per directory are extras (~10K extras total). The remaining
//! files are present in the segment / keep set and must survive both
//! sweeps. Each iteration starts from a freshly populated tempdir so
//! the two paths see identical filesystem state.
//!
//! Run: `cargo bench -p engine --bench delete_end_to_end`

#![deny(unsafe_code)]
#![cfg(unix)]

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use rayon::ThreadPoolBuilder;
use rayon::prelude::*;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

use engine::delete::{
    DeleteEmitter, DeletePlan, DeletePlanMap, DirTraversalCursor, RealDeleteFs, compute_extras,
};
use protocol::flist::FileEntry;

/// Directories per iteration.
const DIR_COUNT: usize = 100;

/// Files per directory before the sweep.
const FILES_PER_DIR: usize = 1_000;

/// Fraction of files (per dir) that are extras and should be deleted.
const EXTRAS_PERCENT: usize = 10;

/// Threads used by the parallel `compute_extras` phase. 8 is the
/// midpoint of the `delete_plan_compute` sweep and matches the scaling
/// curve sweet spot most CI hosts can reproduce.
const PARALLEL_THREADS: usize = 8;

/// One pre-built work item describing one directory's keep set and
/// destination path.
struct DirSpec {
    dest: PathBuf,
    segment: Vec<FileEntry>,
    keep: HashSet<OsString>,
}

/// Builds the on-disk fixture and the per-directory work items.
///
/// Each dir gets `FILES_PER_DIR` files. The first
/// `FILES_PER_DIR * (100 - EXTRAS_PERCENT) / 100` files are listed in
/// the segment / keep set (and thus survive); the trailing files are
/// the extras both sweeps must delete.
fn build_fixture(root_dir: &Path) -> Vec<DirSpec> {
    let keep_per_dir = FILES_PER_DIR * (100 - EXTRAS_PERCENT) / 100;
    let mut specs = Vec::with_capacity(DIR_COUNT);
    for d in 0..DIR_COUNT {
        let dest = root_dir.join(format!("d{d:03}"));
        fs::create_dir(&dest).expect("create dest dir");
        let mut segment = Vec::with_capacity(keep_per_dir);
        let mut keep = HashSet::with_capacity(keep_per_dir);
        for f in 0..FILES_PER_DIR {
            let name = format!("f{f:04}.dat");
            File::create(dest.join(&name)).expect("create file");
            if f < keep_per_dir {
                segment.push(FileEntry::new_file(PathBuf::from(&name), 0, 0o644));
                keep.insert(OsString::from(&name));
            }
        }
        specs.push(DirSpec {
            dest,
            segment,
            keep,
        });
    }
    specs
}

/// Runs the new pipeline: parallel `compute_extras` + serial emitter.
fn run_pipeline(pool: &rayon::ThreadPool, specs: &[DirSpec], root: &Path) {
    let plans = DeletePlanMap::with_capacity(DIR_COUNT + 1);
    plans.insert(DeletePlan::new(root.to_path_buf()));

    pool.install(|| {
        specs.par_iter().for_each(|spec| {
            let extras = compute_extras(&spec.dest, &spec.segment).expect("compute_extras");
            let mut plan = DeletePlan::from_extras(spec.dest.clone(), extras);
            plan.sort_by_name();
            plans.insert(plan);
        });
    });

    // Stage the cursor so it yields every populated directory.
    let mut cursor = DirTraversalCursor::new(root.to_path_buf());
    let children: Vec<FileEntry> = specs
        .iter()
        .map(|s| FileEntry::new_directory(s.dest.clone(), 0o755))
        .collect();
    cursor.observe_segment(root.to_path_buf(), &children);
    for spec in specs.iter() {
        cursor.observe_segment(spec.dest.clone(), &[]);
    }

    let mut emitter = DeleteEmitter::new(RealDeleteFs, plans, cursor);
    emitter.emit_all().expect("emit_all");
    std::hint::black_box(emitter.stats());
}

/// Single-threaded equivalent of the legacy `delete_extraneous_entries`
/// sweep. Walks every directory in turn, computes the set diff against
/// the keep set, and removes each extra via `fs::remove_file`. Matches
/// the syscall shape of the production path
/// (`crates/engine/src/local_copy/executor/cleanup.rs`) without the
/// `CopyContext` plumbing the bench cannot assemble.
fn run_legacy_sweep(specs: &[DirSpec]) {
    let mut total_removed = 0u64;
    for spec in specs {
        let read_dir = fs::read_dir(&spec.dest).expect("read_dir");
        for entry in read_dir {
            let entry = entry.expect("dir entry");
            let name = entry.file_name();
            if spec.keep.contains(name.as_os_str()) {
                continue;
            }
            let file_type = entry.file_type().expect("file_type");
            let path = spec.dest.join(&name);
            if file_type.is_dir() {
                fs::remove_dir_all(&path).expect("remove_dir_all");
            } else {
                fs::remove_file(&path).expect("remove_file");
            }
            total_removed += 1;
        }
    }
    std::hint::black_box(total_removed);
}

fn bench_end_to_end(c: &mut Criterion) {
    let extras_per_dir = FILES_PER_DIR * EXTRAS_PERCENT / 100;
    let total_extras = (DIR_COUNT * extras_per_dir) as u64;

    let mut group = c.benchmark_group("delete_end_to_end");
    group.throughput(Throughput::Elements(total_extras));
    group.sample_size(20);

    group.bench_function("parallel_deterministic_delete_during_100k_files", |b| {
        let pool = ThreadPoolBuilder::new()
            .num_threads(PARALLEL_THREADS)
            .build()
            .expect("rayon pool");
        b.iter_batched(
            || {
                let tmp = TempDir::new().expect("tempdir");
                let specs = build_fixture(tmp.path());
                (tmp, specs)
            },
            |(tmp, specs)| {
                run_pipeline(&pool, &specs, tmp.path());
                drop(specs);
                drop(tmp);
            },
            BatchSize::PerIteration,
        );
    });

    group.bench_function("legacy_batched_delete_during_100k_files", |b| {
        b.iter_batched(
            || {
                let tmp = TempDir::new().expect("tempdir");
                let specs = build_fixture(tmp.path());
                (tmp, specs)
            },
            |(tmp, specs)| {
                run_legacy_sweep(&specs);
                drop(specs);
                drop(tmp);
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_end_to_end);
criterion_main!(benches);
