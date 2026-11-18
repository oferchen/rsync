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
//!
//! Output accounting lives here too: the core counts `stats.total_written`
//! and paces `--bwlimit` on every byte it hands the transport, as perform_io
//! does (upstream: io.c:952, io.c:977-980). That lets it write the multiplex
//! output buffer out itself whenever it is about to wait for input
//! (upstream: io.c:766-768), with no flush from the code that filled it.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bandwidth::BandwidthLimiter;
use fast_io::transport::Transport;
use protocol::iobuf::{IN_BUFFER_SIZE, InBuf};

use super::timeout::IoTimeout;
use crate::writer::SharedMuxOut;

/// One connection's transport, input buffer and wire counters.
pub(crate) struct Core<'a> {
    transport: Box<dyn Transport + 'a>,
    input: InBuf,
    /// Bytes the handshake already took off the transport, served before
    /// anything new is read.
    pending: Vec<u8>,
    /// `stats.total_read`, once input buffering has started; until then reads
    /// are raw and uncounted (upstream: io.c:2164-2168 read_buf() bypasses
    /// perform_io() while `iobuf.in_fd < 0`).
    read_counter: Option<Arc<AtomicU64>>,
    /// The peer closed its output; no more input will arrive
    /// (upstream: io.c:911 sets `iobuf.in_fd = -2`).
    in_eof: bool,
    /// `--timeout`, checked whenever a wait comes back empty.
    timeout: IoTimeout,
    /// `stats.total_written`, once output accounting has started; until then
    /// writes are uncounted, like the setup exchange upstream writes with
    /// safe_write() (io.c:977 counts only inside perform_io()).
    write_counter: Option<Arc<AtomicU64>>,
    /// The sender's `--bwlimit` pacing (upstream: io.c:952 clamps each write
    /// to `bwlimit_writemax`, io.c:980 sleep_for_bwlimit()).
    limiter: Option<BandwidthLimiter>,
    /// The multiplexed output buffer, written out before every wait for input
    /// (upstream: io.c:766-768 arms `iobuf.out` whenever it holds data).
    mux: Option<SharedMuxOut>,
    /// Whether a read that needed input may queue a keepalive on `mux`; only
    /// the sending side's reads do (upstream: io.c:1656 returns for a process
    /// that is not yet sender, receiver or generator, and a forked
    /// receiver's output goes to its generator, not the peer).
    keepalive_on_read: bool,
    /// Prototype I/O model A (`OC_RSYNC_TEST_IO_MODEL=a`, receiving side):
    /// output never waits for the transport. What the transport refuses is
    /// kept in `outq` and leaves during read waits.
    model_a: bool,
    /// Framed bytes the transport has not accepted yet (model A only).
    outq: VecDeque<u8>,
}

thread_local! {
    /// `outq`'s length for the model-A core on this wire thread, `None` when
    /// no such core exists. The receiver's request admission reads it.
    static MODEL_A_QUEUED: Cell<Option<usize>> = const { Cell::new(None) };
}

/// Bytes the wire thread's model-A core has framed but the transport has not
/// accepted; `None` when model A is not active on this thread.
pub(crate) fn model_a_queued() -> Option<usize> {
    MODEL_A_QUEUED.with(Cell::get)
}

/// The handle both stacks hold. Each role runs its connection on one thread,
/// so the core is shared without locking; `Rc` keeps every handle on that
/// thread, since it is neither `Send` nor `Sync`.
pub(crate) type SharedCore<'a> = Rc<RefCell<Core<'a>>>;

