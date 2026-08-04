//! Loopback correctness gates for the feature-gated QUIC transport.

#![cfg(feature = "quic")]

use std::io::{Read, Write};
use std::thread;
use std::time::{Duration, Instant};

use rsync_io::quic::{QuicAcceptor, QuicConnector, QuicTrust, RootCertStore};

const PAYLOAD_LEN: usize = 4096;
/// Prompt-teardown bound: far below any idle timeout, so a pass proves the
/// final ACK and connection close were driven actively, not by expiry.
const TEARDOWN_BOUND: Duration = Duration::from_secs(1);

fn pattern(seed: u8) -> Vec<u8> {
    (0..PAYLOAD_LEN)
        .map(|i| seed.wrapping_add((i % 251) as u8))
        .collect()
}

/// Both directions of one bidirectional stream carry a few KiB byte-exactly,
/// and the stream ends in clean EOF.
///
/// Synchronization is structural, not timed: the client blocks in `connect`
/// until the server's `accept` admits it, each side's `read_exact` blocks
/// until the peer's bytes arrive, and `finish` blocks until the peer has
/// acknowledged every written byte.
#[test]
fn round_trip_and_eof() {
    let acceptor =
        QuicAcceptor::bind("127.0.0.1:0".parse().expect("loopback addr")).expect("bind acceptor");
    let addr = acceptor.local_addr().expect("local addr");
    let cert = acceptor.certificate().clone().into_owned();

    let client_to_server = pattern(0x11);
    let server_to_client = pattern(0xa7);

    let expected_request = client_to_server.clone();
    let reply = server_to_client.clone();
    let server = thread::spawn(move || {
        let mut stream = acceptor.accept().expect("accept stream");
        let mut request = vec![0u8; PAYLOAD_LEN];
        stream.read_exact(&mut request).expect("read request");
        assert_eq!(request, expected_request, "client payload corrupted");
        stream.write_all(&reply).expect("write reply");
        stream.finish().expect("finish reply");
    });

    let connector = QuicConnector::new(&cert).expect("build connector");
    let mut stream = connector.connect(addr, "localhost").expect("connect");
    stream.write_all(&client_to_server).expect("write request");
    stream.finish().expect("finish request");

    let mut received = vec![0u8; PAYLOAD_LEN];
    stream.read_exact(&mut received).expect("read reply");
    assert_eq!(received, server_to_client, "server payload corrupted");
    // After the server finishes its send side, the next read is clean EOF.
    let mut eof = [0u8; 1];
    assert_eq!(stream.read(&mut eof).expect("read eof"), 0, "expected EOF");
    stream.close();

    server.join().expect("server thread");
}

/// The same round trip as `round_trip_and_eof`, but the connector is built
/// from a [`QuicTrust::Roots`] store instead of an exact pin. Both tests drive
/// the identical `connect`/`QuicStream` path; passing both proves the trust
/// source is pluggable - a `RootCertStore` (the shape `--quic-ca` and the
/// system-roots default deliver) reaches the server without any change to the
/// connect path, so QUIC-5b/5c/5d can supply CA/system/TOFU trust without
/// touching the transport.
#[test]
fn round_trip_via_roots_trust() {
    let acceptor =
        QuicAcceptor::bind("127.0.0.1:0".parse().expect("loopback addr")).expect("bind acceptor");
    let addr = acceptor.local_addr().expect("local addr");

    let mut roots = RootCertStore::empty();
    roots
        .add(acceptor.certificate().clone().into_owned())
        .expect("add server cert to roots");

    let client_to_server = pattern(0x11);
    let server_to_client = pattern(0xa7);

    let expected_request = client_to_server.clone();
    let reply = server_to_client.clone();
    let server = thread::spawn(move || {
        let mut stream = acceptor.accept().expect("accept stream");
        let mut request = vec![0u8; PAYLOAD_LEN];
        stream.read_exact(&mut request).expect("read request");
        assert_eq!(request, expected_request, "client payload corrupted");
        stream.write_all(&reply).expect("write reply");
        stream.finish().expect("finish reply");
    });

    let connector = QuicConnector::with_trust(QuicTrust::Roots(roots)).expect("build connector");
    let mut stream = connector.connect(addr, "localhost").expect("connect");
    stream.write_all(&client_to_server).expect("write request");
    stream.finish().expect("finish request");

    let mut received = vec![0u8; PAYLOAD_LEN];
    stream.read_exact(&mut received).expect("read reply");
    assert_eq!(received, server_to_client, "server payload corrupted");
    let mut eof = [0u8; 1];
    assert_eq!(stream.read(&mut eof).expect("read eof"), 0, "expected EOF");
    stream.close();

    server.join().expect("server thread");
}

