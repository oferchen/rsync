//! High-file-count io_uring stat-batching benchmark (IUB-5).
//!
//! Measures io_uring `IORING_OP_STATX` batch submission vs sequential
//! `std::fs::metadata` and `syscall_batch::execute_metadata_ops` at file
//! counts where the hypothesis predicts io_uring pays off: 100K small files
//! (1-4 KiB) and 1M tiny files (64-256 bytes).
//!
//! The primary io_uring win at high file counts is statx batching - the
//! kernel processes N independent `IORING_OP_STATX` SQEs on a single ring
//! submission, amortising the syscall overhead across the batch rather than
//! issuing one `stat(2)` per file. This bench quantifies that payoff.
//!
//! # What this bench measures
//!
//! Two workload tiers x three stat backends:
//!
//! | Tier | Files | Size per file | Total data |
//! |------|-------|---------------|------------|
//! | 100K | 100,000 | 1-4 KiB | ~250 MiB |
//! | 1M | 1,000,000 | 64-256 bytes | ~160 MiB |
//!
//! Backends:
//!
//! - `stdlib_stat` - sequential `std::fs::metadata()` per file.
//! - `syscall_batch_stat` - `fast_io::syscall_batch::execute_metadata_ops()`
//!   which groups by operation type and uses `statx(2)` on Linux.
//! - `iouring_statx_batch` - `fast_io::submit_statx_batch()` which submits
//!   `IORING_OP_STATX` SQEs on a single ring (Linux 5.11+). This backend
//!   is cfg-gated to `target_os = "linux"` + `feature = "io_uring"`.
//!
//! All backends stat the same pre-populated fixture directory. Throughput
//! is reported in elements/second (files stat'd per second).
//!
//! # Workload sizing rationale
//!
//! - **100K files** - representative of a medium source tree or package
//!   cache. At 1-4 KiB each, per-file metadata overhead dominates data
//!   transfer cost. This is where syscall batching should show measurable
//!   gain over sequential stat.
//! - **1M files** - representative of a large monorepo, node_modules tree,
//!   or mail spool. At 64-256 bytes each, the workload is almost entirely
//!   metadata-bound. Ring submission amortisation should deliver the
//!   largest relative speedup here.
//!
//! # When to run
//!
//! Gated by `BENCH_HIGH_FILE_COUNT=1` to prevent accidental invocation
//! (fixture creation alone takes several seconds at 1M files):
//!
//! ```sh
//! BENCH_HIGH_FILE_COUNT=1 \
//!   cargo bench -p fast_io \
//!     --features io_uring \
//!     --bench iouring_high_file_count
//! ```
//!
//! Individual tiers:
//! - `BENCH_HIGH_FILE_COUNT=1` enables the 100K tier.
//! - `BENCH_HIGH_FILE_COUNT_1M=1` additionally enables the 1M tier
//!   (slow fixture creation; requires sufficient inode capacity).
//!
//! Without `BENCH_HIGH_FILE_COUNT=1` the bench prints a skip line and
//! exits 0.
//!
//! # CI gating
//!
//! The io_uring statx batch backend is gated on
//! `cfg(all(target_os = "linux", feature = "io_uring"))`. The stdlib and
//! syscall_batch backends compile on all platforms. Non-Linux builds skip
//! the io_uring cells but still measure stdlib vs syscall_batch. The
//! `Cargo.toml` `[[bench]]` entry carries
//! `required-features = ["io_uring"]` so the bench is excluded from builds
//! that turn the feature off.
//!
//! # What the numbers inform
//!
//! - io_uring statx batch clears stdlib by >= 20% at 100K files: the
//!   receiver/generator stat path should prefer `submit_statx_batch` on
//!   Linux 5.11+ kernels for directory-traversal workloads.
//! - io_uring statx batch clears stdlib by >= 40% at 1M files: prioritise
//!   wiring the batched stat path into the generator's parallel-stat
//!   pipeline.
//! - Within +/- 10%: the overhead of ring construction offsets the
//!   batching gain at these counts; revisit only at higher file counts
//!   or with a session ring that persists across batches.

use std::env;
use std::fs;
use std::path::PathBuf;
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use std::path::Path;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tempfile::TempDir;

