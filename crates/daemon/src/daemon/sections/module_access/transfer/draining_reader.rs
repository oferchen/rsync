// Background socket-drain reader for the daemon delta-transfer phase (#503).
//
// The daemon splits one TCP socket into two blocking `try_clone()` fds (see
// `streams.rs`). The receiver delta loop writes a batch of file requests, then
// blocks reading the sender's response - with no interleaved drain of incoming
// frames. Once both ~128 KB kernel socket buffers fill, neither direction can
// make progress: a full-duplex write-write deadlock. Over SSH/stdio the peer
// runs in a separate process with independent pipe buffers, so the same engine
// code does not wedge; the fault is specific to the single-socket daemon path.
//
// `DrainingReader` breaks the wedge the same way upstream's `perform_io` does:
// it guarantees the peer's send buffer is always being drained. A dedicated
// background thread continuously reads the read-clone fd into an unbounded
// queue; the main receiver loop pulls bytes from that queue instead of blocking
// on the socket directly. Because the peer's bytes are always being consumed
// off the wire, the peer never blocks writing, so this side's writes always
// drain and the deadlock is structurally impossible. The queue is unbounded so
// the drain thread never blocks on a full queue and stop draining the socket -
// that would re-arm the wedge. In-memory growth is bounded by the protocol's
// own write-side flow control (the receiver keeps only a bounded window of
// outstanding requests), mirroring upstream's `iobuf.in` that grows as needed.
//
// This is a transparent byte pipe: it preserves every byte and its order, so
// the multiplex framing, wire format, and goodbye handshake are unchanged. It
// changes only *when* the socket is read, not *what* is read. It is constructed
// only on the daemon-TCP path; SSH/stdio and local transports keep reading the
// socket directly and are byte-identical to before.
//
// The drain thread bounds its blocking `read()` with a short read timeout
// (`SO_RCVTIMEO`) and polls: an idle socket returns `TimedOut`, on which the
// loop re-checks the stop flag before retrying. This bounds the stop-flag wake
// latency so `stop_and_join()` never hangs on a silent peer.
//
// CRITICAL: the drain fd must NOT be put in non-blocking mode. All of the
// daemon's socket clones (read, write, goodbye) are `try_clone()`s of the same
// underlying socket, which on Unix share one open file description. `O_NONBLOCK`
// lives on that shared description, so flipping the drain clone non-blocking
// ALSO makes the concurrent WRITE clone non-blocking: the sender's `write_all`
// then hits `WouldBlock` on a full send buffer and the transfer aborts /
// truncates (`multiplexed payload truncated`, code 23). A read timeout instead
// sets only `SO_RCVTIMEO`; it leaks onto sibling clones too, but the write clone
// only ever writes (governed by `SO_SNDTIMEO`, left unset), so its writes stay
// blocking and lossless.
//
// Shutdown ordering (design doc section 5.2): the drain thread must be stopped
// and joined before the orchestration TCP goodbye drain runs, because that
// drain reads a *different* clone of the same socket. The caller holds a
// `DrainHandle` and calls `stop()` after the transfer engine returns and before
// the goodbye drain. `Drop` also stops-and-joins as a backstop on every exit
// path (success, error, early return), so the thread can never outlive the
// transfer. The read timeout leaks onto the shared socket object, so the drain
// thread clears it on exit - before `stop_and_join()` returns - leaving the
// goodbye-drain clone a normal blocking socket.
//
// upstream: io.c:882-889 perform_io() drains readable multiplex messages
// whenever it is about to write, keeping the peer's send buffer emptied.
//
// This file is `include!`d into the `crate::daemon` scope, so it reuses the
// enclosing module's imports (`Arc`, `Mutex`, `AtomicBool`, `Ordering`,
// `thread`, `io`, `Read`) and fully qualifies the `mpsc` types it adds.

/// Size of each socket read the drain thread performs.
const DRAIN_CHUNK_SIZE: usize = 64 * 1024;

/// Read timeout that bounds each blocking drain `read()`.
///
/// The drain thread reads with this `SO_RCVTIMEO` so an idle socket returns
/// `TimedOut` instead of parking the thread indefinitely. On that signal the
/// loop re-checks the stop flag and retries, so `stop_and_join()`'s wake
/// latency is at most one interval on every platform. A read timeout (unlike
/// non-blocking mode) does not disturb the concurrent write clone's blocking
/// writes - see the module comment. At ~50 ms it adds negligible latency to
/// real draining (bursty + write-side-flow-bounded) and is dwarfed by the
/// transfer's own I/O.
const DRAIN_READ_TIMEOUT: Duration = Duration::from_millis(50);

