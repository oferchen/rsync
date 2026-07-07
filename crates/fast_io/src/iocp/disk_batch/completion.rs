//! Completion port draining, NTSTATUS mapping, and the test-only
//! `WriteFile` fault injector.
//!
//! The drain loop reaps overlapped completions from the single port owned by
//! [`super::IocpDiskBatch`] using `GetQueuedCompletionStatusEx`. Errors
//! surfaced through the `OVERLAPPED::Internal` NTSTATUS field are translated
//! into Win32 DOS error codes by [`ntstatus_to_dos_error`] so they round-trip
//! through `io::Error::from_raw_os_error` like any synchronous Win32 failure.
//!
//! The fault injector is a `#[doc(hidden)]` hook used by the disk-full
//! simulation test to drive deterministic `ERROR_DISK_FULL` coverage without
//! provisioning a limited-capacity volume.

use std::io;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_HANDLE_EOF, FALSE, RtlNtStatusToDosError, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::IO::{GetQueuedCompletionStatusEx, OVERLAPPED_ENTRY};

use crate::iocp::completion_port::CompletionPort;

/// Maximum entries dequeued by a single `GetQueuedCompletionStatusEx` call.
///
/// Matches the io_uring side's CQE batch sizing so both backends use the
/// same drain granularity. Kept fixed (not CPU-scaled) because the disk
/// batch drains exactly the in-flight cohort capped by
/// `IocpConfig::concurrent_ops`, which is already auto-sized by
/// `super::config::default_concurrent_ops`. The clamp ceiling
/// (`super::config::MAX_CONCURRENT_OPS`) is intentionally aligned with this
/// constant so a single drain call can reap the entire cohort.
const COMPLETION_DRAIN_BATCH: usize = 64;

/// Wait timeout for success-path completion drains, in milliseconds. On the
/// success path the disk batch always knows how many completions are
/// outstanding and every op the kernel accepted is guaranteed to post exactly
/// one packet, so it waits indefinitely (`u32::MAX`) until every submitted
/// write has been reaped.
const DRAIN_TIMEOUT_MS: u32 = u32::MAX;

/// Per-wait timeout for the abort-drain path, in milliseconds. Bounds each
/// individual `GetQueuedCompletionStatusEx` wait so the abort drain wakes up
/// periodically to re-check the overall deadline instead of parking forever.
const ABORT_DRAIN_WAIT_MS: u32 = 100;

/// Overall wall-clock budget for the abort-drain path. On the error/abort
/// path we are cleaning up residual outstanding ops whose completions the
/// kernel *should* still post, but an unexpectedly wedged port or a miscounted
/// residual would otherwise hang the disk-commit thread forever. Bounding the
/// total wait converts a potential hang into a surfaced timeout while still
/// giving the kernel ample time (seconds) to post the outstanding packets.
const ABORT_DRAIN_BUDGET: Duration = Duration::from_secs(5);

/// Countdown for the test-only `WriteFile` fault injector. When non-zero,
/// each submission decrements the counter; once it hits zero the next
/// submission returns the OS error code stashed in
/// [`FAULT_INJECT_ERROR_CODE`] instead of issuing `WriteFile`. The hook is
/// dormant by default (counter == 0, error == 0) and adds a single relaxed
/// load to the submit hot path - negligible compared to the syscall it
/// guards.
#[doc(hidden)]
static FAULT_INJECT_COUNTDOWN: AtomicUsize = AtomicUsize::new(0);

/// Win32 error code returned by the next fault-injected submission. Paired
/// with [`FAULT_INJECT_COUNTDOWN`]; see [`inject_next_write_error_for_test`].
#[doc(hidden)]
static FAULT_INJECT_ERROR_CODE: AtomicI32 = AtomicI32::new(0);

/// Arms the test-only `WriteFile` fault injector so the `nth` (1-based)
/// upcoming submission inside [`super::IocpDiskBatch`] returns `os_error`
/// instead of dispatching to the kernel. Subsequent submissions proceed
/// normally after the single fault fires.
///
/// This is a test hook used by `crates/fast_io/tests/iocp_disk_full_simulation.rs`
/// to drive deterministic ERROR_DISK_FULL coverage without provisioning a
/// limited-capacity volume. Production code must never call it; the hook is
/// `#[doc(hidden)]` and excluded from the public API surface.
#[doc(hidden)]
pub fn inject_next_write_error_for_test(nth: usize, os_error: i32) {
    FAULT_INJECT_ERROR_CODE.store(os_error, Ordering::SeqCst);
    FAULT_INJECT_COUNTDOWN.store(nth, Ordering::SeqCst);
}

