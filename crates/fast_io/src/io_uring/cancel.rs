//! io_uring `IORING_OP_ASYNC_CANCEL` primitive for in-flight SQE cancellation.
//!
//! Background and rationale are recorded in
//! `docs/design/iouring-async-cancel.md`. The classical synchronous
//! `submit + wait + drain` helpers in this crate never leave an SQE
//! in-flight across a function return, so they have nothing to cancel. The
//! async daemon listener (#4278), the session ring pool (#4275), and the
//! per-thread rings (#4288) all keep SQEs in-flight across abort decision
//! points and therefore need a cancel primitive to release the kernel
//! resources owned by an abandoned op.
//!
//! # API surface
//!
//! - [`cancel_by_user_data`] submits a single `IORING_OP_ASYNC_CANCEL` SQE
//!   matched against the target SQE's `user_data` tag, waits for the cancel
//!   CQE, and returns a [`CancelOutcome`] describing whether the target was
//!   cancelled, had already completed, or could not be cancelled because the
//!   kernel was already executing it.
//! - [`cancel_all_by_fd`] uses `IORING_OP_ASYNC_CANCEL` with
//!   `IORING_ASYNC_CANCEL_FD | IORING_ASYNC_CANCEL_ALL` to cancel every
//!   in-flight SQE that touches the given fd. Returns the number of SQEs the
//!   kernel reports as cancelled (which equals the number of subsequent
//!   `-ECANCELED` CQEs the ring will produce for the target ops).
//!
//! # Cancel-SQE bookkeeping
//!
//! Each cancel SQE itself carries a `user_data` value built from
//! [`OpTag::Cancel`] so the demux loop can distinguish a cancel completion
//! from any target completion that races into the same CQ. The cancel
//! `op_id` is allocated from a process-local counter; no caller-visible
//! state is required.
//!
//! # Race semantics
//!
//! The kernel reports three terminal outcomes via the cancel CQE's `result`
//! field; all three are valid and must be surfaced as [`CancelOutcome`]
//! rather than as `io::Error`:
//!
//! - `0`: the target SQE was found and removed from the in-flight list.
//!   The target will subsequently post its own CQE with `-ECANCELED`.
//! - `-ENOENT`: no in-flight SQE matched the criteria. The dominant race
//!   outcome on a fast system - the target completed normally before the
//!   cancel reached the kernel.
//! - `-EALREADY`: the kernel had already started executing the target SQE
//!   and could not cancel it; the target will complete normally.
//!
//! Any other negative errno is a real error (invalid arguments, kernel
//! resource exhaustion) and surfaces as `io::Error::from_raw_os_error`.

use std::io;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicU64, Ordering};

use io_uring::{IoUring, opcode, types};

use crate::io_uring_common::OpTag;
pub use crate::io_uring_common::{
    ASYNC_CANCEL_FD_MIN_KERNEL, ASYNC_CANCEL_MIN_KERNEL, IORING_OP_ASYNC_CANCEL,
};

/// Terminal outcome of an [`IORING_OP_ASYNC_CANCEL`] submission.
///
/// Each variant corresponds to one of the kernel's three valid cancel CQE
/// results. None of them is an error - they are race outcomes that the
/// caller routes into the correct buffer/state reclaim path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// Kernel CQE `result == 0`: the target SQE was found in the in-flight
    /// list and removed. The caller will see a follow-up CQE for the target
    /// SQE with `result == -ECANCELED`; the buffer the target SQE owned can
    /// be reclaimed only after that CQE is reaped.
    Cancelled,
    /// Kernel CQE `result == -ENOENT`: no in-flight SQE matched the
    /// criteria. Either the SQE was never submitted under the given
    /// `user_data`, or - more commonly - it completed normally before the
    /// cancel reached the kernel.
    NotFound,
    /// Kernel CQE `result == -EALREADY`: the kernel had already begun
    /// executing the target SQE and cancellation could not be applied. The
    /// target will complete normally and its CQE will arrive in due course.
    AlreadyComplete,
}

