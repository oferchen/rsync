//! The exit funnel: the single site that owns the run's exit decision.
//!
//! Upstream routes every process exit through `_exit_cleanup`, which does three
//! things in one place: it latches the first non-zero error code at the moment
//! the error occurs (`cleanup.c:113-117`), it maps the accumulated `io_error`
//! bitfield into an `RERR_*` code (`cleanup.c:210-218`), and on an interrupt it
//! finalises the partial file before terminating (`cleanup.c:159-184`). oc
//! reaches the same decision from two threads - the transfer's normal return
//! and the signal watcher - so the pieces are gathered here, over the shared
//! [`ExitCodeLatch`], rather than in a single re-entrant C function.
//!
//! This module adds no new abstraction: it is a set of free functions over the
//! existing latch, one owner each for the error->code mapping, the
//! io_error->RERR rule, and the abort-path cleanup.

use super::{ExitCode, ExitCodeLatch};

/// Latches a decided exit `code` into `latch` at the site that mapped an error
/// (or a clean summary) to its `RERR_*` code, and returns the code unchanged.
///
/// This is oc's analog of upstream calling `_exit_cleanup(RERR_*, ...)` at the
/// error site: the code is recorded the instant the run's error->code mapping
/// decides it, before the value propagates back up to `run`'s tail. A second
/// interrupt arriving during that propagation then finds the latch already
/// populated and cannot substitute `RERR_SIGNAL` - upstream's first-writer-wins
/// invariant (`cleanup.c:113-117`), which a tail-only record would leave open
/// for the duration of the unwinding.
pub fn record_exit(latch: &ExitCodeLatch, code: i32) -> i32 {
    latch.record(code);
    code
}

/// Maps an accumulated `io_error` bitfield plus the `got_xfer_error` flag into
/// its `RERR_*` exit code, or `None` when the run is clean.
///
/// Single owner of upstream's `cleanup.c:210-218` rule: the three independent
/// `if`s that order the bits `GENERAL > VANISHED > DEL_LIMIT` (delegated to
/// [`transfer::io_error_flags::to_exit_code`]), followed by the
/// `|| got_xfer_error` clause that lifts an otherwise-clean run to
/// `RERR_PARTIAL`. Both the daemon and SSH stat converters route through here so
/// the rule cannot drift into two divergent copies.
///
/// `got_xfer_error` is not an `io_error` bit: upstream keeps it as a separate
/// global and ORs it into the `RERR_PARTIAL` test, which is the only arm a
/// missing source argument reaches (`flist.c` withholds `IOERR_GENERAL` for
/// `ENOENT`).
#[must_use]
pub fn io_error_exit_code(io_error: i32, got_xfer_error: bool) -> Option<i32> {
    let code = transfer::io_error_flags::to_exit_code(io_error);
    if code != 0 {
        Some(code)
    } else if got_xfer_error {
        Some(ExitCode::PartialTransfer.as_i32())
    } else {
        None
    }
}

