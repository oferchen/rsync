//! URV-5.c.4 - per-connection Landlock setup overhead bench.
//!
//! Measures the cost a daemon connection pays when SEC-1.p engages the
//! Landlock LSM allowlist before transfer begins. Three cells:
//!
//! - `is_supported`: cost of the kernel probe that decides whether to
//!   request a ruleset (runs on every connection regardless of feature).
//! - `restrict_to_module_paths`: full setup of one ruleset over `N`
//!   allowlist roots followed by `restrict_self()`. Each iteration runs
//!   on a fresh worker thread because Landlock irreversibly intersects
//!   the calling thread's policy; reusing the criterion measurement
//!   thread would corrupt later cells.
//! - `thread_spawn_baseline`: noise floor that callers subtract from
//!   the `restrict` cells to isolate the Landlock-attributable cost.
//!
//! The companion shell harness `scripts/landlock_overhead_macro.sh`
//! drives the full daemon-receive comparison (100K-file tree, landlock
//! ON vs OFF) using hyperfine inside the `rsync-profile` container; the
//! aggregated results land in
//! `docs/benchmarks/landlock-overhead-100k.md`.
//!
//! Run with:
//! ```sh
//! cargo bench -p fast_io --bench landlock_overhead --features landlock
//! ```
//!
//! On non-Linux hosts (or without the `landlock` feature) the bench
//! compiles to a stub that prints a skip line, so the suite stays
//! cross-platform clean. See
//! `docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md` for the
//! defense-in-depth rationale and PR #5601 for the allowlist widening
//! that unblocks URV-5.c.5.

#[cfg(all(target_os = "linux", feature = "landlock"))]
use std::hint::black_box;
#[cfg(all(target_os = "linux", feature = "landlock"))]
use std::path::PathBuf;
#[cfg(all(target_os = "linux", feature = "landlock"))]
use std::thread;
#[cfg(all(target_os = "linux", feature = "landlock"))]
use std::time::{Duration, Instant};

#[cfg(all(target_os = "linux", feature = "landlock"))]
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
#[cfg(all(target_os = "linux", feature = "landlock"))]
use tempfile::TempDir;

#[cfg(all(target_os = "linux", feature = "landlock"))]
use fast_io::landlock::{LandlockOutcome, is_supported, restrict_to_module_paths};

/// Allowlist sizes covered by the bench: 1 root models a typical
/// single-module daemon, 4 roots models the worst case after
/// PR #5601 widened the allowlist with `--temp-dir`, `--partial-dir`,
/// and one `ref_dir` (e.g. `--compare-dest`).
#[cfg(all(target_os = "linux", feature = "landlock"))]
const ROOT_COUNTS: &[usize] = &[1, 2, 4];

#[cfg(all(target_os = "linux", feature = "landlock"))]
fn make_roots(count: usize) -> (TempDir, Vec<PathBuf>) {
    let tmp = TempDir::new().expect("tempdir");
    let mut roots = Vec::with_capacity(count);
    for i in 0..count {
        let p = tmp.path().join(format!("root_{i}"));
        std::fs::create_dir_all(&p).expect("create root");
        roots.push(p);
    }
    (tmp, roots)
}

/// Probe-only cell: every daemon connection pays this regardless of
/// whether the feature is wired, so it gives a floor on the cost the
/// URV-5.c.5 default-on flip cannot avoid.
#[cfg(all(target_os = "linux", feature = "landlock"))]
fn bench_is_supported(c: &mut Criterion) {
    let mut group = c.benchmark_group("landlock_overhead/is_supported");
    group.bench_function("probe", |b| {
        b.iter(|| black_box(is_supported()));
    });
    group.finish();
}

/// Full setup cell: each iteration spawns a fresh worker thread,
/// calls `restrict_to_module_paths` exactly once, and joins. The
/// fresh-thread requirement is structural: Landlock applies to the
/// calling thread for the rest of its lifetime, so we cannot reuse
/// the criterion harness thread without corrupting later cells.
/// Criterion's measurement still captures the wall-clock cost of
/// the syscalls + crate plumbing; the thread spawn overhead is the
/// noise floor and is subtracted by comparing to the no-op
/// `thread_spawn_baseline` cell below.
#[cfg(all(target_os = "linux", feature = "landlock"))]
fn bench_restrict_to_module_paths(c: &mut Criterion) {
    let mut group = c.benchmark_group("landlock_overhead/restrict");
    // Keep the sample count modest because each iteration costs a
    // thread spawn + join in addition to the syscalls.
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(10));

    for &count in ROOT_COUNTS {
        let (_tmp, roots) = make_roots(count);
        group.bench_with_input(BenchmarkId::new("roots", count), &roots, |b, roots| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let roots = roots.clone();
                    let handle = thread::Builder::new()
                        .name("landlock-bench".into())
                        .spawn(move || {
                            let refs: Vec<&std::path::Path> =
                                roots.iter().map(|p| p.as_path()).collect();
                            let start = Instant::now();
                            let outcome = restrict_to_module_paths(&refs);
                            let elapsed = start.elapsed();
                            // Touch the outcome so the optimiser cannot
                            // eliminate the call.
                            match outcome {
                                LandlockOutcome::Enforced(_)
                                | LandlockOutcome::Unavailable
                                | LandlockOutcome::Error(_) => {}
                            }
                            elapsed
                        })
                        .expect("spawn landlock-bench worker");
                    total += handle.join().expect("worker panicked");
                }
                total
            });
        });
    }
    group.finish();
}

/// Baseline cell: thread spawn + join with no Landlock work, so the
/// `restrict` cells can be normalised against the noise floor.
#[cfg(all(target_os = "linux", feature = "landlock"))]
fn bench_thread_spawn_baseline(c: &mut Criterion) {
    let mut group = c.benchmark_group("landlock_overhead/baseline");
    group.sample_size(50);
    group.bench_function("thread_spawn_join", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let handle = thread::Builder::new()
                    .name("landlock-bench-baseline".into())
                    .spawn(|| {
                        let start = Instant::now();
                        // No-op body so we measure only spawn + join.
                        black_box(());
                        start.elapsed()
                    })
                    .expect("spawn baseline worker");
                total += handle.join().expect("baseline worker panicked");
            }
            total
        });
    });
    group.finish();
}

#[cfg(all(target_os = "linux", feature = "landlock"))]
criterion_group!(
    benches,
    bench_is_supported,
    bench_thread_spawn_baseline,
    bench_restrict_to_module_paths,
);
#[cfg(all(target_os = "linux", feature = "landlock"))]
criterion_main!(benches);

#[cfg(not(all(target_os = "linux", feature = "landlock")))]
fn main() {
    eprintln!(
        "landlock_overhead bench skipped: requires target_os=\"linux\" and the `landlock` Cargo feature."
    );
}
