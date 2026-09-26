//! Bounded readiness wait on a single descriptor.
//!
//! Upstream rsync never blocks in `read(2)` during a daemon handshake: it
//! `poll(2)`s first with a timeout derived from the handshake deadline and
//! exits once the deadline has passed (upstream: io.c:151-178
//! `handshake_poll_timeout_ms()`). A blocking `Read` cannot be interrupted
//! from outside, so callers that need a wall-clock bound wait here before
//! each read.

use std::io;
use std::os::fd::BorrowedFd;
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, Timespec, poll};

/// Waits until `fd` is readable or `timeout` elapses.
///
/// Returns `Ok(true)` when a read would not block - data, end of file or a
/// pending error - and `Ok(false)` when the timeout expired first. An
/// interrupted wait surfaces as [`io::ErrorKind::Interrupted`] so the caller
/// re-checks its own deadline before waiting again.
///
/// # Errors
///
/// Returns the `poll(2)` error, or [`io::ErrorKind::InvalidInput`] when
/// `timeout` does not fit a `timespec`.
pub fn wait_readable(fd: BorrowedFd<'_>, timeout: Duration) -> io::Result<bool> {
    let timeout = Timespec::try_from(timeout)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "poll timeout out of range"))?;
    let mut fds = [PollFd::new(&fd, PollFlags::IN)];
    Ok(poll(&mut fds, Some(&timeout))? > 0)
}

#[cfg(test)]
mod tests {
    use super::wait_readable;
    use std::io::Write;
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    /// A silent peer must not block the caller past its timeout: this is the
    /// property the daemon connection deadline depends on.
    #[test]
    fn times_out_on_a_silent_descriptor() {
        let (_peer, ours) = UnixStream::pair().expect("socketpair");
        let start = Instant::now();
        assert!(!wait_readable(ours.as_fd(), Duration::from_millis(50)).expect("poll"));
        assert!(start.elapsed() >= Duration::from_millis(50));
    }

    #[test]
    fn reports_pending_data() {
        let (mut peer, ours) = UnixStream::pair().expect("socketpair");
        peer.write_all(b"x").expect("write");
        assert!(wait_readable(ours.as_fd(), Duration::from_secs(5)).expect("poll"));
    }

    /// End of file is readable too: a read returns 0 at once, so a closed
    /// peer must not be mistaken for a timeout.
    #[test]
    fn reports_end_of_file() {
        let (peer, ours) = UnixStream::pair().expect("socketpair");
        drop(peer);
        assert!(wait_readable(ours.as_fd(), Duration::from_secs(5)).expect("poll"));
    }
}
