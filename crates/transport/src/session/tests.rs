use super::*;
use rsync_protocol::{NegotiationPrologueSniffer, ProtocolVersion};
use std::io::{self, Cursor, Read, Write};

#[derive(Debug)]
struct MemoryTransport {
    reader: Cursor<Vec<u8>>,
    writes: Vec<u8>,
    flushes: usize,
}

impl MemoryTransport {
    fn new(input: &[u8]) -> Self {
        Self {
            reader: Cursor::new(input.to_vec()),
            writes: Vec::new(),
            flushes: 0,
        }
    }

    fn writes(&self) -> &[u8] {
        &self.writes
    }

    fn flushes(&self) -> usize {
        self.flushes
    }
}

impl Read for MemoryTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Write for MemoryTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

fn binary_handshake_bytes(version: ProtocolVersion) -> [u8; 4] {
    u32::from(version.as_u8()).to_le_bytes()
}

#[test]
fn negotiate_session_detects_binary_transport() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake succeeds");

    assert_eq!(handshake.decision(), NegotiationPrologue::Binary);
    assert_eq!(handshake.negotiated_protocol(), remote_version);
    assert_eq!(handshake.remote_protocol(), remote_version);
    assert_eq!(
        handshake.remote_advertised_protocol(),
        u32::from(remote_version.as_u8())
    );
    assert!(!handshake.remote_protocol_was_clamped());

    let transport = match handshake.into_binary() {
        Ok(handshake) => {
            assert_eq!(handshake.remote_protocol(), remote_version);
            handshake.into_stream().into_inner()
        }
        Err(_) => panic!("binary handshake expected"),
    };

    assert_eq!(
        transport.writes(),
        &binary_handshake_bytes(ProtocolVersion::NEWEST)
    );
    assert_eq!(transport.flushes(), 1);
}

#[test]
fn negotiate_session_detects_legacy_transport() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("legacy handshake succeeds");

    assert_eq!(handshake.decision(), NegotiationPrologue::LegacyAscii);
    assert_eq!(
        handshake.negotiated_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );
    assert_eq!(
        handshake.remote_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );

    let transport = match handshake.into_legacy() {
        Ok(handshake) => {
            assert_eq!(handshake.server_greeting().advertised_protocol(), 31);
            handshake.into_stream().into_inner()
        }
        Err(_) => panic!("legacy handshake expected"),
    };

    assert_eq!(transport.writes(), b"@RSYNCD: 31.0\n");
    assert_eq!(transport.flushes(), 1);
}

#[test]
fn negotiate_session_with_sniffer_supports_reuse() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport1 = MemoryTransport::new(&binary_handshake_bytes(remote_version));
    let transport2 = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let mut sniffer = NegotiationPrologueSniffer::new();

    let handshake1 =
        negotiate_session_with_sniffer(transport1, ProtocolVersion::NEWEST, &mut sniffer)
            .expect("binary handshake succeeds");
    assert!(matches!(handshake1.decision(), NegotiationPrologue::Binary));
    assert_eq!(handshake1.remote_protocol(), remote_version);

    let transport1 = handshake1
        .into_binary()
        .expect("first handshake is binary")
        .into_stream()
        .into_inner();
    assert_eq!(
        transport1.writes(),
        &binary_handshake_bytes(ProtocolVersion::NEWEST)
    );

    let handshake2 =
        negotiate_session_with_sniffer(transport2, ProtocolVersion::NEWEST, &mut sniffer)
            .expect("legacy handshake succeeds");
    assert!(matches!(
        handshake2.decision(),
        NegotiationPrologue::LegacyAscii
    ));
    assert_eq!(
        handshake2.remote_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );

    let transport2 = handshake2
        .into_legacy()
        .expect("second handshake is legacy")
        .into_stream()
        .into_inner();
    assert_eq!(transport2.writes(), b"@RSYNCD: 31.0\n");
    assert_eq!(transport2.flushes(), 1);
}

#[test]
fn map_stream_inner_preserves_variant_and_metadata() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake succeeds");

    let mut handshake = handshake.map_stream_inner(InstrumentedTransport::new);
    assert_eq!(handshake.decision(), NegotiationPrologue::Binary);
    assert!(handshake.as_binary().is_some());

    handshake
        .stream_mut()
        .write_all(b"payload")
        .expect("write succeeds");

    assert_eq!(
        handshake.remote_advertised_protocol(),
        u32::from(remote_version.as_u8())
    );
    assert!(!handshake.remote_protocol_was_clamped());

    let transport = handshake
        .into_binary()
        .expect("variant remains binary")
        .into_stream()
        .into_inner();

    let mut expected = binary_handshake_bytes(ProtocolVersion::NEWEST).to_vec();
    expected.extend_from_slice(b"payload");
    assert_eq!(transport.writes(), expected.as_slice());
    assert_eq!(transport.flushes(), 1);
}

