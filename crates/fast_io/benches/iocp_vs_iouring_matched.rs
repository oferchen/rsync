//! IOCP vs io_uring under matched workloads. Tracking issue: oc-rsync #1868.
//!
//! The receiver write path picks `Writer::IoUring(..)` on Linux and
//! `Writer::Iocp(..)` on Windows. Both fronts are exposed via
//! `crates/fast_io/src/{io_uring,iocp}/file_factory.rs::writer_from_file`.
//! The two kernels cannot be cross-compared directly: an io_uring number
//! produced on a Linux runner and an IOCP number produced on a Windows
//! runner reflect different storage stacks, schedulers, and filesystems.
//! This bench produces a per-host ratio that the maintainer can compare
//! across hosts without ever needing to run both stacks side-by-side.
//!
//! # Matched workload
//!
//! Each per-host cell uses the same payload sizes (4 KiB / 64 KiB /
//! 1 MiB), the same file count (1000), the same destination temp dir
//! lifecycle (one [`tempfile::TempDir`] per iteration, dropped after the
//! batch), and the same payload pattern (a 4 KiB deterministic seed
//! tiled across the requested size). The only knob that differs across
//! the platform-specific rows is the in-flight concurrency setting that
//! each kernel exposes:
//!
//! - Linux: `IoUringConfig::sq_entries` (default 64 vs 8 in the
//!   `concurrent_ops_8` row), plus an SQPOLL variant that mirrors
//!   `IoUringConfig::sqpoll = true`. Run via [`IoUringDiskBatch`].
//! - Windows: `IocpConfig::concurrent_ops` (default 4 vs 8 in the
//!   `concurrent_ops_8` row). Run via [`IocpDiskBatch`].
//!
//! # Cross-platform normalisation
//!
//! Every host runs a `std_baseline` cell that writes the same files
//! through `File::create` + `write_all`. To compare IOCP vs io_uring
//! across two hosts, divide each platform-specific cell's throughput by
//! that host's `std_baseline` throughput for the same payload size, then
//! compare the resulting ratios. The ratio strips out the host-specific
//! storage stack so the residual delta reflects the kernel-async
//! dispatch style itself.
//!
//! Example output interpretation (1 MiB cell):
//!
//! ```text
//! Linux:   iouring_default = 1.7x std_baseline
//! Windows: iocp_default    = 1.3x std_baseline
//! => io_uring delivers a larger relative win on its host than IOCP
//!    does on its host. Action: keep #1868 closed only after both
//!    ratios are above 1.2x.
//! ```
//!
//! # Per-platform cells
//!
//! - Linux (this file, gated on `all(target_os = "linux", feature = "io_uring")`):
//!   - `iouring_default` - [`IoUringConfig::default`] (`sq_entries = 64`).
//!   - `iouring_concurrent_ops_8` - [`IoUringConfig::default`] with
//!     `sq_entries = 8` to mirror the IOCP `concurrent_ops_8` row.
//!   - `iouring_sqpoll` - [`IoUringConfig::default`] with `sqpoll = true`.
//!     Skips cleanly when SQPOLL setup fails (needs `CAP_SYS_NICE` on
//!     pre-5.13 kernels). Mirrors the production
//!     `IoUringConfig::build_ring` path.
//!   - `std_baseline` - `File::create` + `write_all`.
//! - Windows (this file, gated on `all(target_os = "windows", feature = "iocp"))`:
//!   - `iocp_default` - [`IocpConfig::default`] (`concurrent_ops = 4`).
//!   - `iocp_concurrent_ops_8` - [`IocpConfig::default`] with
//!     `concurrent_ops = 8`.
//!   - `std_baseline` - `File::create` + `write_all`.
//! - Every other host (and Linux without the `io_uring` feature, and
//!   Windows without the `iocp` feature): only the `std_baseline` cell
//!   runs, giving the maintainer a portable common reference row even
//!   on a host that cannot run either kernel-async stack. The bench
//!   file always compiles; non-target-OS bodies degrade to stub `main`
//!   or to a baseline-only group so Criterion's `harness = false`
//!   contract holds on every platform.
//!
//! # When to run
//!
//! ```sh
//! # On a Linux 5.6+ host:
//! OC_RSYNC_BENCH_IOURING_RING=1 \
//!   cargo bench -p fast_io --bench iocp_vs_iouring_matched
//!
//! # On a Linux 5.6+ host with SQPOLL accepted (5.13+ unprivileged, or
//! # CAP_SYS_NICE on 5.6-5.12):
//! OC_RSYNC_BENCH_IOURING_RING=1 \
//! OC_RSYNC_BENCH_IOURING_SQPOLL=1 \
//!   cargo bench -p fast_io --bench iocp_vs_iouring_matched
//!
//! # On a Windows host (no env gate needed):
//! cargo bench -p fast_io --bench iocp_vs_iouring_matched
//! ```
//!
//! Linux io_uring cells are env-gated to keep `cargo bench -p fast_io`
//! cheap on every Linux CI runner that does not opt in. The Windows
//! cells run unconditionally because the IOCP kernel path is built into
//! every supported Windows version.
//!
//! # What the numbers inform
//!
//! Outcome -> action for [#1868] (matched IOCP vs io_uring comparison):
//!
//! - Both kernel cells beat `std_baseline` by >= 25% on the 1 MiB row:
//!   document the win in the audit and close #1868 with a reference to
//!   the produced ratio table.
//! - One kernel beats `std_baseline` and the other does not: open a
//!   follow-up to investigate the lagging side's batching depth
//!   (`sq_entries` on io_uring, `concurrent_ops` on IOCP) before
//!   declaring parity.
//! - Either kernel regresses against `std_baseline` on the 4 KiB row:
//!   expected and informs the small-file fallback policy
//!   (`IOCP_MIN_FILE_SIZE` on Windows, `for_small_files` on Linux).
//!
//! [#1868]: https://github.com/oferchen/oc-rsync/issues/1868
//! [`IoUringDiskBatch`]: ../../src/io_uring/disk_batch.rs
//! [`IoUringConfig::default`]: ../../src/io_uring_common.rs
//! [`IoUringConfig::build_ring`]: ../../src/io_uring/config.rs
//! [`IocpDiskBatch`]: ../../src/iocp/disk_batch.rs
//! [`IocpConfig::default`]: ../../src/iocp/config.rs

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use criterion::{criterion_group, criterion_main};
use tempfile::TempDir;

