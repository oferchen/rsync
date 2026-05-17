//! io_uring SQPOLL vs regular submission vs std::fs on a 100K small-file
//! workload. Tracking issue: oc-rsync #1626.
//!
//! Synthesises the three I/O dispatch styles available for the receiver
//! write path:
//!
//! - `stdfs`: baseline. `File::create` + `write_all`, one synchronous
//!   `write(2)` per file. No io_uring code involved.
//! - `iouring_regular`: io_uring with default submission. Each batch
//!   triggers a real `io_uring_enter(2)` syscall to hand SQEs to the
//!   kernel and wait for completion. Mirrors the lifecycle of
//!   `crates/fast_io/src/io_uring/config.rs::IoUringConfig::build_ring`
//!   when `sqpoll = false`.
//! - `iouring_sqpoll`: io_uring with `IORING_SETUP_SQPOLL`. A kernel
//!   polling thread reads SQEs straight out of shared memory, so the
//!   per-batch `io_uring_enter` syscall disappears entirely while the
//!   poll thread stays warm. Mirrors `IoUringConfig::build_ring` when
//!   `sqpoll = true` and the kernel grants the request (CAP_SYS_NICE on
//!   pre-5.13, unprivileged on 5.13+).
//!
//! Each group writes 100,000 files of 4 KiB into a temp dir and the dir
//! is torn down between iterations. Throughput is reported in elements
//! per second so the three rows can be compared directly.
//!
//! # Hypothesis
//!
//! SQPOLL beats regular submission at high submission rates because it
//! eliminates the per-batch syscall, but only if the kernel poll thread
//! stays busy. At low submission rates the SQPOLL kthread idles out
//! (defaults to 100 ms via `setup_sqpoll`) and the user task has to wake
//! it again via a single `io_uring_enter` with `IORING_ENTER_SQ_WAKEUP`,
//! which claws back much of the win. This bench picks 100K back-to-back
//! 4 KiB writes precisely to keep the poll thread hot end to end.
//!
//! # When to run
//!
//! Linux 5.6+ for regular io_uring; 5.13+ for unprivileged SQPOLL.
//! Before 5.13 the SQPOLL setup needs `CAP_SYS_NICE` or root, and the
//! `iouring_sqpoll` group will skip cleanly via the env-var gate below.
//!
//! ```sh
//! OC_RSYNC_BENCH_IOURING_RING=1 \
//! OC_RSYNC_BENCH_IOURING_SQPOLL=1 \
//!   cargo bench -p fast_io --bench iouring_sqpoll_vs_regular
//! ```
//!
//! The `iouring_sqpoll` group has its own env-var gate
//! (`OC_RSYNC_BENCH_IOURING_SQPOLL`) on top of the shared
//! `OC_RSYNC_BENCH_IOURING_RING` so a privileged container can opt in
//! to SQPOLL without forcing every CI runner to attempt it. Without the
//! gate the bench prints a skip line and exits 0 so
//! `cargo bench -p fast_io` is cheap on every other host.
//!
//! # What the numbers inform
//!
//! Outcome -> action:
//!
//! - SQPOLL clears regular submission by >= 25% on this 4 KiB workload:
//!   strengthens the case for keeping
//!   [`IoUringConfig::sqpoll`](../../src/io_uring/config.rs) opt-in but
//!   recommended for streaming receivers, and informs #2243 (per-file
//!   vs shared ring) since SQPOLL only pays off on a long-lived ring.
//! - Within +/- 10%: keep SQPOLL strictly opt-in and treat the
//!   defensive refusal from #2158
//!   (`docs/audits/io-uring-sqpoll-mmap-interaction.md`) as a low-cost
//!   safety net rather than a regression.
//! - SQPOLL regresses vs regular submission: investigate kthread
//!   wake-up cost; the poll thread may be losing its CPU between
//!   batches even on a tight workload. Surfaces as input for #2045
//!   (registered-buffer adaptive sizing) because longer batches keep
//!   the poll thread busier.
//!
//! # CI gating
//!
//! All measurement code is gated on `target_os = "linux"`; the macOS
//! and Windows builds compile to a stub `main` that prints a skip line.
//! The `Cargo.toml` `[[bench]]` entry also carries `required-features =
//! ["io_uring"]` so it is excluded from builds that turn the feature
//! off.
//!
//! GitHub-hosted Windows and macOS runners cannot execute this bench.
//! When `IoUring::new` returns an error (typical inside locked-down
//! container runtimes) the bench prints a skip line rather than
//! crashing, so a no-op fallback row is the worst case.

