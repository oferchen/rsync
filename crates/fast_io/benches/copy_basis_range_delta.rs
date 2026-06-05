//! Criterion benchmark for `copy_basis_range` vs standard read+write in the
//! delta-apply COPY-token path (IUD-11 #2609).
//!
//! Measures the throughput gain from using `copy_file_range(2)` (Linux) or
//! `ReadFile`/`WriteFile` with `OVERLAPPED` offsets (Windows) instead of the
//! portable `pread`+`write` fallback when replaying COPY tokens during delta
//! reconstruction.
//!
//! # Benchmark groups
//!
//! 1. **sequential** - COPY tokens laid out in basis-offset order, simulating a
//!    file with contiguous matched blocks (best case for prefetching and
//!    kernel-side readahead).
//! 2. **random** - COPY tokens at pseudo-random offsets within the basis,
//!    simulating block reordering from a heavily-edited file.
//! 3. **overlapping** - COPY tokens that re-read the same basis region
//!    multiple times, simulating a file with duplicated content.
//!
//! Each group sweeps three basis file sizes: 1 MB, 100 MB, and 1 GB (the
//! 1 GB tier is gated behind `OC_RSYNC_BENCH_LARGE=1`).
//!
//! # What it compares
//!
//! - `copy_basis_range` - the IUD-10 zero-copy fast path
//!   (`fast_io::copy_basis_range`). On unsupported platforms this returns
//!   `Ok(0)` and the cell reports zero throughput, making the comparison
//!   visible rather than silently skipped.
//! - `pread_write` - portable baseline using `std::os::unix::fs::FileExt`
//!   (or `seek`+`read`+`write` on Windows) through a reusable buffer.
//!
//! # Run
//!
//! ```sh
//! cargo bench -p fast_io --bench copy_basis_range_delta
//! # With the 1 GB tier:
//! OC_RSYNC_BENCH_LARGE=1 cargo bench -p fast_io --bench copy_basis_range_delta
//! ```

use std::fs::{File, OpenOptions};
use std::hint::black_box;
use std::io::{self, Write};
#[cfg(not(unix))]
use std::io::{Seek, SeekFrom};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tempfile::TempDir;

/// Block size used for COPY tokens - matches upstream rsync's typical block
/// size for files in the 1-100 MB range (see `sum_sizes_sqroot()`).
const BLOCK_SIZE: usize = 8 * 1024;

/// Buffer size for the read+write baseline path.
const RW_BUF_SIZE: usize = 256 * 1024;

/// File sizes for the benchmark sweep.
fn file_sizes() -> Vec<(&'static str, usize)> {
    let mut sizes = vec![("1MB", 1024 * 1024), ("100MB", 100 * 1024 * 1024)];
    if std::env::var("OC_RSYNC_BENCH_LARGE").as_deref() == Ok("1") {
        sizes.push(("1GB", 1024 * 1024 * 1024));
    }
    sizes
}

/// Describes a single COPY token: `basis[offset..offset+len]` -> `dest[dest_offset..]`.
#[derive(Clone, Copy)]
struct CopyOp {
    basis_off: u64,
    dest_off: u64,
    len: usize,
}

/// Creates a basis file filled with a deterministic byte pattern.
fn create_basis(dir: &std::path::Path, size: usize) -> File {
    let path = dir.join("basis.bin");
    let mut f = File::create(&path).expect("create basis");
    let chunk: Vec<u8> = (0..BLOCK_SIZE).map(|i| (i % 251) as u8).collect();
    let mut remaining = size;
    while remaining > 0 {
        let n = remaining.min(BLOCK_SIZE);
        f.write_all(&chunk[..n]).expect("write basis");
        remaining -= n;
    }
    f.flush().expect("flush basis");
    drop(f);
    File::open(&path).expect("reopen basis")
}

/// Creates a pre-allocated destination file.
fn create_dest(dir: &std::path::Path, size: usize) -> File {
    let path = dir.join("dest.bin");
    let f = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .expect("create dest");
    f.set_len(size as u64).expect("set dest length");
    f
}

