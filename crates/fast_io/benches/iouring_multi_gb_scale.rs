//! Multi-GB single-file io_uring scale benchmark (IUB-4).
//!
//! Measures io_uring vs standard I/O throughput at production file sizes
//! where the hypothesis predicts io_uring pays off: 2 GiB, 10 GiB, 50 GiB.
//! The existing io_uring benches test at ~148 MB (10 x ~15 MB) or 10 GiB
//! (10 x 1 GiB) and show ~1.00x; this bench isolates the single-file case
//! at scale to determine whether ring submission amortisation over a larger
//! contiguous write/read produces measurable throughput gain.
//!
//! # What this bench measures
//!
//! Twelve cells total: 3 size tiers x 2 backends x 2 directions:
//!
//! - `scale/stdlib_write/{2GiB,10GiB,50GiB}`: streaming write via
//!   `BufWriter<File>` with 1 MiB buffer + fsync. Represents the path taken
//!   on non-Linux hosts and when io_uring is disabled.
//! - `scale/iouring_write/{2GiB,10GiB,50GiB}`: streaming write via
//!   `fast_io::IoUringWriter` (implements `std::io::Write`) + fsync.
//! - `scale/stdlib_read/{2GiB,10GiB,50GiB}`: streaming read via
//!   `BufReader<File>` with 1 MiB buffer.
//! - `scale/iouring_read/{2GiB,10GiB,50GiB}`: streaming read via
//!   `fast_io::IoUringReader` (implements `std::io::Read`).
//!
//! All cells stream in 1 MiB chunks to keep peak memory constant regardless
//! of file size. Throughput is reported in bytes/second for direct comparison.
//!
//! # Workload sizing rationale
//!
//! - **2 GiB** - exceeds typical L3 cache and OS readahead; minimal
//!   page-cache benefit on a second pass. Baseline for ring amortisation.
//! - **10 GiB** - exceeds RAM on many CI runners; forces sustained disk I/O.
//! - **50 GiB** - production-scale single-file transfer (VM images, database
//!   dumps); exercises sustained throughput where submission batching should
//!   dominate per-syscall overhead.
//!
//! # When to run
//!
//! Linux 5.6+ with `io_uring_setup(2)` reachable (no seccomp block). Gated
//! by `BENCH_LARGE=1` to prevent accidental invocation:
//!
//! ```sh
//! BENCH_LARGE=1 \
//! OC_RSYNC_BENCH_NVME_PATH=/mnt/nvme/scratch \
//!   cargo bench -p fast_io \
//!     --features iouring-data-writes,iouring-data-reads \
//!     --bench iouring_multi_gb_scale
//! ```
//!
//! Individual size tiers:
//! - `BENCH_LARGE=1` enables 2 GiB and 10 GiB cells.
//! - `BENCH_LARGE_50G=1` additionally enables the 50 GiB cell (very slow;
//!   requires a machine with sufficient disk space).
//!
//! `OC_RSYNC_BENCH_NVME_PATH` is optional. When set, scratch files are
//! created under that path (use a real NVMe mount for the headline number).
//! When unset, falls back to `TempDir` default location.
//!
//! Without `BENCH_LARGE=1` the bench prints a skip line and exits 0.
//!
//! # CI gating
//!
//! All measurement code is gated on `cfg(all(target_os = "linux",
//! feature = "iouring-data-writes", feature = "iouring-data-reads"))`.
//! Non-Linux and feature-off builds compile to a stub `main` that prints a
//! skip line. The `Cargo.toml` `[[bench]]` entry carries
//! `required-features = ["iouring-data-writes", "iouring-data-reads"]` so
//! the bench is excluded from builds that turn either feature off.

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
use std::fs::File;
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
use std::io::{BufReader, BufWriter, Read, Write};
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
use std::path::Path;

#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
use tempfile::TempDir;

#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
use fast_io::{FileWriter, IoUringConfig, IoUringReader, IoUringWriter};

