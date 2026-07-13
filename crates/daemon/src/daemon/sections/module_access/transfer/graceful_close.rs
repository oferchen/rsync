// Graceful lingering-close drain for the daemon connection teardown.
//
// After the transfer engine has completed the full goodbye handshake (both
// directions of the NDX_DONE exchange) and every user-space byte has been
// flushed, the connection thread must close the socket. On Unix a `close()`
// that still has unread bytes queued in the kernel receive buffer is an
// *abortive* close: the kernel discards the data and sends a TCP RST instead of
// a clean FIN. The peer then surfaces that RST as
// "Connection reset by peer (os error 104)" on its next socket read, which the
// client maps to a partial-transfer failure (exit 23) even though the transfer
// itself completed correctly.
//
// Upstream rsync never hits this because the daemon-receiver child keeps reading
// the socket (`io.c:943 noop_io_until_death()` loops on `read_buf()` until the
// peer dies) right up to process exit, so the kernel receive buffer is empty by
// the time `cleanup.c:265 close_all()` runs. The threaded daemon collapses that
// pattern into an explicit drain-to-EOF here: read the socket until the peer
// sends FIN (`Ok(0)`), so the final `close()` finds an empty receive buffer and
// emits a clean FIN rather than a RST.
//
// The drain is bounded by a read timeout so a wedged or silent peer can never
// pin the connection thread: an idle socket returns `TimedOut`/`WouldBlock` and
// the loop exits. This mirrors upstream's `set_io_timeout(60)` guard around
// `noop_io_until_death()`.
//
// This file is `include!`d into the `crate::daemon` scope (see
// `module_access.rs`), so it reuses the enclosing module's imports (`io`,
// `Read`, `Duration`, `TcpStream`).

/// Read timeout that bounds each blocking drain `read()` during the teardown
/// drain-to-EOF loop.
///
/// A silent peer returns `TimedOut` after this window so the loop exits instead
/// of parking the connection thread. Five seconds is generous for any
/// reasonable goodbye round-trip while keeping a wedged peer from pinning the
/// thread; it also matches the companion `SO_LINGER` window applied to the
/// kernel send buffer.
const GOODBYE_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// A readable connection whose blocking `read` can be bounded by a read timeout
/// so a drain-to-EOF loop can never hang on a wedged peer.
///
/// Implemented by [`DaemonStream`] (production) and `TcpStream` (tests). The
/// timeout setter mirrors `set_read_timeout`; `None` clears it.
trait PeerDrainStream: Read {
    /// Sets (or clears, with `None`) the read timeout on the underlying socket.
    fn set_drain_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
}

impl PeerDrainStream for DaemonStream {
    fn set_drain_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.set_read_timeout(timeout)
    }
}

#[cfg(test)]
impl PeerDrainStream for TcpStream {
    fn set_drain_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.set_read_timeout(timeout)
    }
}

/// Drains the connection's read side until the peer sends FIN (a clean `Ok(0)`
/// EOF), bounded by `timeout`, so the socket's receive buffer is empty when the
/// connection is finally closed.
///
/// This is the invariant that keeps the daemon from ever performing an abortive
/// `close()`: with the receive buffer drained to EOF, the final close emits a
/// clean FIN, so the peer reads a clean EOF rather than a RST. A `close()` with
/// unread bytes still queued would make the kernel send a RST that the peer
/// reports as "Connection reset by peer".
///
/// The loop tolerates every terminal condition equally - EOF, an elapsed read
/// timeout, or a peer-close error (`ConnectionReset`/`BrokenPipe`) - because all
/// three mean there is nothing left to drain. Errors are swallowed: this runs on
/// the teardown path after the transfer result has already been decided, so a
/// drain hiccup must never change the transfer's outcome.
///
/// upstream: `io.c:943-963 noop_io_until_death()` keeps reading until the peer
/// dies; `set_io_timeout(60)` bounds it so a wedged peer cannot hang forever.
fn drain_until_peer_eof<S: PeerDrainStream + ?Sized>(stream: &mut S, timeout: Duration) {
    let _ = stream.set_drain_timeout(Some(timeout));
    let mut sink = [0u8; 4096];
    loop {
        match stream.read(&mut sink) {
            // Peer FINed: the receive buffer is empty, the close will be clean.
            Ok(0) => break,
            // More trailing bytes (peer goodbye / codec trailer); keep draining.
            Ok(_) => continue,
            // Idle-socket timeout, interrupted syscall, or any peer-close error:
            // nothing left to drain, stop so the close can proceed.
            Err(_) => break,
        }
    }
    let _ = stream.set_drain_timeout(None);
}

