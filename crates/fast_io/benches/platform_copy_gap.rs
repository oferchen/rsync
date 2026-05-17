//! Cross-platform synchronous copy-path gap. Tracking issue: oc-rsync #1386.
//!
//! The unified [`fast_io::DefaultPlatformCopy`] dispatcher picks the kernel-native
//! whole-file copy primitive at runtime:
//!
//! - Linux: `FICLONE` (CoW reflink on Btrfs/XFS) -> `copy_file_range` ->
//!   `std::fs::copy`.
//! - macOS: `clonefile` (APFS CoW) -> `fcopyfile` (kernel-accelerated copy) ->
//!   `std::fs::copy`.
//! - Windows: ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE` -> `CopyFileExW` (with
//!   `COPY_FILE_NO_BUFFERING` above 4 MiB) -> `std::fs::copy`.
//!
//! The three kernels cannot be cross-compared directly: a `clonefile` number on
//! an APFS Mac and a `copy_file_range` number on an ext4 Linux box reflect
//! different storage stacks, allocators, and schedulers. This bench produces a
//! per-host ratio that the maintainer can compare across hosts without ever
//! running all three stacks side-by-side.
//!
//! # Comparison method
//!
//! Run the bench on Linux, macOS, and Windows separately. Each host produces
//! two rows per `(payload_size, file_count)` cell:
//!
//! - `platform_copy` - whatever [`DefaultPlatformCopy::copy_file`] picked at
//!   runtime. The actual [`CopyMethod`] used per cell is recorded once during
//!   the warm-up file in the cell label (see eprintln annotations at start).
//! - `std_baseline` - `std::fs::copy`, the portable fallback.
//!
//! To compare `platform_copy` across hosts, divide each cell's `platform_copy`
//! throughput by that host's `std_baseline` throughput for the same cell, then
//! compare the ratios. The ratio strips the host-specific storage stack so the
//! residual delta reflects the kernel-fast-path itself.
//!
//! Example interpretation (1 MiB cell, 100 files):
//!
//! ```text
//! Linux:   platform_copy = 2.6x std_baseline   (chose Ficlone)
//! macOS:   platform_copy = 14.0x std_baseline  (chose Clonefile - O(1) CoW)
//! Windows: platform_copy = 1.1x std_baseline   (chose CopyFileEx)
//! => CoW wins are dominated by filesystem support (APFS, ReFS, Btrfs/XFS).
//!    On non-CoW filesystems the gap collapses to syscall efficiency only.
//! ```
//!
//! # Scope vs the kernel-async bench
//!
//! This bench measures the synchronous in-process whole-file copy primitive
//! that the local-copy executor uses for unchanged files and `-W`/whole-file
//! transfers. The kernel-async path (io_uring vs IOCP) is covered by
//! [`iocp_vs_iouring_matched`] (#1868); the two benches are intentionally
//! independent because they exercise different syscalls (`copy_file_range` and
//! `clonefile` here, `io_uring_setup` / `CreateIoCompletionPort` there).
//!
//! # Payload sizes and file counts
//!
//! The bench cells are sized so that no cell exceeds ~30 s wall-clock on a
//! typical CI runner. Total bytes per cell is bounded near 1 GiB even for the
//! largest payload:
//!
//! | Payload  | Files per cell | Total bytes |
//! |----------|----------------|-------------|
//! | 4 KiB    | 100            | 400 KiB     |
//! | 64 KiB   | 100            | 6.4 MiB     |
//! | 1 MiB    | 100            | 100 MiB     |
//! | 16 MiB   | 10             | 160 MiB     |
//! | 256 MiB  | 2              | 512 MiB     |
//!
//! Small-file cells (4 KiB, 64 KiB) stay at 100 files because per-iteration
//! syscall overhead dominates throughput - more files give a stabler mean.
//! Large-file cells drop the count because each per-iteration setup has to
//! materialise the full source file on disk and a 100x16 MiB or 100x256 MiB
//! cell would dominate wall-clock without changing the kernel-path signal.
//!
//! # When to run
//!
//! ```sh
//! # On every host (no env gates):
//! cargo bench -p fast_io --bench platform_copy_gap
//! ```
//!
//! Then collect the three Criterion JSON reports and compute the per-cell
//! `platform_copy / std_baseline` ratio per host before comparing.
//!
//! [`iocp_vs_iouring_matched`]: ./iocp_vs_iouring_matched.rs
//! [`DefaultPlatformCopy::copy_file`]: ../../src/platform_copy/mod.rs
//! [`CopyMethod`]: ../../src/platform_copy/types.rs

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tempfile::TempDir;

use fast_io::{CopyMethod, DefaultPlatformCopy, PlatformCopy};

/// Per-cell payload/file-count pairs. See module doc for the sizing rationale.
const CELLS: &[(&str, usize, usize)] = &[
    ("4KiB", 4 * 1024, 100),
    ("64KiB", 64 * 1024, 100),
    ("1MiB", 1024 * 1024, 100),
    ("16MiB", 16 * 1024 * 1024, 10),
    ("256MiB", 256 * 1024 * 1024, 2),
];

/// Deterministic 4 KiB pattern. Tiled across the requested payload size so the
/// byte stream is identical across hosts and Criterion runs.
const SEED_BYTES: usize = 4 * 1024;