/// Gate env var. Set to `1` to enable the 2 GiB and 10 GiB cells.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
const ENABLE_ENV: &str = "BENCH_LARGE";

/// Additional gate for the 50 GiB cell (extremely slow).
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
const ENABLE_50G_ENV: &str = "BENCH_LARGE_50G";

/// Optional path for scratch files (e.g., a real NVMe mount).
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
const NVME_PATH_ENV: &str = "OC_RSYNC_BENCH_NVME_PATH";

/// 2 GiB file size.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
const SIZE_2G: u64 = 2 * 1024 * 1024 * 1024;

/// 10 GiB file size.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
const SIZE_10G: u64 = 10 * 1024 * 1024 * 1024;

/// 50 GiB file size.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
const SIZE_50G: u64 = 50 * 1024 * 1024 * 1024;

/// Chunk size for streaming I/O. Matches the production 1 MiB staging
/// buffer granularity used by `IoUringDiskBatch` and `IoUringWriter`.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
const CHUNK_BYTES: usize = 1024 * 1024;

#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn bench_enabled() -> bool {
    matches!(env::var(ENABLE_ENV), Ok(v) if v == "1")
}

#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn bench_50g_enabled() -> bool {
    matches!(env::var(ENABLE_50G_ENV), Ok(v) if v == "1")
}

/// Returns the active size tiers based on env vars.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn active_sizes() -> Vec<(u64, &'static str)> {
    let mut sizes = vec![(SIZE_2G, "2GiB"), (SIZE_10G, "10GiB")];
    if bench_50g_enabled() {
        sizes.push((SIZE_50G, "50GiB"));
    }
    sizes
}

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

/// Populates a file of `size` bytes for read benchmarks. Uses stdlib
/// `BufWriter` to avoid measuring setup cost in the read cells.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn seed_file(path: &Path, size: u64) {
    let file = File::create(path).expect("seed_file create");
    let mut writer = BufWriter::with_capacity(CHUNK_BYTES, file);
    let chunk = vec![0xa5u8; CHUNK_BYTES];
    let mut remaining = size as usize;
    while remaining > 0 {
        let n = remaining.min(CHUNK_BYTES);
        writer.write_all(&chunk[..n]).expect("seed_file write");
        remaining -= n;
    }
    writer.flush().expect("seed_file flush");
    writer
        .into_inner()
        .expect("seed_file unwrap")
        .sync_all()
        .expect("seed_file sync");
}

/// Streaming write via stdlib `BufWriter<File>` + fsync.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn stdlib_write(path: &Path, size: u64) {
    let file = File::create(path).expect("stdlib_write create");
    let mut writer = BufWriter::with_capacity(CHUNK_BYTES, file);
    let chunk = vec![0xa5u8; CHUNK_BYTES];
    let mut remaining = size as usize;
    while remaining > 0 {
        let n = remaining.min(CHUNK_BYTES);
        writer.write_all(&chunk[..n]).expect("stdlib_write chunk");
        remaining -= n;
    }
    writer.flush().expect("stdlib_write flush");
    writer
        .into_inner()
        .expect("stdlib_write unwrap")
        .sync_all()
        .expect("stdlib_write sync");
}

/// Streaming write via `IoUringWriter` (implements `std::io::Write`) + fsync.
/// Uses the `FileWriter::sync()` trait method which submits an
/// `IORING_OP_FSYNC` SQE on the per-thread ring.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn iouring_write(path: &Path, size: u64) {
    let config = IoUringConfig::default();
    let mut writer = IoUringWriter::create(path, &config).expect("iouring_write create");
    let chunk = vec![0xa5u8; CHUNK_BYTES];
    let mut remaining = size as usize;
    while remaining > 0 {
        let n = remaining.min(CHUNK_BYTES);
        writer.write_all(&chunk[..n]).expect("iouring_write chunk");
        remaining -= n;
    }
    writer.sync().expect("iouring_write sync");
}