/// Clears any pending fault-injection state. Tests should call this at the
/// end of every case so a leftover countdown from a panicking test does not
/// leak into a sibling test that shares the same process.
#[doc(hidden)]
pub fn clear_injected_write_error_for_test() {
    FAULT_INJECT_COUNTDOWN.store(0, Ordering::SeqCst);
    FAULT_INJECT_ERROR_CODE.store(0, Ordering::SeqCst);
}

/// Number of upcoming successfully-dequeued completions that
/// [`drain_completions`] should treat as faulted. Armed by
/// [`inject_completion_faults_for_test`]; dormant by default (== 0). When
/// non-zero, each reaped completion is reported as faulted (bytes not credited,
/// [`FAULT_INJECT_COMPLETION_CODE`] recorded as the drain error) and the
/// counter decrements. This exercises the mid-batch completion-error drain
/// without provoking a real kernel NTSTATUS fault, which cannot be produced
/// deterministically on an ordinary volume.
#[doc(hidden)]
static FAULT_INJECT_COMPLETION_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Win32 error code recorded for an injected completion fault. Paired with
/// [`FAULT_INJECT_COMPLETION_COUNT`].
#[doc(hidden)]
static FAULT_INJECT_COMPLETION_CODE: AtomicI32 = AtomicI32::new(0);

/// Arms the test-only completion-fault injector so the next `count`
/// successfully-dequeued completions in [`drain_completions`] are reported as
/// faulted with `os_error`, exercising the mid-batch completion-error drain
/// (retire the dequeued cohort, drain the residual outstanding ops, surface the
/// error). Production code must never call it.
#[doc(hidden)]
pub fn inject_completion_faults_for_test(count: usize, os_error: i32) {
    FAULT_INJECT_COMPLETION_CODE.store(os_error, Ordering::SeqCst);
    FAULT_INJECT_COMPLETION_COUNT.store(count, Ordering::SeqCst);
}

/// Clears any pending completion-fault-injection state.
#[doc(hidden)]
pub fn clear_injected_completion_faults_for_test() {
    FAULT_INJECT_COMPLETION_COUNT.store(0, Ordering::SeqCst);
    FAULT_INJECT_COMPLETION_CODE.store(0, Ordering::SeqCst);
}

/// Returns `Some(os_error)` if the current completion should be reported as
/// faulted, decrementing the armed count. `None` in the dormant production
/// case after a single relaxed load.
fn take_injected_completion_fault() -> Option<i32> {
    if FAULT_INJECT_COMPLETION_COUNT.load(Ordering::Relaxed) == 0 {
        return None;
    }
    let prev = FAULT_INJECT_COMPLETION_COUNT
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| match n {
            0 => None,
            n => Some(n - 1),
        })
        .ok()?;
    if prev >= 1 {
        let code = FAULT_INJECT_COMPLETION_CODE.load(Ordering::SeqCst);
        if code != 0 {
            return Some(code);
        }
    }
    None
}

/// Returns `Some(os_error)` if the current submission should be faulted, or
/// `None` to dispatch normally. Atomically decrements the countdown so only
/// one submission per arm fires.
pub(super) fn take_injected_write_error() -> Option<i32> {
    // Cheap relaxed fast path: when no test has armed the hook (the common
    // case in production) we do a single load and return.
    if FAULT_INJECT_COUNTDOWN.load(Ordering::Relaxed) == 0 {
        return None;
    }
    // Slow path uses SeqCst to interleave correctly with the arming store.
    let prev = FAULT_INJECT_COUNTDOWN
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| match n {
            0 => None,
            n => Some(n - 1),
        })
        .ok()?;
    if prev == 1 {
        let code = FAULT_INJECT_ERROR_CODE.swap(0, Ordering::SeqCst);
        if code != 0 {
            return Some(code);
        }
    }
    None
}