#[cfg(target_os = "linux")]
use std::env;
#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
#[cfg(target_os = "linux")]
use io_uring::{IoUring, opcode, types};
#[cfg(target_os = "linux")]
use tempfile::TempDir;

#[cfg(target_os = "linux")]
const FILE_COUNT: usize = 100_000;
#[cfg(target_os = "linux")]
const PAYLOAD_BYTES: usize = 4 * 1024;
#[cfg(target_os = "linux")]
const SQ_ENTRIES: u32 = 8;
/// SQPOLL kernel-thread idle timeout in milliseconds. Matches the
/// production default in `IoUringConfig::sqpoll_idle_ms` so the bench is
/// representative of what callers actually configure.
#[cfg(target_os = "linux")]
const SQPOLL_IDLE_MS: u32 = 100;

/// Shared env-var gate. Set to `1` to actually run any io_uring group.
#[cfg(target_os = "linux")]
const ENABLE_ENV: &str = "OC_RSYNC_BENCH_IOURING_RING";
/// SQPOLL-specific gate. Set to `1` (in addition to `ENABLE_ENV`) to run
/// the SQPOLL group. Kept separate because SQPOLL needs `CAP_SYS_NICE`
/// on pre-5.13 kernels.
#[cfg(target_os = "linux")]
const SQPOLL_ENV: &str = "OC_RSYNC_BENCH_IOURING_SQPOLL";

/// Returns `true` when the kernel accepts a plain `io_uring_setup(2)`.
#[cfg(target_os = "linux")]
fn io_uring_usable() -> bool {
    IoUring::new(SQ_ENTRIES).is_ok()
}

/// Returns `true` when the kernel accepts `io_uring_setup(2)` with
/// `IORING_SETUP_SQPOLL`. Without `CAP_SYS_NICE` on pre-5.13 kernels
/// this returns `false` and the SQPOLL group skips.
#[cfg(target_os = "linux")]
fn sqpoll_usable() -> bool {
    IoUring::builder()
        .setup_sqpoll(SQPOLL_IDLE_MS)
        .build(SQ_ENTRIES)
        .is_ok()
}

#[cfg(target_os = "linux")]
fn iouring_enabled() -> bool {
    matches!(env::var(ENABLE_ENV), Ok(v) if v == "1") && io_uring_usable()
}

#[cfg(target_os = "linux")]
fn sqpoll_enabled() -> bool {
    iouring_enabled() && matches!(env::var(SQPOLL_ENV), Ok(v) if v == "1") && sqpoll_usable()
}

/// Pre-allocates destination paths and the shared payload buffer.
#[cfg(target_os = "linux")]
fn prepare_workload(dir: &TempDir) -> (Vec<PathBuf>, Vec<u8>) {
    let paths: Vec<PathBuf> = (0..FILE_COUNT)
        .map(|i| dir.path().join(format!("f_{i:07}")))
        .collect();
    let payload = vec![0xa5u8; PAYLOAD_BYTES];
    (paths, payload)
}