/// Gate env var for 100K tier.
const ENABLE_ENV: &str = "BENCH_HIGH_FILE_COUNT";

/// Additional gate for the 1M tier.
const ENABLE_1M_ENV: &str = "BENCH_HIGH_FILE_COUNT_1M";

/// 100K files, 1-4 KiB each.
const FILE_COUNT_100K: usize = 100_000;

/// 1M files, 64-256 bytes each.
const FILE_COUNT_1M: usize = 1_000_000;

/// Batch size for io_uring statx submissions. Matches the chunk size used
/// by `submit_statx_batch` internally (rounds up to next power of two).
/// Kept separate here so the bench can partition the path list into
/// digestible chunks for the batched backends.
const STAT_BATCH_SIZE: usize = 4096;

fn bench_enabled() -> bool {
    matches!(env::var(ENABLE_ENV), Ok(v) if v == "1")
}

fn bench_1m_enabled() -> bool {
    matches!(env::var(ENABLE_1M_ENV), Ok(v) if v == "1")
}

/// Tier descriptor for parameterised benchmark groups.
struct Tier {
    file_count: usize,
    label: &'static str,
}

/// Returns the active tiers based on env vars.
fn active_tiers() -> Vec<Tier> {
    let mut tiers = vec![Tier {
        file_count: FILE_COUNT_100K,
        label: "100K",
    }];
    if bench_1m_enabled() {
        tiers.push(Tier {
            file_count: FILE_COUNT_1M,
            label: "1M",
        });
    }
    tiers
}

/// Creates a fixture directory populated with `count` small files.
///
/// Files are distributed across 1000 subdirectories (files per subdir =
/// count / 1000) to avoid filesystem slowdowns from a single flat directory
/// with millions of entries. File sizes cycle deterministically between
/// `min_bytes` and `max_bytes`.
fn create_fixture(count: usize, min_bytes: usize, max_bytes: usize) -> (TempDir, Vec<PathBuf>) {
    let dir = TempDir::new().expect("fixture tempdir");
    let subdirs = 1000usize;
    let files_per_subdir = count / subdirs;
    let remainder = count % subdirs;

    // Pre-create subdirectories.
    for s in 0..subdirs {
        fs::create_dir(dir.path().join(format!("d{s:04}"))).expect("create subdir");
    }

    let range = max_bytes - min_bytes + 1;
    let mut paths = Vec::with_capacity(count);
    let mut file_idx: usize = 0;

    for s in 0..subdirs {
        let subdir = dir.path().join(format!("d{s:04}"));
        let n = if s < remainder {
            files_per_subdir + 1
        } else {
            files_per_subdir
        };
        for f in 0..n {
            let size = min_bytes + (file_idx % range);
            let path = subdir.join(format!("f{f:07}"));
            // Write a deterministic payload. Content does not matter for stat
            // benchmarks, but file must exist on disk.
            let payload = vec![0xa5u8; size];
            fs::write(&path, &payload).expect("write fixture file");
            paths.push(path);
            file_idx += 1;
        }
    }

    assert_eq!(paths.len(), count);
    (dir, paths)
}

/// Sequential `std::fs::metadata()` per file.
fn stdlib_stat_all(paths: &[PathBuf]) -> usize {
    let mut count = 0usize;
    for path in paths {
        let _meta = fs::metadata(path).expect("stdlib stat");
        count += 1;
    }
    count
}

/// `syscall_batch::execute_metadata_ops()` - groups by type, uses
/// `statx(2)` on Linux when available.
fn syscall_batch_stat_all(paths: &[PathBuf]) -> usize {
    use fast_io::syscall_batch::{MetadataOp, MetadataResult, execute_metadata_ops};

    let mut count = 0usize;
    // Process in chunks to avoid building a single Vec of millions of ops.
    for chunk in paths.chunks(STAT_BATCH_SIZE) {
        let ops: Vec<MetadataOp> = chunk.iter().map(|p| MetadataOp::Stat(p.clone())).collect();
        let results = execute_metadata_ops(&ops);
        for result in &results {
            match result {
                MetadataResult::Stat(Ok(_)) => count += 1,
                MetadataResult::Stat(Err(e)) => panic!("syscall_batch stat failed: {e}"),
                _ => panic!("unexpected result variant"),
            }
        }
    }
    count
}