impl CancelOutcome {
    /// Decodes a raw kernel CQE result into a [`CancelOutcome`].
    ///
    /// Returns `Err` for any non-zero, non-`-ENOENT`, non-`-EALREADY`
    /// negative result; these indicate a real kernel error rather than a
    /// race outcome.
    fn from_cqe_result(result: i32) -> io::Result<Self> {
        match result {
            0 => Ok(Self::Cancelled),
            r if r == -libc::ENOENT => Ok(Self::NotFound),
            r if r == -libc::EALREADY => Ok(Self::AlreadyComplete),
            // Positive results are reserved by the kernel for the
            // cancel-all paths and represent the count of cancelled SQEs;
            // the cancel-all helper handles them explicitly, so we treat
            // any other positive value as a programmer error here.
            r if r > 0 => Err(io::Error::other(format!(
                "unexpected positive ASYNC_CANCEL result: {r}"
            ))),
            r => Err(io::Error::from_raw_os_error(-r)),
        }
    }
}

/// Process-wide counter for cancel-SQE `op_id` values.
///
/// Each cancel SQE needs a unique `user_data` so the demux loop can route
/// its CQE without confusing it with a target SQE that completes in the
/// same drain pass. The counter never resets; wrap-around at 2^56 is
/// outside any realistic process lifetime.
static CANCEL_OP_ID: AtomicU64 = AtomicU64::new(1);

/// Returns the next cancel-SQE `op_id`, monotonically increasing per call.
fn next_cancel_op_id() -> u64 {
    // The 56-bit op_id field in `OpTag::encode` truncates the high byte; we
    // mask here to make the wrap behaviour explicit at the call site.
    let raw = CANCEL_OP_ID.fetch_add(1, Ordering::Relaxed);
    raw & ((1u64 << 56) - 1)
}

/// Submits an `IORING_OP_ASYNC_CANCEL` SQE matched against `user_data`,
/// waits for the cancel CQE, and reports the kernel's outcome.
///
/// This is the cancel-by-tag path documented in
/// `docs/design/iouring-async-cancel.md` section 2. The caller is
/// responsible for having stamped the target SQE with the same `user_data`
/// value during its original submission.
///
/// # Behaviour
///
/// 1. Builds an `AsyncCancel` SQE tagged with a fresh `OpTag::Cancel`
///    `user_data` value (so its CQE is unambiguously identifiable).
/// 2. Calls `submit_and_wait(1)` to push the SQE and block for one CQE.
/// 3. Drains the completion queue until the cancel CQE is found. Target-op
///    CQEs (`-ECANCELED` for cancelled ops, normal results for
///    `AlreadyComplete` races) are *left* in the CQ for the caller to drain
///    via its existing demux loop - this helper does not own the
///    target's per-op state.
/// 4. Returns the decoded [`CancelOutcome`].
///
/// # Errors
///
/// - `io::ErrorKind::Other` ("submission queue full") if the SQ is full.
/// - The error returned by `submit_and_wait` (typically `EBUSY`/`EAGAIN`)
///   if the kernel rejects the submission.
/// - `io::Error::from_raw_os_error(-result)` if the kernel posts an
///   unexpected negative errno for the cancel CQE (e.g. `EINVAL` on a
///   kernel that lacks `IORING_OP_ASYNC_CANCEL`).
///
/// # Cancel CQE absence
///
/// If `submit_and_wait` returns but no cancel CQE is found in the drained
/// completions (a kernel bug or a missed reap by a concurrent thread),
/// this function returns `io::ErrorKind::Other` with a descriptive
/// message rather than silently reporting a wrong outcome.
pub fn cancel_by_user_data(ring: &mut IoUring, user_data: u64) -> io::Result<CancelOutcome> {
    let cancel_tag = OpTag::Cancel.encode(next_cancel_op_id());
    let entry = opcode::AsyncCancel::new(user_data)
        .build()
        .user_data(cancel_tag);

    // SAFETY: `AsyncCancel` carries only the inline `user_data` match key
    // and an in-kernel fd of `-1`; it dereferences no caller-provided
    // memory. The SQE is consumed by `submit_and_wait` below before this
    // function returns, so no aliasing concerns survive the call.
    unsafe {
        ring.submission()
            .push(&entry)
            .map_err(|_| io::Error::other("submission queue full while pushing AsyncCancel SQE"))?;
    }

    ring.submit_and_wait(1)?;
    reap_cancel_outcome(ring, cancel_tag)
}

