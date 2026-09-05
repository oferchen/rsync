//! The bound on each peer-driven daemon handshake phase.
//!
//! upstream: clientserver.c:86-100 + io.c:143-157, 1296-1305.
//!
//! Upstream arms one deadline for the whole handshake phase and consults it
//! inside its single I/O wait. Three properties make it what it is, and all
//! three are load-bearing:
//!
//! 1. It is an ABSOLUTE deadline, not a per-read idle timeout. Upstream stamps
//!    `time(NULL) + secs` once at arm time (io.c:1298-1305), so a peer that
//!    trickles one byte at a time cannot hold the phase open indefinitely. A
//!    timer restarted on every read would be a different mechanism wearing the
//!    same name, and a trickling client defeats it by construction.
//! 2. A `timeout` may only ever SHORTEN the phase, never extend it
//!    (clientserver.c:92-100). The non-positive arm exists because `timeout` is
//!    parsed with `atoi()`, so zero and negative values are reachable from
//!    config; both mean "no configured bound", not "no bound".
//! 3. On expiry upstream DIAGNOSES then exits `RERR_TIMEOUT` (io.c:150-153),
//!    rather than dropping the socket silently.
//!
//! ⚠ This mechanism is NEW IN 3.5.0 - `grep -c daemon_handshake` over io.c and
//! clientserver.c gives 0 at the 3.4.4 pin and 6 at each 3.5.0 file. oc
//! previously left the greeting exchange untimed and cited upstream for it;
//! that citation was accurate against 3.4.4 and became false at 3.5.0.
//!
//! ⚠ Do NOT re-derive "upstream leaves the handshake untimed" from the fact
//! that `io_timeout` stays 0 until a module is selected (options.c:102). That
//! is still true at 3.5.0 and is IRRELEVANT: the handshake deadline is a
//! SEPARATE mechanism from `io_timeout`, so the premise does not carry the
//! conclusion.

use std::io::{self, BufRead, Read};
use std::net::TcpStream;
use std::num::NonZeroU64;
use std::time::{Duration, Instant};

/// Fallback bound on each peer-driven handshake phase, in seconds.
///
/// upstream: clientserver.c:90 `#define DAEMON_HANDSHAKE_TIMEOUT 60`.
pub(crate) const DAEMON_HANDSHAKE_TIMEOUT_SECS: u64 = 60;

/// Resolves a configured `timeout` to the handshake bound.
///
/// upstream: clientserver.c:92-100 `daemon_handshake_timeout()`. A configured
/// value may shorten the phase; anything outside `1..=60` falls back to the
/// full bound. `None` is upstream's `timeout <= 0` arm - `atoi()` maps an
/// absent, zero or negative directive onto it, and oc's `parse_timeout_seconds`
/// already clamps the same way, so the type carries the rule.
pub(crate) fn handshake_timeout(configured: Option<NonZeroU64>) -> Duration {
    let secs = match configured {
        Some(secs) if secs.get() <= DAEMON_HANDSHAKE_TIMEOUT_SECS => secs.get(),
        _ => DAEMON_HANDSHAKE_TIMEOUT_SECS,
    };
    Duration::from_secs(secs)
}

/// The absolute deadline for one handshake phase.
///
/// upstream: `daemon_handshake_deadline` (io.c:1296-1305) plus the check in
/// `handshake_poll_timeout_ms()` (io.c:143-157). Upstream clamps its poll
/// timeout down to the remaining time; [`state`](Self::state) is that clamp,
/// and [`DeadlineBufRead`] applies it to oc's blocking reads.
///
/// ⚠ Upstream measures with `time(NULL)`; this uses [`Instant`], which is
/// monotonic. That is a deliberate improvement, not a drift: a wall-clock step
/// during the handshake would move upstream's deadline and cannot move this
/// one. The bound each observes is otherwise identical.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HandshakeDeadline {
    deadline: Instant,
}