#[test]
fn try_map_stream_inner_preserves_variants() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake succeeds");

    let mut handshake = handshake
        .try_map_stream_inner(
            |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                Ok(InstrumentedTransport::new(inner))
            },
        )
        .expect("mapping succeeds");

    assert!(matches!(handshake.decision(), NegotiationPrologue::Binary));
    handshake
        .stream_mut()
        .write_all(b"payload")
        .expect("write succeeds");
    let transport = handshake
        .into_binary()
        .expect("variant remains binary")
        .into_stream()
        .into_inner();
    assert_eq!(transport.flushes(), 1);
}

#[test]
fn try_map_stream_inner_preserves_original_handshake_on_error() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake succeeds");

    let err = handshake
        .try_map_stream_inner(
            |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                Err((io::Error::new(io::ErrorKind::Other, "boom"), inner))
            },
        )
        .expect_err("mapping fails");

    assert_eq!(err.error().kind(), io::ErrorKind::Other);
    let original = err.into_original();
    let transport = original
        .into_binary()
        .expect("handshake remains binary")
        .into_stream()
        .into_inner();
    assert_eq!(
        transport.writes(),
        &binary_handshake_bytes(ProtocolVersion::NEWEST)
    );
}

#[test]
fn session_reports_clamped_binary_future_version() {
    let future_version = 40u32;
    let transport = MemoryTransport::new(&future_version.to_le_bytes());

    let handshake = negotiate_session(transport, ProtocolVersion::NEWEST)
        .expect("binary handshake clamps future versions");

    assert_eq!(handshake.decision(), NegotiationPrologue::Binary);
    assert_eq!(handshake.remote_protocol(), ProtocolVersion::NEWEST);
    assert_eq!(handshake.remote_advertised_protocol(), future_version);
    assert!(handshake.remote_protocol_was_clamped());

    let parts = handshake.into_stream_parts();
    assert_eq!(parts.decision(), NegotiationPrologue::Binary);
    assert_eq!(parts.remote_protocol(), ProtocolVersion::NEWEST);
    assert_eq!(parts.remote_advertised_protocol(), future_version);
    assert!(parts.remote_protocol_was_clamped());
}

#[test]
fn session_handshake_parts_round_trip_binary_handshake() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake succeeds");

    let parts = handshake.into_stream_parts();
    assert_eq!(parts.decision(), NegotiationPrologue::Binary);
    assert_eq!(parts.negotiated_protocol(), remote_version);
    assert_eq!(parts.remote_protocol(), remote_version);
    assert_eq!(
        parts.remote_advertised_protocol(),
        u32::from(remote_version.as_u8())
    );
    assert!(!parts.remote_protocol_was_clamped());
    assert!(parts.server_greeting().is_none());
    assert_eq!(parts.stream().decision(), NegotiationPrologue::Binary);

    let (remote_advertised_protocol, remote_protocol, negotiated_protocol, stream_parts) =
        parts.into_binary().expect("binary parts available");

    let parts = SessionHandshakeParts::Binary {
        remote_advertised_protocol,
        remote_protocol,
        negotiated_protocol,
        stream: stream_parts.map_inner(InstrumentedTransport::new),
    };

    let handshake = SessionHandshake::from_stream_parts(parts);
    let mut binary = handshake
        .into_binary()
        .expect("parts reconstruct binary handshake");

    assert_eq!(
        binary.remote_advertised_protocol(),
        u32::from(remote_version.as_u8())
    );
    assert!(!binary.remote_protocol_was_clamped());

    binary
        .stream_mut()
        .write_all(b"payload")
        .expect("write succeeds");

    let transport = binary.into_stream().into_inner();
    let mut expected = binary_handshake_bytes(ProtocolVersion::NEWEST).to_vec();
    expected.extend_from_slice(b"payload");
    assert_eq!(transport.writes(), expected.as_slice());
    assert_eq!(transport.flushes(), 1);
}

#[test]
fn session_handshake_parts_try_map_transforms_transport() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake succeeds");

    let parts = handshake.into_stream_parts();
    let parts = parts.into_binary().expect("binary parts available");

    let parts = SessionHandshakeParts::Binary {
        remote_advertised_protocol: parts.0,
        remote_protocol: parts.1,
        negotiated_protocol: parts.2,
        stream: parts.3,
    };

    let parts = parts
        .try_map_stream_inner(
            |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                Ok(InstrumentedTransport::new(inner))
            },
        )
        .expect("mapping succeeds");

    let handshake = SessionHandshake::from_stream_parts(parts);
    let mut binary = handshake.into_binary().expect("variant remains binary");
    binary
        .stream_mut()
        .write_all(b"payload")
        .expect("write succeeds");
    let transport = binary.into_stream().into_inner();
    assert_eq!(transport.flushes(), 1);
}