/// Files per iteration. Matches `iocp_vs_stdio.rs` so the produced rows
/// can be lined up against #1899's IOCP baseline without recomputing
/// throughput.
const FILE_COUNT: usize = 1000;

/// Payload sizes (4 KiB / 64 KiB / 1 MiB). Matches the IOCP bench from
/// #1899 so the matched-workload constraint is enforced by sharing the
/// exact same constant.
const PAYLOAD_SIZES: [usize; 3] = [4 * 1024, 64 * 1024, 1024 * 1024];

/// Size of the deterministic payload seed. 4 KiB is small enough to keep
/// allocator pressure low and large enough that tiling it into a 1 MiB
/// payload still produces 256 distinct blocks, so neither NTFS sparse
/// folding nor ext4 dedup heuristics can collapse the writes.
const SEED_BYTES: usize = 4 * 1024;

/// Builds a deterministic payload of the requested size by tiling a
/// 4 KiB seed. The seed is a linear congruential sequence so the byte
/// stream is incompressible enough to defeat sparse-file folding on
/// every tested filesystem.
#[allow(dead_code)]
fn make_payload(size: usize) -> Vec<u8> {
    // Linear congruential generator constants from Numerical Recipes
    // (a = 1664525, c = 1013904223). Pure pattern, no rng dependency.
    let mut state: u32 = 0xdead_beef;
    let mut seed = Vec::with_capacity(SEED_BYTES);
    while seed.len() < SEED_BYTES {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        seed.extend_from_slice(&state.to_le_bytes());
    }
    seed.truncate(SEED_BYTES);

    let mut out = Vec::with_capacity(size);
    while out.len() < size {
        let remaining = size - out.len();
        let chunk = remaining.min(seed.len());
        out.extend_from_slice(&seed[..chunk]);
    }
    out
}