impl<'a> Core<'a> {
    /// A core over `transport` in raw mode, serving `pending` (bytes an
    /// earlier handshake reader buffered) before reading anything new, and
    /// bounding its waits by `timeout`.
    pub(crate) fn new(
        transport: Box<dyn Transport + 'a>,
        pending: Vec<u8>,
        timeout: IoTimeout,
    ) -> SharedCore<'a> {
        Rc::new(RefCell::new(Self {
            transport,
            input: InBuf::new(IN_BUFFER_SIZE),
            pending,
            read_counter: None,
            in_eof: false,
            timeout,
            write_counter: None,
            limiter: None,
            mux: None,
            keepalive_on_read: false,
            model_a: false,
            outq: VecDeque::new(),
        }))
    }

    /// Bounds the core's waits at `secs` when no `--timeout` was set
    /// (upstream: io.c:1079-1081).
    pub(crate) fn arm_timeout_if_unset(core: &SharedCore<'a>, secs: u64) {
        core.borrow_mut().timeout.arm_if_unset(secs);
    }

    /// Starts counting output into `write_counter` and pacing it by
    /// `limiter`, from the point upstream's writes start going through
    /// perform_io() (upstream: io.c:2700 io_start_multiplex_out(), or
    /// io_start_buffering_out() below protocol 30).
    pub(crate) fn start_output_accounting(
        core: &SharedCore<'a>,
        write_counter: Arc<AtomicU64>,
        limiter: Option<BandwidthLimiter>,
    ) {
        let mut core = core.borrow_mut();
        core.write_counter = Some(write_counter);
        core.limiter = limiter;
    }

    /// Hands the core the multiplex output buffer to write out before it
    /// waits for input.
    pub(crate) fn attach_mux(core: &SharedCore<'a>, mux: SharedMuxOut) {
        core.borrow_mut().mux = Some(mux);
    }

    /// Lets reads that needed input queue a `--timeout` keepalive on the
    /// attached multiplex output, as upstream's sender does (io.c:933-936).
    pub(crate) fn keepalive_on_read(core: &SharedCore<'a>) {
        core.borrow_mut().keepalive_on_read = true;
    }

    /// Starts buffered, counted input, as upstream does once protocol setup
    /// is done (upstream: io.c:1580 io_start_buffering_in()). Bytes still
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

    /// Switches this core to prototype I/O model A: writes never wait.
    pub(crate) fn enable_model_a(core: &SharedCore<'a>) {
        let mut core = core.borrow_mut();
        core.model_a = true;
        core.publish_queued();
    }

    fn publish_queued(&self) {
        if self.model_a {
            MODEL_A_QUEUED.with(|q| q.set(Some(self.outq.len())));
        }
    }

    /// Counts and timestamps `n` bytes the transport accepted.
    fn note_written(&mut self, n: usize) {
        if self.buffering() {
            self.timeout.mark_io();
        }
        if let Some(counter) = &self.write_counter {
            counter.fetch_add(n as u64, Ordering::Relaxed);
        }
    }

    /// Model A: hands the transport as much queued output as it takes now.
    fn pump_output(&mut self) -> io::Result<()> {
        while !self.outq.is_empty() {
            let (front, _) = self.outq.as_slices();
            let written = self
                .transport
                .try_write(front)
                .map_err(super::PeerWriteError::wrap)?;
            match written {
                Some(n) => {
                    self.note_written(n);
                    self.outq.drain(..n);
                }
                None => break,
            }
        }
        self.publish_queued();
        Ok(())
    }

    /// Model A: accepts all of `buf` without waiting - straight to the
    /// transport while nothing is queued, the rest onto the queue.
    fn enqueue(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut done = 0;
        if self.outq.is_empty() {
            while done < buf.len() {
                let written = self
                    .transport
                    .try_write(&buf[done..])
                    .map_err(super::PeerWriteError::wrap)?;
                match written {
                    Some(n) => {
                        self.note_written(n);
                        done += n;
                    }
                    None => break,
                }
            }
        }
        if done < buf.len() {
            self.outq.extend(&buf[done..]);
            self.publish_queued();
        }
        Ok(buf.len())
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
            if let Err(e) = self.drain_mux().and_then(|()| self.pump_output()) {
                // upstream: perform_io() services a readable descriptor
                // (io.c:883-921) before a writable one (io.c:948) in the same
                // pass, so a peer that has gone shows up as the end of its
                // input, not as a write error. Stop writing to it and let the
                // read below report what it finds.
                if !crate::is_early_close_error(&e) {
                    return Err(e);
                }
                self.mux = None;
                self.outq.clear();
                self.publish_queued();
            }
            // A drain that had to wait for room collected the peer's input
            // meanwhile; serve that rather than wait for more.
            if !self.input.is_empty() {
                continue;
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
                        self.count_read(n, true)?;
                        return Ok(n);
                    }
                    None => {}
                }
            } else if self.fill_input(true)?.is_some() {
                continue;
            }
            // upstream io.c perform_io selects before every read; oc reads
            // first and polls only on EAGAIN, which saves ~7,000 polls per
            // push. Same wire bytes; a deliberate performance deviation.
            // Model A: the wait also asks for output room while bytes are
            // queued, and writes them when it comes (upstream: io.c:767-822).
            self.wait(!self.outq.is_empty(), true)?;
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
            self.wait(false, true)?;
        }
    }

    /// Hands `buf` to the transport, collecting the peer's input while the
    /// transport has no room (upstream: io.c:946-994 with io.c:883-921 in the
    /// same pass).
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            let written = self
                .transport
                .try_write(buf)
                .map_err(super::PeerWriteError::wrap)?;
            if let Some(n) = written {
                if self.buffering() {
                    self.timeout.mark_io();
                }
                return Ok(n);
            }
            self.wait(true, false)?;
        }
    }

    /// [`Self::write`] with output accounting: each write is clamped to the
    /// bandwidth limiter's burst, then counted and paced by what the
    /// transport took (upstream: io.c:952, io.c:977-980).
    fn write_out(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.model_a {
            return self.enqueue(buf);
        }
        let len = match &self.limiter {
            Some(limiter) => buf.len().min(limiter.write_max_bytes().max(1)),
            None => buf.len(),
        };
        let n = self.write(&buf[..len])?;
        if let Some(counter) = &self.write_counter {
            counter.fetch_add(n as u64, Ordering::Relaxed);
        }
        if let Some(limiter) = &mut self.limiter {
            let _ = limiter.register(n);
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.pump_output()?;
        self.transport.flush()
    }

    /// Writes out whatever the multiplex buffer holds, so nothing the peer
    /// may be waiting for stays buffered while this side waits for it
    /// (upstream: io.c:766-768).
    fn drain_mux(&mut self) -> io::Result<()> {
        let Some(mux) = self.mux.clone() else {
            return Ok(());
        };
        // The writer stack borrows the buffer only inside its own writes,
        // and a write never waits for input, so the buffer is free here.
        mux.borrow_mut().flush(&mut Output(self))
    }

    /// Reads whatever the transport has ready into the input buffer. `Some(0)`
    /// marks end of file; `None` means nothing was ready. `needed` is true
    /// when a read is waiting for this input rather than a write collecting
    /// it (upstream: `flags & PIO_NEED_INPUT` at io.c:935).
    fn fill_input(&mut self, needed: bool) -> io::Result<Option<usize>> {
        let Self {
            input, transport, ..
        } = self;
        let got = input.fill_with(|span| transport.try_read(span))?;
        match got {
            Some(0) => self.in_eof = true,
            Some(n) => self.count_read(n, needed)?,
            None => {}
        }
        Ok(got)
    }

    /// One `perform_io()` pass: waits for output room when `want_output`,
    /// and, once input is buffered, for input whenever the buffer has room
    /// (upstream: io.c:746-806), then reads what arrived. Before buffering
    /// starts a wait is for one direction only, as upstream's raw
    /// safe_read()/safe_write() are (io.c:310, io.c:387). A wait that ends
    /// with nothing ready checks `--timeout` (upstream: io.c:876, io.c:335).
    ///
    /// A requested shutdown ends the wait before it starts and when it
    /// returns, and an interrupted wait counts as an empty one (upstream:
    /// io.c:849-850 and io.c:997-998 read `got_kill_signal`; io.c:866-884
    /// treats an interrupted poll like a timeout).
    fn wait(&mut self, want_output: bool, reading: bool) -> io::Result<()> {
        crate::shared::check_shutdown()?;
        let poll_input = if self.buffering() {
            !self.in_eof && self.input.has_room()
        } else {
            !want_output
        };
        let ready = match self
            .transport
            .wait(poll_input, want_output, self.timeout.wait_bound())
        {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Default::default(),
            other => other?,
        };
        crate::shared::check_shutdown()?;
        if !ready.readable && !ready.writable {
            return self.timeout.check();
        }
        if ready.readable && self.buffering() {
            self.fill_input(reading)?;
        }
        if ready.writable && !self.outq.is_empty() {
            match self.pump_output() {
                Err(e) if reading && crate::is_early_close_error(&e) => {
                    self.outq.clear();
                    self.publish_queued();
                }
                other => other?,
            }
        }
        Ok(())
    }

    /// Counts and timestamps buffered-mode input; raw reads are neither
    /// (upstream: safe_read() touches neither `stats.total_read` nor
    /// `last_io_in`). A read that `needed` the input may then queue a
    /// keepalive (upstream: io.c:933-936).
    fn count_read(&mut self, n: usize, needed: bool) -> io::Result<()> {
        if let Some(counter) = &self.read_counter {
            counter.fetch_add(n as u64, Ordering::Relaxed);
            self.timeout.mark_io();
            if needed
                && self.keepalive_on_read
                && let Some(mux) = &self.mux
            {
                mux.borrow_mut().keepalive_after_read()?;
            }
        }
        Ok(())
    }
}

