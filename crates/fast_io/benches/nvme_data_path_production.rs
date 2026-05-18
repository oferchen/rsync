//! NVMe data-path benchmark, production wrappers: stdlib vs io_uring.
//!
//! Tracking task: oc-rsync IUD-9 (#2369). Companion bench:
//! `nvme_data_path` (IUD-4, PR #4381) which used a hand-rolled
//! io_uring prototype inline. This bench mirrors the same workload
//! (10 x 1 GiB files) but exercises the *production* wrappers wired
//! by IUD-5 ([`fast_io::write_file_with_io_uring`]) and IUD-6
//! ([`fast_io::read_file_with_io_uring`]) so we can verify the
//! after-change number matches or beats the IUD-4 prototype once
//! both features land.
//!
//! # What this bench measures
//!
//! Four cells, each writing or reading `FILE_COUNT` files of
//! `FILE_BYTES` bytes (10 x 1 GiB by default, total 10 GiB per
//! sample). Throughput is reported in bytes/second so the cells can
//! be compared directly without unit conversion.
//!
//! - `production/stdlib_write/10x1GiB`: baseline write via
//!   [`std::fs::write`]. Represents the path taken on every non-Linux
//!   host and on Linux when `iouring-data-writes` is off.
//! - `production/iouring_write/10x1GiB`: production write via
//!   [`fast_io::write_file_with_io_uring`], which reuses the
//!   `IoUringWriter` + `RegisteredBufferPool` shipped by IUD-5.
//! - `production/stdlib_read/10x1GiB`: baseline read via
//!   [`std::fs::read`]. Represents the path taken on every non-Linux
//!   host and on Linux when `iouring-data-reads` is off.
//! - `production/iouring_read/10x1GiB`: production read via
//!   [`fast_io::read_file_with_io_uring`], which reuses the io_uring
//!   reader path shipped by IUD-6.
//!
//! Both write cells write the same total bytes and run the same
//! end-of-file fsync cadence (stdlib via `std::fs::write` flushing on
//! drop, io_uring via the writer's `sync()` step). Both read cells
//! read the same total bytes from pre-populated files. The wall-time
//! and MiB/s delta is attributable strictly to the dispatch style.
//!
//! # Workload sizing
//!
//! `FILE_COUNT = 10` and `FILE_BYTES = 1 GiB` mirror the IUD-4 bench
//! so the two benches are directly comparable. The 10 GiB working
//! set clears typical RAM page-cache residence on a CI runner and
//! forces real NVMe traffic on a bare-metal host.
//!
//! # When to run
//!
//! Linux 5.6+ with `io_uring_setup(2)` reachable (no seccomp block,
//! no container restriction) and the `iouring-data-writes` +
//! `iouring-data-reads` features enabled. The bench is gated by an
//! env var because each iteration moves 10 GiB of data:
//!
//! ```sh
//! OC_RSYNC_BENCH_NVME_DATA_PATH=1 \
//! OC_RSYNC_BENCH_NVME_PATH=/mnt/nvme/scratch \
//!   cargo bench -p fast_io \
//!     --features iouring-data-writes,iouring-data-reads \
//!     --bench nvme_data_path_production
//! ```
//!
//! `OC_RSYNC_BENCH_NVME_PATH` is optional. When set, the bench
//! creates its scratch dirs under that path (use a real NVMe mount
//! for the headline number). When unset, the bench falls back to the
//! default `TempDir` location, which CI typically backs with a
//! ramdisk so the numbers reflect wrapper-dispatch and submission
//! cost rather than disk bandwidth.
//!
//! Without `OC_RSYNC_BENCH_NVME_DATA_PATH=1` the bench prints a skip
//! line and exits 0, so it is safe to leave registered in
//! `Cargo.toml` and to invoke via `cargo bench -p fast_io` without
//! picking up multi-minute work by accident.
//!
//! # Syscall counting
//!
//! When `perf stat -e syscalls:sys_enter_write,syscalls:sys_enter_read,\
//! syscalls:sys_enter_io_uring_enter` is available, run the bench
//! under `perf stat` to break down the per-cell syscall mix. `perf`
//! is intentionally not invoked from inside the bench: Criterion
//! already produces deterministic wall-time and throughput rows, and
//! adding a `perf` wrapper would force the bench to inspect kernel
//! privileges and `perf_event_paranoid` at runtime. The recommended
//! invocation is:
//!
//! ```sh
//! OC_RSYNC_BENCH_NVME_DATA_PATH=1 perf stat -e \
//!   syscalls:sys_enter_write,syscalls:sys_enter_read,\
//!   syscalls:sys_enter_io_uring_enter,syscalls:sys_enter_fsync \
//!   cargo bench -p fast_io \
//!     --features iouring-data-writes,iouring-data-reads \
//!     --bench nvme_data_path_production -- --profile-time 30
//! ```
//!
//! # What the numbers inform
//!
//! Compare each `production/iouring_*` row against:
//!
//! - The matching `production/stdlib_*` row in this bench (current
//!   delivered headroom of the production wrapper).
//! - The matching `iouring_write_fixed` / `stdlib_write` row in the
//!   sibling `nvme_data_path` bench (whether the production wrapper
//!   captured the gain the IUD-4 prototype projected).
//!
//! If the production io_uring cell trails the prototype cell by more
//! than 5%, the wrapper is leaving headroom on the table; surface the
//! gap as input for IUD-5/IUD-6 selector tuning rather than promoting
//! the feature default.
//!
//! # CI gating
//!
//! All measurement code is gated on `cfg(all(target_os = "linux",
//! feature = "iouring-data-writes", feature = "iouring-data-reads"))`;
//! the macOS, Windows, and feature-off builds compile to a stub `main`
//! that prints a skip line. The `Cargo.toml` `[[bench]]` entry carries
//! `required-features = ["iouring-data-writes", "iouring-data-reads"]`
//! so the bench is excluded from builds that turn either feature off.
//!
//! GitHub-hosted Windows and macOS runners cannot execute this bench.
//! When the io_uring wrapper returns an error (typical inside
//! locked-down container runtimes) the bench surfaces the error
//! rather than masking it, so a regression aborts the sample rather
//! than producing a silently skewed number.