/// Pre-allocates destination paths and the shared payload for one cell.
#[allow(dead_code)]
fn prepare_workload(dir: &TempDir, payload_size: usize) -> (Vec<PathBuf>, Vec<u8>) {
    let paths: Vec<PathBuf> = (0..FILE_COUNT)
        .map(|i| dir.path().join(format!("f_{i:07}")))
        .collect();
    let payload = make_payload(payload_size);
    (paths, payload)
}

/// Writes every file through `File::create` + `write_all`. The
/// cross-platform reference row. Identical on Linux and Windows so the
/// `std_baseline` numbers can be used to normalise the per-host
/// kernel-async cells before cross-host comparison.
#[allow(dead_code)]
fn run_stdfs(paths: &[PathBuf], payload: &[u8]) {
    for path in paths {
        let mut file = File::create(path).expect("create");
        file.write_all(payload).expect("write_all");
    }
}

// ---------------------------------------------------------------------------
// Linux: io_uring cells
// ---------------------------------------------------------------------------

#[cfg(all(target_os = "linux", feature = "io_uring"))]
mod linux_cells {
    use super::{FILE_COUNT, PAYLOAD_SIZES, PathBuf, TempDir, prepare_workload, run_stdfs};
    use criterion::{BenchmarkId, Criterion, Throughput};
    use fast_io::{IoUringConfig, IoUringDiskBatch};
    use std::env;
    use std::fs::OpenOptions;

    /// Shared env-var gate. Matches `iouring_sqpoll_vs_regular.rs` so a
    /// single env-var opts every Linux io_uring bench in on a runner.
    const ENABLE_ENV: &str = "OC_RSYNC_BENCH_IOURING_RING";
    /// SQPOLL-specific gate. Set in addition to [`ENABLE_ENV`] to run
    /// the `iouring_sqpoll` cell on a runner where SQPOLL is accepted.
    const SQPOLL_ENV: &str = "OC_RSYNC_BENCH_IOURING_SQPOLL";

    /// `IoUringConfig::sq_entries` value used by the
    /// `iouring_concurrent_ops_8` cell to mirror the IOCP
    /// `concurrent_ops_8` row.
    const CONCURRENT_OPS_8: u32 = 8;

    fn iouring_enabled() -> bool {
        match env::var(ENABLE_ENV) {
            Ok(v) if v == "1" => IoUringDiskBatch::new(&IoUringConfig::default()).is_ok(),
            _ => false,
        }
    }

    fn sqpoll_enabled() -> bool {
        if !iouring_enabled() {
            return false;
        }
        if !matches!(env::var(SQPOLL_ENV), Ok(v) if v == "1") {
            return false;
        }
        let config = IoUringConfig {
            sqpoll: true,
            ..IoUringConfig::default()
        };
        IoUringDiskBatch::new(&config).is_ok()
    }

    /// Writes every file through one [`IoUringDiskBatch`] with the
    /// supplied config. Mirrors the receiver disk-commit thread:
    /// `begin_file` -> `write_data` -> `commit_file` per file.
    fn run_iouring_batch(paths: &[PathBuf], payload: &[u8], config: &IoUringConfig) {
        let mut batch = IoUringDiskBatch::new(config).expect("iouring batch");
        for path in paths {
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .expect("create");
            batch.begin_file(file).expect("begin_file");
            batch.write_data(payload).expect("write_data");
            // No fsync, to match `run_stdfs`.
            let (_file, _bytes) = batch.commit_file(false).expect("commit_file");
        }
    }

