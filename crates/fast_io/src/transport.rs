//! Non-blocking transfer transports with a readiness wait.
//!
//! Upstream drives every transfer byte through one `perform_io()` loop that
//! `poll(2)`s the input and output descriptors together, reading whatever has
//! arrived while it waits to write (upstream: io.c:647-1042). That needs a
//! transport that can be asked "would this read or write block?" and "wake me
//! when either side is ready". [`Transport`] is that contract; every
//! connection kind implements it once, so the loop above it has one shape.
//!
//! The descriptors are switched to non-blocking mode when the transport is
//! built, as upstream does once at session start (upstream: main.c:1304-1305
//! for a server, main.c:1349-1350 for a client, util1.c:53
//! set_nonblocking()).

use std::io;
use std::time::Duration;

/// Which directions a [`Transport::wait`] found ready.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Readiness {
    /// A read would not block: data, end of file or a pending error.
    pub readable: bool,
    /// A write would not block, or would fail at once.
    pub writable: bool,
}

/// A connection that reads and writes without blocking and can wait for
/// either direction.
///
/// Reads and writes never block: a call that would is answered `Ok(None)`.
/// The only blocking call is [`Transport::wait`], which sleeps until a
/// requested direction is ready or the timeout passes.
pub trait Transport {
    /// Reads into `buf` without blocking.
    ///
    /// Returns `Ok(Some(n))` for `n` bytes read, `Ok(Some(0))` at end of file
    /// and `Ok(None)` when no byte is available yet.
    ///
    /// # Errors
    ///
    /// Returns the transport's read error.
    fn try_read(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>>;

    /// Writes from `buf` without blocking.
    ///
    /// Returns `Ok(Some(n))` for `n` bytes accepted and `Ok(None)` when the
    /// transport has no room yet.
    ///
    /// # Errors
    ///
    /// Returns the transport's write error, such as a broken pipe.
    fn try_write(&mut self, buf: &[u8]) -> io::Result<Option<usize>>;

    /// Blocks until input is readable (when `input`), output is writable (when
    /// `output`), or `timeout` passes; `None` waits without a bound.
    ///
    /// A timeout returns a [`Readiness`] with neither flag set.
    ///
    /// # Errors
    ///
    /// Returns the wait error. An invalid descriptor is
    /// [`io::ErrorKind::InvalidInput`], which upstream treats as fatal
    /// (upstream: io.c:858-866 POLLNVAL).
    fn wait(
        &mut self,
        input: bool,
        output: bool,
        timeout: Option<Duration>,
    ) -> io::Result<Readiness>;

    /// Pushes out anything the transport itself holds back. A descriptor
    /// holds nothing, so the default does nothing.
    ///
    /// # Errors
    ///
    /// Returns the transport's flush error.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A [`Transport`] over a blocking reader and writer that cannot be polled:
/// an in-memory stream, or a caller-supplied `Read`/`Write` pair.
///
/// Its reads and writes block, so a wait never offers both directions: a
/// wait that wants output reports only writable, and one that wants only
/// input reports readable. The loop above therefore never reads while it is
/// waiting to write - the behaviour of a blocking connection, kept exactly.
pub struct BlockingStreams<R, W> {
    reader: R,
    writer: W,
}

impl<R: io::Read, W: io::Write> BlockingStreams<R, W> {
    /// Wraps a blocking `reader` and `writer`.
    pub fn new(reader: R, writer: W) -> Self {
        Self { reader, writer }
    }
}

impl<R: io::Read, W: io::Write> Transport for BlockingStreams<R, W> {
    fn try_read(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        non_blocking(|| self.reader.read(buf))
    }

    fn try_write(&mut self, buf: &[u8]) -> io::Result<Option<usize>> {
        non_blocking(|| self.writer.write(buf))
    }

    fn wait(
        &mut self,
        input: bool,
        output: bool,
        _timeout: Option<Duration>,
    ) -> io::Result<Readiness> {
        Ok(Readiness {
            readable: input && !output,
            writable: output,
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

/// Maps a would-block result to `Ok(None)` and retries an interrupted call.
fn non_blocking<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<Option<T>> {
    loop {
        match op() {
            Ok(value) => return Ok(Some(value)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

#[cfg(unix)]
pub use unix::FdTransport;

#[cfg(unix)]
mod unix {
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::os::fd::{AsFd, BorrowedFd};
    use std::time::Duration;

    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};

    use super::{Readiness, Transport, non_blocking};

    /// upstream: io.c:39-40 `POLL_RD_BITS` and `POLL_WR_BITS`.
    const READ_BITS: PollFlags = PollFlags::IN
        .union(PollFlags::PRI)
        .union(PollFlags::ERR)
        .union(PollFlags::HUP);
    const WRITE_BITS: PollFlags = PollFlags::OUT.union(PollFlags::ERR).union(PollFlags::HUP);

    /// A [`Transport`] over descriptors: a pipe pair (an rsh child or a
    /// `--server`'s stdio) or one socket used in both directions.
    pub struct FdTransport<I, O = File> {
        input: I,
        output: O,
        /// Both directions share one descriptor (a socket), so a wait polls
        /// a single row carrying both events (upstream: io.c:787-797).
        shared: bool,
    }

    impl<I: AsFd + Read, O: AsFd + Write> FdTransport<I, O> {
        /// A transport reading `input` and writing `output`, both switched to
        /// non-blocking mode.
        ///
        /// # Errors
        ///
        /// Returns the `fcntl(2)` error.
        pub fn pair(input: I, output: O) -> io::Result<Self> {
            set_nonblocking(input.as_fd())?;
            set_nonblocking(output.as_fd())?;
            Ok(Self {
                input,
                output,
                shared: false,
            })
        }
    }

    impl<S: AsFd + Read> FdTransport<S, File> {
        /// A transport over one socket used in both directions, switched to
        /// non-blocking mode. Writes go through a duplicate of the descriptor,
        /// which shares the open file description and its status flags.
        ///
        /// # Errors
        ///
        /// Returns the `fcntl(2)` or `dup(2)` error.
        pub fn socket(stream: S) -> io::Result<Self> {
            set_nonblocking(stream.as_fd())?;
            let output = File::from(stream.as_fd().try_clone_to_owned()?);
            Ok(Self {
                input: stream,
                output,
                shared: true,
            })
        }
    }

    /// upstream: util1.c:53 set_nonblocking() - OR `O_NONBLOCK` into the
    /// descriptor's status flags.
    fn set_nonblocking(fd: BorrowedFd<'_>) -> io::Result<()> {
        let flags = fcntl_getfl(fd)?;
        if !flags.contains(OFlags::NONBLOCK) {
            fcntl_setfl(fd, flags | OFlags::NONBLOCK)?;
        }
        Ok(())
    }

    impl<I: AsFd + Read, O: AsFd + Write> Transport for FdTransport<I, O> {
        fn try_read(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
            non_blocking(|| self.input.read(buf))
        }

        fn try_write(&mut self, buf: &[u8]) -> io::Result<Option<usize>> {
            non_blocking(|| self.output.write(buf))
        }

        fn wait(
            &mut self,
            input: bool,
            output: bool,
            timeout: Option<Duration>,
        ) -> io::Result<Readiness> {
            let in_events = if input {
                PollFlags::IN | PollFlags::PRI
            } else {
                PollFlags::empty()
            };
            let out_events = if output {
                PollFlags::OUT
            } else {
                PollFlags::empty()
            };
            let timeout = timeout.map(Timespec::try_from).transpose().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "poll timeout out of range")
            })?;
            let in_fd = self.input.as_fd();
            let out_fd = self.output.as_fd();
            // upstream: io.c:787-797 - one descriptor serving both directions
            // gets a single row carrying both events.
            let revents = if self.shared {
                let mut fds = [PollFd::from_borrowed_fd(in_fd, in_events | out_events)];
                poll(&mut fds, timeout.as_ref())?;
                let r = fds[0].revents();
                (r, r)
            } else {
                let mut fds = [
                    PollFd::from_borrowed_fd(in_fd, in_events),
                    PollFd::from_borrowed_fd(out_fd, out_events),
                ];
                poll(&mut fds, timeout.as_ref())?;
                (fds[0].revents(), fds[1].revents())
            };
            // upstream: io.c:858-866 - a bad descriptor is reported through
            // revents, not the return value.
            if revents.0.contains(PollFlags::NVAL) || revents.1.contains(PollFlags::NVAL) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "poll reported an invalid descriptor",
                ));
            }
            Ok(Readiness {
                readable: input && revents.0.intersects(READ_BITS),
                writable: output && revents.1.intersects(WRITE_BITS),
            })
        }
    }
}

#[cfg(windows)]
pub use windows::SocketTransport;

#[cfg(windows)]
mod windows {
    use std::io::{self, Read, Write};
    use std::net::TcpStream;
    use std::os::windows::io::AsRawSocket;
    use std::time::Duration;

    use windows_sys::Win32::Networking::WinSock::{
        POLLERR, POLLHUP, POLLNVAL, POLLRDNORM, POLLWRNORM, SOCKET, SOCKET_ERROR, WSAPOLLFD,
        WSAPoll,
    };

    use super::{Readiness, Transport, non_blocking};

    /// A [`Transport`] over one TCP socket used in both directions, waited on
    /// with `WSAPoll`.
    pub struct SocketTransport {
        stream: TcpStream,
    }

    impl SocketTransport {
        /// Switches `stream` to non-blocking mode and wraps it.
        ///
        /// # Errors
        ///
        /// Returns the `ioctlsocket` error.
        pub fn new(stream: TcpStream) -> io::Result<Self> {
            stream.set_nonblocking(true)?;
            Ok(Self { stream })
        }
    }

    impl Transport for SocketTransport {
        fn try_read(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
            non_blocking(|| self.stream.read(buf))
        }

        fn try_write(&mut self, buf: &[u8]) -> io::Result<Option<usize>> {
            non_blocking(|| self.stream.write(buf))
        }

        fn wait(
            &mut self,
            input: bool,
            output: bool,
            timeout: Option<Duration>,
        ) -> io::Result<Readiness> {
            let mut events = 0;
            if input {
                events |= POLLRDNORM;
            }
            if output {
                events |= POLLWRNORM;
            }
            let mut fd = WSAPOLLFD {
                fd: self.stream.as_raw_socket() as SOCKET,
                events,
                revents: 0,
            };
            let timeout_ms =
                timeout.map_or(-1, |t| i32::try_from(t.as_millis()).unwrap_or(i32::MAX));
            // SAFETY: `fd` is one valid, initialised WSAPOLLFD on the stack and
            // the count passed is 1, so WSAPoll reads and writes only it. The
            // socket it names is owned by `self.stream`, which outlives the call.
            let rc = unsafe { WSAPoll(&mut fd, 1, timeout_ms) };
            if rc == SOCKET_ERROR {
                return Err(io::Error::last_os_error());
            }
            if fd.revents & POLLNVAL != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "WSAPoll reported an invalid socket",
                ));
            }
            let failed = fd.revents & (POLLERR | POLLHUP) != 0;
            Ok(Readiness {
                readable: input && (fd.revents & POLLRDNORM != 0 || failed),
                writable: output && (fd.revents & POLLWRNORM != 0 || failed),
            })
        }
    }
}