/// Brief yield after a timed-out/would-block read to guard against a busy-spin
/// on the rare path where the read timeout could not be armed.
const DRAIN_SPIN_GUARD: Duration = Duration::from_millis(2);

/// One item handed from the drain thread to the consumer: either a chunk of
/// wire bytes, or the terminal read result (EOF or error) once the socket
/// stops yielding data.
enum DrainItem {
    Data(Vec<u8>),
    End(io::Result<()>),
}

/// A socket the drain thread reads with a bounded read timeout.
///
/// The drain loop must be able to arm a read timeout so an idle socket returns
/// `TimedOut` and the loop can observe `stop()` promptly, without ever putting
/// the shared socket in non-blocking mode (which would make the sibling write
/// clone non-blocking and truncate the transfer - see the module comment). This
/// trait exposes just that capability on top of `Read`, so `DrainingReader::new`
/// stays generic (production wraps a cloned `TcpStream`; tests wrap a loopback
/// `TcpStream`) while still guaranteeing the poll-loop contract at the type
/// level.
trait DrainSource: Read + Send {
    /// Sets (or clears, with `None`) the read timeout on the underlying fd.
    fn set_drain_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
}

impl DrainSource for TcpStream {
    fn set_drain_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.set_read_timeout(timeout)
    }
}

/// A blocking `Read` adapter backed by a background socket-drain thread.
///
/// Wraps the daemon's read-clone fd and spawns a thread that continuously
/// reads it into an unbounded channel. `Read::read` serves bytes from the
/// channel, so the consuming receiver loop never blocks on the socket while a
/// write would otherwise wedge. See the module comment for the full rationale
/// and shutdown contract.
struct DrainingReader {
    rx: std::sync::mpsc::Receiver<DrainItem>,
    /// Leftover bytes from a chunk that did not fit the caller's buffer.
    residual: Vec<u8>,
    residual_pos: usize,
    /// Shared stop flag and join handle, also held by the caller's
    /// `DrainHandle` so the thread can be stopped before the goodbye drain.
    inner: Arc<DrainInner>,
}

/// Shared shutdown state between the reader and its external stop handle.
struct DrainInner {
    stop: AtomicBool,
    join: Mutex<Option<thread::JoinHandle<()>>>,
}

/// External stop handle for a `DrainingReader`'s background thread.
///
/// Held by the daemon transfer orchestrator so the drain thread can be
/// stopped and joined *before* the TCP goodbye drain reads the socket via a
/// different clone. `stop()` is idempotent.
struct DrainHandle {
    inner: Arc<DrainInner>,
}

impl DrainInner {
    /// Signals the drain thread to stop and joins it. Idempotent: safe to call
    /// from both the external `DrainHandle` and `DrainingReader::drop`.
    fn stop_and_join(&self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(mut guard) = self.join.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }
    }
}

impl DrainHandle {
    /// Stops the background drain thread and joins it. Must be called before
    /// the orchestration goodbye drain reads the socket via another clone.
    fn stop(&self) {
        self.inner.stop_and_join();
    }
}

impl DrainingReader {
    /// Wraps `source` (a daemon read-clone fd) and spawns the drain thread.
    ///
    /// Returns the reader plus a `DrainHandle` for out-of-band shutdown. The
    /// thread reads `source` into an unbounded channel until EOF, error, or the
    /// stop flag is set.
    ///
    /// The channel is unbounded so the drain thread NEVER blocks on send: it
    /// keeps the kernel socket receive buffer continuously drained, which is
    /// what breaks the wedge (a bounded queue would let the thread block on a
    /// full queue and stop draining the socket, re-arming the deadlock). This
    /// mirrors upstream's `iobuf.in`, which grows as needed rather than
    /// applying back-pressure to the peer mid-transfer. In-memory growth is
    /// naturally bounded by the protocol's own write-side flow control: the
    /// receiver only ever has a bounded window of outstanding file requests, so
    /// the peer can only stream a bounded amount of delta data ahead of what
    /// the consumer drains.
    fn new<R: DrainSource + 'static>(source: R) -> (Self, DrainHandle) {
        let (tx, rx): (
            std::sync::mpsc::Sender<DrainItem>,
            std::sync::mpsc::Receiver<DrainItem>,
        ) = std::sync::mpsc::channel();
        let inner = Arc::new(DrainInner {
            stop: AtomicBool::new(false),
            join: Mutex::new(None),
        });
        let thread_inner = Arc::clone(&inner);