/// Streaming read via stdlib `BufReader<File>`.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn stdlib_read(path: &Path, expected: u64) {
    let file = File::open(path).expect("stdlib_read open");
    let mut reader = BufReader::with_capacity(CHUNK_BYTES, file);
    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut total: u64 = 0;
    loop {
        let n = reader.read(&mut buf).expect("stdlib_read");
        if n == 0 {
            break;
        }
        total += n as u64;
    }
    assert_eq!(total, expected, "stdlib_read short read");
}

/// Streaming read via `IoUringReader` (implements `std::io::Read`).
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn iouring_read(path: &Path, expected: u64) {
    let config = IoUringConfig::default();
    let mut reader = IoUringReader::open(path, &config).expect("iouring_read open");
    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut total: u64 = 0;
    loop {
        let n = reader.read(&mut buf).expect("iouring_read");
        if n == 0 {
            break;
        }
        total += n as u64;
    }
    assert_eq!(total, expected, "iouring_read short read");
}

/// Emits the standard skip line when `BENCH_LARGE` is unset.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn skip() {
    eprintln!(
        "Skipping iouring_multi_gb_scale: set BENCH_LARGE=1 on a Linux 5.6+ host \
         with iouring-data-writes + iouring-data-reads features to enable. \
         Set BENCH_LARGE_50G=1 to additionally enable the 50 GiB tier. \
         Optionally set OC_RSYNC_BENCH_NVME_PATH to point at a real NVMe mount."
    );
}

/// Write benchmark: stdlib vs io_uring streaming writes at 2/10/50 GiB.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn bench_scale_write(c: &mut Criterion) {
    if !bench_enabled() {
        skip();
        return;
    }

    let sizes = active_sizes();
    let mut group = c.benchmark_group("scale");
    group.sample_size(10);

    for &(size, label) in &sizes {
        group.throughput(Throughput::Bytes(size));

        group.bench_with_input(
            BenchmarkId::new("stdlib_write", label),
            &size,
            |b, &sz| {
                b.iter_with_setup(
                    || {
                        let dir = make_scratch_dir();
                        let path = dir.path().join("bench_file");
                        (dir, path)
                    },
                    |(_dir, path)| {
                        stdlib_write(&path, sz);
                    },
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("iouring_write", label),
            &size,
            |b, &sz| {
                b.iter_with_setup(
                    || {
                        let dir = make_scratch_dir();
                        let path = dir.path().join("bench_file");
                        (dir, path)
                    },
                    |(_dir, path)| {
                        iouring_write(&path, sz);
                    },
                );
            },
        );
    }

    group.finish();
}

/// Read benchmark: stdlib vs io_uring streaming reads at 2/10/50 GiB.
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
fn bench_scale_read(c: &mut Criterion) {
    if !bench_enabled() {
        return;
    }

    let sizes = active_sizes();
    let mut group = c.benchmark_group("scale");
    group.sample_size(10);

    for &(size, label) in &sizes {
        group.throughput(Throughput::Bytes(size));

        group.bench_with_input(
            BenchmarkId::new("stdlib_read", label),
            &size,
            |b, &sz| {
                b.iter_with_setup(
                    || {
                        let dir = make_scratch_dir();
                        let path = dir.path().join("bench_file");
                        seed_file(&path, sz);
                        (dir, path)
                    },
                    |(_dir, path)| {
                        stdlib_read(&path, sz);
                    },
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("iouring_read", label),
            &size,
            |b, &sz| {
                b.iter_with_setup(
                    || {
                        let dir = make_scratch_dir();
                        let path = dir.path().join("bench_file");
                        seed_file(&path, sz);
                        (dir, path)
                    },
                    |(_dir, path)| {
                        iouring_read(&path, sz);
                    },
                );
            },
        );
    }

    group.finish();
}

#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
criterion_group!(multi_gb_scale, bench_scale_write, bench_scale_read);
#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
criterion_main!(multi_gb_scale);

#[cfg(not(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
)))]
fn main() {
    eprintln!(
        "iouring_multi_gb_scale: skipped (Linux-only bench; requires both the \
         `iouring-data-writes` and `iouring-data-reads` features)"
    );
}
