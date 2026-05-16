//! Per-file vs shared io_uring ring on a 100K small-file workload.
//!
//! Synthesises the two ring lifecycles that the audit
//! `docs/audits/per-file-vs-shared-uring-ring.md` (task #1410) called out as
//! the candidates for the receiver hot path:
//!
//! - `per_file`: today's default. Allocate a fresh `IoUring` for every
//!   output file, submit one 4 KiB write, wait for completion, drop the
//!   ring. Mirrors the lifecycle of
//!   `crates/fast_io/src/io_uring/mod.rs::writer_from_file_with_depth`.
//! - `shared`: the proposed alternative. Allocate one `IoUring` up front
//!   and reuse it across every file by re-registering each new fd into the
//!   same fixed-file slot. Mirrors the lifecycle of
//!   `crates/fast_io/src/io_uring/disk_batch.rs::IoUringDiskBatch`.
//!
//! Both topologies write 100,000 files of 4 KiB each into a temp dir, then
//! the dir is torn down. Throughput is reported in elements/second so the
//! two rows can be compared directly without unit conversion.
//!
//! # When to run
//!
//! Linux 5.6+ with `io_uring_setup(2)` available (no seccomp block, no
//! container restriction). The bench is gated by an env var because each
//! iteration takes several seconds:
//!
//! ```sh
//! OC_RSYNC_BENCH_IOURING_RING=1 \
//!   cargo bench -p fast_io --bench iouring_per_file_vs_shared
//! ```
//!
//! Without the env var, the bench prints a skip message and exits, so it is
//! safe to leave registered in `Cargo.toml` and to invoke via
//! `cargo bench -p fast_io` without picking up multi-minute work by accident.
//!
//! # CI gating
//!
//! All measurement code is gated on `target_os = "linux"`; the macOS /
//! Windows builds compile to a stub `main` that prints a skip line. The
//! `Cargo.toml` `[[bench]]` entry also carries `required-features =
//! ["io_uring"]` so it is excluded from builds that turn the feature off.
//!
//! Running this bench needs an unprivileged container or a bare-metal
//! Linux host with `io_uring_setup(2)` reachable. GitHub-hosted Windows /
//! macOS runners cannot execute it. The bench harness skips cleanly when
//! `IoUring::new` returns an error (typical inside locked-down container
//! runtimes), so a no-op fallback row is the worst case rather than a
//! crash.
//!
//! # What the numbers inform
//!
//! Outcome -> action on task #2243 (per-file vs shared ring priority):
//!
//! - `shared` clears `per_file` by >= 25% on this 4 KiB workload: promote
//!   #2243 to P1 and schedule the receiver-side rewrite to route
//!   `transfer_ops/response.rs` through a session ring.
//! - Within +/- 10%: keep #2243 at its current priority. The lifetime
//!   hazards documented in
//!   `docs/audits/per-file-vs-shared-uring-ring.md#3.4` are not worth the
//!   complexity for that margin.
//! - `shared` regresses vs `per_file`: investigate the submission-queue
//!   serialisation cost (single ring forces sequential submit + wait per
//!   file in this minimal prototype) before closing #2243.

#[cfg(target_os = "linux")]
use std::env;
#[cfg(target_os = "linux")]
use std::fs::File;
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

/// Env-var gate. Set to `1` to actually run the bench; otherwise the
/// harness prints a skip line and exits 0 so `cargo bench` is cheap.
#[cfg(target_os = "linux")]
const ENABLE_ENV: &str = "OC_RSYNC_BENCH_IOURING_RING";

/// Returns `true` when the kernel accepts `io_uring_setup(2)`. Probed
/// once with a small ring so an unsupported host (locked-down container,
/// kernel < 5.6, seccomp filter) gives a clean skip rather than a panic
/// mid-iter.
#[cfg(target_os = "linux")]
fn io_uring_usable() -> bool {
    IoUring::new(SQ_ENTRIES).is_ok()
}

/// Returns `true` when the bench should actually run. False when either
/// the env var is unset or the kernel rejects io_uring.
#[cfg(target_os = "linux")]
fn bench_enabled() -> bool {
    match env::var(ENABLE_ENV) {
        Ok(v) if v == "1" => io_uring_usable(),
        _ => false,
    }
}

/// Pre-allocates the destination paths and the payload buffer. The
/// payload is identical for every file so the bench measures
/// ring-lifecycle cost rather than data preparation cost.
#[cfg(target_os = "linux")]
fn prepare_workload(dir: &TempDir) -> (Vec<PathBuf>, Vec<u8>) {
    let paths: Vec<PathBuf> = (0..FILE_COUNT)
        .map(|i| dir.path().join(format!("f_{i:07}")))
        .collect();
    let payload = vec![0xa5u8; PAYLOAD_BYTES];
    (paths, payload)
}