/// Generates sequential COPY ops covering the entire basis.
fn sequential_ops(basis_size: usize) -> Vec<CopyOp> {
    let block_count = basis_size / BLOCK_SIZE;
    (0..block_count)
        .map(|i| CopyOp {
            basis_off: (i * BLOCK_SIZE) as u64,
            dest_off: (i * BLOCK_SIZE) as u64,
            len: BLOCK_SIZE,
        })
        .collect()
}

/// Generates pseudo-random COPY ops sampling from different basis regions.
fn random_ops(basis_size: usize) -> Vec<CopyOp> {
    let block_count = basis_size / BLOCK_SIZE;
    let max_block = block_count.saturating_sub(1);
    (0..block_count)
        .map(|i| {
            // Simple LCG-style shuffle: deterministic, no rand dependency.
            let src_block = ((i.wrapping_mul(7919)).wrapping_add(104729)) % (max_block + 1);
            CopyOp {
                basis_off: (src_block * BLOCK_SIZE) as u64,
                dest_off: (i * BLOCK_SIZE) as u64,
                len: BLOCK_SIZE,
            }
        })
        .collect()
}

/// Generates overlapping COPY ops that re-read the first quarter of the basis.
fn overlapping_ops(basis_size: usize) -> Vec<CopyOp> {
    let block_count = basis_size / BLOCK_SIZE;
    let quarter_blocks = (block_count / 4).max(1);
    (0..block_count)
        .map(|i| {
            let src_block = i % quarter_blocks;
            CopyOp {
                basis_off: (src_block * BLOCK_SIZE) as u64,
                dest_off: (i * BLOCK_SIZE) as u64,
                len: BLOCK_SIZE,
            }
        })
        .collect()
}

/// Applies COPY ops using `fast_io::copy_basis_range` (the IUD-10 fast path).
fn apply_copy_basis_range(basis: &File, dest: &File, ops: &[CopyOp]) -> io::Result<u64> {
    let mut total: u64 = 0;
    for op in ops {
        let copied = fast_io::copy_basis_range(basis, op.basis_off, dest, op.dest_off, op.len)?;
        total += copied as u64;
    }
    Ok(total)
}

/// Applies COPY ops using portable pread+write (the baseline path).
///
/// On Unix uses `FileExt::read_exact_at` / `FileExt::write_all_at` for
/// positioned I/O without seek - matching the upstream `map_ptr`+`write` path.
/// On Windows falls back to seek+read+write.
#[cfg(unix)]
fn apply_pread_write(basis: &File, dest: &File, ops: &[CopyOp]) -> io::Result<u64> {
    use std::os::unix::fs::FileExt;

    let mut buf = vec![0u8; RW_BUF_SIZE];
    let mut total: u64 = 0;

    for op in ops {
        let mut remaining = op.len;
        let mut src_pos = op.basis_off;
        let mut dst_pos = op.dest_off;

        while remaining > 0 {
            let chunk = remaining.min(buf.len());
            basis.read_exact_at(&mut buf[..chunk], src_pos)?;
            dest.write_all_at(&buf[..chunk], dst_pos)?;
            src_pos += chunk as u64;
            dst_pos += chunk as u64;
            remaining -= chunk;
            total += chunk as u64;
        }
    }

    Ok(total)
}

/// Windows baseline: seek+read+write since `FileExt` is Unix-only.
///
/// `Read`, `Write`, and `Seek` are all implemented for `&File` in the
/// standard library, so positioned I/O works through shared references.
#[cfg(not(unix))]
fn apply_pread_write(basis: &File, dest: &File, ops: &[CopyOp]) -> io::Result<u64> {
    use std::io::Read;

    let mut buf = vec![0u8; RW_BUF_SIZE];
    let mut total: u64 = 0;
    let mut basis_ref = basis;
    let mut dest_ref = dest;

    for op in ops {
        let mut remaining = op.len;
        let mut src_pos = op.basis_off;
        let mut dst_pos = op.dest_off;

        while remaining > 0 {
            let chunk = remaining.min(buf.len());
            basis_ref.seek(SeekFrom::Start(src_pos))?;
            basis_ref.read_exact(&mut buf[..chunk])?;
            dest_ref.seek(SeekFrom::Start(dst_pos))?;
            dest_ref.write_all(&buf[..chunk])?;
            src_pos += chunk as u64;
            dst_pos += chunk as u64;
            remaining -= chunk;
            total += chunk as u64;
        }
    }

    Ok(total)
}