/// Outcome of a single completion-drain pass.
///
/// Every OVERLAPPED the kernel dequeued in the batch is reported so the caller
/// can retire it from its in-flight queue. Successful completions carry their
/// transferred byte count in `completions`; a faulted completion (non-zero
/// NTSTATUS in `OVERLAPPED::Internal`, or EOF) is reported through `retired`
/// only - never as a byte-crediting success - and its mapped error is stashed
/// in `error`.
///
/// Reporting faulted completions via `retired` is what closes the mid-batch
/// use-after-free: the caller must remove those ops from its in-flight queue
/// (their pinned buffers are done) yet still drain the *remaining* outstanding
/// ops before dropping any boxes. Abandoning the whole dequeued batch on the
/// first fault - the previous behaviour - both lost track of already-retired
/// ops and dropped their pinned buffers while the kernel might still own the
/// siblings that had not completed yet.
pub(super) struct DrainOutcome {
    /// OVERLAPPED addresses that completed successfully, with bytes written.
    pub(super) completions: Vec<(usize, usize)>,
    /// OVERLAPPED addresses the kernel dequeued this pass (successful and
    /// faulted alike). The caller retires every one of these from its
    /// in-flight queue so the residual count reflects only ops still owned by
    /// the kernel.
    pub(super) retired: Vec<usize>,
    /// First faulted completion mapped to an `io::Error`, if any. When set the
    /// caller must drain the residual outstanding ops, then surface this error.
    pub(super) error: Option<io::Error>,
}

/// Drains one batch of completion entries from the port using
/// `GetQueuedCompletionStatusEx`.
///
/// Processes the *entire* dequeued batch even when a completion reports a
/// faulted NTSTATUS: successful completions are returned with their byte
/// counts, every dequeued OVERLAPPED is listed in `retired`, and the first
/// fault is captured in `DrainOutcome::error`. The caller reconciles `retired`
/// against its in-flight queue and, if an error is present, drains the
/// remaining outstanding ops before propagating - never dropping a pinned
/// buffer while the kernel still owns it.
pub(super) fn drain_completions(port: &CompletionPort, max: usize) -> io::Result<DrainOutcome> {
    let batch = max.clamp(1, COMPLETION_DRAIN_BATCH);
    let mut entries: Vec<OVERLAPPED_ENTRY> = vec![zeroed_entry(); batch];

    loop {
        let mut removed: u32 = 0;
        // SAFETY: `port.handle()` is owned by `port` and lives for the
        // duration of the call; `entries` backs `batch` slots.
        #[allow(unsafe_code)]
        let ok = unsafe {
            GetQueuedCompletionStatusEx(
                port.handle(),
                entries.as_mut_ptr(),
                batch as u32,
                &mut removed,
                DRAIN_TIMEOUT_MS,
                FALSE,
            )
        };

        if ok == FALSE {
            let err = io::Error::last_os_error();
            // Spurious wake without entries: retry.
            if matches!(err.raw_os_error(), Some(c) if c as u32 == WAIT_TIMEOUT) {
                continue;
            }
            return Err(err);
        }

        let reaped = removed as usize;
        let mut completions = Vec::with_capacity(reaped);
        let mut retired = Vec::with_capacity(reaped);
        let mut error: Option<io::Error> = None;
        for entry in entries.iter().take(reaped) {
            let overlapped_ptr = entry.lpOverlapped;
            if overlapped_ptr.is_null() {
                continue;
            }
            retired.push(overlapped_ptr as usize);
            // SAFETY: entry.lpOverlapped points at the OVERLAPPED structure
            // we submitted; the surrounding pinned op is still alive in
            // the in-flight queue, so reading the Internal field is sound.
            #[allow(unsafe_code)]
            let internal = unsafe { (*overlapped_ptr).Internal };
            // Test hook: treat a dequeued completion as faulted so the
            // completion-error drain path is exercised deterministically.
            // Dormant in production (single relaxed load).
            let injected = take_injected_completion_fault();
            if internal != 0 || injected.is_some() {
                // Record the first fault but keep processing so every
                // dequeued op is retired and the caller can drain the rest.
                if error.is_none() {
                    error = Some(match injected {
                        Some(code) => io::Error::from_raw_os_error(code),
                        None => {
                            let dos_error = ntstatus_to_dos_error(internal as u32);
                            if dos_error == ERROR_HANDLE_EOF {
                                io::Error::from(io::ErrorKind::UnexpectedEof)
                            } else {
                                io::Error::from_raw_os_error(dos_error as i32)
                            }
                        }
                    });
                }
                continue;
            }
            completions.push((
                overlapped_ptr as usize,
                entry.dwNumberOfBytesTransferred as usize,
            ));
        }
        return Ok(DrainOutcome {
            completions,
            retired,
            error,
        });
    }
}

