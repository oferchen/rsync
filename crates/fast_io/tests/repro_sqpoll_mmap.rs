//! SQPOLL + mmap'd registered-buffer race reproducer (SQM-1.a).
//!
//! # Why this exists
//!
//! `crates/fast_io/src/io_uring/config.rs:336-373` defensively disables SQPOLL
//! whenever a basis file is mmap'd into the receive pipeline because the
//! SQPOLL kernel thread can race with userspace page-fault handling on the
//! shared mapping. The defensive disable costs roughly 10-15% throughput on
//! NVMe + large basis workloads (see
//! `docs/design/sqpoll-mmap-race-symptoms.md` and
//! `docs/audits/io_uring_sqpoll_mmap_pagefault.md`). To unlock SQM-2 (design
//! a safe workaround) we need a minimal program that surfaces the race
//! statistically on the kernel matrix.
//!
//! # What it does
//!
//! For each iteration:
//!
//! 1. Build a fresh `io_uring` with `IORING_SETUP_SQPOLL`. Skip the iteration
//!    cleanly if the kernel refuses SQPOLL (typically `EPERM` without
//!    `CAP_SYS_NICE`).
//! 2. mmap a 256 MiB scratch file `READ`-only with `MAP_PRIVATE`. The mapping
//!    is intentionally not pre-faulted - the SQPOLL kthread is expected to
//!    drive the page-fault path while servicing a registered-buffer SQE.
//! 3. Register the mmap'd region with the ring via `IORING_REGISTER_BUFFERS`.
//!    Registration is the point at which the kernel calls
//!    `get_user_pages_fast()`; one failure mode is `EFAULT` here.
//! 4. Submit an `IORING_OP_READ_FIXED` against the registered buffer that
//!    reads from a *separate* source file into the mmap'd region. The race
//!    surface is the kthread touching mmap pages whose backing PTEs have not
//!    been faulted in by the userspace task.
//! 5. Wait for the completion with a strict per-iteration timeout so the
//!    reproducer cannot hang even if the ring stalls. On Linux 5.11+
//!    `submit_with_args` accepts a `Timespec` that bounds the
//!    `io_uring_enter(GETEVENTS)` wait.
//! 6. Classify the completion: success / short read / `-EFAULT` /
//!    `-EAGAIN` / other negative errno / timeout. Print one status line per
//!    iteration so external tooling can collate kernel-version coverage.
//!
//! # Safety profile
//!
//! - Bounded: `ITERATIONS` is a small constant (16). No infinite loops.
//! - No fd leaks: every fd is owned by an `OwnedFd` or `File`. The mmap is
//!   wrapped in [`memmap2::MmapMut`] so unmapping happens via `Drop`.
//! - No hangs: every submit/wait uses a 5-second `Timespec` budget.
//! - Cleanup: temporary files live under [`tempfile::tempdir`] and are
//!   removed when the test exits, even on panic.
//!
//! # How to run
//!
//! ```sh
//! cargo nextest run -p fast_io --features io_uring \
//!     -E 'test(repro_sqpoll_mmap)' --no-capture
//! ```
//!
//! The reproducer is `#[ignore]` by default because it requires
//! `CAP_SYS_NICE` on most kernels to enable SQPOLL. Pass `--ignored` to run
//! it. See `docs/design/sqpoll-mmap-race-symptoms.md` for how to interpret
//! the per-iteration status lines and the kernel-version coverage matrix.

#![cfg(all(target_os = "linux", feature = "io_uring"))]

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::time::{Duration, Instant};

use io_uring::{IoUring, opcode, types};
use memmap2::MmapOptions;
use tempfile::tempdir;

/// Size of the mmap'd scratch region (256 MiB). Large enough to span many
/// page-cache entries so the kthread is statistically likely to touch a
/// non-resident page, small enough to fit in the typical CI tmpfs budget.
const SCRATCH_SIZE: usize = 256 * 1024 * 1024;

/// Size of the source file the registered-buffer read pulls bytes from.
/// One page per submission keeps the per-op work small while still requiring
/// the kthread to write into the mmap region (which is the trigger for the
/// page-fault race).
const READ_LEN: usize = 4096;

/// Number of (build-ring + submit + reap) cycles to surface the race
/// statistically. Each iteration is independent - a fresh ring and a fresh
/// registration - so transient kernel state cannot mask the symptom across
/// iterations.
const ITERATIONS: usize = 16;

/// Per-iteration timeout for `io_uring_enter(GETEVENTS)`. The reproducer
/// MUST NOT hang even when the kernel stalls, so this budget is strict.
/// Five seconds is generous for a single 4 KiB read and tight enough that
/// a hung iteration shows up immediately.
const ITER_TIMEOUT: Duration = Duration::from_secs(5);

/// `user_data` tag on the read SQE; arbitrary but distinct so the CQE can
/// be matched without ambiguity.
const READ_USER_DATA: u64 = 0xC0FFEE;