/// Runs the abort-path cleanup and decides whether the caller terminates.
///
/// Mirrors the tail of upstream `_exit_cleanup` reached on `RERR_SIGNAL`: the
/// code is latched first-writer-wins, and the single [`ExitCodeLatch::claim_exit`]
/// admits exactly one entrant to the cleanup body (`cleanup.c:105`, `:126`). The
/// winner performs the interrupted-transfer cleanup - finalising each retained
/// `--partial` file (`cleanup.c:159-184`) - and returns `Some(code)` so the
/// caller can terminate the process. The loser returns `None` and must not
/// terminate: the winner already owns the exit and the loser's code, if any, was
/// latched on the way in.
///
/// The cleanup DISCONNECTS the transfer's pipeline ropes (their SPSC endpoints
/// are dropped as the process tears down, which sets the disconnect flag and
/// wakes any parked peer) and MUST NOT join the pipeline worker threads. On an
/// abort a worker may be parked on a full or empty rope indefinitely, so joining
/// it would block on progress that only the disconnect can unblock - a deadlock.
/// upstream terminates via `_exit()` and never `waitpid()`s the I/O workers on
/// this path.
pub fn abort(latch: &ExitCodeLatch, code: i32) -> Option<i32> {
    let code = latch.resolve(code);
    if !latch.claim_exit() {
        return None;
    }
    // Retention is decided here, at the one exit owner: finalise every temp the
    // interrupted transfer registered (moving `--partial` files onto their
    // destination, unlinking the rest). This neither joins nor waits on any
    // worker thread.
    engine::CleanupManager::global().finalize_partials();
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::{abort, io_error_exit_code, record_exit};
    use crate::exit_code::ExitCodeLatch;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;
    use transfer::io_error_flags::{IOERR_DEL_LIMIT, IOERR_GENERAL, IOERR_VANISHED};

    /// RERR_SIGNAL - the watcher's code for an interrupt-driven abort.
    const SIGNAL: i32 = 20;
    /// RERR_PARTIAL - a transfer that failed on its own.
    const PARTIAL: i32 = 23;

    /// 430 mutation pin. The decided RERR code is latched at the error site,
    /// so a second interrupt firing while that code is still propagating back
    /// to `run`'s tail cannot overwrite it with `RERR_SIGNAL`. Whichever path
    /// then wins the single exit claim terminates with the *error-site* code:
    /// the watcher's `abort` resolves to the latched 23, never the signal's 20.
    ///
    /// Reverting `record_exit` to a no-op at the decision site (the tail-only
    /// shape b1 shipped) turns this RED: the abort then reaches the empty latch
    /// first, latches `RERR_SIGNAL`, and terminates with 20.
    ///
    /// upstream: cleanup.c:113-117.
    #[test]
    fn error_site_latch_survives_a_second_signal_during_propagation() {
        let latch = ExitCodeLatch::new();
        // execute_transfer maps the error and latches the code immediately.
        assert_eq!(record_exit(&latch, PARTIAL), PARTIAL);
        // A second interrupt fires mid-propagation. The watcher may win the
        // claim, but it terminates with the already-latched error code, not the
        // signal code.
        assert_eq!(abort(&latch, SIGNAL), Some(PARTIAL));
        // run's tail then resolves the transfer code: still the error-site code.
        assert_eq!(latch.resolve(PARTIAL), PARTIAL);
    }

    /// Control for the pin above: with NO error-site record, the signal that
    /// arrives first is the one that wins and the run terminates with 20. This
    /// is the exact state the b1 tail-only shape left open and that 430 closes -
    /// the two tests differ only by the presence of the error-site `record_exit`.
    #[test]
    fn without_the_error_site_latch_the_first_signal_wins() {
        let latch = ExitCodeLatch::new();
        // No record_exit here - the abort reaches the empty latch first.
        assert_eq!(abort(&latch, SIGNAL), Some(SIGNAL));
        assert_eq!(latch.resolve(PARTIAL), SIGNAL);
    }

    /// 432 rule pin. The single owner of cleanup.c:210-218: the bitfield
    /// precedence GENERAL > VANISHED > DEL_LIMIT, the `|| got_xfer_error` lift
    /// to RERR_PARTIAL, and a clean run mapping to `None`.
    #[test]
    fn io_error_rule_matches_upstream() {
        assert_eq!(io_error_exit_code(0, false), None);
        assert_eq!(io_error_exit_code(0, true), Some(PARTIAL));
        assert_eq!(io_error_exit_code(IOERR_DEL_LIMIT, false), Some(25));
        assert_eq!(io_error_exit_code(IOERR_VANISHED, false), Some(24));
        assert_eq!(io_error_exit_code(IOERR_GENERAL, false), Some(23));
        // GENERAL outranks DEL_LIMIT even though DEL_LIMIT stopped the run.
        assert_eq!(
            io_error_exit_code(IOERR_DEL_LIMIT | IOERR_GENERAL, false),
            Some(23)
        );
        // A set bit outranks the got_xfer_error lift (VANISHED, not PARTIAL).
        assert_eq!(io_error_exit_code(IOERR_VANISHED, true), Some(24));
    }

    /// 433 constraint pin. On abort the funnel disconnects the pipeline rope by
    /// dropping its endpoint - which releases a parked worker - and never joins
    /// the worker thread.
    ///
    /// A real SPSC rope stands in for the network->disk pipeline: the single
    /// slot is filled, then a worker thread blocks in `send()` (queue full,
    /// consumer alive). `abort` returns its decision while that worker is still
    /// parked, proving it did not join it; dropping the receiver then
    /// disconnects the rope and releases the worker with a `SendError`. A
    /// mutation that joined the worker before disconnecting would block forever
    /// on progress only the disconnect can unblock, which the watchdog below
    /// converts into a failed assertion rather than a hung test.
    ///
    /// upstream: cleanup.c terminates via `_exit()`, never `waitpid()` on the
    /// I/O workers reached from this path.
    #[test]
    fn abort_disconnects_the_rope_and_never_joins_the_worker() {
        let (tx, rx) = transfer::pipeline::spsc::channel::<u8>(1);
        tx.send(0).expect("first push fills the single slot");
        let (parked_tx, parked_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            // Blocks: the queue is full and the consumer is still alive.
            let result = tx.send(1);
            parked_tx.send(()).ok();
            result
        });

        // The worker cannot have completed its send yet - the slot is occupied
        // and nothing has drained it - so this confirms `abort` returns without
        // waiting on the worker.
        let latch = ExitCodeLatch::new();
        assert_eq!(abort(&latch, SIGNAL), Some(SIGNAL));
        assert!(
            parked_rx.try_recv().is_err(),
            "worker must still be parked: abort must not join it"
        );

        // Disconnecting the rope (dropping the receiver) is what releases the
        // worker - not a join.
        drop(rx);
        assert!(
            parked_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the worker must be released by the rope disconnect"
        );
        assert!(
            worker.join().expect("worker thread").is_err(),
            "the parked send must fail once the rope is disconnected"
        );
    }
}