/// Builds a deterministic payload of the requested size by tiling a 4 KiB
/// seed. Uses a linear congruential generator (Numerical Recipes constants)
/// so the seed is incompressible enough to defeat sparse-file folding on
/// every tested filesystem.
fn make_payload(size: usize) -> Vec<u8> {
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

/// Materialises `file_count` source files of `payload_size` bytes each in the
/// supplied temp dir. Each source file gets a unique name; destination paths
/// are returned alongside but not created (the copy step creates them).
fn prepare_cell(dir: &Path, payload_size: usize, file_count: usize) -> Vec<(PathBuf, PathBuf)> {
    let payload = make_payload(payload_size);
    (0..file_count)
        .map(|i| {
            let src = dir.join(format!("src_{i:05}.bin"));
            let dst = dir.join(format!("dst_{i:05}.bin"));
            let mut file = File::create(&src).expect("create source");
            file.write_all(&payload).expect("write source payload");
            file.flush().expect("flush source");
            (src, dst)
        })
        .collect()
}

/// Records which [`CopyMethod`] the dispatcher picks for one warm-up copy in
/// the given cell. Printed once per cell on stderr so the Criterion report
/// can be cross-referenced against the actual kernel path that ran.
fn annotate_cell_method(label: &str, payload_size: usize) {
    let dir = TempDir::new().expect("annotate tempdir");
    let src = dir.path().join("annotate_src.bin");
    let dst = dir.path().join("annotate_dst.bin");
    let payload = make_payload(payload_size);
    {
        let mut file = File::create(&src).expect("annotate src create");
        file.write_all(&payload).expect("annotate src write");
        file.flush().expect("annotate src flush");
    }

    let copier = DefaultPlatformCopy::new();
    match copier.copy_file(&src, &dst, payload_size as u64) {
        Ok(result) => {
            eprintln!(
                "platform_copy_gap: cell {label} ({payload_size} B) picked CopyMethod::{:?} \
                 ({})",
                result.method, result.method,
            );
            // Sanity assertion: only enabled on the platforms where we have a
            // stable expectation of what the dispatcher will pick. On every
            // host the call must succeed and report some method, which the
            // unwrap above already guarantees.
            assert!(matches!(
                result.method,
                CopyMethod::Ficlone
                    | CopyMethod::CopyFileRange
                    | CopyMethod::Clonefile
                    | CopyMethod::Copyfile
                    | CopyMethod::ReFsReflink
                    | CopyMethod::CopyFileEx
                    | CopyMethod::StandardCopy
            ));
        }
        Err(err) => {
            eprintln!("platform_copy_gap: cell {label} annotate copy failed: {err}");
        }
    }
}

/// Drives [`DefaultPlatformCopy::copy_file`] across every `(src, dst)` pair.
/// Each iteration removes the destination first so CoW backends that refuse to
/// overwrite (notably `clonefile`) take the fast path instead of falling back.
fn run_platform_copy(copier: &DefaultPlatformCopy, pairs: &[(PathBuf, PathBuf)]) {
    for (src, dst) in pairs {
        let _ = fs::remove_file(dst);
        let result = copier
            .copy_file(src, dst, 0)
            .expect("platform copy succeeds");
        debug_assert!(matches!(
            result.method,
            CopyMethod::Ficlone
                | CopyMethod::CopyFileRange
                | CopyMethod::Clonefile
                | CopyMethod::Copyfile
                | CopyMethod::ReFsReflink
                | CopyMethod::CopyFileEx
                | CopyMethod::StandardCopy
        ));
    }
}

/// Drives [`std::fs::copy`] across every `(src, dst)` pair. Reference row that
/// runs identically on every host so per-host ratios are comparable across
/// hosts.
fn run_std_baseline(pairs: &[(PathBuf, PathBuf)]) {
    for (src, dst) in pairs {
        let _ = fs::remove_file(dst);
        fs::copy(src, dst).expect("std::fs::copy succeeds");
    }
}

fn bench_platform_copy_gap(c: &mut Criterion) {
    let mut group = c.benchmark_group("platform_copy_gap");
    group.sample_size(10);

    let copier = DefaultPlatformCopy::new();

    for &(label, payload_size, file_count) in CELLS {
        let total_bytes = (payload_size * file_count) as u64;
        group.throughput(Throughput::Bytes(total_bytes));

        annotate_cell_method(label, payload_size);

        group.bench_with_input(
            BenchmarkId::new("platform_copy", label),
            &(payload_size, file_count),
            |b, &(payload_size, file_count)| {
                b.iter_with_setup(
                    || {
                        let dir = TempDir::new().expect("tempdir");
                        let pairs = prepare_cell(dir.path(), payload_size, file_count);
                        (dir, pairs)
                    },
                    |(dir, pairs)| {
                        run_platform_copy(&copier, &pairs);
                        drop(dir);
                    },
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("std_baseline", label),
            &(payload_size, file_count),
            |b, &(payload_size, file_count)| {
                b.iter_with_setup(
                    || {
                        let dir = TempDir::new().expect("tempdir");
                        let pairs = prepare_cell(dir.path(), payload_size, file_count);
                        (dir, pairs)
                    },
                    |(dir, pairs)| {
                        run_std_baseline(&pairs);
                        drop(dir);
                    },
                );
            },
        );
    }

    group.finish();
}

criterion_group!(platform_copy_gap, bench_platform_copy_gap);
criterion_main!(platform_copy_gap);