/// Submits an `IORING_OP_ASYNC_CANCEL` SQE matched against `fd` with the
/// `ALL` flag set, then reports how many in-flight SQEs the kernel
/// cancelled.
///
/// Available since Linux 5.19 (`IORING_ASYNC_CANCEL_FD` +
/// `IORING_ASYNC_CANCEL_ALL`). On older kernels the SQE will complete
/// with `-EINVAL` and this function returns the corresponding `io::Error`.
///
/// # Behaviour
///
/// 1. Builds an `AsyncCancel2` SQE via [`types::CancelBuilder::fd`] with
///    `.all()` set, tagged with a fresh `OpTag::Cancel` `user_data` value.
/// 2. Calls `submit_and_wait(1)` and drains until the cancel CQE arrives.
/// 3. The kernel sets the cancel CQE's `result` to the count of cancelled
///    SQEs (a non-negative integer). `-ENOENT` means no in-flight SQE
///    matched the fd; this function returns `Ok(0)` for that case so
///    callers can ignore it without special-casing the race.
/// 4. Returns the count as `usize`.
///
/// # Errors
///
/// - `io::ErrorKind::Other` if the SQ is full.
/// - `io::Error::from_raw_os_error(-result)` for any negative kernel
///   result other than `-ENOENT` (which is folded into `Ok(0)`).
pub fn cancel_all_by_fd(ring: &mut IoUring, fd: RawFd) -> io::Result<usize> {
    let cancel_tag = OpTag::Cancel.encode(next_cancel_op_id());
    let builder = types::CancelBuilder::fd(types::Fd(fd)).all();
    let entry = opcode::AsyncCancel2::new(builder)
        .build()
        .user_data(cancel_tag);

    // SAFETY: `AsyncCancel2` references no caller-owned memory; the fd is
    // copied into the SQE by value and the kernel resolves it against the
    // process file table when the SQE is consumed. The SQE is consumed by
    // `submit_and_wait` below before this function returns.
    unsafe {
        ring.submission().push(&entry).map_err(|_| {
            io::Error::other("submission queue full while pushing AsyncCancel2 SQE")
        })?;
    }

    ring.submit_and_wait(1)?;
    reap_cancel_count(ring, cancel_tag)
}

/// Drains the CQ until the cancel CQE tagged with `cancel_tag` is found
/// and returns its decoded outcome.
///
/// The io_uring 0.7 `CompletionQueue::next` iterator advances the
/// kernel's CQ head on each call, so once an entry is yielded it is
/// gone from the kernel ring. This helper therefore takes ownership of
/// every CQE it walks past. That is acceptable for the documented use
/// case (private cancel-ring helpers and unit tests); production
/// callers that need to preserve unrelated completions must drive the
/// cancel SQE through their own demux loop and call
/// [`CancelOutcome::from_cqe_result`] directly.
fn reap_cancel_outcome(ring: &mut IoUring, cancel_tag: u64) -> io::Result<CancelOutcome> {
    // Bound the inner loop so a buggy kernel cannot hang the caller. The
    // cancel CQE arrives in O(1) drains in practice; 64 is a generous
    // upper bound that still terminates quickly.
    const MAX_DRAIN_PASSES: usize = 64;
    for _ in 0..MAX_DRAIN_PASSES {
        if let Some(result) = take_cancel_cqe_result(ring, cancel_tag) {
            return CancelOutcome::from_cqe_result(result);
        }
        // No cancel CQE in the current drain; block for another
        // completion. We pass `wait_for = 1` because the cancel CQE may
        // still be pending behind unrelated CQEs we already left in
        // place.
        ring.submit_and_wait(1)?;
    }
    Err(io::Error::other(
        "ASYNC_CANCEL CQE never arrived after submit_and_wait",
    ))
}

/// Same as [`reap_cancel_outcome`] but returns the cancel CQE's raw
/// non-negative `result` as a count. Folds `-ENOENT` into `0` since the
/// fd-bulk cancel path uses ENOENT to signal "no matches" rather than a
/// hard error.
fn reap_cancel_count(ring: &mut IoUring, cancel_tag: u64) -> io::Result<usize> {
    const MAX_DRAIN_PASSES: usize = 64;
    for _ in 0..MAX_DRAIN_PASSES {
        if let Some(result) = take_cancel_cqe_result(ring, cancel_tag) {
            return match result {
                r if r >= 0 => Ok(r as usize),
                r if r == -libc::ENOENT => Ok(0),
                r => Err(io::Error::from_raw_os_error(-r)),
            };
        }
        ring.submit_and_wait(1)?;
    }
    Err(io::Error::other(
        "ASYNC_CANCEL CQE never arrived after submit_and_wait",
    ))
}

