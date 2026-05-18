//! mmap vs `IORING_OP_READ_FIXED` + SQPOLL on sequential basis-file
//! reads. Bench harness only; the hardware run that picks Option 1/2/3
//! from the design doc is tracked separately as SMR-2.
//!
//! Companion design document:
//! `docs/design/mmap-vs-sqpoll-conflict-resolution.md`.
//!
//! # What this bench measures
//!
//! Six cells split across two dispatch styles and three basis sizes,
//! all driven by the same sequential-read access pattern that the
//! `DeltaApplicator` exercises when streaming long runs of COPY tokens
//! (`crates/transfer/src/delta_apply/applicator.rs`):
//!
//! - `mmap/sequential/4MiB`
//! - `mmap/sequential/64MiB`
//! - `mmap/sequential/1GiB`  (env-gated on `OC_RSYNC_BENCH_LARGE=1`)
//! - `read_fixed_sqpoll/sequential/4MiB`
//! - `read_fixed_sqpoll/sequential/64MiB`
//! - `read_fixed_sqpoll/sequential/1GiB`  (env-gated on `OC_RSYNC_BENCH_LARGE=1`)
//!
//! The `mmap` cells open the basis via `memmap2::MmapOptions` and walk
//! the mapping in 1 MiB strides, touching every byte through slice
//! iteration. The `read_fixed_sqpoll` cells open the same basis,
//! register a small ring of 1 MiB page-aligned heap buffers via
//! `IORING_REGISTER_BUFFERS`, and stream the file end-to-end via
//! `IORING_OP_READ_FIXED` on a ring built with `IORING_SETUP_SQPOLL`.
//! Both dispatch styles cover the entire basis on every iteration so
//! `Throughput::Bytes(size)` yields a directly comparable MB/s number.
//!
//! # Hypothesis
//!
//! mmap should win on small, fully-cacheable working sets where the
//! page cache holds the whole file warm after the first iteration:
//! dereferencing a populated page is a single load with no syscall.
//! `READ_FIXED` + SQPOLL should win as the working set grows past page
//! cache residence because the SQPOLL kthread reaps SQEs without the
//! per-batch `io_uring_enter(2)` and `READ_FIXED` skips
//! `iov_iter_get_pages` on each submission. The crossover point is
//! workload- and host-dependent; SMR-2 will run the bench on real
//! NVMe hardware and pick which of the three options the design doc
//! sketches.
//!
//! # When to run
//!
//! Linux 5.13+ for unprivileged SQPOLL; 5.6-5.12 requires
//! `CAP_SYS_NICE` and the SQPOLL cells skip cleanly when the kernel
//! rejects the request. The 4 MiB and 64 MiB cells are cheap enough to
//! run on any host; the 1 GiB cells need `OC_RSYNC_BENCH_LARGE=1` to
//! enable, both because the basis file occupies 1 GiB of scratch space
//! and because each iteration streams a full gigabyte of data.
//!
//! ```sh
//! # Mmap-only run (no kernel privileges required). Default sizes only.
//! cargo bench -p fast_io --bench mmap_vs_read_fixed_basis
//!
//! # Both dispatch styles, default sizes (4 MiB and 64 MiB).
//! OC_RSYNC_BENCH_IOURING_RING=1 \
//! OC_RSYNC_BENCH_IOURING_SQPOLL=1 \
//!   cargo bench -p fast_io --bench mmap_vs_read_fixed_basis
//!
//! # Full matrix including the 1 GiB cells.
//! OC_RSYNC_BENCH_IOURING_RING=1 \
//! OC_RSYNC_BENCH_IOURING_SQPOLL=1 \
//! OC_RSYNC_BENCH_LARGE=1 \
//!   cargo bench -p fast_io --bench mmap_vs_read_fixed_basis
//! ```
//!
//! Without the SQPOLL gates the read_fixed_sqpoll cells print a skip
//! line and exit 0. Without `OC_RSYNC_BENCH_LARGE=1` the 1 GiB cells
//! are skipped so a default `cargo bench` stays cheap.
//!
//! # perf stat recipes
//!
//! The point of the bench is to characterize the syscall and
//! page-fault profile of each dispatch style, not just the wall-clock
//! throughput. `perf stat` recipes for the syscall and fault counters
//! Linux exposes are the recommended way to consume the bench output
//! when picking Option 1/2/3 in SMR-2:
//!
//! ```sh
//! # Syscall counts (io_uring_enter, read, mmap, munmap). Useful for
//! # confirming the SQPOLL kthread really did eliminate
//! # io_uring_enter calls and for sizing the mmap setup cost.
//! perf stat -e \
//!   syscalls:sys_enter_io_uring_enter,syscalls:sys_enter_read,\
//! syscalls:sys_enter_mmap,syscalls:sys_enter_munmap \
//!   cargo bench -p fast_io --bench mmap_vs_read_fixed_basis
//!
//! # Page-fault profile. Major faults dominate when the mmap path is
//! # cold or when the working set exceeds page-cache residence;
//! # READ_FIXED should report ~0 faults because the kernel writes
//! # directly into the registered buffers.
//! perf stat -e \
//!   minor-faults,major-faults,page-faults,context-switches,cpu-migrations \
//!   cargo bench -p fast_io --bench mmap_vs_read_fixed_basis
//!
//! # CPU cycle and instruction breakdown. Mmap dereferences are a
//! # single load per byte; READ_FIXED amortises the cost across the
//! # ring submission + completion path. The IPC ratio is the cleanest
//! # cross-style comparison.
//! perf stat -e cycles,instructions,cache-references,cache-misses \
//!   cargo bench -p fast_io --bench mmap_vs_read_fixed_basis
//! ```
//!
//! # What the numbers inform
//!
//! Outcome -> action (see
//! `docs/design/mmap-vs-sqpoll-conflict-resolution.md` section
//! "Trigger conditions"):
//!
//! - `read_fixed_sqpoll` >= `mmap` at all sizes: adopt option 1 (drop
//!   mmap for SQPOLL-enabled basis reads, use READ_FIXED end-to-end).
//! - mmap wins below some threshold and `read_fixed_sqpoll` wins above:
//!   adopt option 2 (size-threshold heuristic) and pin the threshold to
//!   the 4 MiB / 64 MiB / 1 GiB crossover the bench surfaces.
//! - mmap wins at every size: adopt option 3 (keep mmap, accept the
//!   SQPOLL co-issue ban from #2158).
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
use std::path::Path;
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use std::ptr::NonNull;

