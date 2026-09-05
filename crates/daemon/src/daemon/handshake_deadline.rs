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
//! 2. A module's `timeout` can only ever SHORTEN the phase, never extend it
//!    (clientserver.c:92-100). The `<= 0` arm exists because `timeout` is
//!    parsed with `atoi()`, so negative values are reachable from config.
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

use std::time::{Duration, Instant};

/// Fallback bound on each peer-driven handshake phase, in seconds.
///
/// upstream: clientserver.c:90 `#define DAEMON_HANDSHAKE_TIMEOUT 60`.
pub(crate) const DAEMON_HANDSHAKE_TIMEOUT_SECS: u64 = 60;

/// Resolves a module's configured `timeout` to the handshake bound.
///
/// upstream: clientserver.c:92-100 `daemon_handshake_timeout()`. A module value
/// may shorten the phase; anything outside `1..=60` - including the negative
/// values `atoi()` admits - falls back to the full bound.
pub(crate) fn handshake_timeout(module_timeout: i32) -> Duration {
    let secs = if module_timeout <= 0 || module_timeout as u64 > DAEMON_HANDSHAKE_TIMEOUT_SECS {
        DAEMON_HANDSHAKE_TIMEOUT_SECS
    } else {
        module_timeout as u64
    };
    Duration::from_secs(secs)
}

/// The absolute deadline for one handshake phase.
///
/// upstream: `daemon_handshake_deadline` (io.c:1296-1305) plus the check in
/// `handshake_poll_timeout_ms()` (io.c:143-157). Upstream clamps its poll
/// timeout down to the remaining time; [`remaining`](Self::remaining) is that
/// clamp, and callers apply it to whatever wait they own.
///
/// ⚠ Upstream measures with `time(NULL)`; this uses [`Instant`], which is
/// monotonic. That is a deliberate improvement, not a drift: a wall-clock step
/// during the handshake would move upstream's deadline and cannot move this
/// one. The bound each observes is otherwise identical.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HandshakeDeadline {
    deadline: Instant,
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

    /// Time left before the phase must end, or `None` once it has elapsed.
    ///
    /// upstream: io.c:147-155 - `left <= 0` diagnoses and exits; otherwise the
    /// wait is clamped down to `left`.
    pub(crate) fn remaining(&self) -> Option<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
    }

    /// Whether the deadline has elapsed.
    pub(crate) fn expired(&self) -> bool {
        self.remaining().is_none()
    }
}

/// The diagnostic upstream prints when the handshake deadline elapses.
///
/// upstream: io.c:150-152 - `rprintf(FERROR, "[%s] daemon handshake timeout --
/// exiting\n", who_am_i())`, then `exit_cleanup(RERR_TIMEOUT)`.
pub(crate) fn handshake_timeout_message(who: &str) -> String {
    format!("[{who}] daemon handshake timeout -- exiting")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_module_timeout_can_only_shorten_the_phase() {
        // upstream: clientserver.c:92-100 - the `> DAEMON_HANDSHAKE_TIMEOUT`
        // arm is what makes a longer module value inert.
        assert_eq!(handshake_timeout(10), Duration::from_secs(10));
        assert_eq!(
            handshake_timeout(600),
            Duration::from_secs(DAEMON_HANDSHAKE_TIMEOUT_SECS)
        );
        assert_eq!(
            handshake_timeout(DAEMON_HANDSHAKE_TIMEOUT_SECS as i32),
            Duration::from_secs(DAEMON_HANDSHAKE_TIMEOUT_SECS)
        );
    }

    #[test]
    fn a_nonpositive_module_timeout_falls_back_to_the_full_bound() {
        // `timeout` is parsed with atoi() upstream, so negatives are reachable
        // from config and must not disarm or invert the bound.
        for value in [0, -1, -600, i32::MIN] {
            assert_eq!(
                handshake_timeout(value),
                Duration::from_secs(DAEMON_HANDSHAKE_TIMEOUT_SECS),
                "module timeout {value} must fall back to the full bound"
            );
        }
    }

    #[test]
    fn an_armed_deadline_reports_shrinking_time_and_then_expires() {
        let deadline = HandshakeDeadline::armed(Duration::from_millis(40));
        let first = deadline
            .remaining()
            .expect("deadline is still in the future");
        let second = deadline
            .remaining()
            .expect("deadline is still in the future");
        assert!(
            second <= first,
            "remaining must not grow: {second:?} > {first:?}"
        );
        assert!(first <= Duration::from_millis(40));

        while !deadline.expired() {
            std::hint::spin_loop();
        }
        assert_eq!(deadline.remaining(), None);
    }

    #[test]
    fn the_deadline_is_absolute_not_restarted_by_observation() {
        // THE discriminating property: a per-read idle timeout would reset here
        // and never expire, which is exactly how a trickling client defeats
        // one. Reading `remaining()` repeatedly must not push the deadline out.
        let deadline = HandshakeDeadline::armed(Duration::from_millis(30));
        while !deadline.expired() {
            let _ = deadline.remaining();
        }
        assert!(deadline.expired());
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
