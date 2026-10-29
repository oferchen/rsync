//! The connection's transport core: the one place bytes cross the wire.
//!
//! Upstream's `perform_io()` is the only code that reads or writes the
//! transfer descriptors (upstream: io.c:647-1042). It reads into the fixed
//! 32 KiB `iobuf.in` (io.c:1587) whenever that buffer has room - including
//! while it waits to write - and writes whatever output is pending. [`Core`]
//! is that function's home in oc: the reader stack and the writer stack each
//! sit on a handle to the same core ([`CoreReader`], [`CoreWriter`]), so a
//! write that has to wait still collects the peer's input, and that input is
//! already buffered when the reader stack next asks for it.

use std::cell::RefCell;
use std::io::{self, Read, Write};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use fast_io::transport::Transport;
use protocol::iobuf::{IN_BUFFER_SIZE, InBuf};

/// One connection's transport, input buffer and wire counters.
pub(crate) struct Core<'a> {
    transport: Box<dyn Transport + 'a>,
    input: InBuf,
    /// Bytes the handshake already took off the transport, served before
    /// anything new is read.
    pending: Vec<u8>,
    /// `stats.total_read`, once input buffering has started; until then reads
    /// are raw and uncounted (upstream: io.c:2157-2161 read_buf() bypasses
    /// perform_io() while `iobuf.in_fd < 0`).
    read_counter: Option<Arc<AtomicU64>>,
    /// The peer closed its output; no more input will arrive
    /// (upstream: io.c:905-911 sets `iobuf.in_fd = -2`).
    in_eof: bool,
    /// How long one wait may last before the connection is declared dead;
    /// `None` waits without a bound.
    timeout: Option<Duration>,
}

/// The handle both stacks hold. Each role runs its connection on one thread,
/// so the core is shared without locking.
pub(crate) type SharedCore<'a> = Rc<RefCell<Core<'a>>>;

impl<'a> Core<'a> {
    /// A core over `transport` in raw mode, serving `pending` (bytes an
    /// earlier handshake reader buffered) before reading anything new.
    pub(crate) fn new(transport: Box<dyn Transport + 'a>, pending: Vec<u8>) -> SharedCore<'a> {
        Rc::new(RefCell::new(Self {
            transport,
            input: InBuf::new(IN_BUFFER_SIZE),
            pending,
            read_counter: None,
            in_eof: false,
            timeout: None,
        }))
    }

    /// Starts buffered, counted input, as upstream does once protocol setup
    /// is done (upstream: io.c:1573 io_start_buffering_in()). Bytes still
    /// pending from the handshake move into the input buffer and count as
    /// read, as they did when they first crossed the transport.
    pub(crate) fn start_buffering(core: &SharedCore<'a>, read_counter: Arc<AtomicU64>) {
        let mut core = core.borrow_mut();
        let pending = std::mem::take(&mut core.pending);
        read_counter.fetch_add(pending.len() as u64, Ordering::Relaxed);
        core.read_counter = Some(read_counter);
        let mut rest = &pending[..];
        while !rest.is_empty() {
            let n = core
                .input
                .fill_with(|span| {
                    let n = span.len().min(rest.len());
                    span[..n].copy_from_slice(&rest[..n]);
                    Ok(Some(n))
                })
                .expect("copying from memory cannot fail")
                .expect("an empty input buffer has room");
            rest = &rest[n..];
        }
    }

    fn buffering(&self) -> bool {
        self.read_counter.is_some()
    }

    /// Serves `out`: from the handshake's leftovers or raw from the transport
    /// before buffering starts, from the input buffer after.
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if !self.buffering() {
            return self.read_raw(out);
        }
        loop {
            if !self.input.is_empty() {
                let readable = self.input.readable();
                let n = readable.len().min(out.len());
                out[..n].copy_from_slice(&readable[..n]);
                self.input.consume(n);
                return Ok(n);
            }
            if self.in_eof || out.is_empty() {
                return Ok(0);
            }
            // A request at least as large as the buffer gains nothing from
            // staging, so it reads straight into the caller's slice.
            if out.len() >= self.input.capacity() {
                match self.transport.try_read(out)? {
                    Some(0) => {
                        self.in_eof = true;
                        return Ok(0);
                    }
                    Some(n) => {
                        self.count_read(n);
                        return Ok(n);
                    }
                    None => {}
                }
            } else if self.fill_input()?.is_some() {
                continue;
            }
            self.wait(false)?;
        }
    }

    /// An unbuffered read: exactly what the caller asks for, never more, so
    /// nothing is held back from the stack built after setup.
    fn read_raw(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if !self.pending.is_empty() {
            let n = self.pending.len().min(out.len());
            out[..n].copy_from_slice(&self.pending[..n]);
            self.pending.drain(..n);
            return Ok(n);
        }
        loop {
            if let Some(n) = self.transport.try_read(out)? {
                return Ok(n);
            }
            self.wait(false)?;
        }
    }