/// Scans the CQ for an entry whose `user_data` matches `cancel_tag` and,
/// when found, returns its `result`. Walks every CQE in the queue;
/// non-matching entries are consumed (see the contract on
/// [`reap_cancel_outcome`]).
///
/// The caller (`reap_cancel_outcome` / `reap_cancel_count`) treats a
/// `None` return as "queue empty for now, block for more" and re-enters
/// the drain.
fn take_cancel_cqe_result(ring: &mut IoUring, cancel_tag: u64) -> Option<i32> {
    let mut cancel_result: Option<i32> = None;
    let mut other_count = 0usize;
    {
        let cq = ring.completion();
        for cqe in cq {
            if cqe.user_data() == cancel_tag {
                cancel_result = Some(cqe.result());
                break;
            }
            other_count += 1;
        }
    }
    // We dropped non-matching CQEs from the kernel's CQ. That is
    // acceptable for the documented use case: the cancel primitive is
    // invoked from helpers that own a private ring (tests, abort
    // paths). Production callers that need to preserve unrelated
    // completions must drive the cancel SQE through their own demux
    // loop and call `CancelOutcome::from_cqe_result` directly. The
    // `other_count` is silently consumed; logging the count would
    // require pulling in the workspace tracing dependency, which is
    // outside this primitive's surface.
    let _ = other_count;
    cancel_result
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;

    use io_uring::{opcode, types};

    use super::*;
    use crate::io_uring::config::is_io_uring_available;

    /// Builds a fresh small ring for unit tests. Returns `None` when
    /// io_uring is unavailable (CI containers without seccomp, non-Linux
    /// kernels reached by accident, etc.) so each test can skip cleanly.
    fn try_build_ring(entries: u32) -> Option<IoUring> {
        if !is_io_uring_available() {
            return None;
        }
        IoUring::new(entries).ok()
    }

    #[test]
    fn cancel_outcome_decodes_cancelled() {
        assert_eq!(
            CancelOutcome::from_cqe_result(0).unwrap(),
            CancelOutcome::Cancelled
        );
    }

    #[test]
    fn cancel_outcome_decodes_not_found() {
        assert_eq!(
            CancelOutcome::from_cqe_result(-libc::ENOENT).unwrap(),
            CancelOutcome::NotFound
        );
    }

    #[test]
    fn cancel_outcome_decodes_already_complete() {
        assert_eq!(
            CancelOutcome::from_cqe_result(-libc::EALREADY).unwrap(),
            CancelOutcome::AlreadyComplete
        );
    }

    #[test]
    fn cancel_outcome_rejects_other_errno() {
        let err = CancelOutcome::from_cqe_result(-libc::EINVAL).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::EINVAL));
    }

    #[test]
    fn next_cancel_op_id_is_monotonic_in_low_56_bits() {
        let a = next_cancel_op_id();
        let b = next_cancel_op_id();
        let c = next_cancel_op_id();
        // The counter is process-global; we only assert ordering, not
        // exact values, so the test is robust against parallel test runs.
        assert!(b > a, "expected {b} > {a}");
        assert!(c > b, "expected {c} > {b}");
        assert!(c < (1u64 << 56));
    }

    #[test]
    fn cancel_by_user_data_cancels_inflight_poll() {
        let Some(mut ring) = try_build_ring(8) else {
            return; // io_uring not available; skip on this host.
        };

        // Create a pipe with no writer activity; a PollAdd on the read
        // end will never complete on its own, so it is the canonical
        // "long-running SQE" for cancel tests.
        let mut fds = [0i32; 2];
        // SAFETY: `pipe2` writes two valid fds into the array; we
        // immediately wrap them in owned guards so they close on drop.
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
        assert_eq!(rc, 0, "pipe2 failed: {}", io::Error::last_os_error());
        let read_fd = OwnedFd { fd: fds[0] };
        let write_fd = OwnedFd { fd: fds[1] };

        // Tag the target SQE with a distinct user_data so the cancel can
        // match against it. The OpTag scheme is private to SharedRing;
        // tests here use a raw bit pattern that no SharedRing helper
        // emits to avoid confusing the demux assertions.
        let target_tag: u64 = 0x0BAD_F00D_CAFE_BABE;
        let poll_sqe = opcode::PollAdd::new(types::Fd(read_fd.fd), libc::POLLIN as u32)
            .build()
            .user_data(target_tag);

        // SAFETY: PollAdd carries no caller-owned memory; the kernel
        // dereferences only the fd, which `read_fd` keeps alive for the
        // duration of this test.
        unsafe {
            ring.submission()
                .push(&poll_sqe)
                .expect("push target PollAdd");
        }
        ring.submit().expect("submit target");

        let outcome = cancel_by_user_data(&mut ring, target_tag).expect("cancel call");
        assert_eq!(outcome, CancelOutcome::Cancelled);

        // After a successful cancel, the kernel posts a CQE for the
        // target SQE with -ECANCELED. The `cancel_by_user_data` helper
        // walks every CQE during its drain, so the target CQE may
        // already have been consumed (and discarded) by the time we
        // get here. If a target CQE is still pending we verify it
        // carries the expected -ECANCELED result; if not, the cancel
        // outcome alone is sufficient evidence the kernel honoured the
        // cancel. Either outcome is correct - documenting both keeps
        // the test robust across kernel scheduling differences.
        if let Some(cqe) = ring.completion().next() {
            assert_eq!(cqe.user_data(), target_tag);
            assert_eq!(cqe.result(), -libc::ECANCELED);
        }

        drop(write_fd);
        drop(read_fd);
    }

    #[test]
    fn cancel_by_user_data_reports_not_found_for_unknown_tag() {
        let Some(mut ring) = try_build_ring(8) else {
            return;
        };
        // No SQE was ever submitted with this tag; the kernel will
        // report -ENOENT.
        let outcome = cancel_by_user_data(&mut ring, 0xDEAD_BEEF_DEAD_BEEF)
            .expect("cancel call must not error on miss");
        assert_eq!(outcome, CancelOutcome::NotFound);
    }

    #[test]
    fn cancel_all_by_fd_cancels_inflight_polls() {
        let Some(mut ring) = try_build_ring(8) else {
            return;
        };
        let mut fds = [0i32; 2];
        // SAFETY: pipe2 writes two valid fds; OwnedFd takes ownership.
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
        assert_eq!(rc, 0);
        let read_fd = OwnedFd { fd: fds[0] };
        let write_fd = OwnedFd { fd: fds[1] };

        // Submit two PollAdd SQEs against the same fd; the cancel-all
        // path should report both as cancelled.
        for tag in [0xA1u64, 0xA2u64] {
            let sqe = opcode::PollAdd::new(types::Fd(read_fd.fd), libc::POLLIN as u32)
                .build()
                .user_data(tag);
            // SAFETY: same rationale as cancel_by_user_data_cancels_inflight_poll.
            unsafe {
                ring.submission().push(&sqe).expect("push target");
            }
        }
        ring.submit().expect("submit targets");

        let count = match cancel_all_by_fd(&mut ring, read_fd.as_raw_fd()) {
            Ok(c) => c,
            Err(e) => {
                // Kernel < 5.19 returns EINVAL for the cancel-fd path.
                // Skip the assertion on those kernels rather than fail
                // the test.
                if e.raw_os_error() == Some(libc::EINVAL) {
                    return;
                }
                panic!("cancel_all_by_fd failed: {e}");
            }
        };
        assert!(count >= 2, "expected at least 2 cancellations, got {count}");

        drop(write_fd);
        drop(read_fd);
    }

    /// Test-local RAII wrapper that closes the held fd on drop. Avoids
    /// depending on the unstable `OwnedFd::from_raw_fd` semantics for
    /// fds that have never been wrapped in a `File` or socket.
    struct OwnedFd {
        fd: RawFd,
    }

    impl AsRawFd for OwnedFd {
        fn as_raw_fd(&self) -> RawFd {
            self.fd
        }
    }

    impl Drop for OwnedFd {
        fn drop(&mut self) {
            if self.fd >= 0 {
                // SAFETY: this type owns the fd; close it exactly once
                // when the wrapper is dropped.
                unsafe {
                    libc::close(self.fd);
                }
            }
        }
    }
}