/// Number of SQ entries. Eight is more than enough for one read per
/// iteration and keeps the ring small enough that SQPOLL setup succeeds on
/// constrained CI workers.
const SQ_ENTRIES: u32 = 8;

/// SQPOLL idle window. Short enough that the kthread parks quickly on
/// inactivity (no busy CPU in CI), long enough that a single iteration's
/// submit + reap never races the kthread sleep.
const SQPOLL_IDLE_MS: u32 = 100;

#[test]
#[ignore = "requires CAP_SYS_NICE for SQPOLL; run with --ignored on instrumented kernels"]
fn repro_sqpoll_mmap_race() {
    println!("repro_sqpoll_mmap: {ITERATIONS} iterations, scratch={SCRATCH_SIZE}B, read={READ_LEN}B, timeout={:?}", ITER_TIMEOUT);

    let dir = tempdir().expect("tempdir");
    let scratch_path = dir.path().join("scratch_mmap.bin");
    let source_path = dir.path().join("source.bin");

    {
        let mut f = File::create(&scratch_path).expect("create scratch");
        f.set_len(SCRATCH_SIZE as u64).expect("set_len scratch");
        f.sync_all().expect("sync_all scratch");
    }
    {
        let mut f = File::create(&source_path).expect("create source");
        let payload: Vec<u8> = (0..READ_LEN).map(|i| (i % 251) as u8).collect();
        f.write_all(&payload).expect("write source payload");
        f.sync_all().expect("sync_all source");
    }

    let mut tally = StatusTally::default();
    for iter in 0..ITERATIONS {
        let outcome = run_one_iteration(iter, &scratch_path, &source_path);
        println!("repro_sqpoll_mmap iter={iter:02} status={outcome}");
        tally.record(&outcome);
        if matches!(outcome, IterStatus::SqpollUnavailable) {
            println!(
                "repro_sqpoll_mmap: SQPOLL unavailable on this kernel / privilege \
                 level; remaining iterations would be redundant"
            );
            break;
        }
    }

    println!("repro_sqpoll_mmap summary: {tally}");
}

/// Result of a single reproducer iteration. Each variant maps to one cell in
/// the failure-modes table in `docs/design/sqpoll-mmap-race-symptoms.md`.
#[derive(Debug)]
enum IterStatus {
    Ok { bytes: usize, elapsed: Duration },
    Short { bytes: usize },
    EFault,
    EAgain,
    NegativeErrno(i32),
    Timeout,
    SqpollUnavailable,
    RegisterFailed(std::io::Error),
    SubmitFailed(std::io::Error),
}

impl std::fmt::Display for IterStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IterStatus::Ok { bytes, elapsed } => write!(f, "ok bytes={bytes} elapsed={elapsed:?}"),
            IterStatus::Short { bytes } => write!(f, "short bytes={bytes}"),
            IterStatus::EFault => write!(f, "efault"),
            IterStatus::EAgain => write!(f, "eagain"),
            IterStatus::NegativeErrno(e) => write!(f, "errno={e}"),
            IterStatus::Timeout => write!(f, "timeout"),
            IterStatus::SqpollUnavailable => write!(f, "sqpoll-unavailable"),
            IterStatus::RegisterFailed(e) => write!(f, "register-failed: {e}"),
            IterStatus::SubmitFailed(e) => write!(f, "submit-failed: {e}"),
        }
    }
}

#[derive(Default)]
struct StatusTally {
    ok: usize,
    short: usize,
    efault: usize,
    eagain: usize,
    other_errno: usize,
    timeout: usize,
    sqpoll_unavailable: usize,
    register_failed: usize,
    submit_failed: usize,
}

impl StatusTally {
    fn record(&mut self, status: &IterStatus) {
        match status {
            IterStatus::Ok { .. } => self.ok += 1,
            IterStatus::Short { .. } => self.short += 1,
            IterStatus::EFault => self.efault += 1,
            IterStatus::EAgain => self.eagain += 1,
            IterStatus::NegativeErrno(_) => self.other_errno += 1,
            IterStatus::Timeout => self.timeout += 1,
            IterStatus::SqpollUnavailable => self.sqpoll_unavailable += 1,
            IterStatus::RegisterFailed(_) => self.register_failed += 1,
            IterStatus::SubmitFailed(_) => self.submit_failed += 1,
        }
    }
}

impl std::fmt::Display for StatusTally {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ok={} short={} efault={} eagain={} other_errno={} timeout={} \
             sqpoll_unavailable={} register_failed={} submit_failed={}",
            self.ok,
            self.short,
            self.efault,
            self.eagain,
            self.other_errno,
            self.timeout,
            self.sqpoll_unavailable,
            self.register_failed,
            self.submit_failed,
        )
    }
}

