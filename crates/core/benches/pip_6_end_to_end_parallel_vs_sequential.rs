//! PIP-6 - End-to-end parallel-vs-sequential receive-delta bench.
//!
//! # Why this exists
//!
//! `crates/engine/benches/parallel_receive_delta_perf.rs` (BR-3i.f, #2502
//! completed) drives `ParallelDeltaApplier` directly against in-memory sinks.
//! That harness isolates apply-loop scheduling, but production receivers also
//! pay for protocol framing, signature exchange, fsync, and the cost of the
//! dispatch heuristic itself. BR-3j.f (#2508, pending) is the post-DashMap
//! re-bench of the same apply loop.
//!
//! PIP-6 is the **end-to-end** complement: it drives a real `oc-rsync` client
//! against a real `oc-rsync` daemon over loopback, on workload shapes
//! calibrated to the original Path B heuristic boundary
//! (`file_count > 100 || total_size > 64 MiB`) that PIP-3+5 (PR #4666)
//! wired into the receiver. PIP-8 (#TBD) tore out that dispatch scaffolding
//! after PIP-7 (PR #4730) proved it was a side-effect-only no-op; the
//! workload shapes are retained as a calibration baseline for the
//! eventual PIP-9 re-wiring of `ParallelDeltaApplier` into the receiver
//! through the RJN-3 fan-out caller.
//!
//! The bench compares two binaries running the **same** workload:
//!
//! - `oc-rsync` built with default features.
//! - `oc-rsync` built **without** `parallel-receive-delta`.
//!
//! Until PIP-9 lands the two builds are functionally equivalent (the
//! feature flag is a no-op), so the bench is informational rather than
//! decision-driving. Same workload, same wire framing, same disk; the
//! wall-clock delta is whatever scheduling noise the host produces.
//!
//! # Workload matrix
//!
//! Five shapes calibrated to the dispatch boundary. See
//! `docs/design/pip-6-end-to-end-parallel-vs-sequential-bench-2026-05-21.md`
//! section 4 for the rationale of each shape and the heuristic decision
//! the production dispatcher takes.
//!
//! | Shape              | Files  | Per-file size | Heuristic    |
//! |--------------------|--------|---------------|--------------|
//! | `single_large_file`| 1      | 1 GiB         | parallel     |
//! | `many_small_files` | 10,000 | 4 KiB         | parallel     |
//! | `boundary_under`   | 50     | ~655 KiB      | sequential   |
//! | `boundary_over`    | 200    | 160 KiB       | parallel     |
//! | `mixed_directory`  | 1,000  | 4 KiB - 4 MiB | parallel     |
//!
//! # Decision criteria
//!
//! Lifted from the design doc (section 7):
//!
//! - **Win.** `parallel_wall / sequential_wall <= 0.9` on at least 2 of 5
//!   shapes.
//! - **Regression budget.** No shape regresses by more than 5%.
//! - **Boundary control.** `boundary_under` wall-clock agrees within +/-3%
//!   (both builds dispatch sequential there).
//!
//! # How to run
//!
//! Two `oc-rsync` binaries are required. The bench resolves them via env
//! vars; defaults match the build script convention:
//!
//! ```sh
//! # Default-features build (parallel-receive-delta available)
//! cargo build --release --bin oc-rsync
//!
//! # No-default-features build (parallel-receive-delta compiled out)
//! cargo build --release \
//!     --target-dir target/release-no-parallel \
//!     --no-default-features \
//!     --features 'zstd lz4 xattr iconv' \
//!     --bin oc-rsync
//!
//! # Run the bench
//! cargo bench -p core --bench pip_6_end_to_end_parallel_vs_sequential
//! ```
//!
//! Env-var overrides:
//!
//! - `OC_RSYNC_BIN_PARALLEL` - path to the default-features binary
//!   (default `target/release/oc-rsync`).
//! - `OC_RSYNC_BIN_SEQUENTIAL` - path to the no-default-features binary
//!   (default `target/release-no-parallel/oc-rsync`).
//! - `OC_RSYNC_BENCH_LARGE_GIB` - override the `single_large_file` size in
//!   GiB (default `1`). Useful on storage-constrained dev boxes.
//!
//! When either binary is missing, the bench prints a skip message and
//! returns - matching the `BenchDaemon::start` pattern in
//! `crates/core/benches/transfer_benchmark.rs`.
//!
//! # Cross-references
//!
//! - Design doc:
//!   `docs/design/pip-6-end-to-end-parallel-vs-sequential-bench-2026-05-21.md`
//! - BR-3i.f apply-loop bench:
//!   `crates/engine/benches/parallel_receive_delta_perf.rs`
//! - Sibling end-to-end bench harness this scaffold mirrors:
//!   `crates/core/benches/transfer_benchmark.rs`.

