//! Per-operation threshold sweep, local filesystem versus NFS (#1555).
//!
//! # Why this exists
//!
//! oc-rsync ships several tunables whose defaults were measured on local
//! filesystems (ext4 / APFS / tmpfs). NFS has very different per-syscall
//! latency characteristics - a single `stat()` or `read()` can cost tens
//! to hundreds of microseconds round-trip instead of the sub-microsecond
//! VFS-cached cost on a local mount. The crossover point at which it pays
//! to parallelise (or to switch to a different copy primitive) can move
//! by an order of magnitude. This bench produces evidence so the team can
//! decide whether per-filesystem-class defaults are warranted.
//!
//! # Tunables this bench informs
//!
//! - `transfer::parallel_io::DEFAULT_STAT_THRESHOLD` (currently `64`).
//!   Sequential-vs-rayon crossover for batched `stat()` calls in the
//!   receiver quick-check path (`crates/transfer/src/parallel_io.rs:16`).
//!   The four bench cells (32 / 64 / 128 / 256) bracket the current
//!   default symmetrically.
//! - `transfer::parallel_io::DEFAULT_METADATA_THRESHOLD` (currently
//!   `64`) and `DEFAULT_DELETION_THRESHOLD` (currently `64`). These are
//!   the same shape of per-entry syscall workload as the stat sweep, so
//!   the stat-threshold result generalises.
//! - `fast_io::copy_file_range::COPY_FILE_RANGE_THRESHOLD` (currently
//!   `64 * 1024`). Crossover above which `copy_file_range` is expected
//!   to beat a plain read/write loop. Three cells (4K / 64K / 1M)
//!   bracket the current default with a one-decade radius on each side.
//!
//! # What it measures
//!
//! Two benchmark groups:
//!
//! 1. `parallel_stat` - batched `std::fs::metadata` over N pre-created
//!    files, swept across N in `{32, 64, 128, 256}`. Each cell runs
//!    sequentially and via `rayon::par_iter` so the per-filesystem
//!    crossover point can be read directly off the report. Throughput
//!    is reported in elements/sec.
//! 2. `copy_file_range` - file-to-file copy of a single file of size
//!    `{4 KiB, 64 KiB, 1 MiB}`, via both `std::io::copy` (read/write
//!    fallback baseline) and `fast_io::copy_file_contents` (which picks
//!    `copy_file_range` / `io_uring` as available). Throughput is
//!    reported in bytes/sec.
//!
//! # Filesystem gate
//!
//! Both groups always run against a local `tempfile::TempDir` so the
//! bench is useful on developer laptops and in CI without any extra
//! setup. If the environment variable `OC_RSYNC_BENCH_NFS_PATH` is set
//! to a writable directory on an NFS mount, the same cells re-run
//! against that path and the results are emitted under a second
//! `_nfs` group. When the var is unset (or the path is not writable),
//! the NFS half is silently skipped so the bench stays green.
//!
//! On Linux the bench also probes `statfs(2)` for `NFS_SUPER_MAGIC`
//! (`0x6969`) and logs a warning to stderr if `OC_RSYNC_BENCH_NFS_PATH`
//! points at a non-NFS filesystem. This is advisory only - the cells
//! still run, since the user may have a reason to compare against a
//! mount the probe cannot classify (FUSE-NFS bridges, overlay mounts).
//!
//! # Interpretation
//!
//! For each `(group, filesystem)` pair, the threshold that minimises
//! the wall-clock cost of the workload is the bench-suggested default
//! for that filesystem class. If the local and NFS winners agree, the
//! current static default is fine. If they diverge by more than ~25 %,
//! `parallel_io::Thresholds::auto_for_path` is the natural extension
//! point: detect the mount type once at config build time and pick the
//! per-class default.
//!
//! Run: `cargo bench -p engine --bench per_op_thresholds`
//! Run with NFS: `OC_RSYNC_BENCH_NFS_PATH=/mnt/nfs/scratch \
//!     cargo bench -p engine --bench per_op_thresholds`

#![cfg(unix)]