impl Drop for Core<'_> {
    /// Model A: the session's last output has no read wait left to carry it,
    /// so it is written out here, still collecting input while there is room.
    fn drop(&mut self) {
        if !self.model_a {
            return;
        }
        while !self.outq.is_empty() {
            if self.pump_output().is_err() || self.outq.is_empty() {
                break;
            }
            if self.wait(true, false).is_err() {
                break;
            }
        }
        MODEL_A_QUEUED.with(|q| q.set(None));
    }
}

/// The core's own accounted output path, as a sink for the multiplex buffer.
struct Output<'c, 'a>(&'c mut Core<'a>);

impl Write for Output<'_, '_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write_out(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
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
        self.0.borrow_mut().write_out(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.borrow_mut().flush()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    use std::time::Duration;

    use fast_io::transport::FdTransport;

    use super::*;

    fn core_over_socket() -> (SharedCore<'static>, UnixStream, Arc<AtomicU64>) {
        let (ours, peer) = UnixStream::pair().unwrap();
        let counter = Arc::new(AtomicU64::new(0));
        let core = Core::new(
            Box::new(FdTransport::socket(ours).unwrap()),
            Vec::new(),
            IoTimeout::new(None, "sender"),
        );
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
            IoTimeout::new(None, "sender"),
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

    /// A peer that is gone: every write fails as the socket reports it.
    #[cfg(unix)]
    struct GonePeer;

    #[cfg(unix)]
    impl Transport for GonePeer {
        fn try_read(&mut self, _: &mut [u8]) -> io::Result<Option<usize>> {
            Ok(None)
        }

        fn try_write(&mut self, _: &[u8]) -> io::Result<Option<usize>> {
            Err(io::Error::from_raw_os_error(EPIPE))
        }

        fn wait(
            &mut self,
            _: bool,
            _: bool,
            _: Option<Duration>,
        ) -> io::Result<fast_io::transport::Readiness> {
            Ok(fast_io::transport::Readiness::default())
        }
    }

    /// `EPIPE` on every Unix oc builds for.
    #[cfg(unix)]
    const EPIPE: i32 = 32;

    /// WHY: upstream reports a failed write as a `write error` with the
    /// write's own errno (io.c:961-967), so the core must say that it was the
    /// write that failed while keeping the transport's kind and errno - a
    /// later classifier cannot tell a write's `ECONNRESET` from a read's.
    #[cfg(unix)]
    #[test]
    fn a_failed_write_is_marked_as_the_writes_own_failure() {
        let core = Core::new(
            Box::new(GonePeer),
            Vec::new(),
            IoTimeout::new(None, "sender"),
        );
        let err = CoreWriter::new(&core).write(b"data").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
        let failed = super::super::PeerWriteError::of(&err).expect("marked as the write's failure");
        assert_eq!(failed.raw_os_error(), Some(EPIPE));
    }

    /// A writer stopped by a shutdown signal: the transport ends the write
    /// with the signal's error, as the signal wake does.
    struct SignalledWrite;

    impl Transport for SignalledWrite {
        fn try_read(&mut self, _: &mut [u8]) -> io::Result<Option<usize>> {
            Ok(None)
        }

        fn try_write(&mut self, _: &[u8]) -> io::Result<Option<usize>> {
            Err(fast_io::signal::shutdown_error())
        }

        fn wait(
            &mut self,
            _: bool,
            _: bool,
            _: Option<Duration>,
        ) -> io::Result<fast_io::transport::Readiness> {
            Ok(fast_io::transport::Readiness::default())
        }
    }

    /// WHY: a signal ends rsync from its handler with code 20 (rsync.c
    /// sig_int()), never as a write error with code 10, so a write the signal
    /// wake ended must still read as the signal's shutdown.
    #[test]
    fn a_write_ended_by_a_shutdown_signal_is_not_marked_as_its_failure() {
        let core = Core::new(
            Box::new(SignalledWrite),
            Vec::new(),
            IoTimeout::new(None, "sender"),
        );
        let err = CoreWriter::new(&core).write(b"data").unwrap_err();
        assert!(crate::shared::is_signal_shutdown(&err), "{err:?}");
        assert!(super::super::PeerWriteError::of(&err).is_none(), "{err:?}");
    }

    /// A peer that never answers: every wait comes back empty after the bound
    /// it was given, which advances the fake clock by that bound.
    struct SilentPeer {
        clock: Rc<std::cell::Cell<Duration>>,
        bounds: Rc<RefCell<Vec<Option<Duration>>>>,
    }

    impl Transport for SilentPeer {
        fn try_read(&mut self, _: &mut [u8]) -> io::Result<Option<usize>> {
            Ok(None)
        }

        fn try_write(&mut self, _: &[u8]) -> io::Result<Option<usize>> {
            Ok(None)
        }

        fn wait(
            &mut self,
            _: bool,
            _: bool,
            timeout: Option<Duration>,
        ) -> io::Result<fast_io::transport::Readiness> {
            self.bounds.borrow_mut().push(timeout);
            let bound = timeout.expect("--timeout bounds every wait");
            self.clock.set(self.clock.get() + bound);
            Ok(fast_io::transport::Readiness::default())
        }
    }

    /// WHY: `--timeout` must end a connection whose peer went silent, with
    /// upstream's message, on upstream's cadence: waits of ceil(5/2) = 3 s,
    /// the idle clock starting at the first empty one, so the third empty
    /// wait reports 6 idle seconds (upstream: io.c:229-268 check_timeout()).
    /// The error kind is what maps to RERR_TIMEOUT (30).
    #[test]
    fn a_silent_peer_hits_the_io_timeout_in_reads_and_writes() {
        for buffered in [false, true] {
            let offset = Rc::new(std::cell::Cell::new(Duration::ZERO));
            let bounds = Rc::new(RefCell::new(Vec::new()));
            let base = std::time::Instant::now();
            let reading = Rc::clone(&offset);
            let clock: super::super::timeout::Clock = Rc::new(move || base + reading.get());
            let core = Core::new(
                Box::new(SilentPeer {
                    clock: Rc::clone(&offset),
                    bounds: Rc::clone(&bounds),
                }),
                Vec::new(),
                IoTimeout::with_clock(Some(5), "sender", clock),
            );
            if buffered {
                Core::start_buffering(&core, Arc::new(AtomicU64::new(0)));
            }
            let err = if buffered {
                CoreWriter::new(&core).write(b"x").unwrap_err()
            } else {
                CoreReader::new(&core).read(&mut [0u8; 1]).unwrap_err()
            };
            assert_eq!(err.kind(), io::ErrorKind::TimedOut);
            assert_eq!(
                err.to_string(),
                "[sender] io timeout after 6 seconds -- exiting"
            );
            assert_eq!(*bounds.borrow(), vec![Some(Duration::from_secs(3)); 3]);
        }
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
    /// A core over a socket with the multiplex output attached and counted,
    /// and the writer that fills that output.
    fn core_with_mux() -> (
        SharedCore<'static>,
        UnixStream,
        Arc<AtomicU64>,
        crate::writer::multiplex::MultiplexWriter<CoreWriter<'static>>,
    ) {
        let (core, peer, _) = core_over_socket();
        let written = Arc::new(AtomicU64::new(0));
        Core::start_output_accounting(&core, Arc::clone(&written), None);
        let mux = crate::writer::multiplex::MultiplexWriter::new(CoreWriter::new(&core));
        Core::attach_mux(&core, mux.shared_out());
        (core, peer, written, mux)
    }

    /// What the peer can read right now, without waiting.
    fn peer_ready(peer: &mut UnixStream) -> Vec<u8> {
        peer.set_nonblocking(true).unwrap();
        let mut got = vec![0u8; 64];
        let n = match peer.read(&mut got) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => 0,
            Err(e) => panic!("peer read: {e}"),
        };
        peer.set_nonblocking(false).unwrap();
        got.truncate(n);
        got
    }

    /// WHY: upstream's sender queues an empty `MSG_DATA` after a read that
    /// needed input while `last_io_out` is still 0 (io.c:113, io.c:933-936,
    /// io.c:1665-1668), so with `--timeout` its first frame is that keepalive;
    /// a core whose role does not read that way queues nothing.
    #[test]
    fn a_senders_first_read_queues_the_keepalive_ahead_of_its_output() {
        for sender in [true, false] {
            let (core, mut peer, _, mut mux) = core_with_mux();
            mux.set_allowed_lull(Some(Duration::from_secs(3600)));
            if sender {
                Core::keepalive_on_read(&core);
            }
            peer.write_all(b"x").unwrap();
            let mut one = [0u8; 1];
            CoreReader::new(&core).read_exact(&mut one).unwrap();
            mux.write_all(b"ab").unwrap();
            mux.flush().unwrap();

            let expected: &[u8] = if sender {
                b"\x00\x00\x00\x07\x02\x00\x00\x07ab"
            } else {
                b"\x02\x00\x00\x07ab"
            };
            assert_eq!(peer_ready(&mut peer), expected, "sender={sender}");
        }
    }

    /// WHY: upstream writes `iobuf.out` whenever perform_io() is about to
    /// wait for input (io.c:766-768), so the peer has everything this side
    /// buffered before this side waits on the peer. That drain is the only
    /// flush the sender's per-file loop gets, and the bytes it writes are
    /// counted as `stats.total_written` (io.c:977).
    #[test]
    fn a_read_that_needs_the_transport_writes_the_buffered_frame_first() {
        let (core, mut peer, written, mut mux) = core_with_mux();
        mux.write_all(b"abc").unwrap();
        assert!(peer_ready(&mut peer).is_empty(), "still buffered");

        peer.write_all(b"x").unwrap();
        let mut one = [0u8; 1];
        CoreReader::new(&core).read_exact(&mut one).unwrap();

        assert_eq!(peer_ready(&mut peer), b"\x03\x00\x00\x07abc");
        assert_eq!(written.load(Ordering::Relaxed), 7);
    }

    /// WHY: a drain that waits for output room reads the peer's input in
    /// the same wait (io.c:883-921); the read must serve that input, not go
    /// on to wait for more. A peer that already sent its next request and
    /// now waits for this side's reply would otherwise never be answered.
    #[test]
    fn a_drain_that_collects_input_serves_it_without_another_wait() {
        let waits = Rc::new(RefCell::new(Vec::new()));
        let core = Core::new(
            Box::new(FullUntilWaited {
                pending_input: b"req".to_vec(),
                waited: false,
                waits: Rc::clone(&waits),
            }),
            Vec::new(),
            IoTimeout::new(None, "sender"),
        );
        Core::start_buffering(&core, Arc::new(AtomicU64::new(0)));
        let mut mux = crate::writer::multiplex::MultiplexWriter::new(CoreWriter::new(&core));
        Core::attach_mux(&core, mux.shared_out());
        mux.write_all(b"abc").unwrap();

        let mut got = [0u8; 3];
        CoreReader::new(&core).read_exact(&mut got).unwrap();

        assert_eq!(&got, b"req");
        assert_eq!(*waits.borrow(), vec![(true, true)], "one wait, for room");
    }

    /// WHY: a read the input buffer can already satisfy never reaches
    /// perform_io() (io.c:725-727), so it must not write anything: the next
    /// bytes still join the same frame, as they do upstream.
    #[test]
    fn a_read_served_from_the_input_buffer_leaves_the_output_buffered() {
        let (core, mut peer, _, mut mux) = core_with_mux();
        peer.write_all(b"xy").unwrap();
        let mut reader = CoreReader::new(&core);
        let mut one = [0u8; 1];
        reader.read_exact(&mut one).unwrap();

        mux.write_all(b"abc").unwrap();
        reader.read_exact(&mut one).unwrap();

        assert!(peer_ready(&mut peer).is_empty(), "nothing written");
    }

    /// WHY: perform_io() reads a readable descriptor before it writes a
    /// writable one (io.c:883-921, io.c:948), so a peer that has gone is
    /// reported as the end of its input, the way upstream reports it
    /// (whine_about_eof), not as a write error.
    #[test]
    fn a_peer_that_has_gone_reads_as_end_of_input_not_as_a_write_error() {
        let (core, peer, _, mut mux) = core_with_mux();
        mux.write_all(b"abc").unwrap();
        drop(peer);

        let mut one = [0u8; 1];
        assert_eq!(CoreReader::new(&core).read(&mut one).unwrap(), 0);
    }

    /// WHY: `--bwlimit` paces every byte the transport takes and caps each
    /// write at `bwlimit_writemax` (io.c:952, io.c:980), and every byte is
    /// counted (io.c:977) - including the ones the input wait drains.
    #[test]
    fn output_accounting_clamps_counts_and_paces_each_write() {
        let (core, mut peer, _) = core_over_socket();
        let written = Arc::new(AtomicU64::new(0));
        let limiter =
            bandwidth::BandwidthLimiter::new(std::num::NonZeroU64::new(8 * 1024).unwrap());
        let max = limiter.write_max_bytes();
        Core::start_output_accounting(&core, Arc::clone(&written), Some(limiter));

        let payload = vec![7u8; max + 10];
        let n = CoreWriter::new(&core).write(&payload).unwrap();

        assert_eq!(n, max, "one write is capped at bwlimit_writemax");
        assert_eq!(written.load(Ordering::Relaxed), max as u64);
        let mut got = vec![0u8; max];
        peer.read_exact(&mut got).unwrap();
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
            IoTimeout::new(None, "sender"),
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
    /// as upstream's pre-buffering read_buf() is (io.c:2164-2168); bytes a
    /// handshake reader buffered come first, and once buffering starts the
    /// remainder counts as read.
    #[test]
    fn raw_mode_serves_the_handshake_leftovers_first_then_buffers_and_counts() {
        let counter = Arc::new(AtomicU64::new(0));
        let core = Core::new(
            Box::new(BlockingStreams::new(&b"tail"[..], Vec::new())),
            b"head".to_vec(),
            IoTimeout::new(None, "sender"),
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

    /// A transport that is never writable, whose waits first report an
    /// interruption and then output room, and which can request a shutdown
    /// from inside a wait the way a signal handler would.
    struct InterruptedWait {
        waits: u32,
        shutdown_in_wait: bool,
    }

    impl Transport for InterruptedWait {
        fn try_read(&mut self, _: &mut [u8]) -> io::Result<Option<usize>> {
            Ok(None)
        }

        fn try_write(&mut self, buf: &[u8]) -> io::Result<Option<usize>> {
            Ok((self.waits > 1).then_some(buf.len()))
        }

        fn wait(
            &mut self,
            _: bool,
            output: bool,
            _: Option<std::time::Duration>,
        ) -> io::Result<fast_io::transport::Readiness> {
            self.waits += 1;
            if self.waits == 1 {
                if self.shutdown_in_wait {
                    fast_io::signal::mark_shutdown();
                }
                return Err(io::ErrorKind::Interrupted.into());
            }
            Ok(fast_io::transport::Readiness {
                readable: false,
                writable: output,
            })
        }
    }

    fn write_through(shutdown_in_wait: bool) -> io::Result<usize> {
        fast_io::signal::reset_shutdown_for_testing();
        let core = Core::new(
            Box::new(InterruptedWait {
                waits: 0,
                shutdown_in_wait,
            }),
            Vec::new(),
            IoTimeout::new(None, "sender"),
        );
        Core::start_buffering(&core, Arc::new(AtomicU64::new(0)));
        let result = CoreWriter::new(&core).write(b"data");
        fast_io::signal::reset_shutdown_for_testing();
        result
    }

    /// Task 757: a signal that requests a shutdown while the core waits for
    /// output room ends the write with the shutdown error (graded 20), not
    /// with `Interrupted`, which every `write_all` above would retry
    /// (upstream: io.c:997-998 reads `got_kill_signal` after the wait).
    #[test]
    fn a_shutdown_during_a_wait_ends_the_write_with_the_shutdown_error() {
        let err = write_through(true).unwrap_err();
        assert!(crate::shared::is_signal_shutdown(&err), "{err}");
    }

    /// The control: an interrupted wait with no shutdown requested is an
    /// empty one, and the write carries on (upstream: io.c:866-884).
    #[test]
    fn an_interrupted_wait_without_a_shutdown_is_an_empty_wait() {
        assert_eq!(write_through(false).unwrap(), 4);
    }
}