/// What the deadline says about the wait a caller is about to enter.
///
/// upstream: io.c:147-155 distinguishes exactly these two live cases -
/// `left <= 0` diagnoses and exits, otherwise the wait is clamped to `left`.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum DeadlineState {
    /// Time is left; a wait must be clamped to at most this long.
    Remaining(Duration),
    /// The phase is over. The caller diagnoses and exits `RERR_TIMEOUT`.
    Expired,
}

impl HandshakeDeadline {
    /// Arms the deadline for `timeout` from now.
    ///
    /// upstream: io.c:1298-1300 - `secs > 0` stamps `time(NULL) + secs`.
    pub(crate) fn armed(timeout: Duration) -> Self {
        Self {
            deadline: Instant::now() + timeout,
        }
    }

    /// The deadline's verdict on the wait about to be entered.
    pub(crate) fn state(&self) -> DeadlineState {
        match self.deadline.checked_duration_since(Instant::now()) {
            Some(left) if !left.is_zero() => DeadlineState::Remaining(left),
            _ => DeadlineState::Expired,
        }
    }

    /// Whether the deadline has elapsed.
    pub(crate) fn expired(&self) -> bool {
        self.state() == DeadlineState::Expired
    }
}

/// The diagnostic upstream prints when the handshake deadline elapses.
///
/// upstream: io.c:150-152 - `rprintf(FERROR, "[%s] daemon handshake timeout --
/// exiting\n", who_am_i())`, then `exit_cleanup(RERR_TIMEOUT)`.
pub(crate) fn handshake_timeout_message(who: &str) -> String {
    format!("[{who}] daemon handshake timeout -- exiting")
}

/// A [`BufRead`] that refuses to read past a [`HandshakeDeadline`].
///
/// upstream: io.c:143-157 `handshake_poll_timeout_ms()` is consulted INSIDE the
/// I/O wait, before every `poll()`, not once per protocol line. That placement
/// is the whole mechanism: a peer that trickles one byte every fraction of a
/// second keeps a per-line timer alive forever, and only a check on each
/// individual wait ends the phase. oc reads a blocking socket rather than
/// owning a poll loop, so the clamp is applied two ways at the same site:
/// `SO_RCVTIMEO` bounds a wait for a peer that has gone silent, and the
/// `Expired` arm bounds a peer that is still sending.
///
/// The socket handle is a `try_clone()` of the connection - a second descriptor
/// onto the same socket, so `set_read_timeout` reaches the fd being read from
/// without aliasing the borrow of the reader.
pub(crate) struct DeadlineBufRead<'a, R> {
    inner: &'a mut R,
    socket: Option<&'a TcpStream>,
    deadline: &'a HandshakeDeadline,
}

impl<'a, R> DeadlineBufRead<'a, R> {
    pub(crate) fn new(
        inner: &'a mut R,
        socket: Option<&'a TcpStream>,
        deadline: &'a HandshakeDeadline,
    ) -> Self {
        Self {
            inner,
            socket,
            deadline,
        }
    }

    /// Clamps the wait about to be entered, or refuses it outright.
    fn clamp_next_wait(&self) -> io::Result<()> {
        match self.deadline.state() {
            DeadlineState::Expired => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "daemon handshake deadline elapsed",
            )),
            DeadlineState::Remaining(left) => {
                if let Some(socket) = self.socket {
                    // A failure here only loses the silent-peer half of the
                    // bound; the `Expired` arm above still ends the phase on
                    // the next wait, so it must not fail the read.
                    let _ = socket.set_read_timeout(Some(left));
                }
                Ok(())
            }
        }
    }
}

impl<R: Read> Read for DeadlineBufRead<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.clamp_next_wait()?;
        self.inner.read(buf)
    }
}