#![deny(unsafe_code)]

use std::fs::{self, File};
use std::io::Write;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU16, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tempfile::TempDir;

/// Defaults match the build script convention documented in the module doc.
const DEFAULT_BIN_PARALLEL: &str = "target/release/oc-rsync";
const DEFAULT_BIN_SEQUENTIAL: &str = "target/release-no-parallel/oc-rsync";

/// Daemon-port allocator. Starts above the ephemeral range used by other
/// benches in the workspace to keep parallel bench runs from colliding.
static NEXT_PORT: AtomicU16 = AtomicU16::new(15_200);

fn next_port() -> u16 {
    NEXT_PORT.fetch_add(1, Ordering::SeqCst)
}

/// The two build variants the bench compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BuildVariant {
    /// Default features; parallel-receive-delta available; dispatcher honours
    /// the Path B heuristic.
    Parallel,
    /// `parallel-receive-delta` compiled out; dispatcher logs
    /// `parallel_unavailable` and always picks sequential.
    Sequential,
}

impl BuildVariant {
    fn label(self) -> &'static str {
        match self {
            BuildVariant::Parallel => "parallel",
            BuildVariant::Sequential => "sequential",
        }
    }

    fn env_var(self) -> &'static str {
        match self {
            BuildVariant::Parallel => "OC_RSYNC_BIN_PARALLEL",
            BuildVariant::Sequential => "OC_RSYNC_BIN_SEQUENTIAL",
        }
    }

    fn default_path(self) -> &'static str {
        match self {
            BuildVariant::Parallel => DEFAULT_BIN_PARALLEL,
            BuildVariant::Sequential => DEFAULT_BIN_SEQUENTIAL,
        }
    }

    /// Resolves the binary path for this variant from the env var, falling
    /// back to the documented default. Returns `None` when neither resolves
    /// to a regular file; callers print a skip message and return.
    fn resolve_path(self) -> Option<PathBuf> {
        let candidate = std::env::var_os(self.env_var())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(self.default_path()));
        if candidate.is_file() {
            Some(candidate)
        } else {
            None
        }
    }
}

/// Workload shapes; column 1 of the matrix in the design doc.
#[derive(Clone, Copy, Debug)]
enum WorkloadShape {
    SingleLargeFile,
    ManySmallFiles,
    BoundaryUnder,
    BoundaryOver,
    MixedDirectory,
}

impl WorkloadShape {
    const ALL: &'static [WorkloadShape] = &[
        WorkloadShape::SingleLargeFile,
        WorkloadShape::ManySmallFiles,
        WorkloadShape::BoundaryUnder,
        WorkloadShape::BoundaryOver,
        WorkloadShape::MixedDirectory,
    ];

    fn label(self) -> &'static str {
        match self {
            WorkloadShape::SingleLargeFile => "single_large_file",
            WorkloadShape::ManySmallFiles => "many_small_files",
            WorkloadShape::BoundaryUnder => "boundary_under",
            WorkloadShape::BoundaryOver => "boundary_over",
            WorkloadShape::MixedDirectory => "mixed_directory",
        }
    }

    /// Total source bytes for this shape; the bench reports throughput
    /// against this number.
    fn total_bytes(self) -> u64 {
        match self {
            WorkloadShape::SingleLargeFile => single_large_file_size() as u64,
            WorkloadShape::ManySmallFiles => (10_000 * 4 * 1024) as u64,
            WorkloadShape::BoundaryUnder => (50 * 655 * 1024) as u64,
            WorkloadShape::BoundaryOver => (200 * 160 * 1024) as u64,
            WorkloadShape::MixedDirectory => mixed_directory_total_bytes() as u64,
        }
    }
}