    pub fn bench_matched(c: &mut Criterion) {
        let mut group = c.benchmark_group("iocp_vs_iouring_matched");
        group.sample_size(10);

        let iouring_on = iouring_enabled();
        let sqpoll_on = sqpoll_enabled();

        if !iouring_on {
            eprintln!(
                "iocp_vs_iouring_matched: io_uring cells skipped (set {ENABLE_ENV}=1 on a \
                 Linux 5.6+ host with io_uring_setup(2) reachable to enable them); only the \
                 std_baseline row will run."
            );
        }
        if iouring_on && !sqpoll_on {
            eprintln!(
                "iocp_vs_iouring_matched: iouring_sqpoll cell skipped (set {SQPOLL_ENV}=1 in \
                 addition to {ENABLE_ENV}=1 on a Linux host where IORING_SETUP_SQPOLL is \
                 accepted: 5.13+ unprivileged, or CAP_SYS_NICE on 5.6-5.12)."
            );
        }

        for &payload_size in &PAYLOAD_SIZES {
            group.throughput(Throughput::Bytes((FILE_COUNT * payload_size) as u64));

            group.bench_function(BenchmarkId::new("std_baseline", payload_size), |b| {
                b.iter_with_setup(
                    || {
                        let dir = TempDir::new().expect("tempdir");
                        let (paths, payload) = prepare_workload(&dir, payload_size);
                        (dir, paths, payload)
                    },
                    |(dir, paths, payload)| {
                        run_stdfs(&paths, &payload);
                        drop(dir);
                    },
                );
            });

            if !iouring_on {
                continue;
            }

            group.bench_function(BenchmarkId::new("iouring_default", payload_size), |b| {
                let config = IoUringConfig::default();
                b.iter_with_setup(
                    || {
                        let dir = TempDir::new().expect("tempdir");
                        let (paths, payload) = prepare_workload(&dir, payload_size);
                        (dir, paths, payload)
                    },
                    |(dir, paths, payload)| {
                        run_iouring_batch(&paths, &payload, &config);
                        drop(dir);
                    },
                );
            });

            group.bench_function(
                BenchmarkId::new("iouring_concurrent_ops_8", payload_size),
                |b| {
                    let config = IoUringConfig {
                        sq_entries: CONCURRENT_OPS_8,
                        ..IoUringConfig::default()
                    };
                    b.iter_with_setup(
                        || {
                            let dir = TempDir::new().expect("tempdir");
                            let (paths, payload) = prepare_workload(&dir, payload_size);
                            (dir, paths, payload)
                        },
                        |(dir, paths, payload)| {
                            run_iouring_batch(&paths, &payload, &config);
                            drop(dir);
                        },
                    );
                },
            );

            if sqpoll_on {
                group.bench_function(BenchmarkId::new("iouring_sqpoll", payload_size), |b| {
                    let config = IoUringConfig {
                        sqpoll: true,
                        ..IoUringConfig::default()
                    };
                    b.iter_with_setup(
                        || {
                            let dir = TempDir::new().expect("tempdir");
                            let (paths, payload) = prepare_workload(&dir, payload_size);
                            (dir, paths, payload)
                        },
                        |(dir, paths, payload)| {
                            run_iouring_batch(&paths, &payload, &config);
                            drop(dir);
                        },
                    );
                });
            }
        }

        group.finish();
    }
}

// ---------------------------------------------------------------------------
// Windows: IOCP cells
// ---------------------------------------------------------------------------

#[cfg(all(target_os = "windows", feature = "iocp"))]
mod windows_cells {
    use super::{FILE_COUNT, PAYLOAD_SIZES, PathBuf, TempDir, prepare_workload, run_stdfs};
    use criterion::{BenchmarkId, Criterion, Throughput};
    use fast_io::{IocpConfig, IocpDiskBatch};
    use std::fs::OpenOptions;

    /// `IocpConfig::concurrent_ops` value used by the
    /// `iocp_concurrent_ops_8` cell.
    const CONCURRENT_OPS_8: u32 = 8;

    /// Writes every file through one [`IocpDiskBatch`] with the supplied
    /// config. Mirrors the receiver disk-commit thread: `begin_file` ->
    /// `write_data` -> `commit_file` per file.
    fn run_iocp_batch(paths: &[PathBuf], payload: &[u8], config: &IocpConfig) {
        let mut batch = IocpDiskBatch::new(config).expect("iocp batch");
        for path in paths {
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .expect("create");
            batch.begin_file(file).expect("begin_file");
            batch.write_data(payload).expect("write_data");
            // No fsync, to match `run_stdfs`.
            let (_file, _bytes) = batch.commit_file(false).expect("commit_file");
        }
    }