#[test]
fn session_handshake_parts_try_map_preserves_original_on_error() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake succeeds");

    let parts = handshake.into_stream_parts();
    let parts = parts.into_binary().expect("binary parts available");

    let parts = SessionHandshakeParts::Binary {
        remote_advertised_protocol: parts.0,
        remote_protocol: parts.1,
        negotiated_protocol: parts.2,
        stream: parts.3,
    };

    let err = parts
        .try_map_stream_inner(
            |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                Err((io::Error::new(io::ErrorKind::Other, "boom"), inner))
            },
        )
        .expect_err("mapping fails");

    assert_eq!(err.error().kind(), io::ErrorKind::Other);
    let original = err.into_original();
    let transport = original.into_stream().into_inner();
    assert_eq!(
        transport.writes(),
        &binary_handshake_bytes(ProtocolVersion::NEWEST)
    );
}

#[test]
fn session_handshake_parts_round_trip_legacy_handshake() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("legacy handshake succeeds");

    let parts = handshake.into_stream_parts();
    assert_eq!(parts.decision(), NegotiationPrologue::LegacyAscii);
    let negotiated = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    assert_eq!(parts.negotiated_protocol(), negotiated);
    assert_eq!(parts.remote_protocol(), negotiated);
    assert_eq!(parts.remote_advertised_protocol(), 31);
    assert!(!parts.remote_protocol_was_clamped());
    let server = parts.server_greeting().expect("server greeting retained");
    assert_eq!(server.advertised_protocol(), 31);
    assert_eq!(parts.stream().decision(), NegotiationPrologue::LegacyAscii);

    let (server_greeting, negotiated_protocol, stream_parts) =
        parts.into_legacy().expect("legacy parts available");

    let parts = SessionHandshakeParts::Legacy {
        server_greeting,
        negotiated_protocol,
        stream: stream_parts.map_inner(InstrumentedTransport::new),
    };

    let handshake = SessionHandshake::from_stream_parts(parts);
    let mut legacy = handshake
        .into_legacy()
        .expect("parts reconstruct legacy handshake");

    legacy
        .stream_mut()
        .write_all(b"module\n")
        .expect("write succeeds");

    let transport = legacy.into_stream().into_inner();
    let mut expected = b"@RSYNCD: 31.0\n".to_vec();
    expected.extend_from_slice(b"module\n");
    assert_eq!(transport.writes(), expected.as_slice());
    assert_eq!(transport.flushes(), 1);
}

#[test]
fn session_reports_clamped_future_legacy_version() {
    let transport = MemoryTransport::new(b"@RSYNCD: 40.0\n");

    let handshake = negotiate_session(transport, ProtocolVersion::NEWEST)
        .expect("legacy handshake clamps future advertisement");

    assert_eq!(handshake.decision(), NegotiationPrologue::LegacyAscii);
    assert_eq!(handshake.remote_protocol(), ProtocolVersion::NEWEST);
    assert_eq!(handshake.remote_advertised_protocol(), 40);
    assert!(handshake.remote_protocol_was_clamped());

    let parts = handshake.into_stream_parts();
    assert_eq!(parts.decision(), NegotiationPrologue::LegacyAscii);
    assert_eq!(parts.remote_protocol(), ProtocolVersion::NEWEST);
    assert_eq!(parts.remote_advertised_protocol(), 40);
    assert!(parts.remote_protocol_was_clamped());
}

#[test]
fn session_handshake_parts_preserve_remote_protocol_for_legacy_caps() {
    let desired = ProtocolVersion::from_supported(30).expect("protocol 30 supported");
    let transport = MemoryTransport::new(b"@RSYNCD: 32.0\n");

    let handshake = negotiate_session(transport, desired)
        .expect("legacy handshake succeeds with future advertisement");

    let parts = handshake.into_stream_parts();
    let remote = ProtocolVersion::from_supported(32).expect("protocol 32 supported");
    assert_eq!(parts.negotiated_protocol(), desired);
    assert_eq!(parts.remote_protocol(), remote);
    assert_eq!(parts.remote_advertised_protocol(), 32);
    assert!(!parts.remote_protocol_was_clamped());
    let server = parts.server_greeting().expect("server greeting retained");
    assert_eq!(server.protocol(), remote);
    assert_eq!(server.advertised_protocol(), 32);
}

#[derive(Debug)]
struct InstrumentedTransport {
    inner: MemoryTransport,
}

impl InstrumentedTransport {
    fn new(inner: MemoryTransport) -> Self {
        Self { inner }
    }

    fn writes(&self) -> &[u8] {
        self.inner.writes()
    }

    fn flushes(&self) -> usize {
        self.inner.flushes()
    }
}

impl Read for InstrumentedTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for InstrumentedTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