#[cfg(all(target_os = "linux", feature = "io_uring"))]
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use io_uring::{IoUring, opcode, types};
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use memmap2::MmapOptions;
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use tempfile::TempDir;

/// One mebibyte. Used both as the registered-buffer slot size and as
/// the mmap stride, so the two dispatch styles read the basis file in
/// chunks of the same shape.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const CHUNK_BYTES: usize = 1024 * 1024;

/// Default basis sizes covered on every run.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const DEFAULT_SIZES: &[(u64, &str)] = &[(4 * 1024 * 1024, "4MiB"), (64 * 1024 * 1024, "64MiB")];

/// Large basis size gated by `OC_RSYNC_BENCH_LARGE=1`. Skipped by
/// default so a `cargo bench` from a laptop does not allocate a GiB
/// of scratch.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const LARGE_SIZE: (u64, &str) = (1024 * 1024 * 1024, "1GiB");

/// SQ entries. Sized comfortably above `REG_BUF_COUNT` so the ring
/// never backpressures on submission space.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const SQ_ENTRIES: u32 = 256;

/// SQPOLL kernel-thread idle timeout in milliseconds. Matches the
/// production default in `IoUringConfig::sqpoll_idle_ms` so the bench
/// is representative of what callers actually configure.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const SQPOLL_IDLE_MS: u32 = 100;

/// Number of registered buffers. One per concurrent in-flight read;
/// 8 matches the small-files preset in
/// `crates/fast_io/src/io_uring_common.rs`.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const REG_BUF_COUNT: usize = 8;

/// Shared env-var gate. Set to `1` to actually run any io_uring cell.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const ENABLE_ENV: &str = "OC_RSYNC_BENCH_IOURING_RING";

/// SQPOLL-specific gate. Set to `1` (in addition to `ENABLE_ENV`) to
/// run the SQPOLL cells. Kept separate so unprivileged hosts can still
/// run the mmap cells without attempting SQPOLL setup.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const SQPOLL_ENV: &str = "OC_RSYNC_BENCH_IOURING_SQPOLL";

