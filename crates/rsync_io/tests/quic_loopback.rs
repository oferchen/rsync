//! Loopback round-trip through the feature-gated QUIC transport spike.

#![cfg(feature = "quic")]

use std::io::{Read, Write};
use std::thread;

use rsync_io::quic::{QuicAcceptor, QuicConnector};

const PAYLOAD_LEN: usize = 4096;

fn pattern(seed: u8) -> Vec<u8> {
    (0..PAYLOAD_LEN)
        .map(|i| seed.wrapping_add((i % 251) as u8))
        .collect()
}

/// Both directions of one bidirectional stream carry a few KiB byte-exactly.
///
/// Synchronization is structural, not timed: the client blocks in `connect`
/// until the server's `accept` admits it, each side's `read_exact` blocks
/// until the peer's bytes arrive, and `finish` blocks until the peer has
/// acknowledged every written byte.
#[test]
fn quic_loopback_round_trip() {
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
    // Terminal operation: the client stops polling after this, so it must
    // close the connection instead of leaving the server to its idle timeout.
    stream.close();

    server.join().expect("server thread");
}
