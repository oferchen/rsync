//! NVMe data-path benchmark: stdlib write vs io_uring `WRITE_FIXED`.
//!
//! Tracking task: oc-rsync IUD-4 (#2364). Companion design:
//! `docs/design/iouring-receive-data-path.md` (#4349). Audit precedent:
//! `docs/audits/iouring-data-path-audit.md` confirmed that real receive
//! data already flows through io_uring on most paths; this bench
//! quantifies the headroom left for the `WRITE_FIXED` cutover that the
//! design doc plans behind the `iouring-data-writes` feature.
//!
//! # What this bench measures
//!
//! The two cells synthesise the per-file write pattern that the disk-
//! commit thread issues during a streaming receive
//! (`crates/transfer/src/disk_commit/process.rs:32-118`,
//! `crates/transfer/src/disk_commit/writer.rs:199-211`). Each iteration
//! creates `FILE_COUNT` files of `FILE_BYTES` bytes (10 x 1 GiB by
//! default, total 10 GiB on disk) and writes them sequentially through
//! the chosen path:
//!
//! - `stdlib_write`: today's `Writer::Buffered` fallback. `File::create`
//!   + `BufWriter` with the production 256 KiB buffer
//!   (`crates/transfer/src/disk_commit/writer.rs:71-122`), one 1 MiB
//!   chunk per `write_all` call, `flush` + `sync_all` at end-of-file to
//!   match the production end-of-file fsync. Represents the path taken
//!   today on every non-Linux host and on Linux when `io_uring` is
//!   disabled or the file is forced onto the buffered writer (sparse,
//!   append, or below the 64 KiB threshold from section 4.4 of the
//!   design doc).
//! - `iouring_write_fixed`: the proposed production path for files
//!   sized at least 1 MiB. `IORING_OP_WRITE_FIXED` against a `RegisteredBufferGroup`
//!   of 4 x 1 MiB page-aligned registered slots, one slot per concurrent
//!   in-flight SQE. Mirrors the `submit_write_fixed_batch` helper in
//!   `crates/fast_io/src/io_uring/registered_buffers/submit.rs:159-243`
//!   that the wired path will call once IUD-5 lands. An `IORING_OP_FSYNC`
//!   SQE is submitted at end-of-file to match the production
//!   `commit_file(do_fsync = true)` placement
//!   (`crates/fast_io/src/io_uring/disk_batch.rs:236-263`).
//!
//! Both cells write the same total bytes and run the same fsync
//! cadence, so the wall-time and MiB/s delta is attributable strictly to
//! the dispatch style (stdlib `write(2)` vs `IORING_OP_WRITE_FIXED`).
//! Throughput is reported in bytes/second so the two rows can be
//! compared directly without unit conversion.
//!
//! # Workload sizing
//!
//! `FILE_COUNT = 10` and `FILE_BYTES = 1 GiB` are chosen so the working
//! set (10 GiB) clears typical RAM page-cache residence on a CI runner
//! and forces real NVMe traffic on a bare-metal host. The chunk size of
//! `CHUNK_BYTES = 1 MiB` matches the `IoUringDiskBatch` staging buffer
//! granularity (`crates/fast_io/src/io_uring/disk_batch.rs:124-149`) and
//! the registered-buffer slot size that the production path will use
//! once IUD-5 wires `iouring-data-writes`.
//!
//! # When to run
//!
//! Linux 5.6+ with `io_uring_setup(2)` reachable (no seccomp block, no
//! container restriction). The bench is gated by an env var because each
//! iteration writes 10 GiB to disk:
//!
//! ```sh
//! OC_RSYNC_BENCH_NVME_DATA_PATH=1 \
//! OC_RSYNC_BENCH_NVME_PATH=/mnt/nvme/scratch \
//!   cargo bench -p fast_io --bench nvme_data_path
//! ```
//!
//! `OC_RSYNC_BENCH_NVME_PATH` is optional. When set, the bench creates
//! its scratch dirs under that path (use a real NVMe mount for the
//! headline number). When unset, the bench falls back to the default
//! `TempDir` location, which CI typically backs with a ramdisk so the
//! numbers reflect ring-lifecycle and submission cost rather than disk
//! bandwidth.
//!
//! Without `OC_RSYNC_BENCH_NVME_DATA_PATH=1` the bench prints a skip
//! line and exits 0, so it is safe to leave registered in `Cargo.toml`
//! and to invoke via `cargo bench -p fast_io` without picking up
//! multi-minute work by accident.
//!
//! # Syscall counting
//!
//! When `perf stat -e syscalls:sys_enter_write,syscalls:sys_enter_io_uring_enter`
//! is available, run the bench under `perf stat` to break down the
//! per-cell syscall mix. `perf` is intentionally not invoked from inside
//! the bench: Criterion already produces deterministic wall-time and
//! throughput rows, and adding a `perf` wrapper would force the bench
//! to inspect kernel privileges and `perf_event_paranoid` at runtime.
//! The recommended invocation is:
//!
//! ```sh
//! OC_RSYNC_BENCH_NVME_DATA_PATH=1 perf stat -e \
//!   syscalls:sys_enter_write,syscalls:sys_enter_io_uring_enter,\
//!   syscalls:sys_enter_fsync \
//!   cargo bench -p fast_io --bench nvme_data_path -- --profile-time 30
//! ```
//!
//! # What the numbers inform
//!
//! Outcome -> action on task #2364 (NVMe data-path gap):
//!
//! - `iouring_write_fixed` clears `stdlib_write` by at least 20% on the NVMe
//!   workload: promote `iouring-data-writes` rollout (IUD-8) and flip
//!   `OC_RSYNC_IOURING_DATA_WRITES` default to `auto`.
//! - Within +/- 10%: keep the feature flag opt-in. The two memcpy hops
//!   eliminated by `WRITE_FIXED` (section 1.3 of the design doc) do not
//!   pay for the registered-buffer pool complexity on this workload.
//! - `iouring_write_fixed` regresses vs `stdlib_write`: investigate
//!   submission backpressure (the bench drains every batch before
//!   issuing the next; a real receiver would have multiple files
//!   in-flight and could hide submission cost). Surfaces as input for
//!   IUD-5d (selector tuning).
//!
//! # CI gating
//!
//! All measurement code is gated on
//! `cfg(all(target_os = "linux", feature = "io_uring"))`; the macOS,
//! Windows, and feature-off builds compile to a stub `main` that prints
//! a skip line. The `Cargo.toml` `[[bench]]` entry does not declare
//! `required-features` because the stub `main` keeps the bench
//! compilable on every host.
//!
//! GitHub-hosted Windows and macOS runners cannot execute this bench.
//! When `IoUring::new` returns an error (typical inside locked-down
//! container runtimes) the bench prints a skip line rather than
//! crashing, so a no-op fallback row is the worst case.