#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
use std::env;
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
use std::fs;
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
use std::path::{Path, PathBuf};

#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
use tempfile::TempDir;

/// Files processed per iteration. Matches the IUD-4 bench so the two
/// harnesses can be compared row-for-row.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
const FILE_COUNT: usize = 10;

/// Bytes per file. Matches the IUD-4 bench (1 GiB) so the 10 GiB
/// working set exits page-cache residence on a CI runner and forces
/// real NVMe traffic on a bare-metal host.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
const FILE_BYTES: usize = 1024 * 1024 * 1024;

/// Bench gate env var. Set to `1` to actually run the bench;
/// otherwise the harness prints a skip line and exits 0 so
/// `cargo bench` is cheap.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
const ENABLE_ENV: &str = "OC_RSYNC_BENCH_NVME_DATA_PATH";

/// Optional path env var. When set, the bench creates its scratch
/// dirs under this path so the workload exercises a real NVMe mount
/// instead of the default `TempDir` location (typically a ramdisk on
/// CI).
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
const NVME_PATH_ENV: &str = "OC_RSYNC_BENCH_NVME_PATH";

/// Returns `true` when the bench should actually run.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn bench_enabled() -> bool {
    matches!(env::var(ENABLE_ENV), Ok(v) if v == "1")
}

/// Creates the per-iteration scratch directory. Honours
/// `OC_RSYNC_BENCH_NVME_PATH` so operators can point the bench at a
/// real NVMe mount; falls back to the default `TempDir` location when
/// unset.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn make_scratch_dir() -> TempDir {
    match env::var(NVME_PATH_ENV) {
        Ok(path) if !path.is_empty() => TempDir::new_in(path).expect("nvme tempdir"),
        _ => TempDir::new().expect("tempdir"),
    }
}

/// Pre-allocates destination paths and the shared per-file payload
/// buffer. The payload is identical for every file so the bench
/// measures dispatch cost rather than data preparation cost.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn prepare_write_workload(dir: &TempDir) -> (Vec<PathBuf>, Vec<u8>) {
    let paths: Vec<PathBuf> = (0..FILE_COUNT)
        .map(|i| dir.path().join(format!("nvme_{i:02}")))
        .collect();
    let payload = vec![0xa5u8; FILE_BYTES];
    (paths, payload)
}

/// Populates `FILE_COUNT` files of `FILE_BYTES` bytes for the read
/// cells. Uses `std::fs::write` so the setup cost is identical for
/// both `stdlib_read` and `iouring_read`.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn prepare_read_workload(dir: &TempDir) -> Vec<PathBuf> {
    let payload = vec![0xa5u8; FILE_BYTES];
    let paths: Vec<PathBuf> = (0..FILE_COUNT)
        .map(|i| dir.path().join(format!("nvme_{i:02}")))
        .collect();
    for path in &paths {
        fs::write(path, &payload).expect("seed read workload");
    }
    paths
}

/// Writes one file via the production stdlib wrapper.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn stdlib_write_file(path: &Path, payload: &[u8]) {
    fs::write(path, payload).expect("stdlib_write");
}

/// Writes one file via the production io_uring wrapper
/// ([`fast_io::write_file_with_io_uring`]).
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn iouring_write_file(path: &Path, payload: &[u8]) {
    fast_io::write_file_with_io_uring(path, payload).expect("iouring_write");
}

/// Reads one file via the production stdlib wrapper.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn stdlib_read_file(path: &Path) -> Vec<u8> {
    fs::read(path).expect("stdlib_read")
}

/// Reads one file via the production io_uring wrapper
/// ([`fast_io::read_file_with_io_uring`]).
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn iouring_read_file(path: &Path) -> Vec<u8> {
    fast_io::read_file_with_io_uring(path).expect("iouring_read")
}