/// io_uring `submit_statx_batch()` - submits `IORING_OP_STATX` SQEs on a
/// single ring per batch chunk.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn iouring_statx_batch_all(paths: &[PathBuf]) -> usize {
    use fast_io::submit_statx_batch;

    let mut count = 0usize;
    for chunk in paths.chunks(STAT_BATCH_SIZE) {
        let path_refs: Vec<&Path> = chunk.iter().map(|p| p.as_path()).collect();
        let results = submit_statx_batch(&path_refs, true).expect("submit_statx_batch");
        for result in &results {
            match result {
                Ok(_) => count += 1,
                Err(e) => panic!("iouring statx failed: {e}"),
            }
        }
    }
    count
}

/// Metadata stat benchmark: stdlib vs syscall_batch vs io_uring statx batch.
fn bench_stat(c: &mut Criterion) {
    if !bench_enabled() {
        eprintln!(
            "Skipping iouring_high_file_count: set {ENABLE_ENV}=1 on a Linux 5.11+ host \
             with io_uring to enable the 100K tier. Set {ENABLE_1M_ENV}=1 to additionally \
             enable the 1M tier."
        );
        return;
    }

    let tiers = active_tiers();
    let mut group = c.benchmark_group("high_file_count_stat");
    group.sample_size(10);

    for tier in &tiers {
        let (min_bytes, max_bytes) = if tier.file_count == FILE_COUNT_100K {
            (1024, 4096) // 1-4 KiB for 100K tier
        } else {
            (64, 256) // 64-256 bytes for 1M tier
        };

        eprintln!(
            "Creating fixture: {} files ({}-{} bytes each)...",
            tier.file_count, min_bytes, max_bytes,
        );
        let (dir, paths) = create_fixture(tier.file_count, min_bytes, max_bytes);
        eprintln!("Fixture ready at {:?}", dir.path());

        group.throughput(Throughput::Elements(tier.file_count as u64));

        // stdlib sequential stat
        group.bench_with_input(
            BenchmarkId::new("stdlib_stat", tier.label),
            &paths,
            |b, paths| {
                b.iter(|| {
                    let n = stdlib_stat_all(paths);
                    assert_eq!(n, tier.file_count);
                });
            },
        );

        // syscall_batch stat
        group.bench_with_input(
            BenchmarkId::new("syscall_batch_stat", tier.label),
            &paths,
            |b, paths| {
                b.iter(|| {
                    let n = syscall_batch_stat_all(paths);
                    assert_eq!(n, tier.file_count);
                });
            },
        );

        // io_uring statx batch (Linux only)
        #[cfg(all(target_os = "linux", feature = "io_uring"))]
        {
            if fast_io::statx_supported() {
                group.bench_with_input(
                    BenchmarkId::new("iouring_statx_batch", tier.label),
                    &paths,
                    |b, paths| {
                        b.iter(|| {
                            let n = iouring_statx_batch_all(paths);
                            assert_eq!(n, tier.file_count);
                        });
                    },
                );
            } else {
                eprintln!(
                    "Skipping iouring_statx_batch/{}: IORING_OP_STATX not supported \
                     by this kernel (requires Linux 5.11+)",
                    tier.label,
                );
            }
        }

        // Keep the fixture alive until all cells for this tier finish.
        drop(dir);
    }

    group.finish();
}

