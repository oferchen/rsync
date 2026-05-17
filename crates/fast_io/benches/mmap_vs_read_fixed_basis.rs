//! mmap vs `IORING_OP_READ_FIXED` + SQPOLL on a 1 GiB basis-read workload.
//!
//! Tracking issue: oc-rsync follow-up to #2158 (SQPOLL defensive disable
//! when `mmap_basis_active`). Companion document:
//! `docs/design/mmap-vs-sqpoll-conflict-resolution.md`.
//!
//! # What this bench measures
//!
//! The two cells synthesise the basis-file read pattern that the
//! `DeltaApplicator` issues during delta application
//! (`crates/transfer/src/delta_apply/applicator.rs:161-176`). The basis
//! file is 1 GiB; each iteration issues a fixed number of random reads
//! of 1 KiB to 64 KiB at random offsets, mirroring how the delta
//! algorithm jumps around the basis file as it splices unchanged blocks
//! into the destination.
//!
//! - `mmap_basis_read`: opens the basis file via `memmap2::MmapOptions`
//!   and dereferences `mmap[off..off+len]` for each read. This is the
//!   current `MmapStrategy` path
//!   (`crates/transfer/src/map_file/mmap.rs:27, 38`) when the upstream
//!   selector lets mmap through.
//! - `read_fixed_basis_with_sqpoll`: opens the basis file and reads via
//!   `IORING_OP_READ_FIXED` against a `RegisteredBufferGroup`-equivalent
//!   set of page-aligned heap buffers, on a ring built with
//!   `IORING_SETUP_SQPOLL`. This is the option-1 candidate from the
//!   design doc.
//!
//! # Hypothesis
//!
//! mmap wins on warm-cache, random-access workloads because dereferencing
//! a populated page is a single load instruction with no syscall.
//! `READ_FIXED` + SQPOLL wins on cold-cache or large working-set
//! workloads because the SQPOLL kthread reaps SQEs without the
//! per-batch `io_uring_enter(2)` and `READ_FIXED` skips
//! `iov_iter_get_pages` on each submission. The crossover point is
//! workload-dependent; this bench is designed to surface it for the
//! 1 GiB basis-file case.
//!
//! # When to run
//!
//! Linux 5.13+ for unprivileged SQPOLL; 5.6-5.12 requires `CAP_SYS_NICE`
//! and the bench will skip cleanly when the kernel rejects the request.
//!
//! ```sh
//! OC_RSYNC_BENCH_IOURING_RING=1 \
//! OC_RSYNC_BENCH_IOURING_SQPOLL=1 \
//!   cargo bench -p fast_io --bench mmap_vs_read_fixed_basis
//! ```
//!
//! Without either gate the bench prints a skip line and exits 0.
//!
//! # What the numbers inform
//!
//! Outcome -> action (see `docs/design/mmap-vs-sqpoll-conflict-resolution.md`
//! section "Trigger conditions"):
//!
//! - `read_fixed_basis_with_sqpoll` >= `mmap_basis_read`: adopt option 1
//!   (drop mmap for SQPOLL-enabled basis reads, use READ_FIXED).
//! - Within +/- 10%: still option 1; SQPOLL syscall savings amortise.
//! - Regression > 10%: adopt option 2 (size-threshold heuristic) and
//!   tune the threshold from the per-chunk-size breakdown below.
//!
//! # CI gating
//!
//! All measurement code is gated on
//! `cfg(all(target_os = "linux", feature = "io_uring"))`; the macOS,
//! Windows, and feature-off builds compile to a stub `main` that prints
//! a skip line. The `Cargo.toml` `[[bench]]` entry does not declare
//! `required-features` because the stub `main` keeps the bench
//! compilable on every host.

#[cfg(all(target_os = "linux", feature = "io_uring"))]
use std::alloc::{Layout, alloc_zeroed, dealloc};
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use std::env;
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use std::fs::{File, OpenOptions};
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use std::io::Write;
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
use memmap2::MmapOptions;
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use tempfile::TempDir;

/// Basis-file size. Sized at 1 GiB to match the design-doc workload and
/// to ensure the working set exceeds typical page-cache residence on a
/// CI runner so cold-page costs are visible.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const BASIS_BYTES: u64 = 1024 * 1024 * 1024;