/// Reaps exactly `outstanding` completion packets from the port, tolerating
/// faulted completions, and returns the total bytes transferred across every
/// reaped op.
///
/// Used only on the submission-error cleanup path: once one submission has
/// failed synchronously the batch is doomed, but every op still in flight has
/// already been accepted by the kernel and will post exactly one completion
/// packet. Those packets must be reaped before the pinned `OverlappedOp`
/// boxes are dropped, otherwise the kernel may still be writing into a freed
/// buffer (use-after-free). Unlike [`drain_completions`], a faulted NTSTATUS
/// on a completion is not propagated - it merely marks that op as done so the
/// loop keeps waiting for the remaining outstanding ops. The caller returns
/// the original submission error, so swallowing per-completion faults here
/// does not hide the failure; it only guarantees the buffers outlive the
/// kernel's writes. The returned byte count preserves partial progress the
/// same way the success path does.
///
/// # Bounded wait
///
/// Each `GetQueuedCompletionStatusEx` wait is capped at [`ABORT_DRAIN_WAIT_MS`]
/// and the loop enforces an overall [`ABORT_DRAIN_BUDGET`] deadline. On the
/// success path every accepted op is guaranteed to post a completion, but on
/// this abort path a wedged completion port or a miscounted residual could
/// otherwise park the disk-commit thread inside the wait forever. When the
/// budget elapses with ops still outstanding the drain gives up, emits a
/// throttled `tracing::warn!`, and returns the bytes reaped so far - the caller
/// is already propagating the original abort error, so a bounded return
/// converts a potential hang into a surfaced timeout without masking the
/// primary failure.
pub(super) fn drain_all_ignoring_completion_errors(
    port: &CompletionPort,
    outstanding: usize,
) -> usize {
    let deadline = Instant::now() + ABORT_DRAIN_BUDGET;
    drain_all_ignoring_completion_errors_until(port, outstanding, deadline)
}

/// Deadline-bounded core of [`drain_all_ignoring_completion_errors`], split out
/// so tests can drive a short deadline against a port that will never post a
/// completion and assert the drain returns instead of hanging.
fn drain_all_ignoring_completion_errors_until(
    port: &CompletionPort,
    mut outstanding: usize,
    deadline: Instant,
) -> usize {
    let mut bytes = 0usize;
    while outstanding > 0 {
        if Instant::now() >= deadline {
            // The kernel still owes us `outstanding` completions but the budget
            // is spent. Surface the timeout diagnostically; the caller returns
            // the original abort error regardless.
            tracing::warn!(
                outstanding,
                budget_ms = ABORT_DRAIN_BUDGET.as_millis() as u64,
                "IOCP abort-drain timed out waiting for outstanding completions",
            );
            break;
        }
        let batch = outstanding.min(COMPLETION_DRAIN_BATCH);
        let mut entries: Vec<OVERLAPPED_ENTRY> = vec![zeroed_entry(); batch];
        let mut removed: u32 = 0;
        // SAFETY: `port.handle()` is owned by `port` and lives for the
        // duration of the call; `entries` backs `batch` slots.
        #[allow(unsafe_code)]
        let ok = unsafe {
            GetQueuedCompletionStatusEx(
                port.handle(),
                entries.as_mut_ptr(),
                batch as u32,
                &mut removed,
                ABORT_DRAIN_WAIT_MS,
                FALSE,
            )
        };

        if ok == FALSE {
            let err = io::Error::last_os_error();
            // Bounded-wait expiry with no entries: loop so the deadline check
            // at the top can decide whether to keep waiting or give up. Any
            // other failure means the port itself is unusable, so there is no
            // way left to observe the remaining completions; stop to avoid
            // spinning forever. This matches the pre-existing posture of the
            // success path, where `drain_completions` also returns on such an
            // error and the caller drops the in-flight queue.
            if matches!(err.raw_os_error(), Some(c) if c as u32 == WAIT_TIMEOUT) {
                continue;
            }
            break;
        }

        let reaped = removed as usize;
        if reaped == 0 {
            continue;
        }
        for entry in entries.iter().take(reaped) {
            if entry.lpOverlapped.is_null() {
                continue;
            }
            // SAFETY: entry.lpOverlapped points at an OVERLAPPED we submitted;
            // its pinned op is still alive in the caller's in-flight queue.
            #[allow(unsafe_code)]
            let internal = unsafe { (*entry.lpOverlapped).Internal };
            // Only credit bytes for completions that landed without a fault.
            if internal == 0 {
                bytes += entry.dwNumberOfBytesTransferred as usize;
            }
        }
        outstanding = outstanding.saturating_sub(reaped);
    }
    bytes
}