/// Large-cell gate. Set to `1` to include the 1 GiB rows.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
const LARGE_ENV: &str = "OC_RSYNC_BENCH_LARGE";

/// Returns the basis sizes selected for this run.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn selected_sizes() -> Vec<(u64, &'static str)> {
    let mut sizes: Vec<(u64, &'static str)> = DEFAULT_SIZES.to_vec();
    if matches!(env::var(LARGE_ENV), Ok(v) if v == "1") {
        sizes.push(LARGE_SIZE);
    }
    sizes
}

/// Linear-congruential pseudo-random generator. Deterministic so the
/// basis content is the same across runs of the bench. Borrowed from
/// Numerical Recipes; full-period 2^64.
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

/// Writes a `size`-byte pseudo-random basis file at `path`. Streamed
/// via `CHUNK_BYTES` chunks so peak RSS stays bounded.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn build_basis(path: &Path, size: u64) {
    let mut f = File::create(path).expect("basis create");
    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut rng = Lcg::new(0xCAFE_F00D_DEAD_BEEFu64);
    let mut remaining = size;
    while remaining > 0 {
        let take = remaining.min(CHUNK_BYTES as u64) as usize;
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
/// this returns `false` and the SQPOLL cells skip.
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

/// Walks the mmap from offset 0 to `size` in `CHUNK_BYTES` strides,
/// summing the first and last byte of every chunk into a black-box
/// accumulator so the optimiser cannot elide the dereference.
///
/// # Panics
///
/// Panics if `MmapOptions::map` rejects the file (typical on a host
/// without enough virtual address space or where the file vanished
/// between basis creation and the read). Surfaced via `expect(..)` so
/// the regression aborts the sample rather than producing a silently
/// skewed number.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn run_mmap_sequential(file: &File, size: u64) -> u64 {
    // SAFETY: file is a freshly opened read-only fd referring to a
    // regular file on local storage; both preconditions of
    // MmapOptions::map.
    let mmap = unsafe { MmapOptions::new().map(file) }.expect("mmap");
    let mut acc: u64 = 0;
    let mut off: usize = 0;
    let end = size as usize;
    while off < end {
        let take = CHUNK_BYTES.min(end - off);
        let slice = &mmap[off..off + take];
        acc = acc.wrapping_add(slice[0] as u64);
        acc = acc.wrapping_add(slice[take - 1] as u64);
        off += take;
    }
    acc
}

