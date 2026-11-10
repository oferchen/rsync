//! Cooperative shutdown checks for the network transfer loops.
//!
//! A signal handler only flips [`fast_io::signal`]'s process-global flag; the
//! loops that must react to it poll the flag at a boundary where stopping
//! cannot leave the wire mid-frame. This mirrors upstream, whose `sig_int`
//! records the signal in `got_kill_signal` (`rsync.c:868-870`) and whose
//! `perform_io()` acts on it only at its own loop boundaries (`io.c:849`,
//! `io.c:997`, `io.c:1022`).
//!
//! The check is *not* what unblocks a transfer parked in a transport read -
//! the client's signal watcher does that by closing the socket
//! (`core::signal::wake_blocked_io`). Polling here is what turns the resulting
//! wake-up into an ordered stop rather than a torn protocol state, and it is
//! what stops the loops on transports that carry no wakeable socket.

use std::fmt;
use std::io;

/// Source of the error [`check_shutdown`] returns; graded `RERR_SIGNAL` (20).
#[derive(Debug)]
pub struct SignalShutdown;

impl fmt::Display for SignalShutdown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("received SIGINT, SIGTERM, or SIGHUP")
    }
}

impl std::error::Error for SignalShutdown {}

/// Returns a [`SignalShutdown`] error when a shutdown signal has been received.
///
/// Call at a loop boundary or before a frame - never between the halves of a
/// wire frame. The returned error unwinds through the same teardown as a
/// dropped connection, so in-progress temp files are finalised by their guards
/// according to `--partial` / `--partial-dir`.
///
/// The kind is `Other`, not `Interrupted`: the multiplex reader calls this
/// beneath the `EINTR`-retry loops, which would spin on an `Interrupted` error.
///
/// upstream: `io.c:600 handle_kill_signal()` -> `exit_cleanup(RERR_SIGNAL)`.
#[inline]
pub fn check_shutdown() -> io::Result<()> {
    if fast_io::signal::shutdown_requested() {
        return Err(io::Error::other(SignalShutdown));
    }
    Ok(())
}

/// Whether `error` is the one [`check_shutdown`] returned.
#[must_use]
pub fn is_signal_shutdown(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<SignalShutdown>())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flag is process-global, so these two cases must not run in parallel.
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn check_is_ok_while_no_signal_pending() {
        let _serial = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        fast_io::signal::reset_shutdown_for_testing();
        assert!(check_shutdown().is_ok());
    }

    #[test]
    fn check_reports_interrupted_once_a_signal_is_pending() {
        let _serial = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        fast_io::signal::reset_shutdown_for_testing();
        fast_io::signal::mark_shutdown();
        let err = check_shutdown().expect_err("a pending shutdown must stop the loop");
        assert!(is_signal_shutdown(&err));
        // An Interrupted kind would be retried as EINTR by the frame readers.
        assert_ne!(err.kind(), io::ErrorKind::Interrupted);
        assert_eq!(crate::error::rerr_for_io_error(&err), 20);
        fast_io::signal::reset_shutdown_for_testing();
        assert!(
            check_shutdown().is_ok(),
            "clearing the flag must re-enable the loop"
        );
    }
}