/// Resolves the `single_large_file` size from the env var, defaulting to
/// 1 GiB per the design doc. Useful for storage-constrained dev boxes that
/// cannot spare a full GiB on the bench tempdir.
fn single_large_file_size() -> usize {
    let gib = std::env::var("OC_RSYNC_BENCH_LARGE_GIB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1);
    gib * 1024 * 1024 * 1024
}

/// Sizes drawn for the `mixed_directory` shape. Mirrors the `mixed` cell in
/// `crates/engine/benches/parallel_receive_delta_perf.rs::mixed_spec` so
/// the apply-loop and end-to-end benches share a workload vocabulary.
const MIXED_SIZES: &[usize] = &[
    4 * 1024,
    16 * 1024,
    64 * 1024,
    256 * 1024,
    1024 * 1024,
    4 * 1024 * 1024,
];

fn mixed_directory_total_bytes() -> usize {
    (0..1_000usize)
        .map(|i| MIXED_SIZES[i % MIXED_SIZES.len()])
        .sum()
}

/// Writes `size` bytes of a deterministic, non-zero pattern to `path`.
/// Non-zero so the sparse-file fast path does not skip the bytes; the
/// pattern is keyed by the file index so different files have different
/// content (which the delta path can actually do work against).
fn write_pattern(path: &Path, size: usize, seed: u8) {
    let mut file = File::create(path).expect("create source file");
    let chunk: Vec<u8> = (0..(64 * 1024))
        .map(|i| ((i as u8).wrapping_add(seed)) | 0x01)
        .collect();
    let mut remaining = size;
    while remaining > 0 {
        let take = remaining.min(chunk.len());
        file.write_all(&chunk[..take]).expect("write chunk");
        remaining -= take;
    }
    file.flush().expect("flush");
}

/// Populates `module_dir` with the source tree for `shape`. Idempotent:
/// callers should `clear_module` first so each iteration sees a clean
/// source tree (and so file mtimes are fresh, which prevents the upstream
/// quick-check from spuriously skipping files between iterations).
fn populate_module(module_dir: &Path, shape: WorkloadShape) {
    match shape {
        WorkloadShape::SingleLargeFile => {
            write_pattern(&module_dir.join("large.dat"), single_large_file_size(), 0);
        }
        WorkloadShape::ManySmallFiles => {
            for i in 0..10_000 {
                let path = module_dir.join(format!("small_{i:05}.dat"));
                write_pattern(&path, 4 * 1024, (i & 0xff) as u8);
            }
        }
        WorkloadShape::BoundaryUnder => {
            for i in 0..50 {
                let path = module_dir.join(format!("file_{i:03}.dat"));
                write_pattern(&path, 655 * 1024, (i & 0xff) as u8);
            }
        }
        WorkloadShape::BoundaryOver => {
            for i in 0..200 {
                let path = module_dir.join(format!("file_{i:03}.dat"));
                write_pattern(&path, 160 * 1024, (i & 0xff) as u8);
            }
        }
        WorkloadShape::MixedDirectory => {
            for i in 0..1_000 {
                let size = MIXED_SIZES[i % MIXED_SIZES.len()];
                let path = module_dir.join(format!("mixed_{i:04}.dat"));
                write_pattern(&path, size, (i & 0xff) as u8);
            }
        }
    }
}

/// Removes every entry in `module_dir` without removing the directory itself.
fn clear_module(module_dir: &Path) {
    if !module_dir.exists() {
        return;
    }
    for entry in fs::read_dir(module_dir).expect("read module") {
        let entry = entry.expect("entry");
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path).expect("remove dir");
        } else {
            fs::remove_file(&path).expect("remove file");
        }
    }
}

/// A daemon process launched from one of the two `oc-rsync` build variants.
/// Pointed at a tempdir module so the bench can swap the source tree per
/// workload without restarting the daemon.
struct OcRsyncDaemon {
    _workdir: TempDir,
    module_path: PathBuf,
    port: u16,
    process: Child,
    binary: PathBuf,
}