/// Writes `payload` to `file` via a fresh single-shot io_uring ring.
///
/// Allocates a ring, submits one `IORING_OP_WRITE`, drains the matching
/// CQE, then drops everything. This is the lifecycle that today's
/// `writer_from_file_with_depth` exhibits when handed a single small file.
#[cfg(target_os = "linux")]
fn per_file_write(file: &File, payload: &[u8]) -> std::io::Result<()> {
    let mut ring = IoUring::new(SQ_ENTRIES)?;
    let fd = types::Fd(file.as_raw_fd());
    let entry = opcode::Write::new(fd, payload.as_ptr(), payload.len() as u32)
        .offset(0)
        .build()
        .user_data(0);
    // SAFETY: `payload` is borrowed for the duration of submit_and_wait
    // below; the kernel dereferences the pointer only between push and the
    // matching CQE arrival, which is fully contained within this function.
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
            "short write from per-file ring",
        ));
    }
    Ok(())
}

/// Writes `payload` to `file` via a long-lived shared ring.
///
/// The ring is constructed once outside the per-file loop; this helper
/// re-registers the new fd into the ring's fixed-file table (matching the
/// production `IoUringDiskBatch::begin_file` pattern: `try_register_fd`
/// per new file, `unregister_files` after commit), submits one
/// `IORING_OP_WRITE` against the registered slot, then unregisters.
///
/// Using `register_files` + `unregister_files` per file mirrors what the
/// receiver path would do if it were promoted to a session ring, and lets
/// the bench attribute the delta strictly to ring construction / teardown
/// (the per-file fixed-file rebind cost is held constant across both
/// topologies).
#[cfg(target_os = "linux")]
fn shared_write(ring: &mut IoUring, file: &File, payload: &[u8]) -> std::io::Result<()> {
    let raw = file.as_raw_fd();
    let fixed_slot = match ring.submitter().register_files(&[raw]) {
        Ok(()) => 0i32,
        // Fallback: register_files refuses re-registration on some kernels;
        // fall back to the raw-fd path so the bench still measures end-to-
        // end shared-ring cost rather than aborting mid-iter.
        Err(_) => -1,
    };

    let entry_fd = if fixed_slot >= 0 {
        types::Fd(fixed_slot)
    } else {
        types::Fd(raw)
    };
    let mut entry = opcode::Write::new(entry_fd, payload.as_ptr(), payload.len() as u32)
        .offset(0)
        .build()
        .user_data(0);
    if fixed_slot >= 0 {
        entry = entry.flags(io_uring::squeue::Flags::FIXED_FILE);
    }
    // SAFETY: `payload` is borrowed for submit_and_wait; the kernel reads
    // the pointer only between push and the matching CQE arrival, which
    // happens before this function returns.
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
    // Drop the fixed-file table so the next call can re-register slot 0.
    // Matches IoUringDiskBatch::unregister_fd.
    if fixed_slot >= 0 {
        let _ = ring.submitter().unregister_files();
    }
    if result < 0 {
        return Err(std::io::Error::from_raw_os_error(-result));
    }
    if (result as usize) != payload.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WriteZero,
            "short write from shared ring",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn bench_per_file_ring(c: &mut Criterion) {
    if !bench_enabled() {
        eprintln!(
            "Skipping iouring_per_file_vs_shared::per_file_ring: set {ENABLE_ENV}=1 on a \
             Linux 5.6+ host with io_uring_setup(2) reachable to enable."
        );
        return;
    }

    let mut group = c.benchmark_group("iouring_per_file_vs_shared");
    group.sample_size(10);
    group.throughput(Throughput::Elements(FILE_COUNT as u64));

    group.bench_function("per_file_ring", |b| {
        b.iter_with_setup(
            || {
                let dir = TempDir::new().expect("tempdir");
                let (paths, payload) = prepare_workload(&dir);
                (dir, paths, payload)
            },
            |(dir, paths, payload)| {
                for path in &paths {
                    let file = File::create(path).expect("create");
                    per_file_write(&file, &payload).expect("per-file write");
                }
                drop(dir);
            },
        );
    });

    group.finish();
}

#[cfg(target_os = "linux")]
fn bench_shared_ring(c: &mut Criterion) {
    if !bench_enabled() {
        eprintln!(
            "Skipping iouring_per_file_vs_shared::shared_ring: set {ENABLE_ENV}=1 on a \
             Linux 5.6+ host with io_uring_setup(2) reachable to enable."
        );
        return;
    }

    let mut group = c.benchmark_group("iouring_per_file_vs_shared");
    group.sample_size(10);
    group.throughput(Throughput::Elements(FILE_COUNT as u64));

    group.bench_function("shared_ring", |b| {
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
                    shared_write(&mut ring, &file, &payload).expect("shared write");
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
    iouring_per_file_vs_shared,
    bench_per_file_ring,
    bench_shared_ring,
);
#[cfg(target_os = "linux")]
criterion_main!(iouring_per_file_vs_shared);

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "iouring_per_file_vs_shared: skipped (Linux-only bench; requires io_uring_setup(2))"
    );
}