        // Arm a short read timeout so `read()` returns `TimedOut` on an idle
        // socket, letting the loop observe the stop flag within one
        // `DRAIN_READ_TIMEOUT` on every platform. A read timeout - unlike
        // non-blocking mode - only sets `SO_RCVTIMEO`; it leaks onto the shared
        // socket object (and thus the sibling write clone), but the write clone
        // never reads, so its blocking writes stay lossless. Non-blocking mode
        // would leak onto the write clone and abort/truncate the transfer.
        // Setting the timeout is best-effort: on failure the loop still works
        // (it may block on `read()` until data or FIN); the FIN on a real
        // transfer close still unblocks it, and `Drop` remains a backstop.
        //
        // The timeout leaks onto the daemon's OTHER clone of this socket - the
        // `DaemonStream` the orchestrator's goodbye drain reads - so the drain
        // thread CLEARS it on exit (below), before `stop_and_join()` returns, so
        // the goodbye drain sees a normal blocking socket and its own bounded
        // read-timeout loop behaves as before. `stop()` joins the thread, so the
        // clear is complete before the goodbye drain runs.
        let _ = source.set_drain_read_timeout(Some(DRAIN_READ_TIMEOUT));

        let handle = thread::Builder::new()
            .name("daemon-delta-drain".to_owned())
            .spawn(move || {
                let mut source = source;
                let mut buf = vec![0u8; DRAIN_CHUNK_SIZE];
                // Run the drain loop, then unconditionally clear the read timeout
                // on the shared socket so the goodbye-drain clone is unaffected.
                'drain: loop {
                    if thread_inner.stop.load(Ordering::Acquire) {
                        break 'drain;
                    }
                    match source.read(&mut buf) {
                        Ok(0) => {
                            let _ = tx.send(DrainItem::End(Ok(())));
                            break 'drain;
                        }
                        Ok(n) => {
                            // Unbounded send never blocks, so the thread loops
                            // straight back to `read()` and keeps the socket
                            // receive buffer drained. A send error means the
                            // consumer dropped the receiver (transfer over).
                            if tx.send(DrainItem::Data(buf[..n].to_vec())).is_err() {
                                break 'drain;
                            }
                        }
                        // Read timeout elapsed on an idle socket (`TimedOut`), or
                        // an interrupted syscall: not a wire error. Re-check the
                        // stop flag and retry. This bounds the stop-flag
                        // observation latency to one `DRAIN_READ_TIMEOUT` on every
                        // platform, so `stop_and_join()` always unblocks the
                        // thread promptly. `WouldBlock` is tolerated too as a
                        // backstop, though the drain fd is never put in
                        // non-blocking mode (that would truncate the sibling write
                        // clone - see the module comment).
                        Err(ref e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::Interrupted
                                    | io::ErrorKind::WouldBlock
                                    | io::ErrorKind::TimedOut
                            ) =>
                        {
                            if thread_inner.stop.load(Ordering::Acquire) {
                                break 'drain;
                            }
                            // The read timeout already paces the loop (each
                            // `read()` blocks up to `DRAIN_READ_TIMEOUT`). A brief
                            // yield guards against a busy-spin if the timeout could
                            // not be armed and the fd nonetheless returns
                            // `WouldBlock`.
                            thread::sleep(DRAIN_SPIN_GUARD);
                            continue;
                        }
                        Err(e) => {
                            let _ = tx.send(DrainItem::End(Err(e)));
                            break 'drain;
                        }
                    }
                }
                // Clear the read timeout on the shared socket before the thread
                // exits (and thus before `stop_and_join()` returns), so the
                // separate goodbye-drain clone reads a normal blocking socket.
                let _ = source.set_drain_read_timeout(None);
            })
            .expect("failed to spawn daemon delta-drain thread");

        if let Ok(mut guard) = inner.join.lock() {
            *guard = Some(handle);
        }

        let reader = DrainingReader {
            rx,
            residual: Vec::new(),
            residual_pos: 0,
            inner: Arc::clone(&inner),
        };
        let stop_handle = DrainHandle { inner };
        (reader, stop_handle)
    }

    /// Copies as many residual bytes as fit into `out`, returning the count.
    fn drain_residual(&mut self, out: &mut [u8]) -> usize {
        let available = self.residual.len() - self.residual_pos;
        let n = available.min(out.len());
        out[..n].copy_from_slice(&self.residual[self.residual_pos..self.residual_pos + n]);
        self.residual_pos += n;
        if self.residual_pos == self.residual.len() {
            self.residual.clear();
            self.residual_pos = 0;
        }
        n
    }
}