/// Streams the basis end-to-end via `IORING_OP_READ_FIXED` on a SQPOLL
/// ring. Issues up to `REG_BUF_COUNT` reads in parallel, each filling
/// a registered 1 MiB buffer. Sums the first and last byte of every
/// completed chunk into a black-box accumulator.
///
/// # Panics
///
/// Panics if `submission().push` rejects an SQE (only possible if the
/// ring's submission queue overflows, which would indicate
/// `SQ_ENTRIES` is mis-sized for `REG_BUF_COUNT`), if
/// `submit_and_wait` returns an error (kernel rejected the submission
/// - the bench cannot proceed without completions), or if a CQE
/// reports a negative result (kernel read error - typically EIO on a
/// faulty storage device). Surfaced via `expect(..)` so the regression
/// aborts the sample rather than producing a silently skewed number.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn run_read_fixed_sequential(ring: &mut IoUring, file: &File, bufs: &RegBufs, size: u64) -> u64 {
    let fd = types::Fd(file.as_raw_fd());
    let mut acc: u64 = 0;
    let mut offset: u64 = 0;
    let end = size;

    // Build the full list of (offset, len) chunks then submit in
    // rounds of REG_BUF_COUNT to keep the ring saturated without
    // exceeding the number of available registered slots.
    let mut chunks: Vec<(u64, usize)> = Vec::new();
    while offset < end {
        let take = CHUNK_BYTES.min((end - offset) as usize);
        chunks.push((offset, take));
        offset += take as u64;
    }

    for round in chunks.chunks(REG_BUF_COUNT) {
        let n = round.len() as u32;
        for (i, &(off, len)) in round.iter().enumerate() {
            let slot = i % REG_BUF_COUNT;
            let entry =
                opcode::ReadFixed::new(fd, bufs.ptrs[slot].as_ptr(), len as u32, slot as u16)
                    .offset(off)
                    .build()
                    .user_data(i as u64);
            // SAFETY: the registered buffer at `slot` is valid for the
            // duration of submit_and_wait below; the kernel
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
            // SAFETY: the registered buffer at `slot` is alive for the
            // entirety of the round; we read the first and last byte
            // of the chunk the kernel just wrote.
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

/// Runs the `mmap/sequential/<size>` cells. The mmap path needs no
/// kernel privileges, so these cells always run on Linux.
///
/// # Panics
///
/// Panics if bench setup or measurement fails: `TempDir::new` cannot
/// create a scratch directory, `OpenOptions::open` rejects the basis
/// file, or `run_mmap_sequential` (which forwards mmap and
/// slice-index errors) fails. Surfaced via `expect(..)` so the
/// regression aborts the sample rather than producing a silently
/// skewed number.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn bench_mmap(c: &mut Criterion) {
    let mut group = c.benchmark_group("mmap_vs_read_fixed_basis/mmap/sequential");
    group.sample_size(10);

    for &(size, label) in selected_sizes().iter() {
        group.throughput(Throughput::Bytes(size));
        group.bench_with_input(BenchmarkId::from_parameter(label), &size, |b, &size| {
            let dir = TempDir::new().expect("tempdir");
            let basis = dir.path().join("basis.bin");
            build_basis(&basis, size);
            b.iter_with_setup(
                || {
                    OpenOptions::new()
                        .read(true)
                        .open(&basis)
                        .expect("basis open")
                },
                |file| {
                    std::hint::black_box(run_mmap_sequential(&file, size));
                },
            );
            drop(dir);
        });
    }

    group.finish();
}

/// Runs the `read_fixed_sqpoll/sequential/<size>` cells. Skips cleanly
/// when the host kernel rejects SQPOLL setup or the env-var gates are
/// unset.
///
/// # Panics
///
/// Panics if bench setup or measurement fails: `TempDir::new` cannot
/// create a scratch directory, the SQPOLL ring builder rejects the
/// request on a host that previously reported SQPOLL as usable, the
/// registered-buffer registration syscall fails, or
/// `run_read_fixed_sequential` (which forwards SQE submission and CQE
/// errors) fails. Surfaced via `expect(..)` so the regression aborts
/// the sample rather than producing a silently skewed number.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn bench_read_fixed_sqpoll(c: &mut Criterion) {
    if !sqpoll_enabled() {
        eprintln!(
            "Skipping mmap_vs_read_fixed_basis::read_fixed_sqpoll/sequential/*: set \
             {ENABLE_ENV}=1 and {SQPOLL_ENV}=1 on a Linux host where IORING_SETUP_SQPOLL \
             is accepted (5.13+ unprivileged, or CAP_SYS_NICE on 5.6-5.12)."
        );
        return;
    }

    let mut group = c.benchmark_group("mmap_vs_read_fixed_basis/read_fixed_sqpoll/sequential");
    group.sample_size(10);

    for &(size, label) in selected_sizes().iter() {
        group.throughput(Throughput::Bytes(size));
        group.bench_with_input(BenchmarkId::from_parameter(label), &size, |b, &size| {
            let dir = TempDir::new().expect("tempdir");
            let basis = dir.path().join("basis.bin");
            build_basis(&basis, size);
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
                    let bufs = RegBufs::new(REG_BUF_COUNT, CHUNK_BYTES);
                    let iovecs = bufs.iovecs();
                    // SAFETY: iovecs reference page-aligned heap
                    // buffers owned by `bufs` for the lifetime of the
                    // ring; the ring is dropped before bufs by virtue
                    // of LIFO drop order in the captured tuple.
                    unsafe {
                        ring.submitter()
                            .register_buffers(&iovecs)
                            .expect("register_buffers");
                    }
                    (file, ring, bufs)
                },
                |(file, mut ring, bufs)| {
                    std::hint::black_box(run_read_fixed_sequential(&mut ring, &file, &bufs, size));
                },
            );
            drop(dir);
        });
    }

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
    eprintln!("mmap_vs_read_fixed_basis: skipped (not Linux or io_uring feature disabled)");
}