#[cfg(all(target_os = "linux", feature = "io_uring"))]
use std::alloc::{Layout, alloc_zeroed, dealloc};
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use std::env;
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use std::fs::{File, OpenOptions};
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use std::io::{BufWriter, Write};
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use std::os::unix::io::AsRawFd;
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use std::path::PathBuf;
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use std::ptr::NonNull;

#[cfg(all(target_os = "linux", feature = "io_uring"))]
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use io_uring::{IoUring, opcode, types};
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use tempfile::TempDir;

/// Files written per iteration. Ten files at 1 GiB each gives 10 GiB
/// total per sample, large enough to exit page-cache residence on a CI
/// runner with 16 GiB RAM.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const FILE_COUNT: usize = 10;

/// Bytes per file. Matches the design-doc workload (large-file
/// streaming receive) and ensures the bench measures sustained NVMe
/// throughput rather than ring construction cost.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const FILE_BYTES: usize = 1024 * 1024 * 1024;

/// Chunk size per `write_all` / `IORING_OP_WRITE_FIXED` SQE. Matches
/// the production `IoUringDiskBatch` staging buffer granularity
/// (`crates/fast_io/src/io_uring/disk_batch.rs:124-149`) and the
/// registered-buffer slot size that `iouring-data-writes` will use.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const CHUNK_BYTES: usize = 1024 * 1024;

/// Buffered-writer capacity. Matches the production `ReusableBufWriter`
/// size in `crates/transfer/src/disk_commit/writer.rs:71-122` so the
/// `stdlib_write` cell reflects today's `Writer::Buffered` flush
/// cadence (one `write(2)` per 256 KiB of buffered data).
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const BUF_WRITER_CAPACITY: usize = 256 * 1024;

/// Submission queue entries. Sized larger than `REG_BUF_COUNT` so
/// `submit_and_wait` never blocks on submission backpressure rather
/// than kernel completion latency.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const SQ_ENTRIES: u32 = 16;

