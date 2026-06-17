//! macOS `kqueue` `EVFILT_TIMER` sleep primitive.
//!
//! [`TimerSleeper`] wraps a kqueue file descriptor pre-registered with a
//! single `EVFILT_TIMER` event. Each call to [`TimerSleeper::sleep`]
//! re-arms the timer with `EV_ADD | EV_ONESHOT` and then blocks on
//! `kevent(2)` until the timer event fires. The timer is configured with
//! `NOTE_NSECONDS` so sub-millisecond intervals are honoured at the
//! kernel's Mach absolute-time resolution, avoiding the ~1 ms granularity
//! and CPU spin that `std::thread::sleep` exhibits for very short waits
//! on macOS.
//!
//! The bandwidth limiter is the first consumer (`KQ-S.4`): paced sleeps
//! issued by `BandwidthLimiter::register` are typically tens of
//! microseconds long when `--bwlimit` is in the hundreds of KiB/s range,
//! exactly the range where `std::thread::sleep` jitter dominates.
//!
//! # Resolution and accuracy
//!
//! `kevent(2)` returns once the kernel timer fires; the underlying
//! `EVFILT_TIMER` is driven by Mach absolute time and supports
//! nanosecond timeouts. The actual wall-clock wait is bounded below by
//! the kernel scheduler quantum (typically ~10 us on modern Apple
//! silicon) and above by jitter from competing kqueue activity. The
//! regression test in `crates/bandwidth/tests/kqueue_sleep_backend.rs`
//! verifies the limiter stays within 10% jitter at `--bwlimit=1k`.

use std::io;
use std::os::unix::io::RawFd;
use std::time::Duration;

/// Fixed `ident` used for the single EVFILT_TIMER event each
/// [`TimerSleeper`] manages. The value is arbitrary - the timer is
/// owned by a dedicated kqueue fd so there is no namespace collision.
const TIMER_IDENT: libc::uintptr_t = 1;

/// Single-shot kqueue timer wrapped as a safe sleep primitive.
///
/// Each [`TimerSleeper`] owns a dedicated kqueue file descriptor. The
/// fd is closed when the sleeper is dropped, mirroring the
/// [`super::KqueueLoop`] ownership shape.
///
/// `TimerSleeper` is `Send` because the underlying kqueue fd is
/// per-process and movable across threads, but is not `Sync`:
/// concurrent calls to [`sleep`](Self::sleep) from multiple threads
/// would race on the timer arming and must be serialised externally.
/// The bandwidth limiter satisfies this requirement by owning one
/// sleeper per registration thread.
#[derive(Debug)]
pub struct TimerSleeper {
    kq: RawFd,
}

impl TimerSleeper {
    /// Creates a new timer-backed sleeper.
    ///
    /// Allocates a fresh kqueue file descriptor via `kqueue(2)`. The
    /// timer is not armed until the first call to
    /// [`sleep`](Self::sleep) so an idle sleeper consumes no timer
    /// resources beyond the kqueue fd itself.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if `kqueue(2)` fails (typically
    /// `EMFILE` / `ENFILE` from fd table exhaustion).
    pub fn new() -> io::Result<Self> {
        // SAFETY: `kqueue(2)` takes no arguments and returns a fresh
        // file descriptor on success or -1 on failure. There are no
        // pointer or lifetime invariants to uphold here.
        #[allow(unsafe_code)]
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { kq })
    }

    /// Sleeps for the requested duration using `EVFILT_TIMER`.
    ///
    /// Re-arms the timer with `EV_ADD | EV_ONESHOT | NOTE_NSECONDS`
    /// for the supplied duration and blocks on `kevent(2)` until the
    /// timer event is delivered. A zero or sub-nanosecond duration
    /// returns immediately without touching the kernel - matching
    /// [`std::thread::sleep`] semantics for an empty wait.
    ///
    /// `EINTR` is translated into a no-op return so callers that
    /// receive a signal during the wait observe the same semantics
    /// as `std::thread::sleep` (which silently restarts).
    ///
    /// Durations beyond `i64::MAX` nanoseconds (about 292 years) are
    /// clamped to that ceiling so the `kevent` timer payload stays
    /// within the kernel's signed-64-bit field. Real callers never
    /// exercise the clamp.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if `kevent(2)` rejects the timer
    /// registration (e.g. invalid kqueue fd) or fails for a reason
    /// other than `EINTR`.
    pub fn sleep(&self, duration: Duration) -> io::Result<()> {
        if duration.is_zero() {
            return Ok(());
        }

        let nanos = duration_to_kevent_data(duration);
        if nanos == 0 {
            return Ok(());
        }

        let change = libc::kevent {
            ident: TIMER_IDENT,
            filter: libc::EVFILT_TIMER,
            flags: libc::EV_ADD | libc::EV_ONESHOT,
            fflags: libc::NOTE_NSECONDS,
            data: nanos as libc::intptr_t,
            udata: std::ptr::null_mut(),
        };
        let mut events = [empty_kevent()];

        // SAFETY: `self.kq` is a valid kqueue fd owned for the
        // lifetime of `self`. `&change` and `events.as_mut_ptr()` are
        // borrowed for the duration of the call only. The `nchanges`
        // and `nevents` arguments match the sizes of the respective
        // buffers. The timeout argument is null so `kevent(2)` blocks
        // until the timer fires or a signal interrupts the call.
        #[allow(unsafe_code)]
        let rc = unsafe {
            libc::kevent(
                self.kq,
                &change,
                1,
                events.as_mut_ptr(),
                events.len() as i32,
                std::ptr::null(),
            )
        };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                return Ok(());
            }
            return Err(err);
        }

        if rc > 0 && events[0].flags & libc::EV_ERROR != 0 {
            let raw = events[0].data as i32;
            if raw != 0 {
                return Err(io::Error::from_raw_os_error(raw));
            }
        }

        Ok(())
    }
}

