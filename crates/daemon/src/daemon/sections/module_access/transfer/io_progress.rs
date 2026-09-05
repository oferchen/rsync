// The transfer-phase idle deadline: upstream's `check_timeout()` clock.
//
// upstream: io.c:211-250 `check_timeout()`. Three clauses of that function are
// load-bearing and all three are reproduced here:
//
//  1. `if (!io_timeout) return;` - an unset timeout means NO check at all.
//     [`TransferDeadline::arm`] returns `None` for `None`, so an unconfigured
//     module has no deadline object to consult and pays no clock cost.
//  2. `if (am_receiver) return;` - the RECEIVER never times out. Upstream's own
//     comment (io.c:215-225) explains why: a receiver can spend a long time
//     hashing without touching the socket, so timing it out would abort healthy
//     work. The role gate lives at the call site, which is the only place that
//     knows the negotiated role.
//  3. `chk = MAX(last_io_out, last_io_in)` - idleness is measured from the
//     LATER of the last read and the last WRITE. [`IoProgress`] is therefore a
//     single cell stamped by BOTH directions; the maximum falls out of "most
//     recent write to the cell" without a second field.
//
// ⚠ Clause 3 is why this type exists at all rather than a read-side timer. A
// deadline measured on reads alone would abort a perfectly healthy large-file
// send whose generator happens to be quiet - an availability regression, not a
// timeout fix. The drain thread sees only reads, so it cannot be the sole
// source of truth; [`ProgressWriter`] supplies the write half.
//
// ⚠ This deadline is deliberately NOT `SO_RCVTIMEO`. The daemon hands the same
// socket to several `try_clone()` descriptors, and a socket option is shared by
// every one of them: whichever clone writes it last wins. The background drain
// arms its own short `SO_RCVTIMEO` as a POLL CADENCE, which silently replaced
// the reconciled session timeout stored there (measured by strace: the drain's
// `fcntl(fd, F_DUPFD_CLOEXEC)` clone writes 50 ms over the session's 5 s, 173us
// later). Keeping the deadline in userspace against a monotonic `Instant` makes
// the two quantities independent by construction.
//
// This file is `include!`d into the `crate::daemon` scope, so it declares no
// `use` items and fully qualifies the types `daemon.rs` does not already
// import (`AtomicU64`, `Instant`), matching `draining_reader.rs`'s handling of
// the `mpsc` types it adds.

/// Shared "last I/O made progress" clock, upstream's `MAX(last_io_out, last_io_in)`.
///
/// Stamped by every successful read AND every successful write on the transfer
/// socket, so the value is always the more recent of the two directions.
#[derive(Debug)]
struct IoProgress {
    /// Session start; every stamp is stored relative to this.
    epoch: std::time::Instant,
    /// Milliseconds since `epoch` at the last successful read or write.
    last_millis: std::sync::atomic::AtomicU64,
}

