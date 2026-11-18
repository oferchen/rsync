//! A pipe the shutdown flag rings, so a wait on any thread sees a signal.
//!
//! A process-directed signal is delivered to one thread that does not block
//! it, which need not be the thread waiting on the wire, so `EINTR` cannot be
//! relied on to end that wait. Upstream is single-threaded and does rely on it
//! (`io.c:866-884` treats an interrupted `poll` as a timeout, then
//! `io.c:849-850` reads `got_kill_signal`). [`mark_shutdown`] therefore also
//! writes a byte here, and a transport polls [`wake_fd`] next to its own
//! descriptors, so the wait returns whichever thread took the signal.
//!
//! Only the first shutdown writes, so the pipe holds at most one byte and
//! never needs draining. The byte is left unread on purpose: a shutdown is
//! final, and a readable wake descriptor makes every later wait on every
//! thread return at once. A waiter that consumed it could race a second
//! waiter between that waiter's flag check and its `poll`, and park it.
//!
//! [`mark_shutdown`]: super::mark_shutdown

use std::io::{self, PipeReader, PipeWriter, Read};
use std::os::fd::{AsFd, BorrowedFd};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};

/// The read end (polled) and the write end (rung) of the wake pipe.
static WAKE: OnceLock<(PipeReader, PipeWriter)> = OnceLock::new();

/// Set by the first [`ring`]; later shutdowns find the byte already there.
static RUNG: AtomicBool = AtomicBool::new(false);

/// Creates the wake pipe. Call before installing a handler that calls
/// [`mark_shutdown`](super::mark_shutdown); later calls do nothing.
///
/// Both ends are `O_NONBLOCK`: the handler's write must never block, and a
/// wait must never block reading the pipe.
///
/// # Errors
///
/// Returns the `pipe(2)` or `fcntl(2)` error.
pub fn arm_wake() -> io::Result<()> {
    if WAKE.get().is_some() {
        return Ok(());
    }
    let (read_end, write_end) = io::pipe()?;
    set_nonblocking(&read_end)?;
    set_nonblocking(&write_end)?;
    let _ = WAKE.set((read_end, write_end));
    Ok(())
}

fn set_nonblocking(fd: impl AsFd) -> io::Result<()> {
    let flags = fcntl_getfl(&fd)?;
    fcntl_setfl(&fd, flags | OFlags::NONBLOCK)?;
    Ok(())
}

/// The descriptor a transport adds to its poll set, once [`arm_wake`] ran.
/// It becomes readable when a shutdown is requested and stays readable.
#[must_use]
pub fn wake_fd() -> Option<BorrowedFd<'static>> {
    WAKE.get().map(|(read_end, _)| read_end.as_fd())
}

/// Writes the wake byte on the first shutdown. Async-signal-safe:
/// `OnceLock::get` is an atomic load, the latch an atomic swap, and the write
/// one `write(2)` on a non-blocking descriptor whose result (`EAGAIN`
/// included) is ignored.
pub(super) fn ring() {
    if let Some((_, write_end)) = WAKE.get()
        && !RUNG.swap(true, Ordering::SeqCst)
    {
        let _ = rustix::io::write(write_end, &[1]);
    }
}

/// Empties the wake pipe and rearms the latch, for tests that reset the
/// shutdown flag.
pub(super) fn drain() {
    if let Some((read_end, _)) = WAKE.get() {
        let mut reader = read_end;
        let mut buf = [0u8; 64];
        while matches!(reader.read(&mut buf), Ok(n) if n > 0) {}
    }
    RUNG.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::event::{PollFd, PollFlags, Timespec, poll};

    fn wake_readable() -> bool {
        let fd = wake_fd().expect("armed");
        let mut fds = [PollFd::from_borrowed_fd(fd, PollFlags::IN)];
        poll(&mut fds, Some(&Timespec::default())).expect("poll");
        fds[0].revents().contains(PollFlags::IN)
    }

    fn queued_bytes() -> usize {
        let (read_end, _) = WAKE.get().expect("armed");
        let mut reader = read_end;
        let mut buf = [0u8; 64];
        let mut total = 0;
        while let Ok(n @ 1..) = reader.read(&mut buf) {
            total += n;
        }
        total
    }

    /// The wake pipe is quiet until a shutdown is requested and stays
    /// readable after it, so every later wait returns at once.
    #[test]
    fn marking_a_shutdown_makes_the_wake_descriptor_readable() {
        arm_wake().expect("arm");
        super::super::reset_shutdown_for_testing();
        assert!(!wake_readable());
        super::super::mark_shutdown();
        assert!(wake_readable());
        assert!(wake_readable(), "the byte is not consumed by a wait");
        super::super::reset_shutdown_for_testing();
        assert!(!wake_readable());
    }

    /// Repeated signals write one byte in all, so an undrained pipe can
    /// never fill and the handler's write never meets a full pipe.
    #[test]
    fn repeated_shutdowns_queue_a_single_byte() {
        arm_wake().expect("arm");
        super::super::reset_shutdown_for_testing();
        for _ in 0..1000 {
            super::super::mark_shutdown();
        }
        assert_eq!(queued_bytes(), 1);
        super::super::reset_shutdown_for_testing();
    }
}
