//! IOCP writer path vs std::fs baseline on the Windows write hot path.
//! Tracking issue: oc-rsync #1899.
//!
//! Synthesises the four file-write dispatch styles available on Windows so
//! the receiver thread can be evaluated against a true baseline:
//!
//! - `iocp_default`: [`IocpDiskBatch`] with [`IocpConfig::default`]
//!   (`concurrent_ops = 4`, 64 KiB buffer). Mirrors what
//!   `crates/fast_io/src/iocp/file_factory.rs::writer_from_file` hands the
//!   disk-commit thread when a caller does not override the config.
//! - `iocp_concurrent_ops_8`: [`IocpDiskBatch`] with `concurrent_ops = 8`.
//!   The only knob in [`IocpConfig`] that meaningfully changes wall-clock
//!   throughput is the number of overlapped `WriteFile` calls kept in
//!   flight (`disk_batch.rs::submit_write_batch`). This row isolates that
//!   effect.
//! - `stdfs`: baseline. [`std::fs::File::create`] + `write_all`. One
//!   synchronous `WriteFile` per call. No IOCP code involved.
//! - `bufwriter_64k`: [`std::io::BufWriter`] with a 64 KiB buffer over
//!   `std::fs::File`. Same syscall as `stdfs` but batched at the same
//!   buffer size the IOCP path uses, so the comparison isolates the
//!   IOCP overlapped submission from the buffering effect.
//!
//! Each cell writes 1000 files (4 KiB, 64 KiB, or 1 MiB) into a temp dir
//! and the dir is torn down between iterations. Throughput is reported in
//! elements per second so the four rows can be compared directly.
//!
//! # Hypothesis
//!
//! The IOCP path's overlapped submission should only pull ahead of a
//! buffered stdio writer when the payload exceeds the per-write buffer
//! (so multiple overlapped chunks fly in parallel) and the working set is
//! large enough that the kernel cannot fold every write into the page
//! cache. At 4 KiB payloads we expect IOCP to lose to `bufwriter_64k`
//! because the per-file completion-port setup is amortised across only a
//! single sub-chunk. At 1 MiB payloads with `concurrent_ops = 8` the
//! overlapped submission has room to pipeline and should pull ahead.
//!
//! # When to run
//!
//! Windows only with the `iocp` feature enabled (default). On Linux and
//! macOS the bench compiles to a stub `main` that prints a skip line so
//! `cargo bench -p fast_io` is cheap on those hosts.
//!
//! ```sh
//! cargo bench -p fast_io --bench iocp_vs_stdio
//! ```
//!
//! # What the numbers inform
//!
//! Outcome -> action for [#1899] (IOCP wired but never benchmarked):
//!
//! - IOCP clears `bufwriter_64k` by >= 20% on the 1 MiB cell:
//!   strengthens the case for keeping
//!   [`IocpDiskBatch`] on the receiver hot path and informs #1929 /
//!   #1930 (handle-source validation, error classification) as worth
//!   keeping rather than ripping out.
//! - Within +/- 10% across every cell: IOCP carries its own complexity
//!   tax without paying it back; revisit whether the disk-commit thread
//!   on Windows should fall back to `BufWriter<File>` by default and
//!   only opt into IOCP for the very-large-file path
//!   ([`IocpConfig::for_large_files`]).
//! - IOCP regresses on the 4 KiB cell vs `bufwriter_64k`: expected and
//!   acceptable; informs the [`IOCP_MIN_FILE_SIZE`] threshold in
//!   `crates/fast_io/src/iocp/config.rs` (today 64 KiB) and the
//!   automatic fallback in `writer_from_file`.
//!
//! # CI gating
//!
//! All measurement code is gated on `cfg(all(target_os = "windows",
//! feature = "iocp"))`; non-Windows builds compile to a stub `main` that
//! prints a skip line. The `Cargo.toml` `[[bench]]` entry does not pin
//! `required-features` because the `iocp` feature is part of `default`
//! and the gated body is a no-op when the feature is off.
//!
//! [#1899]: https://github.com/oferchen/oc-rsync/issues/1899
//! [`IocpDiskBatch`]: ../../src/iocp/disk_batch.rs
//! [`IocpConfig`]: ../../src/iocp/config.rs
//! [`IocpConfig::default`]: ../../src/iocp/config.rs
//! [`IocpConfig::for_large_files`]: ../../src/iocp/config.rs
//! [`IOCP_MIN_FILE_SIZE`]: ../../src/iocp/config.rs

#[cfg(all(target_os = "windows", feature = "iocp"))]
use std::fs::{File, OpenOptions};
#[cfg(all(target_os = "windows", feature = "iocp"))]
use std::io::{BufWriter, Write};
#[cfg(all(target_os = "windows", feature = "iocp"))]
use std::path::PathBuf;

#[cfg(all(target_os = "windows", feature = "iocp"))]
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
#[cfg(all(target_os = "windows", feature = "iocp"))]
use fast_io::iocp::{IocpConfig, IocpDiskBatch};
#[cfg(all(target_os = "windows", feature = "iocp"))]
use tempfile::TempDir;

/// Files per iteration. Picked to keep wall-clock per sample under a few
/// seconds even at the 1 MiB cell while still giving the IOCP setup
/// overhead room to amortise.
#[cfg(all(target_os = "windows", feature = "iocp"))]
const FILE_COUNT: usize = 1000;