/// Runs one benchmark group for a given access pattern.
fn bench_pattern(c: &mut Criterion, group_name: &str, ops_fn: fn(usize) -> Vec<CopyOp>) {
    let mut group = c.benchmark_group(group_name);
    group.sample_size(10);

    for (label, size) in file_sizes() {
        let ops = ops_fn(size);
        let total_bytes: u64 = ops.iter().map(|o| o.len as u64).sum();
        group.throughput(Throughput::Bytes(total_bytes));

        // copy_basis_range (IUD-10 fast path)
        group.bench_with_input(
            BenchmarkId::new("copy_basis_range", label),
            &size,
            |b, &size| {
                let dir = TempDir::new().unwrap();
                let basis = create_basis(dir.path(), size);
                let dest = create_dest(dir.path(), size);
                b.iter(|| {
                    let copied = apply_copy_basis_range(&basis, &dest, &ops).unwrap();
                    black_box(copied)
                });
            },
        );

        // pread+write baseline
        group.bench_with_input(BenchmarkId::new("pread_write", label), &size, |b, &size| {
            let dir = TempDir::new().unwrap();
            let basis = create_basis(dir.path(), size);
            let dest = create_dest(dir.path(), size);
            b.iter(|| {
                let copied = apply_pread_write(&basis, &dest, &ops).unwrap();
                black_box(copied)
            });
        });
    }

    group.finish();
}

fn bench_sequential(c: &mut Criterion) {
    bench_pattern(c, "delta_copy_token/sequential", sequential_ops);
}

fn bench_random(c: &mut Criterion) {
    bench_pattern(c, "delta_copy_token/random", random_ops);
}

fn bench_overlapping(c: &mut Criterion) {
    bench_pattern(c, "delta_copy_token/overlapping", overlapping_ops);
}

/// Measures the per-op overhead of `copy_basis_range` vs `pread`+`write` at
/// the typical block size, independent of total file size. This isolates the
/// syscall dispatch cost from bulk throughput.
fn bench_single_block(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_copy_token/single_block");
    group.sample_size(100);

    let block_sizes: &[(&str, usize)] = &[
        ("4KB", 4 * 1024),
        ("8KB", 8 * 1024),
        ("64KB", 64 * 1024),
        ("256KB", 256 * 1024),
    ];

    for &(label, bsize) in block_sizes {
        group.throughput(Throughput::Bytes(bsize as u64));

        group.bench_with_input(
            BenchmarkId::new("copy_basis_range", label),
            &bsize,
            |b, &bsize| {
                let dir = TempDir::new().unwrap();
                let basis = create_basis(dir.path(), bsize);
                let dest = create_dest(dir.path(), bsize);
                b.iter(|| {
                    let copied = fast_io::copy_basis_range(&basis, 0, &dest, 0, bsize).unwrap();
                    black_box(copied)
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("pread_write", label),
            &bsize,
            |b, &bsize| {
                let dir = TempDir::new().unwrap();
                let basis = create_basis(dir.path(), bsize);
                let dest = create_dest(dir.path(), bsize);
                let op = CopyOp {
                    basis_off: 0,
                    dest_off: 0,
                    len: bsize,
                };
                b.iter(|| {
                    let copied = apply_pread_write(&basis, &dest, &[op]).unwrap();
                    black_box(copied)
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .measurement_time(std::time::Duration::from_secs(5));
    targets =
        bench_sequential,
        bench_random,
        bench_overlapping,
        bench_single_block
);

criterion_main!(benches);