impl<R: BufRead> BufRead for DeadlineBufRead<'_, R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.clamp_next_wait()?;
        self.inner.fill_buf()
    }

    fn consume(&mut self, amt: usize) {
        self.inner.consume(amt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    fn secs(value: u64) -> Option<NonZeroU64> {
        NonZeroU64::new(value)
    }

    #[test]
    fn a_configured_timeout_can_only_shorten_the_phase() {
        // upstream: clientserver.c:92-100 - the `> DAEMON_HANDSHAKE_TIMEOUT`
        // arm is what makes a longer configured value inert.
        assert_eq!(handshake_timeout(secs(10)), Duration::from_secs(10));
        assert_eq!(
            handshake_timeout(secs(600)),
            Duration::from_secs(DAEMON_HANDSHAKE_TIMEOUT_SECS)
        );
        assert_eq!(
            handshake_timeout(secs(DAEMON_HANDSHAKE_TIMEOUT_SECS)),
            Duration::from_secs(DAEMON_HANDSHAKE_TIMEOUT_SECS)
        );
    }

    #[test]
    fn an_unset_timeout_falls_back_to_the_full_bound() {
        // `timeout` is parsed with atoi() upstream, so zero and negative values
        // are reachable from config; oc's parser maps both to `None`, which
        // must not disarm the phase.
        assert_eq!(
            handshake_timeout(None),
            Duration::from_secs(DAEMON_HANDSHAKE_TIMEOUT_SECS)
        );
    }

    #[test]
    fn an_armed_deadline_reports_shrinking_time_and_then_expires() {
        let deadline = HandshakeDeadline::armed(Duration::from_millis(40));
        let DeadlineState::Remaining(first) = deadline.state() else {
            panic!("deadline is still in the future");
        };
        let DeadlineState::Remaining(second) = deadline.state() else {
            panic!("deadline is still in the future");
        };
        assert!(
            second <= first,
            "remaining must not grow: {second:?} > {first:?}"
        );
        assert!(first <= Duration::from_millis(40));

        while !deadline.expired() {
            std::hint::spin_loop();
        }
        assert_eq!(deadline.state(), DeadlineState::Expired);
    }

    #[test]
    fn the_deadline_is_absolute_not_restarted_by_observation() {
        // THE discriminating property: a per-read idle timeout would reset here
        // and never expire, which is exactly how a trickling client defeats
        // one. Reading `state()` repeatedly must not push the deadline out.
        let deadline = HandshakeDeadline::armed(Duration::from_millis(30));
        while !deadline.expired() {
            let _ = deadline.state();
        }
        assert!(deadline.expired());
    }

    #[test]
    fn an_expired_deadline_refuses_the_read_even_with_bytes_available() {
        // The trickling client in upstream's `daemon-handshake-timeout` cell
        // always has a byte ready, so a reader that only surfaced the socket
        // timeout would never end the phase. The guard must refuse on the
        // deadline, not on the absence of data.
        let mut source = BufReader::new(&b"@RSYNCD: 31.0"[..]);
        let deadline = HandshakeDeadline::armed(Duration::from_millis(5));
        while !deadline.expired() {
            std::hint::spin_loop();
        }

        let mut guarded = DeadlineBufRead::new(&mut source, None, &deadline);
        let error = guarded.fill_buf().expect_err("expired deadline refuses");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn a_live_deadline_passes_the_read_through_unchanged() {
        // Non-vacuity companion for the test above: without it, a guard that
        // refused every read would satisfy the expiry assertion too.
        let mut source = BufReader::new(&b"@RSYNCD: 31.0\n"[..]);
        let deadline = HandshakeDeadline::armed(Duration::from_secs(60));
        let mut guarded = DeadlineBufRead::new(&mut source, None, &deadline);
        assert_eq!(
            guarded.fill_buf().expect("live deadline allows the read"),
            b"@RSYNCD: 31.0\n"
        );
    }

    #[test]
    fn the_message_matches_upstreams_wording() {
        // The testsuite cell greps the daemon log for this text, so the
        // wording is part of the contract, not a formatting choice.
        assert_eq!(
            handshake_timeout_message("rsyncd"),
            "[rsyncd] daemon handshake timeout -- exiting"
        );
    }
}