#[cfg(test)]
mod blocking_tests {
    use std::io::Cursor;

    use super::{BlockingStreams, Readiness, Transport};

    /// WHY: a stream that cannot be polled must never be read while the loop
    /// waits to write, or a blocking read would stall the write forever.
    #[test]
    fn a_wait_for_output_never_offers_input() {
        let mut t = BlockingStreams::new(Cursor::new(b"in".to_vec()), Vec::new());
        assert_eq!(
            t.wait(true, true, None).unwrap(),
            Readiness {
                readable: false,
                writable: true
            }
        );
        assert_eq!(
            t.wait(true, false, None).unwrap(),
            Readiness {
                readable: true,
                writable: false
            }
        );
        let mut buf = [0u8; 2];
        assert_eq!(t.try_read(&mut buf).unwrap(), Some(2));
        assert_eq!(t.try_write(b"out").unwrap(), Some(3));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    use super::{FdTransport, Readiness, Transport};

    const NOW: Option<Duration> = Some(Duration::ZERO);

    fn pair() -> (FdTransport<UnixStream>, UnixStream) {
        let (ours, peer) = UnixStream::pair().unwrap();
        (FdTransport::socket(ours).unwrap(), peer)
    }

    /// WHY: perform_io must never park in read(2); an empty transport answers
    /// "nothing yet" instead of blocking (upstream: io.c:913 EWOULDBLOCK).
    #[test]
    fn an_empty_read_reports_would_block() {
        let (mut t, _peer) = pair();
        let mut buf = [0u8; 8];
        assert_eq!(t.try_read(&mut buf).unwrap(), None);
        assert_eq!(t.wait(true, false, NOW).unwrap(), Readiness::default());
    }

    /// WHY: the loop reads as soon as poll says input is there.
    #[test]
    fn arrived_input_is_readable_and_read() {
        let (mut t, mut peer) = pair();
        peer.write_all(b"peer").unwrap();
        let ready = t.wait(true, true, NOW).unwrap();
        assert!(ready.readable && ready.writable);
        let mut buf = [0u8; 8];
        assert_eq!(t.try_read(&mut buf).unwrap(), Some(4));
        assert_eq!(&buf[..4], b"peer");
    }

    /// WHY: a full send buffer must make the writer wait in poll, not spin
    /// and not block in write(2) (upstream: io.c:967 EWOULDBLOCK).
    #[test]
    fn a_full_output_reports_would_block_and_not_writable() {
        let (mut t, mut peer) = pair();
        let chunk = [0u8; 65536];
        let mut sent = 0usize;
        while let Some(n) = t.try_write(&chunk).unwrap() {
            sent += n;
        }
        assert!(sent > 0);
        assert!(!t.wait(false, true, NOW).unwrap().writable);
        let mut drain = vec![0u8; sent];
        peer.read_exact(&mut drain).unwrap();
        assert!(t.wait(false, true, NOW).unwrap().writable);
    }

    /// WHY: a peer that closes is input upstream must still read - EOF is a
    /// readable event (POLL_RD_BITS includes POLLHUP), and a write then fails
    /// rather than blocking.
    #[test]
    fn a_closed_peer_is_readable_eof_and_a_write_error() {
        let (mut t, peer) = pair();
        drop(peer);
        assert!(t.wait(true, false, NOW).unwrap().readable);
        let mut buf = [0u8; 8];
        assert_eq!(t.try_read(&mut buf).unwrap(), Some(0));
        let err = t.try_write(b"x").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
    }

    /// WHY: an rsh transport has two descriptors; each direction must be
    /// polled on its own row and reported separately.
    #[test]
    fn a_pipe_pair_reports_each_direction_from_its_own_descriptor() {
        let (in_ours, mut in_peer) = UnixStream::pair().unwrap();
        let (out_ours, _out_peer) = UnixStream::pair().unwrap();
        let mut t = FdTransport::pair(in_ours, out_ours).unwrap();
        assert_eq!(
            t.wait(true, true, NOW).unwrap(),
            Readiness {
                readable: false,
                writable: true
            }
        );
        in_peer.write_all(b"x").unwrap();
        assert!(t.wait(true, false, NOW).unwrap().readable);
    }

    /// WHY: building the transport is what switches the descriptor to
    /// non-blocking mode, as upstream's set_nonblocking() does at start.
    #[test]
    fn construction_sets_nonblocking() {
        use rustix::fs::{OFlags, fcntl_getfl};
        let (ours, _peer) = UnixStream::pair().unwrap();
        let alias = ours.try_clone().unwrap();
        let _t = FdTransport::socket(ours).unwrap();
        // The flag lives on the open file description, which `alias` shares.
        let flags = fcntl_getfl(&alias).unwrap();
        assert!(flags.contains(OFlags::NONBLOCK));
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use std::time::Duration;

    use super::{Readiness, SocketTransport, Transport};

    const NOW: Option<Duration> = Some(Duration::ZERO);

    fn pair() -> (SocketTransport, TcpStream) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let ours = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (peer, _) = listener.accept().unwrap();
        (SocketTransport::new(ours).unwrap(), peer)
    }

    /// WHY: perform_io must never park in recv(); an empty socket answers
    /// "nothing yet" (upstream: io.c:913 EWOULDBLOCK).
    #[test]
    fn an_empty_read_reports_would_block() {
        let (mut t, _peer) = pair();
        let mut buf = [0u8; 8];
        assert_eq!(t.try_read(&mut buf).unwrap(), None);
        assert!(!t.wait(true, false, NOW).unwrap().readable);
    }

    /// WHY: WSAPoll's POLLRDNORM must surface as readable so the loop reads
    /// what has arrived.
    #[test]
    fn arrived_input_is_readable_and_read() {
        let (mut t, mut peer) = pair();
        peer.write_all(b"peer").unwrap();
        let ready = t.wait(true, false, None).unwrap();
        assert_eq!(
            ready,
            Readiness {
                readable: true,
                writable: false
            }
        );
        let mut buf = [0u8; 8];
        assert_eq!(t.try_read(&mut buf).unwrap(), Some(4));
        assert_eq!(&buf[..4], b"peer");
    }

    /// WHY: a full send buffer must make the writer wait, not spin.
    #[test]
    fn a_full_output_reports_would_block_and_not_writable() {
        let (mut t, mut peer) = pair();
        let chunk = [0u8; 65536];
        let mut sent = 0usize;
        while let Some(n) = t.try_write(&chunk).unwrap() {
            sent += n;
        }
        assert!(sent > 0);
        assert!(!t.wait(false, true, NOW).unwrap().writable);
        let mut drain = vec![0u8; sent];
        peer.read_exact(&mut drain).unwrap();
        assert!(t.wait(false, true, None).unwrap().writable);
    }

    /// WHY: a peer that closes is input upstream still reads - end of file is
    /// a readable event.
    #[test]
    fn a_closed_peer_is_readable_eof() {
        let (mut t, peer) = pair();
        drop(peer);
        assert!(t.wait(true, false, None).unwrap().readable);
        let mut buf = [0u8; 8];
        assert_eq!(t.try_read(&mut buf).unwrap(), Some(0));
    }
}
