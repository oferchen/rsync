//! Criterion micro-benchmark: emitter dispatch overhead at scale.
//!
//! # Why this exists
//!
//! Task DDP-I2 (#2283). `engine::delete::DeleteEmitter::emit_all` is
//! the single-threaded drain that consumes a pre-populated
//! `DeletePlanMap` in `DirTraversalCursor` order, issuing one
//! `DeleteFs` call per planned entry. The pipeline assumes the
//! emitter's dispatch overhead is negligible relative to the kernel's
//! `unlink(2)` cost; this bench measures the dispatch path in
//! isolation by routing every call through `RecordingDeleteFs`, which
//! never touches disk.
//!
//! Together with `delete_plan_compute.rs` (DDP-I1, #2282) and
//! `delete_end_to_end.rs` (DDP-I3, #2284), this feeds the DDP-F3
//! decision: if the end-to-end pipeline stays within 5% of the legacy
//! batched sweep, the legacy `handle_post_transfer_deletions` code
//! path can be removed.
//!
//! # Workloads (Criterion group `delete_emitter_unlink`)
//!
//! - `emit_all/10k_extras_100_dirs`
//! - `emit_all/100k_extras_100_dirs`
//! - `emit_all/1M_extras_100_dirs`
//!
//! Each iteration rebuilds a fresh `DeletePlanMap`, `DirTraversalCursor`,
//! and `DeleteEmitter`, then drives `emit_all` to completion. Map and
//! plan construction live outside the timed section via
//! `iter_batched`. The bench captures pure dispatch overhead, not
//! disk I/O.
//!
//! Run: `cargo bench -p engine --bench delete_emitter_unlink`

#![deny(unsafe_code)]
#![cfg(unix)]

use std::ffi::OsString;
use std::path::PathBuf;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use engine::delete::{
    DeleteEmitter, DeleteEntry, DeleteEntryKind, DeletePlan, DeletePlanMap, DirTraversalCursor,
    RecordingDeleteFs,
};
use protocol::flist::FileEntry;

/// Number of directories the fixture spreads extras across.
const DIR_COUNT: usize = 100;

/// Scales swept by the bench. Each value is the total number of extras
/// across all directories.
const SCALES: &[(&str, usize)] = &[
    ("10k_extras_100_dirs", 10_000),
    ("100k_extras_100_dirs", 100_000),
    ("1M_extras_100_dirs", 1_000_000),
];

/// Builds a pre-populated `DeletePlanMap` and a primed cursor for the
/// given total extras spread across `DIR_COUNT` directories.
///
/// Each directory `root/d{i}` carries `total / DIR_COUNT` plain file
/// extras. The cursor is observed for the root so it yields every
/// directory in upstream order before `emit_all` is invoked.
fn build_inputs(total_extras: usize) -> (DeletePlanMap, DirTraversalCursor) {
    let per_dir = total_extras / DIR_COUNT;
    let plans = DeletePlanMap::with_capacity(DIR_COUNT + 1);
    plans.insert(DeletePlan::new(PathBuf::from("root")));

    let mut children = Vec::with_capacity(DIR_COUNT);
    for d in 0..DIR_COUNT {
        let dir = PathBuf::from(format!("root/d{d:03}"));
        let mut plan = DeletePlan::new(dir.clone());
        for n in 0..per_dir {
            plan.push(DeleteEntry::new(
                OsString::from(format!("f{n:07}.dat")),
                DeleteEntryKind::File,
            ));
        }
        plans.insert(plan);
        children.push(FileEntry::new_directory(dir, 0o755));
    }

    let mut cursor = DirTraversalCursor::new(PathBuf::from("root"));
    cursor.observe_segment(PathBuf::from("root"), &children);
    // Each child dir has no further descendants in this fixture.
    for d in 0..DIR_COUNT {
        let dir = PathBuf::from(format!("root/d{d:03}"));
        cursor.observe_segment(dir, &[]);
    }
    (plans, cursor)
}

fn bench_emit_all(c: &mut Criterion) {
    let mut group = c.benchmark_group("delete_emitter_unlink");

    for &(label, total_extras) in SCALES {
        group.throughput(Throughput::Elements(total_extras as u64));
        let id = BenchmarkId::new("emit_all", label);
        group.bench_function(id, |b| {
            b.iter_batched(
                || build_inputs(total_extras),
                |(plans, cursor)| {
                    let mut emitter = DeleteEmitter::new(RecordingDeleteFs::new(), plans, cursor);
                    emitter.emit_all().expect("emit_all succeeds");
                    std::hint::black_box(emitter.stats());
                },
                BatchSize::LargeInput,
            );
        });
    }

    group.finish();
}

criterion_group!(benches, bench_emit_all);
criterion_main!(benches);
