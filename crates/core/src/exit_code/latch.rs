//! First-writer-wins exit-code latch.
//!
//! Upstream funnels every process exit through `_exit_cleanup`, which keeps
//! its state in function-local statics: the first caller that supplies a
//! non-zero code decides the process exit code, and re-entering the function
//! (recursion from a cleanup step, or a signal handler firing mid-cleanup)
//! can neither overwrite that code nor re-run the cleanup steps.
//!
//! upstream: cleanup.c:113-117 - `if (!exit_code) { exit_code = code; ... }`
//! preserves the first error's exit info when recursing, and
//! cleanup.c:210-212 lets a later non-zero code fill in when the first entry
//! carried 0. upstream: cleanup.c:105 + cleanup.c:126 - the `switch_step`
//! static makes the cleanup body single-entrant.
//!
//! oc reaches the exit decision from two threads instead of from recursion:
//! the transfer's normal return path and the signal watcher thread race to
//! decide the process exit code. The static latch therefore becomes an
//! atomic, and the `switch_step` single-entrant rule becomes [`claim_exit`]:
//! whichever thread claims first performs process termination, and the loser
//! defers so it cannot cut off the winner's final output mid-write.
//!
//! [`claim_exit`]: ExitCodeLatch::claim_exit

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// First-writer-wins latch for the process exit code.
///
/// Zero is the "unset" sentinel, exactly as in upstream's `exit_code` static:
/// recording 0 never latches, so a clean first entrant leaves the latch open
/// for a later error (upstream: cleanup.c:210-212).
#[derive(Debug)]
pub struct ExitCodeLatch {
    /// The latched exit code; 0 means no error has been recorded yet.
    code: AtomicI32,
    /// Whether a thread has already claimed process termination.
    claimed: AtomicBool,
}

impl ExitCodeLatch {
    /// Creates an empty latch.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            code: AtomicI32::new(0),
            claimed: AtomicBool::new(false),
        }
    }

    /// Records `code` if no non-zero code has been recorded yet.
    ///
    /// upstream: cleanup.c:113-117 - only the first non-zero writer wins;
    /// a zero `code` never latches.
    pub fn record(&self, code: i32) {
        if code != 0 {
            let _ = self
                .code
                .compare_exchange(0, code, Ordering::SeqCst, Ordering::SeqCst);
        }
    }

    /// Records `code`, then returns the code the process must exit with:
    /// the latched value if any writer has latched one, otherwise `code`.
    ///
    /// upstream: cleanup.c:113-117 (first writer wins) combined with
    /// cleanup.c:210-212 (a zero first entry takes a later non-zero code).
    pub fn resolve(&self, code: i32) -> i32 {
        self.record(code);
        let latched = self.code.load(Ordering::SeqCst);
        if latched != 0 { latched } else { code }
    }

    /// Claims process termination; returns `true` for the first caller only.
    ///
    /// upstream: cleanup.c:105 + cleanup.c:126 - `switch_step` runs the
    /// cleanup body once no matter how often `_exit_cleanup` is re-entered.
    /// In oc's threaded shape the second entrant is another thread, so the
    /// loser must not call `process::exit` at all: it defers to the winner,
    /// whose latched code already includes anything the loser recorded.
    pub fn claim_exit(&self) -> bool {
        !self.claimed.swap(true, Ordering::SeqCst)
    }

    /// Resets the latch for a fresh top-level invocation.
    ///
    /// Upstream never needs this because every run is a fresh process with
    /// fresh statics. `cli::run` is a reentrant library entry point (tests and
    /// embedders call it repeatedly in one process), so each client run starts
    /// a new latch epoch to keep those invocations independent.
    pub fn reset(&self) {
        self.code.store(0, Ordering::SeqCst);
        self.claimed.store(false, Ordering::SeqCst);
    }
}

impl Default for ExitCodeLatch {
    fn default() -> Self {
        Self::new()
    }
}

/// The process-wide latch shared by the normal return path and the signal
/// watcher thread.
#[must_use]
pub fn process_latch() -> &'static ExitCodeLatch {
    static LATCH: ExitCodeLatch = ExitCodeLatch::new();
    &LATCH
}

#[cfg(test)]
mod tests {
    use super::ExitCodeLatch;

    #[test]
    fn first_nonzero_writer_wins() {
        let latch = ExitCodeLatch::new();
        latch.record(23);
        latch.record(20);
        assert_eq!(latch.resolve(0), 23);
    }

    #[test]
    fn zero_never_latches_so_a_later_error_fills_in() {
        // upstream: cleanup.c:210-212 - a clean first entrant leaves the
        // latch open for a later non-zero code.
        let latch = ExitCodeLatch::new();
        assert_eq!(latch.resolve(0), 0);
        latch.record(20);
        assert_eq!(latch.resolve(0), 20);
    }

    #[test]
    fn resolve_latches_its_own_nonzero_code() {
        let latch = ExitCodeLatch::new();
        assert_eq!(latch.resolve(23), 23);
        assert_eq!(latch.resolve(20), 23);
    }

    #[test]
    fn claim_exit_admits_exactly_one_claimant() {
        let latch = ExitCodeLatch::new();
        assert!(latch.claim_exit());
        assert!(!latch.claim_exit());
    }

    #[test]
    fn reset_opens_a_fresh_epoch() {
        let latch = ExitCodeLatch::new();
        assert_eq!(latch.resolve(23), 23);
        assert!(latch.claim_exit());
        latch.reset();
        assert_eq!(latch.resolve(0), 0);
        assert!(latch.claim_exit());
    }
}
