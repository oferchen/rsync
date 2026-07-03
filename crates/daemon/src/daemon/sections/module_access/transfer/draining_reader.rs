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
// The drain thread runs its read-clone fd in non-blocking mode and polls: an
// empty socket returns `WouldBlock`, on which the loop re-checks the stop flag
// and sleeps a couple of milliseconds before retrying. This bounds the
// stop-flag wake latency on every platform without depending on read-timeout
// wakeups, which on the daemon's cloned Windows socket fd did not reliably wake
// a blocked `read()` and wedged `join()` in CI.
//
// Shutdown ordering (design doc section 5.2): the drain thread must be stopped
// and joined before the orchestration TCP goodbye drain runs, because that
// drain reads a *different* clone of the same socket. The caller holds a
// `DrainHandle` and calls `stop()` after the transfer engine returns and before
// the goodbye drain. `Drop` also stops-and-joins as a backstop on every exit
// path (success, error, early return), so the thread can never outlive the
// transfer. Non-blocking mode is a property of the shared socket object (both
// clones observe it), so the drain thread restores blocking mode on exit -
// before `stop_and_join()` returns - leaving the goodbye-drain clone a normal
// blocking socket.
//
// upstream: io.c:882-889 perform_io() drains readable multiplex messages
// whenever it is about to write, keeping the peer's send buffer emptied.
//
// This file is `include!`d into the `crate::daemon` scope, so it reuses the
// enclosing module's imports (`Arc`, `Mutex`, `AtomicBool`, `Ordering`,
// `thread`, `io`, `Read`) and fully qualifies the `mpsc` types it adds.

/// Size of each socket read the drain thread performs.
const DRAIN_CHUNK_SIZE: usize = 64 * 1024;

/// Poll interval when the non-blocking drain read has no data ready.
///
/// The drain thread runs the read handle in non-blocking mode: an empty socket
/// returns `WouldBlock` immediately instead of parking the thread. On that
/// signal the loop re-checks the stop flag and, if still running, sleeps this
/// long before retrying. This bounds `stop_and_join()`'s wake latency to at
/// most one interval on every platform, without relying on read-timeout
/// wakeups (which do not fire reliably on the daemon's cloned Windows socket
/// fd - the failure mode that wedged `join()` in CI). At ~2 ms the poll adds
/// negligible latency to real draining (bursty + write-side-flow-bounded) and
/// is dwarfed by the transfer's own I/O.
const DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(2);

/// One item handed from the drain thread to the consumer: either a chunk of
/// wire bytes, or the terminal read result (EOF or error) once the socket
/// stops yielding data.
enum DrainItem {
    Data(Vec<u8>),
    End(io::Result<()>),
}

/// A socket the drain thread reads in non-blocking mode.
///
/// The drain loop must be able to switch its read handle to non-blocking so an
/// empty socket returns `WouldBlock` immediately instead of parking the thread
/// past `stop()`. This trait exposes just that capability on top of `Read`, so
/// `DrainingReader::new` stays generic (production wraps a cloned `TcpStream`;
/// tests wrap a loopback `TcpStream`) while still guaranteeing the poll-loop
/// contract at the type level.
trait DrainSource: Read + Send {
    /// Switches the underlying fd between non-blocking and blocking mode.
    fn set_drain_nonblocking(&self, nonblocking: bool) -> io::Result<()>;
}

impl DrainSource for TcpStream {
    fn set_drain_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.set_nonblocking(nonblocking)
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

        // Put the drain fd in non-blocking mode so `read()` returns `WouldBlock`
        // on an empty socket instead of parking the thread. This is what lets
        // the loop notice the stop flag within one `DRAIN_POLL_INTERVAL` on
        // every platform - no reliance on read-timeout wakeups, which do not
        // fire reliably on the daemon's cloned Windows socket fd and previously
        // wedged `join()` in CI. A failure to set non-blocking is non-fatal:
        // the loop still functions (it may just block on `read()` until data or
        // FIN), matching the earlier best-effort read-timeout handling.
        //
        // On both Unix (`ioctl` FIONBIO / `SO_NONBLOCK`) and Windows
        // (`ioctlsocket` FIONBIO), non-blocking mode is a property of the shared
        // socket object, so it is also observed by the daemon's OTHER clone of
        // this socket - the `DaemonStream` the orchestrator's goodbye drain
        // reads. The drain thread therefore restores BLOCKING mode on exit
        // (below), before `stop_and_join()` returns, so the goodbye drain sees a
        // normal blocking socket and its bounded read-timeout loop behaves as
        // before. `stop()` joins the thread, so the restore is complete before
        // the goodbye drain runs.
        let _ = source.set_drain_nonblocking(true);

        let handle = thread::Builder::new()
            .name("daemon-delta-drain".to_owned())
            .spawn(move || {
                let mut source = source;
                let mut buf = vec![0u8; DRAIN_CHUNK_SIZE];
                // Run the drain loop, then unconditionally restore blocking mode
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
                        // Non-blocking read with no data ready (`WouldBlock`),
                        // or an interrupted syscall: not a wire error. Re-check
                        // the stop flag, then sleep one poll interval before
                        // retrying. This bounds the stop-flag observation
                        // latency on every platform without depending on
                        // read-timeout semantics, so `stop_and_join()` always
                        // unblocks the thread within ~one interval. `TimedOut`
                        // is tolerated too as a backstop for any handle still
                        // carrying a stale read timeout.
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
                            thread::sleep(DRAIN_POLL_INTERVAL);
                            continue;
                        }
                        Err(e) => {
                            let _ = tx.send(DrainItem::End(Err(e)));
                            break 'drain;
                        }
                    }
                }
                // Restore blocking mode on the shared socket before the thread
                // exits (and thus before `stop_and_join()` returns), so the
                // separate goodbye-drain clone reads a blocking socket.
                let _ = source.set_drain_nonblocking(false);
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
        // non-blocking poll loop returns `WouldBlock` on the empty socket and
        // checks the stop flag every poll interval, so `stop()`'s join returns
        // promptly instead of hanging forever. This is the exact deadlock the
        // daemon negotiation tests hit on Windows, where a blocking `read()`
        // (even with a read timeout on the cloned fd) never observed the stop
        // flag. The join must complete well within a second.
        //
        // The socket is deliberately left in default (blocking) mode here to
        // prove the fix does not depend on the caller pre-arming a read timeout
        // or non-blocking flag: `DrainingReader::new` sets non-blocking itself.
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
        // Left in default blocking mode on purpose: the drain thread flips it
        // to non-blocking itself, so the prompt join must not depend on any
        // caller-side timeout or flag.
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
        // (`ctx.reader`'s `DaemonStream`), not the drain clone. Putting the
        // drain clone in non-blocking mode must not disturb bytes that arrive
        // for the goodbye reader on another clone of the same connection. Here
        // the drain thread consumes the first burst; after `stop()`, a second
        // clone reads a trailing "goodbye" burst intact.
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

        // Stop the drain thread (drain clone stays non-blocking, then dropped),
        // then let the peer send its trailing goodbye bytes.
        handle.stop();
        drop(reader);
        go_tx.send(()).expect("signal peer to send goodbye");

        // The separate goodbye clone must read the trailing bytes; the
        // non-blocking flag on the drain clone must not have stolen or reordered
        // them. Give the goodbye clone a bounded read timeout so the test cannot
        // hang if the invariant is violated.
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
}
