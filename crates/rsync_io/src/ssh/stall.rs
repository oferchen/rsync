//! I/O stall detection for the SSH transport.
//!
//! Upstream rsync enforces the negotiated `--timeout` (`io_timeout`) uniformly
//! across every transport: the `select`/`poll` loop in `io.c` aborts with
//! [`RERR_TIMEOUT`] (exit code 30) when no I/O has made progress for
//! `io_timeout` seconds, and keepalive writes reset the progress clock so a
//! legitimate computation lull does not trip the timeout.
//!
//! The SSH data channel here is the spawned `ssh` child's inherited stdio - a
//! pair of anonymous pipes - which cannot carry a `SO_RCVTIMEO`/`SO_SNDTIMEO`
//! deadline the way a socket can. This module reproduces the upstream stall
//! semantics with a background watchdog that mirrors the existing
//! [`ConnectWatchdog`](super::connection): a shared progress clock is bumped on
//! every successful read/write, and a watchdog thread wakes every
//! `allowed_lull` to compare the idle interval against `io_timeout`. On expiry
//! it invokes an abort action (killing the child, which unblocks the pending
//! pipe read) and latches a `fired` flag so the read/write half can translate
//! the resulting EOF/error into an [`io::ErrorKind::TimedOut`] - the kind that
//! `core` maps to `ExitCode::Timeout` (30).
//!
//! upstream: io.c `check_timeout` / `set_io_timeout` / `RERR_TIMEOUT`
//! (errcode.h:47).

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Lower bound on the watchdog poll interval.
///
/// For sub-second timeouts (used by tests) a naive `io_timeout / 2` would busy
/// loop, so the poll cadence is floored here. It stays well below any realistic
/// `io_timeout` so detection latency is dominated by `io_timeout` itself.
const MIN_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Upper bound on the watchdog poll interval.
///
/// upstream: io.c:35 `SELECT_TIMEOUT` caps `select_timeout` at 60 seconds even
/// when `allowed_lull` would be larger.
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Normalises a configured `--timeout` value into an effective I/O timeout.
///
/// Returns `None` when the timeout is absent or zero, matching upstream's
/// "`io_timeout == 0` means off" convention (`options.c` leaves `io_timeout`
/// at `0` and `set_io_timeout(0)` disables the check). Any positive duration is
/// returned unchanged. This is the single point that decides whether stall
/// detection is armed at all.
///
/// upstream: io.c:179 `if (!io_timeout) return;`
#[must_use]
pub(crate) fn effective_io_timeout(timeout: Option<Duration>) -> Option<Duration> {
    timeout.filter(|d| !d.is_zero())
}

/// Computes the watchdog poll interval for a given `io_timeout`.
///
/// Mirrors upstream's `allowed_lull = (io_timeout + 1) / 2` (io.c:1151),
/// clamped to `[MIN_POLL_INTERVAL, MAX_POLL_INTERVAL]`. Polling at half the
/// timeout guarantees the stall is detected within `~1.5 * io_timeout`.
#[must_use]
pub(crate) fn stall_poll_interval(io_timeout: Duration) -> Duration {
    (io_timeout / 2).clamp(MIN_POLL_INTERVAL, MAX_POLL_INTERVAL)
}

/// Shared progress clock recording the instant of the most recent successful
/// I/O on the SSH channel.
///
/// Both the read and write halves bump the same clock, so a keepalive write
/// (emitted by the transfer layer's `maybe_send_keepalive`) resets it exactly
/// like an inbound read - matching upstream's `MAX(last_io_out, last_io_in)`
/// (io.c:195).
#[derive(Debug)]
pub(crate) struct StallProgress {
    /// Monotonic base captured at construction. All offsets are measured from
    /// here so the clock is immune to wall-clock adjustments.
    base: Instant,
    /// Milliseconds elapsed from `base` at the last recorded progress.
    last_millis: AtomicU64,
}

impl StallProgress {
    /// Creates a progress clock seeded with "progress just happened".
    fn new() -> Self {
        Self {
            base: Instant::now(),
            last_millis: AtomicU64::new(0),
        }
    }