/// Number of reads per iteration. The design doc calls for 100
/// iterations; Criterion controls iteration count via `sample_size`,
/// and each iteration issues this many reads. Total bench wall time
/// stays reasonable under the configured `sample_size(10)` on an NVMe
/// host.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const READS_PER_ITER: usize = 100;

/// Minimum read size. Matches the lower bound of the delta-apply COPY
/// token range; smaller copies are folded into the LITERAL stream.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const MIN_READ: usize = 1024;

/// Maximum read size. Matches `IoUringConfig::buffer_size` for the
/// large-file preset (`crates/fast_io/src/io_uring_common.rs:151`),
/// so registered-buffer slots can hold any single read without
/// chunking.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const MAX_READ: usize = 64 * 1024;

/// SQ entries. Sized larger than the per-iteration submission count
/// chunk so the ring never blocks on `submit_and_wait` for
/// backpressure rather than for kernel completion latency.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const SQ_ENTRIES: u32 = 256;

/// SQPOLL kernel-thread idle timeout in milliseconds. Matches the
/// production default in `IoUringConfig::sqpoll_idle_ms` so the bench
/// is representative of what callers actually configure.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const SQPOLL_IDLE_MS: u32 = 100;

/// Number of registered buffers. One per concurrent in-flight read,
/// matching the small-files preset
/// (`crates/fast_io/src/io_uring_common.rs:174`).
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const REG_BUF_COUNT: usize = 8;

/// Shared env-var gate. Set to `1` to actually run any io_uring cell.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const ENABLE_ENV: &str = "OC_RSYNC_BENCH_IOURING_RING";

/// SQPOLL-specific gate. Set to `1` (in addition to `ENABLE_ENV`) to
/// run the SQPOLL cell. Kept separate so unprivileged hosts can still
/// run the mmap cell without attempting SQPOLL setup.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const SQPOLL_ENV: &str = "OC_RSYNC_BENCH_IOURING_SQPOLL";

/// Linear-congruential pseudo-random generator. Deterministic so the
/// two cells exercise the exact same offset / size pattern across
/// iterations. Borrowed from Numerical Recipes; full-period 2^64.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
struct Lcg(u64);

#[cfg(all(target_os = "linux", feature = "io_uring"))]
impl Lcg {
    fn new(seed: u64) -> Self {
        Self(
            seed.wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407),
        )
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }
}

/// Generates the random `(offset, len)` plan shared by both cells.
///
/// Both bench cells consume the exact same plan to make wall-time
/// deltas attributable to dispatch style rather than offset choice.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn build_plan(seed: u64) -> Vec<(u64, usize)> {
    let mut rng = Lcg::new(seed);
    let mut plan = Vec::with_capacity(READS_PER_ITER);
    for _ in 0..READS_PER_ITER {
        let len_range = (MAX_READ - MIN_READ) as u64;
        let len = MIN_READ + (rng.next_u64() % len_range) as usize;
        let max_off = BASIS_BYTES - len as u64;
        let off = rng.next_u64() % max_off;
        plan.push((off, len));
    }
    plan
}

/// Writes a 1 GiB pseudo-random basis file at `path`. Streamed via
/// 1 MiB chunks so peak RSS stays bounded.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn build_basis(path: &PathBuf) {
    let mut f = File::create(path).expect("basis create");
    let chunk_size = 1024 * 1024usize;
    let mut buf = vec![0u8; chunk_size];
    let mut rng = Lcg::new(0xCAFE_F00D_DEAD_BEEFu64);
    let mut remaining = BASIS_BYTES;
    while remaining > 0 {
        let take = remaining.min(chunk_size as u64) as usize;
        for b in buf[..take].iter_mut() {
            *b = (rng.next_u64() & 0xff) as u8;
        }
        f.write_all(&buf[..take]).expect("basis write");
        remaining -= take as u64;
    }
    f.sync_all().expect("basis sync");
}

/// Returns `true` when the kernel accepts a plain `io_uring_setup(2)`.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn io_uring_usable() -> bool {
    IoUring::new(SQ_ENTRIES).is_ok()
}