/// Payload sizes. 4 KiB exercises the small-file path that the IOCP
/// factory is supposed to skip via [`IOCP_MIN_FILE_SIZE`]; 64 KiB matches
/// the default IOCP buffer; 1 MiB is the receiver-thread sweet spot for
/// overlapped pipelining.
#[cfg(all(target_os = "windows", feature = "iocp"))]
const PAYLOAD_SIZES: [usize; 3] = [4 * 1024, 64 * 1024, 1024 * 1024];

/// BufWriter buffer size: matches [`IocpConfig::default`]'s `buffer_size`
/// so the comparison isolates overlapped submission from buffering.
#[cfg(all(target_os = "windows", feature = "iocp"))]
const BUFWRITER_CAPACITY: usize = 64 * 1024;

/// Builds a deterministic pseudo-random payload of the requested size.
///
/// Random-looking content prevents the OS or filesystem from collapsing
/// the write into a zero-fill or sparse-region optimisation, which would
/// distort the comparison between dispatch styles.
#[cfg(all(target_os = "windows", feature = "iocp"))]
fn make_payload(size: usize) -> Vec<u8> {
    // Linear congruential generator: cheap, deterministic, and produces a
    // byte stream incompressible enough to defeat sparse-file folding on
    // NTFS. Constants from Numerical Recipes (`a = 1664525`, `c = 1013904223`).
    let mut state: u32 = 0xdead_beef;
    let mut out = Vec::with_capacity(size);
    while out.len() < size {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(size);
    out
}

/// Pre-allocates destination paths and the shared payload for one cell.
#[cfg(all(target_os = "windows", feature = "iocp"))]
fn prepare_workload(dir: &TempDir, payload_size: usize) -> (Vec<PathBuf>, Vec<u8>) {
    let paths: Vec<PathBuf> = (0..FILE_COUNT)
        .map(|i| dir.path().join(format!("f_{i:07}")))
        .collect();
    let payload = make_payload(payload_size);
    (paths, payload)
}

/// Writes every file through one [`IocpDiskBatch`] with the supplied
/// config. Mirrors how the receiver disk-commit thread drives the batch:
/// `begin_file` -> `write_data` -> `commit_file` per file.
#[cfg(all(target_os = "windows", feature = "iocp"))]
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
        // `false` for fsync: the stdio baseline does not fsync either, so
        // skipping it keeps the comparison apples-to-apples.
        let (_file, _bytes) = batch.commit_file(false).expect("commit_file");
    }
}

/// Writes every file through `File::create` + `write_all`. One synchronous
/// `WriteFile` per call.
#[cfg(all(target_os = "windows", feature = "iocp"))]
fn run_stdfs(paths: &[PathBuf], payload: &[u8]) {
    for path in paths {
        let mut file = File::create(path).expect("create");
        file.write_all(payload).expect("write_all");
    }
}

/// Writes every file through `BufWriter<File>` with a 64 KiB buffer.
/// Same syscall pattern as `run_stdfs` but batched to match the IOCP
/// buffer size, so the row isolates overlapped submission from buffering.
#[cfg(all(target_os = "windows", feature = "iocp"))]
fn run_bufwriter(paths: &[PathBuf], payload: &[u8]) {
    for path in paths {
        let file = File::create(path).expect("create");
        let mut writer = BufWriter::with_capacity(BUFWRITER_CAPACITY, file);
        writer.write_all(payload).expect("write_all");
        writer.flush().expect("flush");
    }
}

/// Drives the four-cell criterion group that compares IOCP submission,
/// IOCP with eight overlapped writes, std::fs, and `BufWriter<File>`.
///
/// # Panics
///
/// Panics if bench setup or measurement fails: `TempDir::new` cannot
/// create a scratch directory, `OpenOptions::open` / `File::create`
/// cannot create a destination file, `IocpDiskBatch::new` rejects the
/// supplied [`IocpConfig`], or any `begin_file` / `write_data` /
/// `commit_file` / `write_all` / `flush` call on the IOCP, std::fs, or
/// `BufWriter` path returns an error. These are bench-setup failures
/// and are reported via `expect(..)` so a regression surfaces as a hard
/// stop rather than a silently skewed sample.
#[cfg(all(target_os = "windows", feature = "iocp"))]
fn bench_iocp_vs_stdio(c: &mut Criterion) {
    let mut group = c.benchmark_group("iocp_vs_stdio");
    group.sample_size(10);

    for &payload_size in &PAYLOAD_SIZES {
        group.throughput(Throughput::Bytes((FILE_COUNT * payload_size) as u64));

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
                    concurrent_ops: 8,
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

        group.bench_function(BenchmarkId::new("stdfs", payload_size), |b| {
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

        group.bench_function(BenchmarkId::new("bufwriter_64k", payload_size), |b| {
            b.iter_with_setup(
                || {
                    let dir = TempDir::new().expect("tempdir");
                    let (paths, payload) = prepare_workload(&dir, payload_size);
                    (dir, paths, payload)
                },
                |(dir, paths, payload)| {
                    run_bufwriter(&paths, &payload);
                    drop(dir);
                },
            );
        });
    }

    group.finish();
}

#[cfg(all(target_os = "windows", feature = "iocp"))]
criterion_group!(iocp_vs_stdio, bench_iocp_vs_stdio);
#[cfg(all(target_os = "windows", feature = "iocp"))]
criterion_main!(iocp_vs_stdio);

#[cfg(not(all(target_os = "windows", feature = "iocp")))]
fn main() {
    eprintln!(
        "iocp_vs_stdio: skipped (Windows-only bench; requires the `iocp` feature, which is on by \
         default)"
    );
}