    /// Records that I/O just made progress.
    fn record(&self) {
        let elapsed = self.base.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        self.last_millis.store(elapsed, Ordering::Release);
    }

    /// Returns how long the channel has been idle since the last progress.
    fn idle(&self) -> Duration {
        let now = self.base.elapsed();
        let last = Duration::from_millis(self.last_millis.load(Ordering::Acquire));
        now.saturating_sub(last)
    }
}

/// Read/write-half handle that enforces the stall deadline.
///
/// Cloned into both the [`SshReader`](super::connection::SshReader) and
/// [`SshWriter`](super::connection::SshWriter) so each I/O direction bumps the
/// shared progress clock and observes the latched timeout.
#[derive(Clone, Debug)]
pub(crate) struct StallHandle {
    progress: Arc<StallProgress>,
    fired: Arc<AtomicBool>,
    io_timeout: Duration,
}

impl StallHandle {
    /// Records successful progress of `n` bytes; a zero-length transfer (EOF)
    /// is not progress and never resets the clock.
    pub(crate) fn record(&self, n: usize) {
        if n > 0 {
            self.progress.record();
        }
    }

    /// Returns `true` once the watchdog has latched a timeout.
    pub(crate) fn timed_out(&self) -> bool {
        self.fired.load(Ordering::Acquire)
    }

    /// Builds the [`io::ErrorKind::TimedOut`] error surfaced to callers, which
    /// `core` maps to `ExitCode::Timeout` (upstream `RERR_TIMEOUT`, 30).
    pub(crate) fn timeout_error(&self) -> io::Error {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "io timeout after {} seconds -- no data received",
                self.io_timeout.as_secs()
            ),
        )
    }
}

/// Background watchdog that aborts the SSH channel when I/O stalls beyond
/// `io_timeout`.
///
/// Mirrors [`ConnectWatchdog`](super::connection): a condvar-driven thread
/// avoids busy polling and lets [`cancel`](Self::cancel) / drop wake it
/// immediately. On expiry it latches `fired` and runs the injected abort action
/// (which kills the child to unblock a pending pipe read). The abort action is
/// injected rather than hard-coded so the watchdog can be unit-tested against a
/// loopback socket without spawning a subprocess.
pub(crate) struct IoStallWatchdog {
    cancelled: Arc<AtomicBool>,
    condvar_pair: Arc<(Mutex<bool>, Condvar)>,
    handle: Option<JoinHandle<()>>,
}

impl IoStallWatchdog {
    /// Arms a stall watchdog and returns it together with the [`StallHandle`]
    /// the read/write halves must consult.
    ///
    /// The watchdog thread polls every `stall_poll_interval(io_timeout)` and,
    /// when the channel has been idle for at least `io_timeout`, latches the
    /// timeout and calls `abort` exactly once. `abort` is responsible for
    /// unblocking any in-flight read (for the SSH path, by killing the child).
    pub(crate) fn arm(
        io_timeout: Duration,
        abort: Box<dyn FnOnce() + Send + 'static>,
    ) -> (Self, StallHandle) {
        let progress = Arc::new(StallProgress::new());
        let fired = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let condvar_pair = Arc::new((Mutex::new(false), Condvar::new()));
        let poll = stall_poll_interval(io_timeout);

        let thread_progress = Arc::clone(&progress);
        let thread_fired = Arc::clone(&fired);
        let thread_cancelled = Arc::clone(&cancelled);
        let thread_pair = Arc::clone(&condvar_pair);

        let handle = thread::Builder::new()
            .name("ssh-io-stall-watchdog".into())
            .spawn(move || {
                let mut abort = Some(abort);
                let (lock, cvar) = &*thread_pair;
                loop {
                    let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                    let (_guard, _res) = cvar
                        .wait_timeout_while(guard, poll, |notified| !*notified)
                        .unwrap_or_else(|e| e.into_inner());

                    if thread_cancelled.load(Ordering::Acquire) {
                        return;
                    }

                    // upstream: io.c:196 `if (t - chk >= io_timeout)` aborts.
                    if thread_progress.idle() >= io_timeout {
                        thread_fired.store(true, Ordering::Release);
                        if let Some(action) = abort.take() {
                            action();
                        }
                        return;
                    }
                }
            })
            .expect("failed to spawn ssh io stall watchdog thread");

        (
            Self {
                cancelled,
                condvar_pair,
                handle: Some(handle),
            },
            StallHandle {
                progress,
                fired,
                io_timeout,
            },
        )
    }