/// Split handles plus a teardown guard: a read handle (`try_clone`) and a
/// write handle over one stream carry a full request/reply round trip, and the
/// [`QuicShutdown`](rsync_io::quic) guard flushes the last write (finish + FIN)
/// and closes the connection on drop. This is the shape the daemon `@RSYNCD`
/// handshake needs - an independent blocking `Read` half and `Write` half whose
/// simple drops never truncate the tail because the guard owns the flush.
#[test]
fn split_handles_round_trip_and_guard_flushes_on_drop() {
    let acceptor =
        QuicAcceptor::bind("127.0.0.1:0".parse().expect("loopback addr")).expect("bind acceptor");
    let addr = acceptor.local_addr().expect("local addr");
    let cert = acceptor.certificate().clone().into_owned();

    let request = pattern(0x42);
    let reply = pattern(0x24);

    let expected_request = request.clone();
    let server_reply = reply.clone();
    let server = thread::spawn(move || {
        let mut stream = acceptor.accept().expect("accept stream");
        let mut got = vec![0u8; PAYLOAD_LEN];
        stream.read_exact(&mut got).expect("read request");
        assert_eq!(got, expected_request, "request corrupted");
        stream.write_all(&server_reply).expect("write reply");
        stream.finish().expect("finish reply");
        // After the client's write handle is dropped and the guard drops, the
        // client's FIN must arrive - proving the guard drives the flush + FIN
        // even though the write handle was never explicitly finished.
        let mut trailing = Vec::new();
        stream
            .read_to_end(&mut trailing)
            .expect("read to client FIN");
        assert!(trailing.is_empty(), "client sent no trailing bytes");
    });

    let connector = QuicConnector::new(&cert).expect("build connector");
    let stream = connector.connect(addr, "localhost").expect("connect");

    let guard = stream.shutdown_guard();
    let mut read_half = stream.try_clone();
    let mut write_half = stream;

    write_half.write_all(&request).expect("write request");
    // Drop the write handle WITHOUT finishing: the guard owns the flush + FIN.
    drop(write_half);

    let mut received = vec![0u8; PAYLOAD_LEN];
    read_half.read_exact(&mut received).expect("read reply");
    assert_eq!(received, reply, "reply corrupted");

    drop(read_half);
    // Dropping the guard finishes the send stream (delivering the FIN the
    // server's read_to_end is waiting for) and closes the connection.
    drop(guard);

    server.join().expect("server thread");
}

/// Half-close semantics: after the client sends FIN, its read side keeps
/// working - the server reads to EOF, then streams a multi-chunk reply the
/// client receives intact.
#[test]
fn half_close_read_continues() {
    let acceptor =
        QuicAcceptor::bind("127.0.0.1:0".parse().expect("loopback addr")).expect("bind acceptor");
    let addr = acceptor.local_addr().expect("local addr");
    let cert = acceptor.certificate().clone().into_owned();

    let request = pattern(0x3c);
    let reply_chunks: Vec<Vec<u8>> = (0..8u8).map(pattern).collect();

    let expected_request = request.clone();
    let chunks = reply_chunks.clone();
    let server = thread::spawn(move || {
        let mut stream = acceptor.accept().expect("accept stream");
        let mut buf = Vec::new();
        // Reads past the payload until the client's FIN: EOF on the receive
        // half while the send half stays usable.
        stream.read_to_end(&mut buf).expect("read to FIN");
        assert_eq!(buf, expected_request, "request corrupted");
        for chunk in &chunks {
            stream.write_all(chunk).expect("write reply chunk");
        }
        stream.finish().expect("finish reply");
    });

    let connector = QuicConnector::new(&cert).expect("build connector");
    let mut stream = connector.connect(addr, "localhost").expect("connect");
    stream.write_all(&request).expect("write request");
    // Write FIN; the read half must remain open.
    stream.finish().expect("finish request");

    let mut received = Vec::new();
    stream.read_to_end(&mut received).expect("read reply");
    let expected: Vec<u8> = reply_chunks.concat();
    assert_eq!(received, expected, "reply corrupted after half-close");
    stream.close();

    server.join().expect("server thread");
}

/// Prompt teardown: from the last byte written until both sides have
/// observed the end of the session and closed takes well under a second -
/// no idle-timeout reliance.
#[test]
fn teardown_is_prompt() {
    let acceptor =
        QuicAcceptor::bind("127.0.0.1:0".parse().expect("loopback addr")).expect("bind acceptor");
    let addr = acceptor.local_addr().expect("local addr");
    let cert = acceptor.certificate().clone().into_owned();

    let payload = pattern(0x55);
    let expected = payload.clone();
    let server = thread::spawn(move || {
        let mut stream = acceptor.accept().expect("accept stream");
        let mut request = vec![0u8; PAYLOAD_LEN];
        stream.read_exact(&mut request).expect("read request");
        assert_eq!(request, expected, "payload corrupted");
        let mut eof = [0u8; 1];
        assert_eq!(stream.read(&mut eof).expect("read eof"), 0, "expected EOF");
        stream.close();
    });

    let connector = QuicConnector::new(&cert).expect("build connector");
    let mut stream = connector.connect(addr, "localhost").expect("connect");
    stream.write_all(&payload).expect("write payload");
    let last_byte_written = Instant::now();
    // The delivery barrier for the final write: FIN sent and every byte
    // acknowledged.
    stream.finish().expect("finish");
    stream.close();
    server.join().expect("server thread");
    let elapsed = last_byte_written.elapsed();
    assert!(
        elapsed < TEARDOWN_BOUND,
        "teardown took {elapsed:?}, expected < {TEARDOWN_BOUND:?} (idle-timeout reliance?)"
    );
}