/// Registered-buffer slots. Four concurrent in-flight 1 MiB SQEs is
/// the design-doc default for the `iouring-data-writes` path and gives
/// the kernel enough parallelism to keep an NVMe queue depth filled
/// without exceeding the typical `RLIMIT_MEMLOCK` budget on a CI host.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const REG_BUF_COUNT: usize = 4;

/// Bench gate env var. Set to `1` to actually run the bench; otherwise
/// the harness prints a skip line and exits 0 so `cargo bench` is cheap.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const ENABLE_ENV: &str = "OC_RSYNC_BENCH_NVME_DATA_PATH";

/// Optional path env var. When set, the bench creates its scratch dirs
/// under this path so the workload exercises a real NVMe mount instead
/// of the default `TempDir` location (typically a ramdisk on CI).
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const NVME_PATH_ENV: &str = "OC_RSYNC_BENCH_NVME_PATH";

/// Returns `true` when the kernel accepts `io_uring_setup(2)` with the
/// registered-buffer count this bench needs.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn io_uring_usable() -> bool {
    IoUring::new(SQ_ENTRIES).is_ok()
}

/// Returns `true` when the bench should actually run. False when either
/// the env var is unset or the kernel rejects io_uring.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn bench_enabled() -> bool {
    matches!(env::var(ENABLE_ENV), Ok(v) if v == "1") && io_uring_usable()
}

/// Creates the per-iteration scratch directory. Honours
/// `OC_RSYNC_BENCH_NVME_PATH` so operators can point the bench at a
/// real NVMe mount; falls back to the default `TempDir` location when
/// unset.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn make_scratch_dir() -> TempDir {
    match env::var(NVME_PATH_ENV) {
        Ok(path) if !path.is_empty() => TempDir::new_in(path).expect("nvme tempdir"),
        _ => TempDir::new().expect("tempdir"),
    }
}

/// Pre-allocates destination paths and the shared 1 MiB payload buffer.
/// The payload is identical for every chunk so the bench measures
/// dispatch cost rather than data preparation cost.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn prepare_workload(dir: &TempDir) -> (Vec<PathBuf>, Vec<u8>) {
    let paths: Vec<PathBuf> = (0..FILE_COUNT)
        .map(|i| dir.path().join(format!("nvme_{i:02}")))
        .collect();
    let payload = vec![0xa5u8; CHUNK_BYTES];
    (paths, payload)
}

/// Owns a set of page-aligned heap buffers registered with the ring
/// via `IORING_REGISTER_BUFFERS`. Mirrors the production
/// `RegisteredBufferGroup` shape
/// (`crates/fast_io/src/io_uring/registered_buffers/registry.rs`) at
/// the minimum complexity needed for this bench.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
struct RegBufs {
    ptrs: Vec<NonNull<u8>>,
    layout: Layout,
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
impl RegBufs {
    fn new(count: usize, buf_size: usize) -> Self {
        let layout = Layout::from_size_align(buf_size, 4096).expect("layout");
        let mut ptrs = Vec::with_capacity(count);
        for _ in 0..count {
            // SAFETY: layout has non-zero size (buf_size > 0) and a
            // valid power-of-two alignment (4096). NonNull::new returns
            // None only on allocation failure, which we surface as a
            // panic since this is bench setup.
            let raw = unsafe { alloc_zeroed(layout) };
            ptrs.push(NonNull::new(raw).expect("alloc"));
        }
        Self { ptrs, layout }
    }