/// Readdir + stat benchmark: measures the combined cost of directory
/// enumeration followed by per-entry stat. This simulates the file-list
/// building hot path where the generator enumerates a directory tree and
/// stats every entry.
fn bench_readdir_stat(c: &mut Criterion) {
    if !bench_enabled() {
        return;
    }

    let tiers = active_tiers();
    let mut group = c.benchmark_group("high_file_count_readdir_stat");
    group.sample_size(10);

    for tier in &tiers {
        let (min_bytes, max_bytes) = if tier.file_count == FILE_COUNT_100K {
            (1024, 4096)
        } else {
            (64, 256)
        };

        let (dir, _paths) = create_fixture(tier.file_count, min_bytes, max_bytes);
        let root = dir.path().to_path_buf();

        group.throughput(Throughput::Elements(tier.file_count as u64));

        // stdlib readdir + stat: walk subdirectories, stat each entry
        group.bench_with_input(
            BenchmarkId::new("stdlib_readdir_stat", tier.label),
            &root,
            |b, root| {
                b.iter(|| {
                    let mut count = 0usize;
                    for subdir_entry in fs::read_dir(root).expect("read root") {
                        let subdir = subdir_entry.expect("subdir entry").path();
                        if !subdir.is_dir() {
                            continue;
                        }
                        for entry in fs::read_dir(&subdir).expect("read subdir") {
                            let path = entry.expect("entry").path();
                            let _meta = fs::metadata(&path).expect("stat");
                            count += 1;
                        }
                    }
                    assert_eq!(count, tier.file_count);
                });
            },
        );

        // io_uring readdir + batched statx: walk subdirectories, collect
        // entries per subdir, then batch-stat the entire subdir at once.
        #[cfg(all(target_os = "linux", feature = "io_uring"))]
        {
            if fast_io::statx_supported() {
                group.bench_with_input(
                    BenchmarkId::new("iouring_readdir_statx_batch", tier.label),
                    &root,
                    |b, root| {
                        b.iter(|| {
                            use fast_io::submit_statx_batch;

                            let mut count = 0usize;
                            for subdir_entry in fs::read_dir(root).expect("read root") {
                                let subdir = subdir_entry.expect("subdir entry").path();
                                if !subdir.is_dir() {
                                    continue;
                                }
                                let entries: Vec<PathBuf> = fs::read_dir(&subdir)
                                    .expect("read subdir")
                                    .map(|e| e.expect("entry").path())
                                    .collect();
                                let refs: Vec<&Path> =
                                    entries.iter().map(|p| p.as_path()).collect();
                                let results =
                                    submit_statx_batch(&refs, true).expect("submit_statx_batch");
                                for r in &results {
                                    r.as_ref().expect("statx result");
                                    count += 1;
                                }
                            }
                            assert_eq!(count, tier.file_count);
                        });
                    },
                );
            }
        }

        drop(dir);
    }

    group.finish();
}

/// Parallel stat benchmark using rayon: measures whether io_uring batching
/// outperforms rayon-parallelised sequential stat at high file counts.
/// This simulates the receiver's `PARALLEL_STAT_THRESHOLD`-gated path.
fn bench_parallel_stat(c: &mut Criterion) {
    if !bench_enabled() {
        return;
    }

    let tiers = active_tiers();
    let mut group = c.benchmark_group("high_file_count_parallel_stat");
    group.sample_size(10);

    for tier in &tiers {
        let (min_bytes, max_bytes) = if tier.file_count == FILE_COUNT_100K {
            (1024, 4096)
        } else {
            (64, 256)
        };

        let (dir, paths) = create_fixture(tier.file_count, min_bytes, max_bytes);

        group.throughput(Throughput::Elements(tier.file_count as u64));

        // Rayon parallel stat using std::fs::metadata
        group.bench_with_input(
            BenchmarkId::new("rayon_par_stat", tier.label),
            &paths,
            |b, paths| {
                use rayon::prelude::*;

                b.iter(|| {
                    let count: usize = paths
                        .par_iter()
                        .map(|p| {
                            let _meta = fs::metadata(p).expect("rayon stat");
                            1usize
                        })
                        .sum();
                    assert_eq!(count, tier.file_count);
                });
            },
        );

        // io_uring batched statx (single-threaded ring, chunked submission)
        #[cfg(all(target_os = "linux", feature = "io_uring"))]
        {
            if fast_io::statx_supported() {
                group.bench_with_input(
                    BenchmarkId::new("iouring_statx_batch", tier.label),
                    &paths,
                    |b, paths| {
                        b.iter(|| {
                            let n = iouring_statx_batch_all(paths);
                            assert_eq!(n, tier.file_count);
                        });
                    },
                );
            }
        }

        drop(dir);
    }

    group.finish();
}

criterion_group!(
    high_file_count,
    bench_stat,
    bench_readdir_stat,
    bench_parallel_stat
);
criterion_main!(high_file_count);