    /// Cancels the watchdog and joins its thread.
    ///
    /// Call this once the transfer has completed normally so the thread does
    /// not fire during connection teardown.
    fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        let (lock, cvar) = &*self.condvar_pair;
        if let Ok(mut notified) = lock.lock() {
            *notified = true;
            cvar.notify_one();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for IoStallWatchdog {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl std::fmt::Debug for IoStallWatchdog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IoStallWatchdog")
            .field("cancelled", &self.cancelled.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_io_timeout_treats_zero_and_absent_as_off() {
        // upstream: io_timeout == 0 disables the check entirely.
        assert_eq!(effective_io_timeout(None), None);
        assert_eq!(effective_io_timeout(Some(Duration::ZERO)), None);
        assert_eq!(
            effective_io_timeout(Some(Duration::from_secs(30))),
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn poll_interval_is_half_the_timeout_within_bounds() {
        // 30s -> 15s (upstream allowed_lull), within [50ms, 60s].
        assert_eq!(
            stall_poll_interval(Duration::from_secs(30)),
            Duration::from_secs(15)
        );
        // Tiny timeouts floor at MIN_POLL_INTERVAL instead of busy looping.
        assert_eq!(
            stall_poll_interval(Duration::from_millis(20)),
            MIN_POLL_INTERVAL
        );
        // Huge timeouts cap at SELECT_TIMEOUT (60s).
        assert_eq!(
            stall_poll_interval(Duration::from_secs(600)),
            MAX_POLL_INTERVAL
        );
    }

    #[test]
    fn progress_clock_resets_idle_on_record() {
        let progress = StallProgress::new();
        std::thread::sleep(Duration::from_millis(30));
        let before = progress.idle();
        assert!(
            before >= Duration::from_millis(20),
            "idle should accrue: {before:?}"
        );
        progress.record();
        assert!(progress.idle() < before, "record must reset the idle clock");
    }

    #[test]
    fn watchdog_fires_and_aborts_when_idle_exceeds_timeout() {
        let aborted = Arc::new(AtomicBool::new(false));
        let aborted_thread = Arc::clone(&aborted);
        let (_watchdog, handle) = IoStallWatchdog::arm(
            Duration::from_millis(150),
            Box::new(move || aborted_thread.store(true, Ordering::Release)),
        );

        // Never record progress: the watchdog must latch a timeout and run the
        // abort action within a generous window (fires at ~150-225ms; 10s
        // tolerates a heavily loaded/slow runner without a false pass).
        let deadline = Instant::now() + Duration::from_secs(10);
        while !handle.timed_out() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            handle.timed_out(),
            "watchdog should latch a timeout on a stall"
        );
        assert!(
            aborted.load(Ordering::Acquire),
            "abort action should run on expiry"
        );

        let err = handle.timeout_error();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn watchdog_does_not_fire_while_progress_continues() {
        // Chosen so the record cadence is a large fraction below the timeout:
        // recording every ~5ms against an 800ms timeout tolerates a ~160x
        // scheduling overshoot before the idle clock could reach the timeout,
        // so the test cannot false-trip on a heavily loaded/slow runner.
        let timeout = Duration::from_millis(800);
        let (_watchdog, handle) = IoStallWatchdog::arm(
            timeout,
            Box::new(|| panic!("abort must not run while progress continues")),
        );

        // Span longer than the timeout (plus a poll cycle) so the watchdog
        // genuinely had the opportunity to fire; continuous progress (as
        // keepalive writes provide) must keep it quiet the whole time.
        let start = Instant::now();
        while start.elapsed() < timeout + stall_poll_interval(timeout) {
            handle.record(1);
            assert!(
                !handle.timed_out(),
                "keepalive progress must prevent a false timeout"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!handle.timed_out(), "progress must prevent any timeout");
    }
}