use std::env;
use std::fs::{self, File};
use std::hint::black_box;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rayon::prelude::*;
use tempfile::TempDir;

use fast_io::copy_file_range::copy_file_contents;

/// Batch sizes for the parallel-stat sweep. Brackets the current
/// `DEFAULT_STAT_THRESHOLD = 64` symmetrically on each side so the
/// crossover point is visible in the report.
const STAT_BATCH_SIZES: &[usize] = &[32, 64, 128, 256];

/// File sizes for the copy-file-range sweep. Brackets the current
/// `COPY_FILE_RANGE_THRESHOLD = 64 KiB` with a one-decade radius.
const COPY_SIZES: &[usize] = &[4 * 1024, 64 * 1024, 1024 * 1024];

/// Holds an opened working directory plus the `TempDir` (if any) that
/// owns it, so the directory is cleaned up when the holder is dropped.
struct WorkDir {
    path: PathBuf,
    _temp: Option<TempDir>,
}

impl WorkDir {
    fn local() -> io::Result<Self> {
        let temp = TempDir::new()?;
        Ok(Self {
            path: temp.path().to_path_buf(),
            _temp: Some(temp),
        })
    }

    fn nfs() -> Option<Self> {
        let root = env::var_os("OC_RSYNC_BENCH_NFS_PATH")?;
        let root = PathBuf::from(root);
        if !root.is_dir() {
            eprintln!(
                "per_op_thresholds: OC_RSYNC_BENCH_NFS_PATH={} is not a directory, skipping NFS cells",
                root.display(),
            );
            return None;
        }
        let scratch = root.join(format!("oc_rsync_bench_{}", std::process::id()));
        if let Err(err) = fs::create_dir_all(&scratch) {
            eprintln!(
                "per_op_thresholds: cannot create {}: {err}, skipping NFS cells",
                scratch.display(),
            );
            return None;
        }
        if !looks_like_nfs(&scratch) {
            eprintln!(
                "per_op_thresholds: warning: {} does not appear to be NFS (statfs probe)",
                scratch.display(),
            );
        }
        Some(Self {
            path: scratch,
            _temp: None,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        if self._temp.is_none() {
            // NFS scratch dir we created by hand - clean up explicitly.
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// Returns `true` if `path` lives on an NFS filesystem according to
/// `statfs(2)::f_type == NFS_SUPER_MAGIC (0x6969)`. Linux only - on
/// other Unices `statfs` does not expose a filesystem-class magic
/// number, so the probe conservatively returns `true` (treat as
/// "cannot rule out NFS", warn-only path).
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn looks_like_nfs(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    const NFS_SUPER_MAGIC: libc::__fsword_t = 0x6969;
    let Ok(cpath) = CString::new(path.as_os_str().as_bytes()) else {
        return true;
    };
    // SAFETY: `cpath` is a NUL-terminated C string owned for the call,
    // and `buf` is a stack-resident `libc::statfs` we own for the call
    // duration. Both pointers are valid and properly aligned. Standard
    // `libc::statfs` invocation idiom.
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(cpath.as_ptr(), &mut buf) };
    if rc != 0 {
        return true;
    }
    buf.f_type == NFS_SUPER_MAGIC
}

#[cfg(not(target_os = "linux"))]
fn looks_like_nfs(_path: &Path) -> bool {
    true
}

/// Creates `count` small files in `dir` and returns their paths. Each
/// file holds a 64-byte payload - large enough that `stat()` reports a
/// non-zero size, small enough that creation cost on NFS does not
/// dominate setup wall time.
fn create_stat_files(dir: &Path, count: usize) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::with_capacity(count);
    let payload = [0u8; 64];
    for i in 0..count {
        let path = dir.join(format!("stat_{i:05}.bin"));
        let mut f = File::create(&path)?;
        f.write_all(&payload)?;
        paths.push(path);
    }
    Ok(paths)
}

fn stat_sequential(paths: &[PathBuf]) -> u64 {
    paths
        .iter()
        .map(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .sum()
}

fn stat_parallel(paths: &[PathBuf]) -> u64 {
    paths
        .par_iter()
        .map(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .sum()
}

fn run_stat_group(c: &mut Criterion, label: &str, work_dir: &Path) {
    let mut group = c.benchmark_group(format!("parallel_stat_{label}"));
    for &count in STAT_BATCH_SIZES {
        let cell_dir = work_dir.join(format!("stat_cell_{count}"));
        if let Err(err) = fs::create_dir_all(&cell_dir) {
            eprintln!("per_op_thresholds: skipping {label}/stat/{count}: {err}");
            continue;
        }
        let paths = match create_stat_files(&cell_dir, count) {
            Ok(p) => p,
            Err(err) => {
                eprintln!("per_op_thresholds: skipping {label}/stat/{count}: {err}");
                continue;
            }
        };
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::new("sequential", count), &paths, |b, paths| {
            b.iter(|| black_box(stat_sequential(paths)))
        });
        group.bench_with_input(BenchmarkId::new("parallel", count), &paths, |b, paths| {
            b.iter(|| black_box(stat_parallel(paths)))
        });
    }
    group.finish();
}

fn create_copy_file(dir: &Path, size: usize, tag: &str) -> io::Result<PathBuf> {
    let path = dir.join(format!("copy_src_{tag}.bin"));
    let mut f = File::create(&path)?;
    let chunk = vec![0xA5u8; size.min(64 * 1024)];
    let mut remaining = size;
    while remaining > 0 {
        let n = remaining.min(chunk.len());
        f.write_all(&chunk[..n])?;
        remaining -= n;
    }
    f.sync_all()?;
    Ok(path)
}

fn copy_via_stdio(src: &Path, dst: &Path) -> io::Result<u64> {
    let mut s = File::open(src)?;
    let mut d = File::create(dst)?;
    io::copy(&mut s, &mut d)
}

fn copy_via_fast_io(src: &Path, dst: &Path, size: u64) -> io::Result<u64> {
    let s = File::open(src)?;
    let d = File::create(dst)?;
    copy_file_contents(&s, &d, size)
}

fn run_copy_group(c: &mut Criterion, label: &str, work_dir: &Path) {
    let mut group = c.benchmark_group(format!("copy_file_range_{label}"));
    for &size in COPY_SIZES {
        let cell_dir = work_dir.join(format!("copy_cell_{size}"));
        if let Err(err) = fs::create_dir_all(&cell_dir) {
            eprintln!("per_op_thresholds: skipping {label}/copy/{size}: {err}");
            continue;
        }
        let src = match create_copy_file(&cell_dir, size, "fixed") {
            Ok(p) => p,
            Err(err) => {
                eprintln!("per_op_thresholds: skipping {label}/copy/{size}: {err}");
                continue;
            }
        };
        let dst_stdio = cell_dir.join("dst_stdio.bin");
        let dst_fast = cell_dir.join("dst_fast.bin");
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(
            BenchmarkId::new("stdio_copy", size),
            &(src.clone(), dst_stdio.clone()),
            |b, (src, dst)| b.iter(|| black_box(copy_via_stdio(src, dst).unwrap())),
        );
        group.bench_with_input(
            BenchmarkId::new("fast_io", size),
            &(src.clone(), dst_fast.clone(), size as u64),
            |b, (src, dst, size)| b.iter(|| black_box(copy_via_fast_io(src, dst, *size).unwrap())),
        );
    }
    group.finish();
}

fn benches(c: &mut Criterion) {
    let local = match WorkDir::local() {
        Ok(d) => d,
        Err(err) => {
            eprintln!("per_op_thresholds: cannot create local tempdir: {err}");
            return;
        }
    };
    run_stat_group(c, "local", local.path());
    run_copy_group(c, "local", local.path());

    if let Some(nfs) = WorkDir::nfs() {
        run_stat_group(c, "nfs", nfs.path());
        run_copy_group(c, "nfs", nfs.path());
    } else {
        eprintln!(
            "per_op_thresholds: OC_RSYNC_BENCH_NFS_PATH not set; skipping NFS cells. \
             Set it to a writable NFS-mounted directory to compare.",
        );
    }
}

criterion_group!(per_op, benches);
criterion_main!(per_op);