impl Read for DrainingReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }

        // Serve any bytes left over from a previous oversized chunk first.
        if self.residual_pos < self.residual.len() {
            return Ok(self.drain_residual(out));
        }

        // Items are FIFO, so the terminal `End` only arrives after every
        // `Data` chunk has been drained: no wire bytes are dropped ahead of
        // EOF/error.
        match self.rx.recv() {
            Ok(DrainItem::Data(chunk)) => {
                self.residual = chunk;
                self.residual_pos = 0;
                Ok(self.drain_residual(out))
            }
            Ok(DrainItem::End(result)) => match result {
                Ok(()) => Ok(0),
                Err(e) => Err(e),
            },
            // Sender dropped without a terminal item (thread stopped): treat as
            // clean EOF - the caller's higher-level framing surfaces any real
            // truncation.
            Err(_) => Ok(0),
        }
    }
}

impl Drop for DrainingReader {
    fn drop(&mut self) {
        // Backstop for every exit path: stop and join the thread so it can
        // never outlive the transfer or race the goodbye drain. Idempotent
        // with the external `DrainHandle::stop`.
        self.inner.stop_and_join();
    }
}

#[cfg(test)]
mod draining_reader_tests {
    //! Byte-pipe fidelity and anti-deadlock (#503) tests for `DrainingReader`.

    use super::DrainingReader;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    /// A loopback TCP pair: the background writer feeds `payload` in small
    /// chunks with a flush between them, mimicking a peer streaming delta
    /// tokens into a socket the receiver reads through a `DrainingReader`.
    fn spawn_socket_feeder(payload: Vec<u8>, chunk: usize) -> TcpStream {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let feeder = thread::spawn(move || {
            let (mut server, _) = listener.accept().expect("accept");
            for part in payload.chunks(chunk.max(1)) {
                server.write_all(part).expect("write chunk");
                server.flush().expect("flush chunk");
            }
            // Drop `server` -> FIN -> EOF on the reader side.
        });
        let client = TcpStream::connect(addr).expect("connect loopback");
        // Detach: the feeder finishes on its own; the reader observes EOF.
        let _ = feeder;
        client
    }

    #[test]
    fn preserves_bytes_and_order() {
        // Every byte the peer writes must arrive at the consumer in order,
        // regardless of chunking: the wrapper is a transparent byte pipe.
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let sock = spawn_socket_feeder(payload.clone(), 1500);
        let (mut reader, _handle) = DrainingReader::new(sock);

        let mut received = Vec::new();
        reader.read_to_end(&mut received).expect("read to end");
        assert_eq!(received, payload, "DrainingReader must preserve every byte in order");
    }

    #[test]
    fn drains_socket_while_consumer_is_slow() {
        // The anti-deadlock guarantee (#503): the background thread keeps the
        // socket receive buffer drained even when the consumer reads slowly.
        // A payload larger than the kernel socket buffer would wedge a
        // single-threaded reader that only reads after writing; here the drain
        // thread consumes it while the consumer trickles reads one byte at a
        // time, and all bytes still arrive intact.
        let payload: Vec<u8> = (0..500_000u32).map(|i| (i % 253) as u8).collect();
        let sock = spawn_socket_feeder(payload.clone(), 4096);
        let (mut reader, _handle) = DrainingReader::new(sock);

        let mut received = Vec::with_capacity(payload.len());
        let mut one = [0u8; 1];
        loop {
            match reader.read(&mut one) {
                Ok(0) => break,
                Ok(_) => received.push(one[0]),
                Err(e) => panic!("read error: {e}"),
            }
        }
        assert_eq!(received.len(), payload.len());
        assert_eq!(received, payload);
    }

    #[test]
    fn stop_handle_joins_thread_idempotently() {
        // The external stop handle must halt and join the drain thread before
        // the goodbye drain runs, and be safe to call more than once (Drop
        // calls it again). No panic, no hang.
        let payload = vec![7u8; 64];
        let sock = spawn_socket_feeder(payload, 16);
        let (reader, handle) = DrainingReader::new(sock);
        handle.stop();
        handle.stop(); // idempotent
        drop(reader); // Drop's stop_and_join must also be a no-op now.
    }