/// Translates an NTSTATUS from `OVERLAPPED::Internal` into its Win32 DOS error
/// code so a faulted overlapped completion surfaces the same `errno`/exit code
/// as an equivalent synchronous Win32 failure (and, in turn, the same
/// cross-platform mapping the standard and io_uring writers produce).
///
/// Delegates to `ntdll!RtlNtStatusToDosError`, the OS's own authoritative and
/// complete NTSTATUS -> Win32 table, rather than a hand-maintained subset. A
/// partial table left common disk faults - `STATUS_DISK_FULL` (0xC000007F),
/// `STATUS_ACCESS_DENIED` (0xC0000022), `STATUS_DEVICE_*` - falling through to
/// the raw NTSTATUS value, which is not a valid Win32 error and produces
/// exit-code/message divergence from the other writers.
fn ntstatus_to_dos_error(status: u32) -> u32 {
    // SAFETY: `RtlNtStatusToDosError` is a pure translation function in
    // ntdll.dll with no preconditions on `status`; any NTSTATUS (including
    // unrecognised ones, which map to ERROR_MR_MID_NOT_FOUND) is valid input.
    #[allow(unsafe_code)]
    unsafe {
        RtlNtStatusToDosError(status as i32)
    }
}

/// Constructs a zeroed `OVERLAPPED_ENTRY` for batch dequeues.
fn zeroed_entry() -> OVERLAPPED_ENTRY {
    // SAFETY: OVERLAPPED_ENTRY is plain-old-data and valid when zeroed.
    #[allow(unsafe_code)]
    unsafe {
        std::mem::zeroed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_DISK_FULL, ERROR_OPERATION_ABORTED,
    };

    // Common NTSTATUS values overlapped file I/O can surface, paired with the
    // Win32 error code the other writers (standard/io_uring) and upstream
    // rsync produce for the same failure. Verifying the mapping goes through
    // `RtlNtStatusToDosError` guards against a regression back to the partial
    // hand table that leaked raw NTSTATUS values for common disk faults.
    #[test]
    fn ntstatus_maps_to_matching_win32_error() {
        // STATUS_END_OF_FILE -> ERROR_HANDLE_EOF (drain_completions relies on
        // this exact equality to raise UnexpectedEof).
        assert_eq!(ntstatus_to_dos_error(0xC000_0011), ERROR_HANDLE_EOF);
        // STATUS_DISK_FULL -> ERROR_DISK_FULL (112). The old partial table
        // fell through to the raw 0xC000_007F here, diverging from every
        // other writer's ENOSPC-equivalent exit code.
        assert_eq!(ntstatus_to_dos_error(0xC000_007F), ERROR_DISK_FULL);
        // STATUS_ACCESS_DENIED -> ERROR_ACCESS_DENIED (5).
        assert_eq!(ntstatus_to_dos_error(0xC000_0022), ERROR_ACCESS_DENIED);
        // STATUS_CANCELLED -> ERROR_OPERATION_ABORTED (995).
        assert_eq!(ntstatus_to_dos_error(0xC000_0120), ERROR_OPERATION_ABORTED);
    }

    // A faulted NTSTATUS must round-trip through io::Error as the mapped Win32
    // code, not the raw status, so callers observe the same errno as a
    // synchronous failure.
    #[test]
    fn faulted_status_round_trips_as_win32_errno() {
        let dos = ntstatus_to_dos_error(0xC000_007F);
        let err = io::Error::from_raw_os_error(dos as i32);
        assert_eq!(err.raw_os_error(), Some(ERROR_DISK_FULL as i32));
    }

    // The abort-drain must return once its deadline elapses instead of parking
    // forever when the kernel never posts the outstanding completions. Drive a
    // fresh port with a residual count but no submitted ops and an
    // already-expired deadline: the loop must give up promptly and report zero
    // reaped bytes rather than hang.
    #[test]
    fn abort_drain_returns_on_deadline_instead_of_hanging() {
        let port = CompletionPort::new(1).expect("create completion port");
        let deadline = Instant::now() - Duration::from_millis(1);
        let start = Instant::now();
        let bytes = drain_all_ignoring_completion_errors_until(&port, 4, deadline);
        assert_eq!(bytes, 0, "no completions were posted, so no bytes reaped");
        assert!(
            start.elapsed() < ABORT_DRAIN_BUDGET,
            "abort-drain must return on the deadline, not run the full budget",
        );
    }

    // A short but non-zero budget must also terminate: no ops are outstanding
    // that will ever complete, so the drain waits at most one bounded wait
    // slice past the deadline before returning.
    #[test]
    fn abort_drain_bounds_total_wait() {
        let port = CompletionPort::new(1).expect("create completion port");
        let deadline = Instant::now() + Duration::from_millis(50);
        let start = Instant::now();
        let _ = drain_all_ignoring_completion_errors_until(&port, 2, deadline);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "bounded abort-drain must not approach the full 5s budget here",
        );
    }
}
