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
// Shutdown ordering (design doc section 5.2): the drain thread must be stopped
// and joined before the orchestration TCP goodbye drain runs, because that
// drain reads a *different* clone of the same socket. The caller holds a
// `DrainHandle` and calls `stop()` after the transfer engine returns and before
// the goodbye drain. `Drop` also stops-and-joins as a backstop on every exit
// path (success, error, early return), so the thread can never outlive the
// transfer.
//
// upstream: io.c:882-889 perform_io() drains readable multiplex messages
// whenever it is about to write, keeping the peer's send buffer emptied.
//
// This file is `include!`d into the `crate::daemon` scope, so it reuses the
// enclosing module's imports (`Arc`, `Mutex`, `AtomicBool`, `Ordering`,
// `thread`, `io`, `Read`) and fully qualifies the `mpsc` types it adds.

/// Size of each socket read the drain thread performs.
const DRAIN_CHUNK_SIZE: usize = 64 * 1024;

/// One item handed from the drain thread to the consumer: either a chunk of
/// wire bytes, or the terminal read result (EOF or error) once the socket
/// stops yielding data.
enum DrainItem {
    Data(Vec<u8>),
    End(io::Result<()>),
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
    fn new<R: Read + Send + 'static>(mut source: R) -> (Self, DrainHandle) {
        let (tx, rx): (
            std::sync::mpsc::Sender<DrainItem>,
            std::sync::mpsc::Receiver<DrainItem>,
        ) = std::sync::mpsc::channel();
        let inner = Arc::new(DrainInner {
            stop: AtomicBool::new(false),
            join: Mutex::new(None),
        });
        let thread_inner = Arc::clone(&inner);

        let handle = thread::Builder::new()
            .name("daemon-delta-drain".to_owned())
            .spawn(move || {
                let mut buf = vec![0u8; DRAIN_CHUNK_SIZE];
                loop {
                    if thread_inner.stop.load(Ordering::Acquire) {
                        return;
                    }
                    match source.read(&mut buf) {
                        Ok(0) => {
                            let _ = tx.send(DrainItem::End(Ok(())));
                            return;
                        }
                        Ok(n) => {
                            // Unbounded send never blocks, so the thread loops
                            // straight back to `read()` and keeps the socket
                            // receive buffer drained. A send error means the
                            // consumer dropped the receiver (transfer over).
                            if tx.send(DrainItem::Data(buf[..n].to_vec())).is_err() {
                                return;
                            }
                        }
                        // A read timeout (set on the socket by the caller so a
                        // blocking `read()` cannot pin the thread past `stop()`)
                        // or an interrupted syscall is not a wire error: loop
                        // back to re-check the stop flag and keep draining. This
                        // is what makes `stop_and_join()` reliably unblock the
                        // thread on every platform, including Windows, where a
                        // permanently-blocking `read()` would otherwise never
                        // observe the stop flag and `join()` would hang.
                        Err(ref e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::Interrupted
                                    | io::ErrorKind::WouldBlock
                                    | io::ErrorKind::TimedOut
                            ) =>
                        {
                            continue
                        }
                        Err(e) => {
                            let _ = tx.send(DrainItem::End(Err(e)));
                            return;
                        }
                    }
                }
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
        // sends and never closes leaves the drain thread parked in a blocking
        // `read()`. With a read timeout on the socket, the loop wakes, observes
        // the stop flag, and exits, so `stop()`'s join returns instead of
        // hanging forever. This is the exact deadlock the daemon negotiation
        // tests hit on Windows, where a blocking `read()` only unblocks on peer
        // close. The join must complete well within the read timeout budget.
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
        let sock = TcpStream::connect(addr).expect("connect loopback");
        sock.set_read_timeout(Some(std::time::Duration::from_millis(200)))
            .expect("set read timeout");
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
}