/// Writes `payload` to `file` via a long-lived shared ring.
///
/// Used by both regular and SQPOLL groups; the difference is in how
/// `ring` was constructed (`IoUring::new` vs `IoUring::builder().
/// setup_sqpoll(..)`). The submission path here is identical, which is
/// the point: any throughput delta between the two groups is
/// attributable to submission style, not ring topology.
#[cfg(target_os = "linux")]
fn ring_write(ring: &mut IoUring, file: &File, payload: &[u8]) -> std::io::Result<()> {
    let fd = types::Fd(file.as_raw_fd());
    let entry = opcode::Write::new(fd, payload.as_ptr(), payload.len() as u32)
        .offset(0)
        .build()
        .user_data(0);
    // SAFETY: `payload` is borrowed for the duration of submit_and_wait
    // below; the kernel dereferences the pointer only between push and
    // the matching CQE arrival, which is fully contained here.
    unsafe {
        ring.submission()
            .push(&entry)
            .map_err(|_| std::io::Error::other("submission queue full"))?;
    }
    ring.submit_and_wait(1)?;
    let cqe = ring
        .completion()
        .next()
        .ok_or_else(|| std::io::Error::other("no completion"))?;
    let result = cqe.result();
    if result < 0 {
        return Err(std::io::Error::from_raw_os_error(-result));
    }
    if (result as usize) != payload.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WriteZero,
            "short write from ring",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[allow(clippy::missing_panics_doc)]
fn bench_stdfs(c: &mut Criterion) {
    let mut group = c.benchmark_group("iouring_sqpoll_vs_regular");
    group.sample_size(10);
    group.throughput(Throughput::Elements(FILE_COUNT as u64));

    group.bench_function("stdfs", |b| {
        b.iter_with_setup(
            || {
                let dir = TempDir::new().expect("tempdir");
                let (paths, payload) = prepare_workload(&dir);
                (dir, paths, payload)
            },
            |(dir, paths, payload)| {
                for path in &paths {
                    let mut file = File::create(path).expect("create");
                    file.write_all(&payload).expect("write_all");
                }
                drop(dir);
            },
        );
    });

    group.finish();
}

#[cfg(target_os = "linux")]
#[allow(clippy::missing_panics_doc)]
fn bench_iouring_regular(c: &mut Criterion) {
    if !iouring_enabled() {
        eprintln!(
            "Skipping iouring_sqpoll_vs_regular::iouring_regular: set {ENABLE_ENV}=1 on a \
             Linux 5.6+ host with io_uring_setup(2) reachable to enable."
        );
        return;
    }

    let mut group = c.benchmark_group("iouring_sqpoll_vs_regular");
    group.sample_size(10);
    group.throughput(Throughput::Elements(FILE_COUNT as u64));

    group.bench_function("iouring_regular", |b| {
        b.iter_with_setup(
            || {
                let dir = TempDir::new().expect("tempdir");
                let (paths, payload) = prepare_workload(&dir);
                let ring = IoUring::new(SQ_ENTRIES).expect("ring");
                (dir, paths, payload, ring)
            },
            |(dir, paths, payload, mut ring)| {
                for path in &paths {
                    let file = File::create(path).expect("create");
                    ring_write(&mut ring, &file, &payload).expect("regular write");
                }
                drop(dir);
                drop(ring);
            },
        );
    });

    group.finish();
}

#[cfg(target_os = "linux")]
#[allow(clippy::missing_panics_doc)]
fn bench_iouring_sqpoll(c: &mut Criterion) {
    if !sqpoll_enabled() {
        eprintln!(
            "Skipping iouring_sqpoll_vs_regular::iouring_sqpoll: set {ENABLE_ENV}=1 and \
             {SQPOLL_ENV}=1 on a Linux host where IORING_SETUP_SQPOLL is accepted \
             (5.13+ unprivileged, or CAP_SYS_NICE on 5.6-5.12)."
        );
        return;
    }

    let mut group = c.benchmark_group("iouring_sqpoll_vs_regular");
    group.sample_size(10);
    group.throughput(Throughput::Elements(FILE_COUNT as u64));

    group.bench_function("iouring_sqpoll", |b| {
        b.iter_with_setup(
            || {
                let dir = TempDir::new().expect("tempdir");
                let (paths, payload) = prepare_workload(&dir);
                let ring = IoUring::builder()
                    .setup_sqpoll(SQPOLL_IDLE_MS)
                    .build(SQ_ENTRIES)
                    .expect("sqpoll ring");
                (dir, paths, payload, ring)
            },
            |(dir, paths, payload, mut ring)| {
                for path in &paths {
                    let file = File::create(path).expect("create");
                    ring_write(&mut ring, &file, &payload).expect("sqpoll write");
                }
                drop(dir);
                drop(ring);
            },
        );
    });

    group.finish();
}

#[cfg(target_os = "linux")]
criterion_group!(
    iouring_sqpoll_vs_regular,
    bench_stdfs,
    bench_iouring_regular,
    bench_iouring_sqpoll,
);
#[cfg(target_os = "linux")]
criterion_main!(iouring_sqpoll_vs_regular);

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "iouring_sqpoll_vs_regular: skipped (Linux-only bench; requires io_uring_setup(2))"
    );
}