    fn iovecs(&self) -> Vec<libc::iovec> {
        self.ptrs
            .iter()
            .map(|p| libc::iovec {
                iov_base: p.as_ptr().cast(),
                iov_len: self.layout.size(),
            })
            .collect()
    }
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
impl Drop for RegBufs {
    fn drop(&mut self) {
        for p in self.ptrs.drain(..) {
            // SAFETY: each pointer was returned by alloc_zeroed with
            // the same layout we now pass to dealloc. The ring is
            // dropped before the RegBufs (Criterion drops captured
            // setup state in LIFO order), so the kernel no longer
            // references the pages by the time we free.
            unsafe { dealloc(p.as_ptr(), self.layout) };
        }
    }
}

/// Writes one file via the stdlib `BufWriter` path, mirroring today's
/// `Writer::Buffered` fallback.
///
/// # Panics
///
/// Panics if `File::create`, `write_all`, `flush`, or `sync_all`
/// fails. Bench-setup failures surface via `expect(..)` so the
/// regression aborts the sample rather than producing a silently
/// skewed number.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn stdlib_write_file(path: &PathBuf, payload: &[u8]) {
    let file = File::create(path).expect("stdlib create");
    let mut writer = BufWriter::with_capacity(BUF_WRITER_CAPACITY, file);
    let chunks = FILE_BYTES / payload.len();
    for _ in 0..chunks {
        writer.write_all(payload).expect("stdlib write_all");
    }
    writer.flush().expect("stdlib flush");
    let file = writer.into_inner().expect("stdlib into_inner");
    file.sync_all().expect("stdlib sync_all");
}

/// Writes one file via `IORING_OP_WRITE_FIXED` against the registered
/// buffer pool, then issues a final `IORING_OP_FSYNC`.
///
/// Mirrors the production `submit_write_fixed_batch` semantics
/// (`crates/fast_io/src/io_uring/registered_buffers/submit.rs:159-243`)
/// at the minimum complexity needed for this bench: copy the payload
/// into each registered slot, push one `WriteFixed` SQE per slot, drain
/// the matching CQE batch, advance the file offset, repeat. End-of-file
/// fsync uses `IORING_OP_FSYNC` to match the production
/// `IoUringDiskBatch::commit_file(do_fsync = true)` placement.
///
/// # Panics
///
/// Panics if `OpenOptions::open` rejects the path, if SQE submission
/// or `submit_and_wait` fails, or if a CQE reports a negative result
/// (kernel write error, typically `ENOSPC` on a full NVMe). Surfaced
/// via `expect(..)` so the regression aborts the sample rather than
/// producing a silently skewed number.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn write_fixed_file(ring: &mut IoUring, bufs: &RegBufs, path: &PathBuf, payload: &[u8]) {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .expect("write_fixed create");
    let fd = types::Fd(file.as_raw_fd());
    let chunk_size = payload.len();
    let total_chunks = FILE_BYTES / chunk_size;
    let mut chunk_idx = 0usize;

    while chunk_idx < total_chunks {
        let remaining = total_chunks - chunk_idx;
        let n = remaining.min(REG_BUF_COUNT) as u32;
        for i in 0..n as usize {
            let slot = i;
            // Copy the payload into the registered buffer. This matches
            // `submit_write_fixed_batch:188-192` where the helper stages
            // chunks into registered slots before submission.
            //
            // SAFETY: `slot` is < REG_BUF_COUNT and the registered
            // buffer at that slot is `CHUNK_BYTES` long, matching the
            // payload size we copy.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    payload.as_ptr(),
                    bufs.ptrs[slot].as_ptr(),
                    chunk_size,
                );
            }
            let file_offset = ((chunk_idx + i) * chunk_size) as u64;
            let entry = opcode::WriteFixed::new(
                fd,
                bufs.ptrs[slot].as_ptr(),
                chunk_size as u32,
                slot as u16,
            )
            .offset(file_offset)
            .build()
            .user_data(i as u64);
            // SAFETY: the registered buffer at `slot` contains valid
            // data and is pinned for the duration of submit_and_wait
            // below; the kernel dereferences the pointer only between
            // push and the matching CQE arrival, which is fully
            // contained within this round.
            unsafe {
                ring.submission()
                    .push(&entry)
                    .expect("write_fixed submission queue full");
            }
        }
        ring.submit_and_wait(n as usize)
            .expect("write_fixed submit_and_wait");
        let mut completed = 0u32;
        while completed < n {
            let cqe = ring.completion().next().expect("write_fixed missing CQE");
            let result = cqe.result();
            assert!(result >= 0, "write_fixed CQE error: {}", -result);
            assert_eq!(
                result as usize, chunk_size,
                "write_fixed short write: {result} < {chunk_size}"
            );
            completed += 1;
        }
        chunk_idx += n as usize;
    }

    // End-of-file fsync via IORING_OP_FSYNC, matching the production
    // commit_file(do_fsync = true) placement in
    // crates/fast_io/src/io_uring/disk_batch.rs:236-263.
    let fsync = opcode::Fsync::new(fd).build().user_data(u64::MAX);
    // SAFETY: `fd` refers to the file we just wrote and stays open for
    // the duration of submit_and_wait below.
    unsafe {
        ring.submission()
            .push(&fsync)
            .expect("fsync submission queue full");
    }
    ring.submit_and_wait(1).expect("fsync submit_and_wait");
    let cqe = ring.completion().next().expect("fsync missing CQE");
    let result = cqe.result();
    assert!(result >= 0, "fsync CQE error: {}", -result);
}