    pub fn bench_matched(c: &mut Criterion) {
        let mut group = c.benchmark_group("iocp_vs_iouring_matched");
        group.sample_size(10);

        for &payload_size in &PAYLOAD_SIZES {
            group.throughput(Throughput::Bytes((FILE_COUNT * payload_size) as u64));

            group.bench_function(BenchmarkId::new("std_baseline", payload_size), |b| {
                b.iter_with_setup(
                    || {
                        let dir = TempDir::new().expect("tempdir");
                        let (paths, payload) = prepare_workload(&dir, payload_size);
                        (dir, paths, payload)
                    },
                    |(dir, paths, payload)| {
                        run_stdfs(&paths, &payload);
                        drop(dir);
                    },
                );
            });

            group.bench_function(BenchmarkId::new("iocp_default", payload_size), |b| {
                let config = IocpConfig::default();
                b.iter_with_setup(
                    || {
                        let dir = TempDir::new().expect("tempdir");
                        let (paths, payload) = prepare_workload(&dir, payload_size);
                        (dir, paths, payload)
                    },
                    |(dir, paths, payload)| {
                        run_iocp_batch(&paths, &payload, &config);
                        drop(dir);
                    },
                );
            });

            group.bench_function(
                BenchmarkId::new("iocp_concurrent_ops_8", payload_size),
                |b| {
                    let config = IocpConfig {
                        concurrent_ops: CONCURRENT_OPS_8,
                        ..IocpConfig::default()
                    };
                    b.iter_with_setup(
                        || {
                            let dir = TempDir::new().expect("tempdir");
                            let (paths, payload) = prepare_workload(&dir, payload_size);
                            (dir, paths, payload)
                        },
                        |(dir, paths, payload)| {
                            run_iocp_batch(&paths, &payload, &config);
                            drop(dir);
                        },
                    );
                },
            );
        }

        group.finish();
    }
}

// ---------------------------------------------------------------------------
// Fallback for hosts without a kernel-async backend on this build (the
// std_baseline cell still runs so every host produces a portable
// reference row).
// ---------------------------------------------------------------------------

#[cfg(not(any(
    all(target_os = "linux", feature = "io_uring"),
    all(target_os = "windows", feature = "iocp"),
)))]
mod baseline_only {
    use super::{FILE_COUNT, PAYLOAD_SIZES, TempDir, prepare_workload, run_stdfs};
    use criterion::{BenchmarkId, Criterion, Throughput};

    pub fn bench_matched(c: &mut Criterion) {
        let mut group = c.benchmark_group("iocp_vs_iouring_matched");
        group.sample_size(10);

        eprintln!(
            "iocp_vs_iouring_matched: only std_baseline runs on this host (neither io_uring \
             nor IOCP backend is built). Run on Linux (with the `io_uring` feature) or Windows \
             (with the `iocp` feature) to produce comparable kernel-async numbers."
        );

        for &payload_size in &PAYLOAD_SIZES {
            group.throughput(Throughput::Bytes((FILE_COUNT * payload_size) as u64));

            group.bench_function(BenchmarkId::new("std_baseline", payload_size), |b| {
                b.iter_with_setup(
                    || {
                        let dir = TempDir::new().expect("tempdir");
                        let (paths, payload) = prepare_workload(&dir, payload_size);
                        (dir, paths, payload)
                    },
                    |(dir, paths, payload)| {
                        run_stdfs(&paths, &payload);
                        drop(dir);
                    },
                );
            });
        }

        group.finish();
    }
}

// ---------------------------------------------------------------------------
// Criterion entry point.
// ---------------------------------------------------------------------------

#[cfg(all(target_os = "linux", feature = "io_uring"))]
criterion_group!(iocp_vs_iouring_matched, linux_cells::bench_matched);

#[cfg(all(target_os = "windows", feature = "iocp"))]
criterion_group!(iocp_vs_iouring_matched, windows_cells::bench_matched);

#[cfg(not(any(
    all(target_os = "linux", feature = "io_uring"),
    all(target_os = "windows", feature = "iocp"),
)))]
criterion_group!(iocp_vs_iouring_matched, baseline_only::bench_matched);

criterion_main!(iocp_vs_iouring_matched);
