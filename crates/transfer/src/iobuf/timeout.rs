//! The connection's `--timeout`, checked each time a wait comes back empty
//! (upstream: io.c:229-268 check_timeout(), called from io.c:876 and io.c:335).

use std::io;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Longest single wait, in seconds (upstream: rsync.h SELECT_TIMEOUT).
const SELECT_TIMEOUT_SECS: u64 = 60;

/// The clock the timeout reads; a seam so tests can drive it.
pub(crate) type Clock = Rc<dyn Fn() -> Instant>;

/// The `--timeout` state of one connection.
///
/// Every core is built with one, so a transport that can wait never runs
/// without the configured timeout: `None` means the user set no timeout,
/// never that nobody wired it in.
pub(crate) struct IoTimeout {
    limit: Option<Duration>,
    who: &'static str,
    clock: Clock,
    /// The last transfer I/O; `None` until the first I/O or the first
    /// empty wait (upstream: io.c:254-255 `if (!last_io_in) last_io_in = t`).
    last_io: Option<Instant>,
}

impl IoTimeout {
    /// The timeout of `--timeout=secs` (`None` or 0 disables it), reported as
    /// `who` (upstream: rsync.c:987 who_am_i()).
    pub(crate) fn new(secs: Option<u64>, who: &'static str) -> Self {
        Self::with_clock(secs, who, Rc::new(Instant::now))
    }

    /// [`IoTimeout::new`] reading `clock`.
    pub(crate) fn with_clock(secs: Option<u64>, who: &'static str, clock: Clock) -> Self {
        Self {
            limit: secs.filter(|&s| s > 0).map(Duration::from_secs),
            who,
            clock,
            last_io: None,
        }
    }

    /// How long one wait may last: `ceil(timeout / 2)`, at most a minute, or
    /// unbounded with no timeout, where upstream's wake-ups do nothing
    /// (upstream: io.c:1306-1310 set_io_timeout(), io.c:143 poll_timeout_ms()).
    pub(crate) fn wait_bound(&self) -> Option<Duration> {
        self.limit.map(|limit| {
            let lull = limit.as_secs().div_ceil(2);
            Duration::from_secs(lull.min(SELECT_TIMEOUT_SECS))
        })
    }

    /// Records transfer I/O (upstream: io.c:934 `last_io_in`, io.c:976
    /// `last_io_out`).
    pub(crate) fn mark_io(&mut self) {
        if self.limit.is_some() {
            self.last_io = Some((self.clock)());
        }
    }

    /// Called when a wait ended with nothing ready: fails once the connection
    /// has been idle for the whole timeout, with upstream's message.
    pub(crate) fn check(&mut self) -> io::Result<()> {
        let Some(limit) = self.limit else {
            return Ok(());
        };
        let now = (self.clock)();
        let last = *self.last_io.get_or_insert(now);
        let idle = now.saturating_duration_since(last);
        if idle < limit {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "[{}] io timeout after {} seconds -- exiting",
                self.who,
                idle.as_secs()
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn fake_clock() -> (Clock, Rc<Cell<Duration>>) {
        let base = Instant::now();
        let offset = Rc::new(Cell::new(Duration::ZERO));
        let reading = Rc::clone(&offset);
        (Rc::new(move || base + reading.get()), offset)
    }

    /// WHY: the wait bound is upstream's select_timeout, so keep-alive and
    /// timeout checks happen on its cadence.
    #[test]
    fn the_wait_bound_is_half_the_timeout_rounded_up_and_at_most_a_minute() {
        let bound = |secs| IoTimeout::new(secs, "sender").wait_bound();
        assert_eq!(bound(None), None);
        assert_eq!(bound(Some(0)), None, "0 disables the timeout");
        assert_eq!(bound(Some(1)), Some(Duration::from_secs(1)));
        assert_eq!(bound(Some(5)), Some(Duration::from_secs(3)));
        assert_eq!(bound(Some(1000)), Some(Duration::from_secs(60)));
    }

    /// WHY: the idle clock starts at the first empty wait, not at
    /// construction, and I/O restarts it - so the timeout fires on upstream's
    /// third empty one-second wait for `--timeout=2`.
    #[test]
    fn the_timeout_fires_after_a_full_idle_interval_since_the_last_io() {
        let (clock, offset) = fake_clock();
        let mut timeout = IoTimeout::with_clock(Some(2), "Receiver", clock);
        offset.set(Duration::from_secs(1));
        timeout.check().unwrap();
        offset.set(Duration::from_secs(2));
        timeout.check().unwrap();
        timeout.mark_io();
        offset.set(Duration::from_secs(3));
        timeout.check().unwrap();
        offset.set(Duration::from_secs(4));
        let err = timeout.check().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            err.to_string(),
            "[Receiver] io timeout after 2 seconds -- exiting"
        );
    }

    /// WHY: without `--timeout` an idle connection is never declared dead.
    #[test]
    fn no_timeout_never_fires() {
        let (clock, offset) = fake_clock();
        let mut timeout = IoTimeout::with_clock(None, "sender", clock);
        offset.set(Duration::from_secs(1 << 40));
        timeout.check().unwrap();
    }
}