impl Drop for TimerSleeper {
    fn drop(&mut self) {
        if self.kq >= 0 {
            // SAFETY: `self.kq` was returned from `kqueue(2)` and is
            // not closed elsewhere - `TimerSleeper` owns it
            // exclusively. `close(2)` may fail but there is nothing
            // useful to do on failure in `Drop`.
            #[allow(unsafe_code)]
            unsafe {
                libc::close(self.kq);
            }
        }
    }
}

// SAFETY: A kqueue fd is per-process and can be moved across threads.
// `TimerSleeper` does not implement `Sync`; concurrent access requires
// external synchronization, matching the [`super::KqueueLoop`]
// composition rule.
#[allow(unsafe_code)]
unsafe impl Send for TimerSleeper {}

fn empty_kevent() -> libc::kevent {
    libc::kevent {
        ident: 0,
        filter: 0,
        flags: 0,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

/// Converts a `Duration` to the signed-i64 nanosecond payload used by
/// `EVFILT_TIMER` with `NOTE_NSECONDS`. Saturates at `i64::MAX` for
/// durations beyond the kernel's representable range.
fn duration_to_kevent_data(duration: Duration) -> i64 {
    let nanos = duration.as_nanos();
    if nanos > i64::MAX as u128 {
        i64::MAX
    } else {
        nanos as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn new_succeeds() {
        let sleeper = TimerSleeper::new().expect("kqueue allocation succeeds");
        // SAFETY: read-only access to a private field for assertion.
        assert!(sleeper.kq >= 0, "kqueue fd is valid");
    }

    #[test]
    fn sleep_zero_returns_immediately() {
        let sleeper = TimerSleeper::new().expect("kqueue allocation succeeds");
        let start = Instant::now();
        sleeper
            .sleep(Duration::ZERO)
            .expect("zero sleep is a no-op");
        assert!(
            start.elapsed() < Duration::from_millis(5),
            "zero sleep should not block"
        );
    }

    #[test]
    fn sleep_fifty_ms_is_within_tolerance() {
        let sleeper = TimerSleeper::new().expect("kqueue allocation succeeds");
        let target = Duration::from_millis(50);
        let start = Instant::now();
        sleeper.sleep(target).expect("kqueue timer fires");
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(40),
            "kqueue timer slept too short: elapsed={elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(100),
            "kqueue timer slept too long: elapsed={elapsed:?}"
        );
    }

    #[test]
    fn sleep_sub_millisecond_returns_promptly() {
        let sleeper = TimerSleeper::new().expect("kqueue allocation succeeds");
        let start = Instant::now();
        sleeper
            .sleep(Duration::from_micros(200))
            .expect("kqueue timer fires");
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(10),
            "sub-millisecond sleep should not block long: elapsed={elapsed:?}"
        );
    }

    #[test]
    fn sleep_can_be_reused() {
        let sleeper = TimerSleeper::new().expect("kqueue allocation succeeds");
        for _ in 0..4 {
            let start = Instant::now();
            sleeper
                .sleep(Duration::from_millis(10))
                .expect("kqueue timer fires");
            let elapsed = start.elapsed();
            assert!(
                elapsed >= Duration::from_millis(5),
                "sleep too short on reuse: elapsed={elapsed:?}"
            );
            assert!(
                elapsed < Duration::from_millis(60),
                "sleep too long on reuse: elapsed={elapsed:?}"
            );
        }
    }

    #[test]
    fn duration_to_kevent_data_handles_overflow() {
        assert_eq!(duration_to_kevent_data(Duration::ZERO), 0);
        assert_eq!(duration_to_kevent_data(Duration::from_nanos(1_000)), 1_000);
        assert_eq!(
            duration_to_kevent_data(Duration::from_millis(50)),
            50_000_000
        );
        // u128::MAX nanoseconds saturates to i64::MAX
        let huge = Duration::new(u64::MAX, 999_999_999);
        assert_eq!(duration_to_kevent_data(huge), i64::MAX);
    }
}