    /// Hands `buf` to the transport, collecting the peer's input while the
    /// transport has no room (upstream: io.c:946-994 with io.c:883-921 in the
    /// same pass).
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            if let Some(n) = self.transport.try_write(buf)? {
                return Ok(n);
            }
            self.wait(true)?;
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.transport.flush()
    }

    /// Reads whatever the transport has ready into the input buffer. `Some(0)`
    /// marks end of file; `None` means nothing was ready.
    fn fill_input(&mut self) -> io::Result<Option<usize>> {
        let Self {
            input, transport, ..
        } = self;
        let got = input.fill_with(|span| transport.try_read(span))?;
        match got {
            Some(0) => self.in_eof = true,
            Some(n) => self.count_read(n),
            None => {}
        }
        Ok(got)
    }

    /// One `perform_io()` pass: waits for output room when `want_output`,
    /// and, once input is buffered, for input whenever the buffer has room
    /// (upstream: io.c:746-806), then reads what arrived. Before buffering
    /// starts a wait is for one direction only, as upstream's raw
    /// safe_read()/safe_write() are (io.c:310, io.c:387).
    fn wait(&mut self, want_output: bool) -> io::Result<()> {
        let poll_input = if self.buffering() {
            !self.in_eof && self.input.has_room()
        } else {
            !want_output
        };
        let ready = self.transport.wait(poll_input, want_output, self.timeout)?;
        if !ready.readable && !ready.writable {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for the connection",
            ));
        }
        if ready.readable && self.buffering() {
            self.fill_input()?;
        }
        Ok(())
    }

    fn count_read(&self, n: usize) {
        if let Some(counter) = &self.read_counter {
            counter.fetch_add(n as u64, Ordering::Relaxed);
        }
    }
}

/// The reader stack's bottom: input served from the core.
pub(crate) struct CoreReader<'a>(SharedCore<'a>);

impl<'a> CoreReader<'a> {
    /// A reader over `core`.
    pub(crate) fn new(core: &SharedCore<'a>) -> Self {
        Self(Rc::clone(core))
    }
}

impl Read for CoreReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.borrow_mut().read(buf)
    }
}

/// The writer stack's bottom: output pushed through the core.
pub(crate) struct CoreWriter<'a>(SharedCore<'a>);

impl<'a> CoreWriter<'a> {
    /// A writer over `core`.
    pub(crate) fn new(core: &SharedCore<'a>) -> Self {
        Self(Rc::clone(core))
    }
}

impl Write for CoreWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.borrow_mut().flush()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    use fast_io::transport::FdTransport;

    use super::*;

    fn core_over_socket() -> (SharedCore<'static>, UnixStream, Arc<AtomicU64>) {
        let (ours, peer) = UnixStream::pair().unwrap();
        let counter = Arc::new(AtomicU64::new(0));
        let core = Core::new(Box::new(FdTransport::socket(ours).unwrap()), Vec::new());
        Core::start_buffering(&core, Arc::clone(&counter));
        (core, peer, counter)
    }

    /// A transport whose output is full until a wait has been made, holding
    /// one pending input message, and recording what each wait asked for.
    struct FullUntilWaited {
        pending_input: Vec<u8>,
        waited: bool,
        waits: Rc<RefCell<Vec<(bool, bool)>>>,
    }

    impl Transport for FullUntilWaited {
        fn try_read(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
            if self.pending_input.is_empty() {
                return Ok(None);
            }
            let n = self.pending_input.len().min(buf.len());
            buf[..n].copy_from_slice(&self.pending_input[..n]);
            self.pending_input.drain(..n);
            Ok(Some(n))
        }

        fn try_write(&mut self, buf: &[u8]) -> io::Result<Option<usize>> {
            Ok(self.waited.then_some(buf.len()))
        }

        fn wait(
            &mut self,
            input: bool,
            output: bool,
            _timeout: Option<Duration>,
        ) -> io::Result<fast_io::transport::Readiness> {
            self.waits.borrow_mut().push((input, output));
            self.waited = true;
            Ok(fast_io::transport::Readiness {
                readable: input && !self.pending_input.is_empty(),
                writable: output,
            })
        }
    }

    /// WHY: this is the point of the core. A write that has to wait for room
    /// must poll the input too and collect what the peer sent, so a peer's
    /// message is already buffered when the write later fails (upstream:
    /// io.c:746-806 arms both directions; io.c:883-921 reads inside the
    /// write wait).
    #[test]
    fn a_waiting_write_polls_and_collects_the_peers_input() {
        let waits = Rc::new(RefCell::new(Vec::new()));
        let counter = Arc::new(AtomicU64::new(0));
        let core = Core::new(
            Box::new(FullUntilWaited {
                pending_input: b"refused".to_vec(),
                waited: false,
                waits: Rc::clone(&waits),
            }),
            Vec::new(),
        );
        Core::start_buffering(&core, Arc::clone(&counter));
        assert_eq!(CoreWriter::new(&core).write(b"data").unwrap(), 4);
        assert_eq!(*waits.borrow(), vec![(true, true)]);
        assert_eq!(
            counter.load(Ordering::Relaxed),
            7,
            "collected during the write"
        );
        let mut got = [0u8; 7];
        CoreReader::new(&core).read_exact(&mut got).unwrap();
        assert_eq!(&got, b"refused");
    }

    /// WHY: a read waits for input and returns it, and a peer that closes is
    /// end of file - the same answers the blocking stack gave.
    #[test]
    fn reads_wait_for_input_and_report_eof() {
        let (core, mut peer, counter) = core_over_socket();
        let mut reader = CoreReader::new(&core);
        let sender = std::thread::spawn(move || {
            peer.write_all(b"late").unwrap();
        });
        let mut got = [0u8; 4];
        reader.read_exact(&mut got).unwrap();
        sender.join().unwrap();
        assert_eq!(&got, b"late");
        assert_eq!(reader.read(&mut got).unwrap(), 0);
        assert_eq!(counter.load(Ordering::Relaxed), 4);
    }
}