impl OcRsyncDaemon {
    /// Spawns the daemon listening on a fresh port; module name is `bench`.
    /// Returns `None` when the binary is missing - callers print a skip
    /// message and return without panicking.
    fn start(variant: BuildVariant) -> Option<Self> {
        let binary = variant.resolve_path()?;

        let workdir = TempDir::new().ok()?;
        let port = next_port();
        let config_path = workdir.path().join("oc-rsyncd.conf");
        let pid_path = workdir.path().join("oc-rsyncd.pid");
        let module_path = workdir.path().join("module");
        fs::create_dir_all(&module_path).ok()?;

        let config = format!(
            "pid file = {}\nport = {}\nuse chroot = false\nnumeric ids = yes\n\n\
             [bench]\n    path = {}\n    read only = false\n",
            pid_path.display(),
            port,
            module_path.display()
        );
        fs::write(&config_path, &config).ok()?;

        let process = Command::new(&binary)
            .arg("--daemon")
            .arg("--config")
            .arg(&config_path)
            .arg("--no-detach")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
                return Some(Self {
                    _workdir: workdir,
                    module_path,
                    port,
                    process,
                    binary,
                });
            }
            thread::sleep(Duration::from_millis(50));
        }
        None
    }

    fn module_url(&self) -> String {
        format!("rsync://127.0.0.1:{}/bench/", self.port)
    }
}

impl Drop for OcRsyncDaemon {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// Runs the client binary against `source_url`, writing into `dest`.
/// Returns the wall-clock duration of the transfer. Panics on transfer
/// failure - a non-zero exit from a bench iteration is a regression that
/// must surface, not a quiet skip.
fn run_client(binary: &Path, source_url: &str, dest: &Path) -> Duration {
    let start = Instant::now();
    let status = Command::new(binary)
        .arg("-a")
        .arg(source_url)
        .arg(dest)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn client");
    assert!(
        status.success(),
        "client transfer failed (binary={}, source={}, dest={})",
        binary.display(),
        source_url,
        dest.display()
    );
    start.elapsed()
}

/// Runs one workload shape against one build variant. The daemon and
/// client are both spawned from the same `variant` binary so the receiver
/// dispatch path is the one under test (the daemon-side receiver in pull
/// mode is the receiver whose strategy decision matters).
fn bench_shape_variant(c: &mut Criterion, shape: WorkloadShape, variant: BuildVariant) {
    let Some(daemon) = OcRsyncDaemon::start(variant) else {
        eprintln!(
            "PIP-6 skip: {} binary not found (set {}={})",
            variant.label(),
            variant.env_var(),
            variant.default_path()
        );
        return;
    };

    clear_module(&daemon.module_path);
    populate_module(&daemon.module_path, shape);

    let total_bytes = shape.total_bytes();
    let group_name = format!("pip_6_end_to_end/{}", shape.label());
    let mut group = c.benchmark_group(group_name);
    group.throughput(Throughput::Bytes(total_bytes));
    // Bench shapes vary by ~5 orders of magnitude in wall-clock; pin sample
    // size and measurement window per shape so the large-file cell does
    // not blow the criterion budget while the small cells still get
    // statistically meaningful sample counts.
    let (samples, secs) = match shape {
        WorkloadShape::SingleLargeFile => (10u32, 60u64),
        WorkloadShape::ManySmallFiles => (10u32, 30u64),
        WorkloadShape::BoundaryUnder => (15u32, 20u64),
        WorkloadShape::BoundaryOver => (15u32, 20u64),
        WorkloadShape::MixedDirectory => (10u32, 45u64),
    };
    group.sample_size(samples as usize);
    group.measurement_time(Duration::from_secs(secs));

    let binary = daemon.binary.clone();
    let module_url = daemon.module_url();

    group.bench_with_input(
        BenchmarkId::new(variant.label(), shape.label()),
        &shape,
        |b, _shape| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let dest = TempDir::new().expect("dest tempdir");
                    total += run_client(&binary, &module_url, dest.path());
                }
                total
            });
        },
    );

    group.finish();
}

/// Drives the full matrix: every shape against every variant. Each
/// `(shape, variant)` pair gets its own daemon so daemon-side state never
/// leaks between cells - the receiver builds its file list per transfer,
/// not per process.
fn bench_pip_6(c: &mut Criterion) {
    for &shape in WorkloadShape::ALL {
        for &variant in &[BuildVariant::Sequential, BuildVariant::Parallel] {
            bench_shape_variant(c, shape, variant);
        }
    }
}

criterion_group!(
    name = pip_6_benches;
    config = Criterion::default()
        // Per-cell sample/measurement overrides set inside bench_shape_variant.
        .warm_up_time(Duration::from_secs(2));
    targets = bench_pip_6
);

criterion_main!(pip_6_benches);