/// Runs the `stdlib_write` cell: establishes the baseline that today's
/// `Writer::Buffered` fallback would deliver on the same workload.
///
/// # Panics
///
/// Panics if bench setup or measurement fails: `TempDir::new` /
/// `TempDir::new_in` cannot create a scratch directory, or any
/// `File::create` / `write_all` / `flush` / `sync_all` call returns an
/// error. These are bench-setup failures and are surfaced via
/// `expect(..)` so a regression aborts the sample rather than
/// producing a silently skewed number.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn bench_stdlib_write(c: &mut Criterion) {
    if !bench_enabled() {
        eprintln!(
            "Skipping nvme_data_path::stdlib_write: set {ENABLE_ENV}=1 on a Linux 5.6+ host \
             with io_uring_setup(2) reachable to enable. Optionally set {NVME_PATH_ENV} to a \
             real NVMe mount to exercise sustained disk throughput."
        );
        return;
    }

    let mut group = c.benchmark_group("nvme_data_path");
    group.sample_size(10);
    group.throughput(Throughput::Bytes((FILE_COUNT * FILE_BYTES) as u64));

    group.bench_function("stdlib_write", |b| {
        b.iter_with_setup(
            || {
                let dir = make_scratch_dir();
                let (paths, payload) = prepare_workload(&dir);
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

/// Runs the `iouring_write_fixed` cell: the proposed production path
/// for files sized at least 1 MiB once `iouring-data-writes` is wired. Skips
/// cleanly when the host kernel rejects the bench gate.
///
/// # Panics
///
/// Panics if bench setup or measurement fails: `TempDir::new` /
/// `TempDir::new_in` cannot create a scratch directory, `IoUring::new`
/// rejects the request on a host that previously reported the ring as
/// usable, `register_buffers` fails, or `write_fixed_file` (which
/// forwards SQE submission, CQE, and fsync errors) fails. These are
/// bench-setup failures and are surfaced via `expect(..)` so a
/// regression aborts the sample rather than producing a silently
/// skewed number.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn bench_iouring_write_fixed(c: &mut Criterion) {
    if !bench_enabled() {
        eprintln!(
            "Skipping nvme_data_path::iouring_write_fixed: set {ENABLE_ENV}=1 on a Linux \
             5.6+ host with io_uring_setup(2) reachable to enable. Optionally set \
             {NVME_PATH_ENV} to a real NVMe mount to exercise sustained disk throughput."
        );
        return;
    }

    let mut group = c.benchmark_group("nvme_data_path");
    group.sample_size(10);
    group.throughput(Throughput::Bytes((FILE_COUNT * FILE_BYTES) as u64));

    group.bench_function("iouring_write_fixed", |b| {
        b.iter_with_setup(
            || {
                let dir = make_scratch_dir();
                let (paths, payload) = prepare_workload(&dir);
                let mut ring = IoUring::new(SQ_ENTRIES).expect("ring");
                let bufs = RegBufs::new(REG_BUF_COUNT, CHUNK_BYTES);
                let iovecs = bufs.iovecs();
                // SAFETY: iovecs reference page-aligned heap buffers
                // owned by `bufs` for the lifetime of the ring; the
                // ring is dropped before bufs by virtue of LIFO drop
                // order in the captured tuple.
                unsafe {
                    ring.submitter()
                        .register_buffers(&iovecs)
                        .expect("register_buffers");
                }
                (dir, paths, payload, ring, bufs)
            },
            |(dir, paths, payload, mut ring, bufs)| {
                for path in &paths {
                    write_fixed_file(&mut ring, &bufs, path, &payload);
                }
                drop(dir);
                drop(ring);
                drop(bufs);
            },
        );
    });

    group.finish();
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
criterion_group!(
    nvme_data_path,
    bench_stdlib_write,
    bench_iouring_write_fixed,
);
#[cfg(all(target_os = "linux", feature = "io_uring"))]
criterion_main!(nvme_data_path);

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
fn main() {
    eprintln!("nvme_data_path: skipped (Linux-only bench; requires the `io_uring` feature)");
}