impl IoProgress {
    fn new() -> Self {
        Self {
            epoch: std::time::Instant::now(),
            last_millis: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Records that I/O just made progress in either direction.
    ///
    /// upstream: io.c sets `last_io_in` / `last_io_out` at its read and write
    /// sites; one cell holds the maximum because the later stamp overwrites the
    /// earlier one.
    fn mark(&self) {
        let now = u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
        // `fetch_max` rather than `store`: the drain reader and the transfer
        // writer stamp this concurrently, and a plain store could move the clock
        // BACKWARDS if a slightly stale thread lands after a newer one.
        // Backwards would EXTEND the deadline, which is the failure direction
        // that matters.
        self.last_millis
            .fetch_max(now, std::sync::atomic::Ordering::Relaxed);
    }

    /// How long since either direction last made progress.
    fn idle_for(&self) -> Duration {
        let last = self.last_millis.load(std::sync::atomic::Ordering::Relaxed);
        self.epoch
            .elapsed()
            .saturating_sub(Duration::from_millis(last))
    }
}

/// The reconciled transfer timeout, paired with the clock it is measured against.
///
/// upstream: io.c:243 `if (t - chk >= io_timeout)`. Constructed only when the
/// session actually has a timeout AND the local role is one upstream checks, so
/// its mere existence is clause 1 and clause 2 already decided.
#[derive(Debug, Clone)]
struct TransferDeadline {
    progress: Arc<IoProgress>,
    timeout: Duration,
}

impl TransferDeadline {
    /// Arms the deadline, or returns `None` when upstream would not check.
    ///
    /// upstream: io.c:226-227 - `if (!io_timeout) return;`. `None` in, `None`
    /// out: an unconfigured `timeout` leaves the transfer phase unbounded, which
    /// is upstream's default (`io_timeout = 0`). The clock is owned by the
    /// deadline rather than passed alongside it, so a session with no timeout
    /// never allocates or stamps one.
    fn arm(timeout: Option<Duration>) -> Option<Self> {
        timeout.map(|timeout| Self {
            progress: Arc::new(IoProgress::new()),
            timeout,
        })
    }

    /// The clock both directions stamp.
    fn progress(&self) -> Arc<IoProgress> {
        Arc::clone(&self.progress)
    }

    /// The idle interval, if it has reached the configured timeout.
    ///
    /// upstream: io.c:243 compares with `>=`, and io.c:246 reports the MEASURED
    /// elapsed time `(int)(t - chk)`, not the configured value - so the caller
    /// gets the observed interval to put in the diagnostic.
    fn expired(&self) -> Option<Duration> {
        let idle = self.progress.idle_for();
        (idle >= self.timeout).then_some(idle)
    }
}

/// Stamps [`IoProgress`] on every successful write - the `last_io_out` half.
///
/// upstream: io.c updates `last_io_out` in its unbuffered write path, so a
/// session that is streaming data out is never idle even while the peer is
/// silent. Without this wrapper the deadline would measure reads only and abort
/// a healthy single-file send whose peer has nothing to say.
struct ProgressWriter<W: Write> {
    inner: W,
    progress: Arc<IoProgress>,
}

impl<W: Write> Write for ProgressWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        // Only a byte actually accepted by the peer's socket buffer is progress;
        // a zero-length write moved nothing and must not refresh the clock.
        if written > 0 {
            self.progress.mark();
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Wraps `writer` so successful writes stamp the deadline's clock.
///
/// Returns `writer` untouched when there is no deadline: upstream's clause 1
/// means an unconfigured session has nothing to measure, so it pays nothing.
fn writer_marking_progress(
    writer: Box<dyn Write + Send>,
    deadline: Option<&TransferDeadline>,
) -> Box<dyn Write + Send> {
    match deadline {
        Some(deadline) => Box::new(ProgressWriter {
            inner: writer,
            progress: deadline.progress(),
        }),
        None => writer,
    }
}

/// The diagnostic upstream prints when the transfer timeout elapses.
///
/// upstream: io.c:246-247 - `rprintf(FERROR, "[%s] io timeout after %d seconds
/// -- exiting\n", who_am_i(), (int)(t-chk))`, then `exit_cleanup(RERR_TIMEOUT)`.
/// The seconds are the MEASURED idle interval, truncated to whole seconds by
/// upstream's `(int)` cast on a `time_t` difference.
fn io_timeout_message(who: &str, idle: Duration) -> String {
    format!(
        "[{who}] io timeout after {} seconds -- exiting",
        idle.as_secs()
    )
}

#[cfg(test)]
mod io_progress_tests {
    //! Clause-by-clause tests for upstream's `check_timeout()` (io.c:211-250).
    //!
    //! Every assertion here is condition-based rather than sleep-based: the
    //! clock is driven by a spin on its own `elapsed()`, and expiry is decided
    //! by the configured bound, so no test depends on a scheduler window.

    use super::{IoProgress, TransferDeadline, io_timeout_message, writer_marking_progress};
    use std::io::Write;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    /// A bound no test run can reach, standing in for "the session is healthy".
    const NEVER: Duration = Duration::from_secs(3600);
    /// A bound every observation has already passed, standing in for "idle".
    const ALWAYS: Duration = Duration::ZERO;

    /// Spins until the clock has advanced far enough that a stamp is
    /// distinguishable from the epoch. Condition-based, so it cannot flake the
    /// way a fixed sleep can.
    fn advance_past_one_millisecond(progress: &IoProgress) {
        while progress.epoch.elapsed() < Duration::from_millis(2) {
            std::hint::spin_loop();
        }
    }

    #[test]
    fn an_unset_timeout_arms_no_deadline() {
        // upstream: io.c:226-227 `if (!io_timeout) return;` - with no timeout
        // there is nothing to check, so no deadline object exists at all and
        // the transfer phase stays unbounded.
        assert!(TransferDeadline::arm(None).is_none());
    }

    #[test]
    fn a_configured_timeout_arms_a_deadline() {
        // Non-vacuity companion for the clause above: the `None` result must
        // come from the absent timeout, not from `arm` never succeeding.
        assert!(TransferDeadline::arm(Some(NEVER)).is_some());
    }

    #[test]
    fn a_deadline_below_the_bound_has_not_expired() {
        let deadline = TransferDeadline::arm(Some(NEVER)).expect("armed");
        assert!(deadline.expired().is_none());
    }

    #[test]
    fn a_deadline_at_or_past_the_bound_has_expired() {
        // upstream: io.c:243 compares with `>=`, so an idle interval equal to
        // the bound already counts as expired.
        let deadline = TransferDeadline::arm(Some(ALWAYS)).expect("armed");
        assert!(deadline.expired().is_some());
    }

    #[test]
    fn a_successful_write_stamps_the_clock() {
        // upstream: io.c:242 `chk = MAX(last_io_out, last_io_in)` - the WRITE
        // half. Without this the deadline would measure reads alone and abort
        // a healthy send whose peer is simply quiet.
        let deadline = TransferDeadline::arm(Some(NEVER)).expect("armed");
        let progress = deadline.progress();
        advance_past_one_millisecond(&progress);

        let mut writer = writer_marking_progress(Box::new(Vec::new()), Some(&deadline));
        writer.write_all(b"delta").expect("write");

        assert!(
            progress.last_millis.load(Ordering::Relaxed) > 0,
            "a successful write must refresh the progress clock"
        );
    }

    #[test]
    fn time_alone_does_not_stamp_the_clock() {
        // Non-vacuity companion: proves the stamp above came from the write and
        // not from the spin, which would make that assertion meaningless.
        let progress = Arc::new(IoProgress::new());
        advance_past_one_millisecond(&progress);
        assert_eq!(progress.last_millis.load(Ordering::Relaxed), 0);
    }

    /// A sink whose bytes stay readable after the writer is boxed away, so the
    /// transparency assertion is on the delivered bytes rather than on the
    /// absence of a panic.
    #[derive(Clone, Default)]
    struct RecordingSink(Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for RecordingSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("sink lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_wrapped_writer_is_byte_transparent() {
        // Stamping must not disturb the wire: the wrapper forwards every byte.
        let deadline = TransferDeadline::arm(Some(NEVER)).expect("armed");
        let sink = RecordingSink::default();
        let mut writer = writer_marking_progress(Box::new(sink.clone()), Some(&deadline));
        writer.write_all(b"@RSYNCD: OK\n").expect("write");
        writer.flush().expect("flush");
        assert_eq!(&sink.0.lock().expect("sink lock")[..], b"@RSYNCD: OK\n");
    }

    #[test]
    fn an_unarmed_writer_is_byte_transparent_too() {
        // The `None` arm returns the writer untouched, so a session with no
        // timeout keeps exactly the wire behaviour it had before.
        let sink = RecordingSink::default();
        let mut writer = writer_marking_progress(Box::new(sink.clone()), None);
        writer.write_all(b"@RSYNCD: OK\n").expect("write");
        assert_eq!(&sink.0.lock().expect("sink lock")[..], b"@RSYNCD: OK\n");
    }

    #[test]
    fn the_diagnostic_reports_the_measured_interval() {
        // upstream: io.c:246 prints `(int)(t - chk)` - the OBSERVED idle time,
        // not the configured timeout, truncated to whole seconds.
        assert_eq!(
            io_timeout_message("rsyncd", Duration::from_millis(7_900)),
            "[rsyncd] io timeout after 7 seconds -- exiting"
        );
    }
}