/// Returns `true` when the kernel accepts `io_uring_setup(2)` with
/// `IORING_SETUP_SQPOLL`. Without `CAP_SYS_NICE` on pre-5.13 kernels
/// this returns `false` and the SQPOLL cell skips.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn sqpoll_usable() -> bool {
    IoUring::<io_uring::squeue::Entry>::builder()
        .setup_sqpoll(SQPOLL_IDLE_MS)
        .build(SQ_ENTRIES)
        .is_ok()
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn iouring_enabled() -> bool {
    matches!(env::var(ENABLE_ENV), Ok(v) if v == "1") && io_uring_usable()
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn sqpoll_enabled() -> bool {
    iouring_enabled() && matches!(env::var(SQPOLL_ENV), Ok(v) if v == "1") && sqpoll_usable()
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

/// Issues `(off, len)` reads via the mmap path. Sums the first and
/// last byte of each window into a black-box accumulator so the
/// optimiser cannot elide the dereference.
///
/// # Panics
///
/// Panics if `MmapOptions::map` rejects the file (typical on a host
/// without enough virtual address space or where the file vanished
/// between basis creation and the read), or if a planned `(off, len)`
/// pair would index past the mapped region (cannot happen unless
/// `build_plan` is changed without updating `BASIS_BYTES`). Surfaced
/// via `expect(..)` so the regression aborts the sample rather than
/// producing a silently skewed number.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn run_mmap(file: &File, plan: &[(u64, usize)]) -> u64 {
    // SAFETY: file is a freshly opened read-only fd referring to a
    // regular file on local storage; both preconditions of
    // MmapOptions::map.
    let mmap = unsafe { MmapOptions::new().map(file) }.expect("mmap");
    let mut acc: u64 = 0;
    for &(off, len) in plan {
        let slice = &mmap[off as usize..off as usize + len];
        acc = acc.wrapping_add(slice[0] as u64);
        acc = acc.wrapping_add(slice[len - 1] as u64);
    }
    acc
}

/// Issues `(off, len)` reads via `IORING_OP_READ_FIXED` on a SQPOLL
/// ring. Sums the first and last byte of each completed buffer into a
/// black-box accumulator.
///
/// # Panics
///
/// Panics if `submission().push` rejects an SQE (only possible if the
/// ring's submission queue overflows, which would indicate
/// `SQ_ENTRIES` is mis-sized for `REG_BUF_COUNT`), if
/// `submit_and_wait` returns an error (kernel rejected the
/// submission - the bench cannot proceed without completions), or if
/// a CQE reports a negative result (kernel read error - typically EIO
/// on a faulty storage device). Surfaced via `expect(..)` so the
/// regression aborts the sample rather than producing a silently
/// skewed number.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn run_read_fixed(ring: &mut IoUring, file: &File, bufs: &RegBufs, plan: &[(u64, usize)]) -> u64 {
    let fd = types::Fd(file.as_raw_fd());
    let mut acc: u64 = 0;
    // Process the plan in rounds of REG_BUF_COUNT submissions; one
    // registered buffer per in-flight SQE.
    for round in plan.chunks(REG_BUF_COUNT) {
        let n = round.len() as u32;
        for (i, &(off, len)) in round.iter().enumerate() {
            let slot = i % REG_BUF_COUNT;
            let entry =
                opcode::ReadFixed::new(fd, bufs.ptrs[slot].as_ptr(), len as u32, slot as u16)
                    .offset(off)
                    .build()
                    .user_data(i as u64);
            // SAFETY: the registered buffer at `slot` is valid for
            // the duration of submit_and_wait below; the kernel
            // dereferences the pointer only between push and the
            // matching CQE arrival, which is fully contained in this
            // round.
            unsafe {
                ring.submission()
                    .push(&entry)
                    .expect("submission queue full");
            }
        }
        ring.submit_and_wait(n as usize).expect("submit_and_wait");
        let mut got = 0u32;
        while got < n {
            let cqe = ring.completion().next().expect("missing CQE");
            let res = cqe.result();
            assert!(res >= 0, "read_fixed CQE error: {}", -res);
            let idx = cqe.user_data() as usize;
            let slot = idx % REG_BUF_COUNT;
            let len = round[idx].1;
            // SAFETY: the registered buffer at `slot` is alive for
            // the entirety of the round; we read the first and last
            // byte of the chunk the kernel just wrote.
            unsafe {
                let p = bufs.ptrs[slot].as_ptr();
                acc = acc.wrapping_add(*p as u64);
                acc = acc.wrapping_add(*p.add(len - 1) as u64);
            }
            got += 1;
        }
    }
    acc
}