#[cfg(test)]
mod graceful_close_tests {
    //! Drain-to-EOF invariant tests for [`drain_until_peer_eof`].
    //!
    //! The daemon must fully drain the peer's trailing bytes and wait for the
    //! peer's FIN before closing, so the kernel receive buffer is empty and the
    //! final `close()` emits a clean FIN instead of an abortive RST. These tests
    //! pin that invariant without depending on timing luck.

    use super::drain_until_peer_eof;
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::channel;
    use std::thread;
    use std::time::{Duration, Instant};

    /// A peer that writes a trailing goodbye burst then closes must be drained
    /// to EOF: every trailing byte is consumed and the loop returns only once
    /// the FIN arrives. This is the exact shape of the teardown race - trailing
    /// goodbye/codec bytes still in flight when the daemon starts to close.
    #[test]
    fn drains_all_trailing_bytes_then_returns_on_fin() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let peer = thread::spawn(move || {
            let (mut server, _) = listener.accept().expect("accept");
            // Trailing bytes larger than one drain chunk, then FIN via drop.
            server.write_all(&vec![0x5Au8; 20_000]).expect("write trailing");
            server.flush().expect("flush trailing");
            // Drop -> FIN -> EOF on the drain side.
        });

        let mut client = TcpStream::connect(addr).expect("connect loopback");
        drain_until_peer_eof(&mut client, Duration::from_secs(5));
        // Reaching here means the loop observed EOF (Ok(0)); if it had returned
        // early on the first non-empty read, the peer's FIN would be unobserved
        // and a real close would risk an abortive RST.
        peer.join().expect("peer join");
    }

    /// A silent peer that never sends and never FINs must not pin the thread:
    /// the bounded read timeout makes the drain return promptly. This is the
    /// anti-hang guard - the drain can never block indefinitely on a wedged
    /// peer (upstream's `set_io_timeout` around `noop_io_until_death`).
    #[test]
    fn returns_promptly_when_peer_is_silent() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let (drop_tx, drop_rx) = channel::<()>();
        let peer = thread::spawn(move || {
            let (server, _) = listener.accept().expect("accept");
            // Hold the connection open, silent, until the test says to drop it.
            let _ = drop_rx.recv();
            drop(server);
        });

        let mut client = TcpStream::connect(addr).expect("connect loopback");
        let start = Instant::now();
        drain_until_peer_eof(&mut client, Duration::from_millis(200));
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "drain must return within the read-timeout window on a silent peer"
        );
        drop_tx.send(()).expect("signal peer");
        peer.join().expect("peer join");
    }

    /// A peer that FINs immediately with no data drains instantly: the first
    /// read is EOF and the loop returns without blocking. This is the common
    /// case (peer already closed by the time the daemon reaches teardown).
    #[test]
    fn returns_immediately_on_prompt_eof() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let peer = thread::spawn(move || {
            let (server, _) = listener.accept().expect("accept");
            drop(server); // immediate FIN
        });

        let mut client = TcpStream::connect(addr).expect("connect loopback");
        let start = Instant::now();
        drain_until_peer_eof(&mut client, Duration::from_secs(5));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "a prompt peer FIN must drain immediately, not wait out the timeout"
        );
        peer.join().expect("peer join");
    }
}