    #[test]
    fn surfaces_eof_after_all_bytes() {
        // After the last chunk, a read returns Ok(0) exactly once the queue is
        // drained - no bytes dropped ahead of EOF.
        let payload = vec![1u8, 2, 3, 4, 5];
        let sock = spawn_socket_feeder(payload.clone(), 2);
        let (mut reader, _handle) = DrainingReader::new(sock);
        let mut got = Vec::new();
        reader.read_to_end(&mut got).expect("read to end");
        assert_eq!(got, payload);
        // Subsequent reads keep returning EOF.
        let mut extra = [0u8; 4];
        assert_eq!(reader.read(&mut extra).expect("post-eof read"), 0);
    }

    #[test]
    fn stop_joins_promptly_when_peer_is_silent() {
        // Regression (#503, Windows CI): a silent peer that connects but never
        // sends and never closes must NOT leave the drain thread parked. The
        // bounded read timeout makes `read()` return `TimedOut` on the idle
        // socket and the loop checks the stop flag each interval, so `stop()`'s
        // join returns promptly instead of hanging forever. The join must
        // complete well within a second.
        //
        // The socket is left in default (no read timeout) mode here to prove the
        // fix does not depend on the caller pre-arming anything:
        // `DrainingReader::new` arms the read timeout itself.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        // Accept and then sit silent, holding the connection open (no data,
        // no FIN) until the test drops it.
        let silent_peer = thread::spawn(move || {
            let (server, _) = listener.accept().expect("accept");
            // Park the accepted socket alive; the reader must not depend on a
            // FIN to stop its drain thread.
            thread::sleep(std::time::Duration::from_secs(2));
            drop(server);
        });
        // Left with no read timeout on purpose: the drain thread arms it
        // itself, so the prompt join must not depend on any caller-side flag.
        let sock = TcpStream::connect(addr).expect("connect loopback");
        let (reader, handle) = DrainingReader::new(sock);

        let start = std::time::Instant::now();
        handle.stop();
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "stop() must join the drain thread promptly even with a silent peer"
        );
        drop(reader);
        let _ = silent_peer.join();
    }

    #[test]
    fn replays_all_buffered_bytes_before_stop() {
        // Queue-drain invariant (mirrors upstream perform_io io.c:882): every
        // byte the drain thread pulled off the socket must be replayed to the
        // consumer in order, even after `stop()` halts further draining. The
        // consumer reads the full transfer payload through the reader, then
        // stops the handle; no byte the thread already buffered may be lost.
        let payload: Vec<u8> = (0..40_000u32).map(|i| (i % 249) as u8).collect();
        let sock = spawn_socket_feeder(payload.clone(), 3000);
        let (mut reader, handle) = DrainingReader::new(sock);

        // Drain the entire payload through the reader (it arrives via the
        // background thread's queue), then stop.
        let mut received = Vec::with_capacity(payload.len());
        reader.read_to_end(&mut received).expect("read to end");
        handle.stop();

        assert_eq!(
            received, payload,
            "every drained byte must replay to the consumer in order, no goodbye-byte loss"
        );
    }

    #[test]
    fn goodbye_clone_reads_after_drain_stops() {
        // The goodbye drain reads the socket via a SEPARATE clone
        // (`ctx.reader`'s `DaemonStream`), not the drain clone. Arming the
        // drain clone's read timeout must not disturb bytes that arrive for the
        // goodbye reader on another clone of the same connection, and the drain
        // thread must clear that timeout on stop so the goodbye reader sees a
        // normal blocking socket. Here the drain thread consumes the first
        // burst; after `stop()`, a second clone reads a trailing "goodbye" burst
        // intact.
        //
        // The peer is sequenced (send first burst, wait for a go-ahead, then
        // send goodbye) so the drain thread cannot race the goodbye bytes into
        // its own queue - matching the real daemon order where `stop()` runs
        // after the engine's goodbye and before the goodbye-drain clone reads.
        use std::sync::mpsc::channel;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let first: Vec<u8> = (0..8_000u32).map(|i| (i % 251) as u8).collect();
        let goodbye: Vec<u8> = vec![0xABu8; 512];
        let first_for_peer = first.clone();
        let goodbye_for_peer = goodbye.clone();
        let (go_tx, go_rx) = channel::<()>();
        let peer = thread::spawn(move || {
            let (mut server, _) = listener.accept().expect("accept");
            server.write_all(&first_for_peer).expect("write first burst");
            server.flush().expect("flush first");
            // Wait until the consumer has drained the first burst and stopped
            // the drain thread before writing the trailing goodbye bytes.
            let _ = go_rx.recv();
            server.write_all(&goodbye_for_peer).expect("write goodbye");
            server.flush().expect("flush goodbye");
            thread::sleep(std::time::Duration::from_millis(50));
            drop(server);
        });

        let drain_sock = TcpStream::connect(addr).expect("connect loopback");
        // The goodbye reader is a separate clone of the same socket, exactly as
        // the daemon splits `read_stream` (drain) from `ctx.reader` (goodbye).
        let mut goodbye_reader = drain_sock.try_clone().expect("clone for goodbye");
        let (mut reader, handle) = DrainingReader::new(drain_sock);

        // Consume the first burst through the drain reader.
        let mut got_first = vec![0u8; first.len()];
        reader.read_exact(&mut got_first).expect("read first burst");
        assert_eq!(got_first, first, "drain reader must replay the first burst intact");

        // Stop the drain thread (it clears the read timeout on exit, then the
        // drain clone is dropped), then let the peer send its trailing goodbye
        // bytes.
        handle.stop();
        drop(reader);
        go_tx.send(()).expect("signal peer to send goodbye");

        // The separate goodbye clone must read the trailing bytes; the drain
        // clone's read timeout must not have stolen or reordered them. Give the
        // goodbye clone a bounded read timeout so the test cannot hang if the
        // invariant is violated.
        goodbye_reader
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .expect("set goodbye read timeout");
        let mut got_goodbye = Vec::new();
        goodbye_reader
            .read_to_end(&mut got_goodbye)
            .expect("read goodbye burst");
        assert_eq!(
            got_goodbye, goodbye,
            "the separate goodbye clone must read the trailing bytes intact after drain stop"
        );

        let _ = peer.join();
    }

    #[test]
    fn sibling_write_clone_stays_blocking_and_lossless() {
        // Regression for the truncation bug (code 23, `multiplexed payload
        // truncated`): the daemon's read, write, and goodbye handles are all
        // `try_clone()`s of one socket sharing a single open file description.
        // If `DrainingReader::new` had put the drain clone in NON-blocking mode,
        // the sibling WRITE clone would become non-blocking too and the sender's
        // `write_all` would abort with `WouldBlock` on a full send buffer,
        // truncating the transfer. The read-timeout fix must leave the write
        // clone blocking so a large write drains fully and losslessly even when
        // the peer reads slowly enough to fill the send buffer.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        // Payload larger than a typical socket send buffer, so a non-blocking
        // write clone would return `WouldBlock` before draining it all and the
        // peer would receive fewer bytes. A blocking write clone streams it in
        // full regardless of send-buffer pressure.
        let payload: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
        let payload_len = payload.len();

        // Peer reads exactly `payload_len` bytes, draining the sender's send
        // buffer so a blocking `write_all` completes. Reading a fixed count
        // (not to EOF) avoids depending on a FIN: the drain thread keeps its own
        // clone of the client socket open, so the last handle never closes.
        let peer = thread::spawn(move || {
            let (mut server, _) = listener.accept().expect("accept");
            let mut got = vec![0u8; payload_len];
            server.read_exact(&mut got).expect("peer read exact");
            got
        });

        let sock = TcpStream::connect(addr).expect("connect loopback");
        let mut write_clone = sock.try_clone().expect("clone for write");
        // Arming the read timeout on the drain clone must NOT make `write_clone`
        // non-blocking (which would truncate the write below).
        let (_reader, handle) = DrainingReader::new(sock);

        // Write from a dedicated thread so the test cannot deadlock if the
        // invariant is violated: the peer drains concurrently.
        let payload_for_writer = payload.clone();
        let writer = thread::spawn(move || {
            write_clone
                .write_all(&payload_for_writer)
                .expect("blocking write_all must not fail with WouldBlock");
            write_clone.flush().expect("flush");
        });

        writer.join().expect("writer join");
        let got = peer.join().expect("peer join");
        assert_eq!(
            got.len(),
            payload_len,
            "the sibling write clone must deliver every byte (no truncation)"
        );
        assert_eq!(got, payload, "write clone bytes must arrive intact and in order");
        handle.stop();
    }
}