fn run_one_iteration(
    iter: usize,
    scratch_path: &std::path::Path,
    source_path: &std::path::Path,
) -> IterStatus {
    let mut ring = match IoUring::builder()
        .setup_sqpoll(SQPOLL_IDLE_MS)
        .build(SQ_ENTRIES)
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("repro_sqpoll_mmap iter={iter:02}: SQPOLL build failed: {e}");
            return IterStatus::SqpollUnavailable;
        }
    };

    let scratch = match OpenOptions::new().read(true).write(true).open(scratch_path) {
        Ok(f) => f,
        Err(e) => return IterStatus::SubmitFailed(e),
    };
    let source = match File::open(source_path) {
        Ok(f) => f,
        Err(e) => return IterStatus::SubmitFailed(e),
    };

    // SAFETY: the test owns `scratch_path` exclusively for the duration of
    // the iteration; no other process mutates the file while the mapping
    // is alive, so the kernel's view stays stable as required by `MmapMut`.
    let mut mmap = match unsafe { MmapOptions::new().len(SCRATCH_SIZE).map_mut(&scratch) } {
        Ok(m) => m,
        Err(e) => return IterStatus::SubmitFailed(e),
    };

    // Deliberately do NOT touch any page here. The page-fault race depends
    // on the SQPOLL kthread being the first to write into the mapping.

    let iovec = libc::iovec {
        iov_base: mmap.as_mut_ptr().cast::<libc::c_void>(),
        iov_len: READ_LEN,
    };
    // SAFETY: `iovec` points at the live `mmap` mapping which outlives the
    // ring; the registered buffer stays valid until `unregister_buffers`
    // runs below. The kernel's `IORING_REGISTER_BUFFERS` call only inspects
    // the iovec during the syscall.
    let register_result = unsafe { ring.submitter().register_buffers(&[iovec]) };
    if let Err(e) = register_result {
        return IterStatus::RegisterFailed(e);
    }

    let read_sqe = opcode::ReadFixed::new(
        types::Fd(source.as_raw_fd()),
        mmap.as_mut_ptr(),
        READ_LEN as u32,
        0,
    )
    .offset(0)
    .build()
    .user_data(READ_USER_DATA);

    // SAFETY: `read_sqe` references `source` (live for the iteration) and
    // the registered mmap buffer (live for the iteration); both outlast the
    // submission + reap below.
    unsafe {
        if let Err(e) = ring.submission().push(&read_sqe) {
            return IterStatus::SubmitFailed(std::io::Error::other(format!(
                "sq push failed: {e}"
            )));
        }
    }

    let timeout = types::Timespec::new()
        .sec(ITER_TIMEOUT.as_secs())
        .nsec(ITER_TIMEOUT.subsec_nanos());
    let args = types::SubmitArgs::new().timespec(&timeout);

    let started = Instant::now();
    let submit_result = ring.submitter().submit_with_args(1, &args);
    match submit_result {
        Ok(_) => {}
        Err(e) => {
            // ETIME (62) is the documented return when GETEVENTS times out
            // without a completion landing. Map it to the dedicated variant
            // so the tally separates real submit failures from kernel
            // stalls.
            if e.raw_os_error() == Some(libc::ETIME) {
                return IterStatus::Timeout;
            }
            return IterStatus::SubmitFailed(e);
        }
    }
    if started.elapsed() >= ITER_TIMEOUT {
        return IterStatus::Timeout;
    }

    let mut cq = ring.completion();
    let cqe = match cq.next() {
        Some(c) => c,
        None => return IterStatus::Timeout,
    };
    assert_eq!(cqe.user_data(), READ_USER_DATA, "stray CQE");
    let result = cqe.result();
    drop(cq);

    // Drop the buffer registration before the mmap unmaps so the kernel
    // releases its pinned references first; matches the ring-then-buffer
    // drop-order invariant documented in
    // `crates/fast_io/src/io_uring/registered_buffers/mod.rs`.
    if let Err(e) = ring.submitter().unregister_buffers() {
        eprintln!("repro_sqpoll_mmap iter={iter:02}: unregister_buffers failed: {e}");
    }

    if result < 0 {
        let errno = -result;
        return match errno {
            libc::EFAULT => IterStatus::EFault,
            libc::EAGAIN => IterStatus::EAgain,
            other => IterStatus::NegativeErrno(other),
        };
    }
    let bytes = result as usize;
    if bytes != READ_LEN {
        return IterStatus::Short { bytes };
    }
    IterStatus::Ok {
        bytes,
        elapsed: started.elapsed(),
    }
}

#[cfg(test)]
mod sanity {
    use super::*;

    #[test]
    fn constants_are_sane() {
        assert!(SCRATCH_SIZE.is_multiple_of(READ_LEN));
        assert!(READ_LEN > 0);
        assert!(ITERATIONS > 0 && ITERATIONS < 1024);
        assert!(ITER_TIMEOUT.as_secs() > 0);
    }
}