/// Runs the `mmap_basis_read` cell. Establishes the mmap baseline.
///
/// # Panics
///
/// Panics if bench setup or measurement fails: `TempDir::new` cannot
/// create a scratch directory, `OpenOptions::open` rejects the basis
/// file, or `run_mmap` (which forwards mmap and slice-index errors)
/// fails. Surfaced via `expect(..)` so the regression aborts the
/// sample rather than producing a silently skewed number.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn bench_mmap(c: &mut Criterion) {
    let mut group = c.benchmark_group("mmap_vs_read_fixed_basis");
    group.sample_size(10);
    group.throughput(Throughput::Elements(READS_PER_ITER as u64));

    group.bench_function("mmap_basis_read", |b| {
        let dir = TempDir::new().expect("tempdir");
        let basis = dir.path().join("basis.bin");
        build_basis(&basis);
        b.iter_with_setup(
            || {
                let file = OpenOptions::new()
                    .read(true)
                    .open(&basis)
                    .expect("basis open");
                let plan = build_plan(0xA5A5_5A5Au64);
                (file, plan)
            },
            |(file, plan)| {
                criterion::black_box(run_mmap(&file, &plan));
            },
        );
        drop(dir);
    });

    group.finish();
}

/// Runs the `read_fixed_basis_with_sqpoll` cell. Skips cleanly when
/// the host kernel rejects SQPOLL setup or the SQPOLL gate env var is
/// unset.
///
/// # Panics
///
/// Panics if bench setup or measurement fails: `TempDir::new` cannot
/// create a scratch directory, the SQPOLL ring builder rejects the
/// request on a host that previously reported SQPOLL as usable, the
/// registered-buffer registration syscall fails, or `run_read_fixed`
/// (which forwards SQE submission and CQE errors) fails. Surfaced via
/// `expect(..)` so the regression aborts the sample rather than
/// producing a silently skewed number.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn bench_read_fixed_sqpoll(c: &mut Criterion) {
    if !sqpoll_enabled() {
        eprintln!(
            "Skipping mmap_vs_read_fixed_basis::read_fixed_basis_with_sqpoll: set \
             {ENABLE_ENV}=1 and {SQPOLL_ENV}=1 on a Linux host where IORING_SETUP_SQPOLL \
             is accepted (5.13+ unprivileged, or CAP_SYS_NICE on 5.6-5.12)."
        );
        return;
    }

    let mut group = c.benchmark_group("mmap_vs_read_fixed_basis");
    group.sample_size(10);
    group.throughput(Throughput::Elements(READS_PER_ITER as u64));

    group.bench_function("read_fixed_basis_with_sqpoll", |b| {
        let dir = TempDir::new().expect("tempdir");
        let basis = dir.path().join("basis.bin");
        build_basis(&basis);
        b.iter_with_setup(
            || {
                let file = OpenOptions::new()
                    .read(true)
                    .open(&basis)
                    .expect("basis open");
                let mut ring = IoUring::<io_uring::squeue::Entry>::builder()
                    .setup_sqpoll(SQPOLL_IDLE_MS)
                    .build(SQ_ENTRIES)
                    .expect("sqpoll ring");
                let bufs = RegBufs::new(REG_BUF_COUNT, MAX_READ);
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
                let plan = build_plan(0xA5A5_5A5Au64);
                (file, ring, bufs, plan)
            },
            |(file, mut ring, bufs, plan)| {
                criterion::black_box(run_read_fixed(&mut ring, &file, &bufs, &plan));
            },
        );
        drop(dir);
    });

    group.finish();
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
criterion_group!(
    mmap_vs_read_fixed_basis,
    bench_mmap,
    bench_read_fixed_sqpoll,
);
#[cfg(all(target_os = "linux", feature = "io_uring"))]
criterion_main!(mmap_vs_read_fixed_basis);

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
fn main() {
    eprintln!(
        "mmap_vs_read_fixed_basis: skipped (Linux-only bench; requires the `io_uring` feature)"
    );
}