#[cfg(test)]
mod stream_tests {
    use std::io::{self, Read};

    use fast_io::transport::BlockingStreams;

    use super::*;

    /// A reader that yields one byte per call, so the core's buffering is the
    /// only thing between the caller and a byte-at-a-time descriptor.
    struct Dribble<'a>(&'a [u8]);

    impl Read for Dribble<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.0.is_empty() || out.is_empty() {
                return Ok(0);
            }
            out[0] = self.0[0];
            self.0 = &self.0[1..];
            Ok(1)
        }
    }

    fn core_over<'a>(source: impl Read + 'a) -> (SharedCore<'a>, Arc<AtomicU64>) {
        let counter = Arc::new(AtomicU64::new(0));
        let core = Core::new(
            Box::new(BlockingStreams::new(source, Vec::new())),
            Vec::new(),
        );
        Core::start_buffering(&core, Arc::clone(&counter));
        (core, counter)
    }

    /// WHY: however the descriptor fragments the stream, the reader stack
    /// must see it unchanged, and the read counter must count every byte
    /// exactly once (upstream: io.c:938 `stats.total_read += n`).
    #[test]
    fn a_fragmented_stream_is_delivered_unchanged_and_counted_once() {
        let source: Vec<u8> = (0..70_000u32).map(|i| (i * 3) as u8).collect();
        let (core, counter) = core_over(Dribble(&source));
        let mut got = Vec::new();
        CoreReader::new(&core).read_to_end(&mut got).unwrap();
        assert_eq!(got, source);
        assert_eq!(counter.load(Ordering::Relaxed), source.len() as u64);
    }

    /// WHY: a request at least as large as iobuf.in bypasses staging, so a
    /// big read costs one transport read, not one per buffer refill.
    #[test]
    fn a_large_request_reads_straight_from_the_transport() {
        let source: Vec<u8> = (0..IN_BUFFER_SIZE as u32).map(|i| i as u8).collect();
        let (core, _) = core_over(&source[..]);
        let mut out = vec![0u8; IN_BUFFER_SIZE];
        assert_eq!(
            CoreReader::new(&core).read(&mut out).unwrap(),
            IN_BUFFER_SIZE
        );
        assert_eq!(out, source);
    }

    /// WHY: protocol setup reads exactly what it parses and is not counted,
    /// as upstream's pre-buffering read_buf() is (io.c:2157-2161); bytes a
    /// handshake reader buffered come first, and once buffering starts the
    /// remainder counts as read.
    #[test]
    fn raw_mode_serves_the_handshake_leftovers_first_then_buffers_and_counts() {
        let counter = Arc::new(AtomicU64::new(0));
        let core = Core::new(
            Box::new(BlockingStreams::new(&b"tail"[..], Vec::new())),
            b"head".to_vec(),
        );
        let mut reader = CoreReader::new(&core);
        let mut two = [0u8; 2];
        reader.read_exact(&mut two).unwrap();
        assert_eq!(&two, b"he", "leftovers come before the transport");
        Core::start_buffering(&core, Arc::clone(&counter));
        assert_eq!(counter.load(Ordering::Relaxed), 2, "unread leftovers count");
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"adtail");
        assert_eq!(counter.load(Ordering::Relaxed), 6);
    }
}