/// Emits the standard skip line when the gate env var is unset.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn skip(cell: &str) {
    eprintln!(
        "Skipping nvme_data_path_production::{cell}: set {ENABLE_ENV}=1 on a Linux 5.6+ host \
         with iouring-data-writes + iouring-data-reads features built in to enable. Optionally \
         set {NVME_PATH_ENV} to a real NVMe mount to exercise sustained disk throughput."
    );
}

/// `production/stdlib_write/10x1GiB`: baseline establishing what
/// today's stdlib wrapper delivers on the IUD-4 workload.
///
/// # Panics
///
/// Panics on `TempDir` creation failure or any `std::fs::write`
/// error; surfaced via `expect(..)` so a regression aborts the sample
/// rather than producing a silently skewed number.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn bench_stdlib_write(c: &mut Criterion) {
    if !bench_enabled() {
        skip("stdlib_write");
        return;
    }

    let mut group = c.benchmark_group("production");
    group.sample_size(10);
    group.throughput(Throughput::Bytes((FILE_COUNT * FILE_BYTES) as u64));

    group.bench_function("stdlib_write/10x1GiB", |b| {
        b.iter_with_setup(
            || {
                let dir = make_scratch_dir();
                let (paths, payload) = prepare_write_workload(&dir);
                (dir, paths, payload)
            },
            |(dir, paths, payload)| {
                for path in &paths {
                    stdlib_write_file(path, &payload);
                }
                drop(dir);
            },
        );
    });

    group.finish();
}

/// `production/iouring_write/10x1GiB`: production io_uring write path
/// shipped by IUD-5.
///
/// # Panics
///
/// Panics on `TempDir` creation failure or any
/// [`fast_io::write_file_with_io_uring`] error; surfaced via
/// `expect(..)` so a regression aborts the sample rather than
/// producing a silently skewed number.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn bench_iouring_write(c: &mut Criterion) {
    if !bench_enabled() {
        skip("iouring_write");
        return;
    }

    let mut group = c.benchmark_group("production");
    group.sample_size(10);
    group.throughput(Throughput::Bytes((FILE_COUNT * FILE_BYTES) as u64));

    group.bench_function("iouring_write/10x1GiB", |b| {
        b.iter_with_setup(
            || {
                let dir = make_scratch_dir();
                let (paths, payload) = prepare_write_workload(&dir);
                (dir, paths, payload)
            },
            |(dir, paths, payload)| {
                for path in &paths {
                    iouring_write_file(path, &payload);
                }
                drop(dir);
            },
        );
    });

    group.finish();
}

/// `production/stdlib_read/10x1GiB`: baseline establishing what
/// today's stdlib wrapper delivers on the IUD-4 workload.
///
/// # Panics
///
/// Panics on `TempDir` creation failure, read-seed failure, or any
/// `std::fs::read` error; surfaced via `expect(..)` so a regression
/// aborts the sample rather than producing a silently skewed number.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn bench_stdlib_read(c: &mut Criterion) {
    if !bench_enabled() {
        skip("stdlib_read");
        return;
    }

    let mut group = c.benchmark_group("production");
    group.sample_size(10);
    group.throughput(Throughput::Bytes((FILE_COUNT * FILE_BYTES) as u64));

    group.bench_function("stdlib_read/10x1GiB", |b| {
        b.iter_with_setup(
            || {
                let dir = make_scratch_dir();
                let paths = prepare_read_workload(&dir);
                (dir, paths)
            },
            |(dir, paths)| {
                for path in &paths {
                    let bytes = stdlib_read_file(path);
                    assert_eq!(bytes.len(), FILE_BYTES, "stdlib_read short read");
                }
                drop(dir);
            },
        );
    });

    group.finish();
}

/// `production/iouring_read/10x1GiB`: production io_uring read path
/// shipped by IUD-6.
///
/// # Panics
///
/// Panics on `TempDir` creation failure, read-seed failure, or any
/// [`fast_io::read_file_with_io_uring`] error; surfaced via
/// `expect(..)` so a regression aborts the sample rather than
/// producing a silently skewed number.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn bench_iouring_read(c: &mut Criterion) {
    if !bench_enabled() {
        skip("iouring_read");
        return;
    }

    let mut group = c.benchmark_group("production");
    group.sample_size(10);
    group.throughput(Throughput::Bytes((FILE_COUNT * FILE_BYTES) as u64));

    group.bench_function("iouring_read/10x1GiB", |b| {
        b.iter_with_setup(
            || {
                let dir = make_scratch_dir();
                let paths = prepare_read_workload(&dir);
                (dir, paths)
            },
            |(dir, paths)| {
                for path in &paths {
                    let bytes = iouring_read_file(path);
                    assert_eq!(bytes.len(), FILE_BYTES, "iouring_read short read");
                }
                drop(dir);
            },
        );
    });

    group.finish();
}

#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
criterion_group!(
    nvme_data_path_production,
    bench_stdlib_write,
    bench_iouring_write,
    bench_stdlib_read,
    bench_iouring_read,
);
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
criterion_main!(nvme_data_path_production);

#[cfg(not(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
)))]
fn main() {
    eprintln!(
        "nvme_data_path_production: skipped (Linux-only bench; requires both the \
         `iouring-data-writes` and `iouring-data-reads` features)"
    );
}
