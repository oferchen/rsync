use super::*;
use crate::RemoteProtocolAdvertisement;
use crate::binary::{BinaryHandshake, BinaryHandshakeParts, negotiate_binary_session};
use crate::daemon::{
    LegacyDaemonHandshake, LegacyDaemonHandshakeParts, negotiate_legacy_daemon_session,
};
use crate::negotiation::{NEGOTIATION_PROLOGUE_UNDETERMINED_MSG, NegotiatedStream};
use crate::sniff_negotiation_stream;
use rsync_protocol::{
    NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion, format_legacy_daemon_greeting,
};
use std::convert::TryFrom;
use std::io::{self, Cursor, Read, Write};

#[derive(Clone, Debug)]
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
    assert!(handshake.is_binary());
    assert!(!handshake.is_legacy());
    assert_eq!(handshake.negotiated_protocol(), remote_version);
    assert_eq!(handshake.remote_protocol(), remote_version);
    assert_eq!(
        handshake.remote_advertised_protocol(),
        u32::from(remote_version.as_u8())
    );
    assert_eq!(
        handshake.local_advertised_protocol(),
        ProtocolVersion::NEWEST
    );
    assert_eq!(
        handshake.remote_advertisement(),
        RemoteProtocolAdvertisement::Supported(remote_version)
    );
    assert!(!handshake.remote_protocol_was_clamped());
    assert!(!handshake.local_protocol_was_capped());

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
    assert!(handshake.is_legacy());
    assert!(!handshake.is_binary());
    assert_eq!(
        handshake.negotiated_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );
    assert_eq!(
        handshake.remote_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );
    assert_eq!(
        handshake
            .server_greeting()
            .expect("legacy handshake exposes greeting")
            .advertised_protocol(),
        31
    );
    assert_eq!(
        handshake.remote_advertisement(),
        RemoteProtocolAdvertisement::Supported(
            ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
        )
    );
    assert_eq!(
        handshake.local_advertised_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported")
    );
    assert!(!handshake.local_protocol_was_capped());

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
fn session_handshake_rehydrates_sniffer_for_binary() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let mut bytes = binary_handshake_bytes(remote_version).to_vec();
    bytes.extend_from_slice(b"payload");
    let transport = MemoryTransport::new(&bytes);

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake succeeds");

    let mut sniffer = NegotiationPrologueSniffer::new();
    handshake
        .rehydrate_sniffer(&mut sniffer)
        .expect("rehydration succeeds");

    assert!(sniffer.is_binary());
    assert_eq!(sniffer.buffered(), handshake.stream().buffered());
    assert_eq!(
        sniffer.sniffed_prefix_len(),
        handshake.stream().sniffed_prefix_len()
    );
}

#[test]
fn session_handshake_rehydrates_sniffer_for_legacy() {
    let mut bytes = b"@RSYNCD: 31.0\n".to_vec();
    bytes.extend_from_slice(b"@RSYNCD: OK\n");
    let transport = MemoryTransport::new(&bytes);

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("legacy handshake succeeds");

    let mut sniffer = NegotiationPrologueSniffer::new();
    handshake
        .rehydrate_sniffer(&mut sniffer)
        .expect("rehydration succeeds");

    assert!(sniffer.is_legacy());
    assert_eq!(sniffer.buffered(), handshake.stream().buffered());
    assert_eq!(
        sniffer.sniffed_prefix_len(),
        handshake.stream().sniffed_prefix_len()
    );
}

#[test]
fn negotiate_session_parts_exposes_binary_metadata() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let parts =
        negotiate_session_parts(transport, ProtocolVersion::NEWEST).expect("binary parts succeed");

    assert_eq!(parts.decision(), NegotiationPrologue::Binary);
    assert!(parts.is_binary());
    assert!(!parts.is_legacy());
    assert_eq!(parts.remote_protocol(), remote_version);
    assert_eq!(parts.negotiated_protocol(), remote_version);
    assert!(!parts.remote_protocol_was_clamped());
    assert!(!parts.local_protocol_was_capped());

    let tuple_parts = parts
        .clone()
        .into_binary()
        .expect("binary tuple parts available");
    let (
        remote_advertised,
        remote_protocol,
        local_advertised,
        negotiated_protocol,
        _stream_parts_tuple,
    ) = tuple_parts;
    assert_eq!(remote_advertised, u32::from(remote_version.as_u8()));
    assert_eq!(remote_protocol, remote_version);
    assert_eq!(local_advertised, ProtocolVersion::NEWEST);
    assert_eq!(negotiated_protocol, remote_version);

    let binary_parts = BinaryHandshakeParts::try_from(parts).expect("binary parts conversion");
    assert_eq!(binary_parts.remote_protocol(), remote_version);
    assert_eq!(binary_parts.negotiated_protocol(), remote_version);
    assert_eq!(
        binary_parts.local_advertised_protocol(),
        ProtocolVersion::NEWEST
    );

    let stream_parts = binary_parts.into_stream_parts();
    let transport = stream_parts.into_stream().into_inner();
    assert_eq!(
        transport.writes(),
        &binary_handshake_bytes(ProtocolVersion::NEWEST)
    );
    assert_eq!(transport.flushes(), 1);
}

#[test]
fn session_handshake_parts_into_stream_parts_preserves_buffer_state() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let parts = negotiate_session(
        MemoryTransport::new(&binary_handshake_bytes(remote_version)),
        ProtocolVersion::NEWEST,
    )
    .expect("binary handshake succeeds")
    .into_stream_parts();

    let binary_stream_parts = parts.clone().into_stream_parts();
    assert_eq!(binary_stream_parts.buffered(), parts.stream().buffered());
    assert_eq!(binary_stream_parts.decision(), parts.decision());

    let parts = negotiate_session(
        MemoryTransport::new(b"@RSYNCD: 31.0\n@RSYNCD: OK\n"),
        ProtocolVersion::NEWEST,
    )
    .expect("legacy handshake succeeds")
    .into_stream_parts();

    let legacy_stream_parts = parts.clone().into_stream_parts();
    assert_eq!(legacy_stream_parts.buffered(), parts.stream().buffered());
    assert_eq!(legacy_stream_parts.decision(), parts.decision());
}

#[test]
fn session_into_inner_returns_binary_transport() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake succeeds");

    let mut raw = handshake.into_inner();
    assert_eq!(
        raw.writes(),
        &binary_handshake_bytes(ProtocolVersion::NEWEST)
    );
    assert_eq!(raw.flushes(), 1);

    let mut replay = Vec::new();
    raw.read_to_end(&mut replay)
        .expect("remaining bytes readable");
    assert!(replay.is_empty());
}

#[test]
fn session_into_inner_returns_legacy_transport() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("legacy handshake succeeds");

    let mut raw = handshake.into_inner();
    assert_eq!(raw.writes(), b"@RSYNCD: 31.0\n");
    assert_eq!(raw.flushes(), 1);

    let mut replay = Vec::new();
    raw.read_to_end(&mut replay)
        .expect("remaining bytes readable");
    assert!(replay.is_empty());
}

#[test]
fn session_parts_into_inner_returns_binary_transport() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let parts =
        negotiate_session_parts(transport, ProtocolVersion::NEWEST).expect("binary parts succeed");

    let mut raw = parts.into_inner();
    assert_eq!(
        raw.writes(),
        &binary_handshake_bytes(ProtocolVersion::NEWEST)
    );
    assert_eq!(raw.flushes(), 1);

    let mut replay = Vec::new();
    raw.read_to_end(&mut replay)
        .expect("remaining bytes readable");
    assert!(replay.is_empty());
}

#[test]
fn session_parts_into_inner_returns_legacy_transport() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

    let parts =
        negotiate_session_parts(transport, ProtocolVersion::NEWEST).expect("legacy parts succeed");

    let mut raw = parts.into_inner();
    assert_eq!(raw.writes(), b"@RSYNCD: 31.0\n");
    assert_eq!(raw.flushes(), 1);

    let mut replay = Vec::new();
    raw.read_to_end(&mut replay)
        .expect("remaining bytes readable");
    assert!(replay.is_empty());
}

#[test]
fn negotiate_session_parts_from_stream_handles_binary_transport() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let stream = sniff_negotiation_stream(MemoryTransport::new(&binary_handshake_bytes(
        remote_version,
    )))
    .expect("binary sniff succeeds");

    let parts = negotiate_session_parts_from_stream(stream, ProtocolVersion::NEWEST)
        .expect("binary parts succeed");

    assert_eq!(parts.decision(), NegotiationPrologue::Binary);
    assert!(parts.is_binary());
    assert!(!parts.is_legacy());
    assert_eq!(parts.negotiated_protocol(), remote_version);
    assert_eq!(parts.remote_protocol(), remote_version);

    let transport = parts.clone().into_handshake().into_inner();
    assert_eq!(
        transport.writes(),
        &binary_handshake_bytes(ProtocolVersion::NEWEST)
    );
    assert_eq!(transport.flushes(), 1);
}

#[test]
fn negotiate_session_parts_from_stream_handles_legacy_transport() {
    let stream = sniff_negotiation_stream(MemoryTransport::new(b"@RSYNCD: 31.0\n@RSYNCD: OK\n"))
        .expect("legacy sniff succeeds");

    let parts = negotiate_session_parts_from_stream(stream, ProtocolVersion::NEWEST)
        .expect("legacy parts succeed");

    assert_eq!(parts.decision(), NegotiationPrologue::LegacyAscii);
    assert!(parts.is_legacy());
    assert!(!parts.is_binary());
    assert_eq!(
        parts.negotiated_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );

    let greeting = parts
        .server_greeting()
        .expect("legacy parts expose greeting");
    assert_eq!(greeting.advertised_protocol(), 31);

    let transport = parts.into_handshake().into_inner();
    assert_eq!(transport.writes(), b"@RSYNCD: 31.0\n");
    assert_eq!(transport.flushes(), 1);
}

#[test]
fn negotiate_session_parts_exposes_legacy_metadata() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

    let parts =
        negotiate_session_parts(transport, ProtocolVersion::NEWEST).expect("legacy parts succeed");

    assert_eq!(parts.decision(), NegotiationPrologue::LegacyAscii);
    assert!(parts.is_legacy());
    assert!(!parts.is_binary());
    assert_eq!(
        parts.remote_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );
    assert_eq!(
        parts.negotiated_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );
    assert!(!parts.local_protocol_was_capped());

    let tuple_parts = parts
        .clone()
        .into_legacy()
        .expect("legacy tuple parts available");
    let (greeting, negotiated_protocol, _stream_parts_tuple) = tuple_parts;
    assert_eq!(greeting.advertised_protocol(), 31);
    assert_eq!(
        greeting.protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );
    assert_eq!(
        negotiated_protocol,
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );

    let legacy_parts =
        LegacyDaemonHandshakeParts::try_from(parts).expect("legacy parts conversion");
    assert_eq!(legacy_parts.negotiated_protocol(), negotiated_protocol);
    assert_eq!(
        legacy_parts.server_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );

    let stream_parts = legacy_parts.into_stream_parts();
    let transport = stream_parts.into_stream().into_inner();
    assert_eq!(transport.writes(), b"@RSYNCD: 31.0\n");
    assert_eq!(transport.flushes(), 1);
}

#[test]
fn try_from_parts_rejects_mismatched_variant() {
    let transport = MemoryTransport::new(&binary_handshake_bytes(ProtocolVersion::NEWEST));
    let parts =
        negotiate_session_parts(transport, ProtocolVersion::NEWEST).expect("binary parts succeed");

    let err = LegacyDaemonHandshakeParts::try_from(parts.clone()).unwrap_err();
    assert_eq!(err.decision(), NegotiationPrologue::Binary);

    let legacy_transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let legacy_parts = negotiate_session_parts(legacy_transport, ProtocolVersion::NEWEST)
        .expect("legacy parts succeed");

    let err = BinaryHandshakeParts::try_from(legacy_parts.clone()).unwrap_err();
    assert_eq!(err.decision(), NegotiationPrologue::LegacyAscii);
}

#[test]
fn negotiate_session_parts_with_sniffer_supports_reuse() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport1 = MemoryTransport::new(&binary_handshake_bytes(remote_version));
    let transport2 = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let mut sniffer = NegotiationPrologueSniffer::new();

    let parts1 =
        negotiate_session_parts_with_sniffer(transport1, ProtocolVersion::NEWEST, &mut sniffer)
            .expect("binary parts succeed");
    assert_eq!(parts1.decision(), NegotiationPrologue::Binary);
    assert_eq!(parts1.remote_protocol(), remote_version);

    let (_remote_advertised, _remote_protocol, _local_advertised, _negotiated, stream_parts) =
        parts1.into_binary().expect("binary parts");
    let transport1 = stream_parts.into_stream().into_inner();
    assert_eq!(
        transport1.writes(),
        &binary_handshake_bytes(ProtocolVersion::NEWEST)
    );

    let parts2 =
        negotiate_session_parts_with_sniffer(transport2, ProtocolVersion::NEWEST, &mut sniffer)
            .expect("legacy parts succeed");
    assert_eq!(parts2.decision(), NegotiationPrologue::LegacyAscii);
    assert_eq!(
        parts2.remote_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );

    let (_greeting, _negotiated, stream_parts) = parts2.into_legacy().expect("legacy parts");
    let transport2 = stream_parts.into_stream().into_inner();
    assert_eq!(transport2.writes(), b"@RSYNCD: 31.0\n");
    assert_eq!(transport2.flushes(), 1);
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
    assert!(!handshake1.local_protocol_was_capped());

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
    assert!(!handshake2.local_protocol_was_capped());

    let transport2 = handshake2
        .into_legacy()
        .expect("second handshake is legacy")
        .into_stream()
        .into_inner();
    assert_eq!(transport2.writes(), b"@RSYNCD: 31.0\n");
    assert_eq!(transport2.flushes(), 1);
}

#[test]
fn session_handshake_server_greeting_matches_variant() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let binary_transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));
    let binary_handshake =
        negotiate_session(binary_transport, ProtocolVersion::NEWEST).expect("binary handshake");

    assert!(!binary_handshake.local_protocol_was_capped());
    assert!(binary_handshake.server_greeting().is_none());

    let legacy_transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let legacy_handshake =
        negotiate_session(legacy_transport, ProtocolVersion::NEWEST).expect("legacy handshake");

    assert!(!legacy_handshake.local_protocol_was_capped());
    let greeting = legacy_handshake
        .server_greeting()
        .expect("legacy handshake exposes greeting");
    assert_eq!(greeting.advertised_protocol(), 31);
    assert_eq!(
        greeting.protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );
}

#[test]
fn as_variant_mut_helpers_allow_mutating_streams() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));
    let mut binary_handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake succeeds");

    assert!(binary_handshake.as_legacy_mut().is_none());
    {
        let stream = binary_handshake
            .as_binary_mut()
            .expect("binary handshake exposes mutable reference")
            .stream_mut();
        stream
            .write_all(b"payload")
            .expect("writes propagate through binary handshake");
    }

    let transport = binary_handshake
        .into_binary()
        .expect("handshake remains binary")
        .into_stream()
        .into_inner();
    let mut expected = binary_handshake_bytes(ProtocolVersion::NEWEST).to_vec();
    expected.extend_from_slice(b"payload");
    assert_eq!(transport.writes(), expected.as_slice());

    let legacy_transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let mut legacy_handshake = negotiate_session(legacy_transport, ProtocolVersion::NEWEST)
        .expect("legacy handshake succeeds");

    assert!(legacy_handshake.as_binary_mut().is_none());
    {
        let stream = legacy_handshake
            .as_legacy_mut()
            .expect("legacy handshake exposes mutable reference")
            .stream_mut();
        stream
            .write_all(b"payload")
            .expect("writes propagate through legacy handshake");
    }

    let transport = legacy_handshake
        .into_legacy()
        .expect("handshake remains legacy")
        .into_stream()
        .into_inner();
    let mut expected = b"@RSYNCD: 31.0\n".to_vec();
    expected.extend_from_slice(b"payload");
    assert_eq!(transport.writes(), expected.as_slice());
}

#[test]
fn map_stream_inner_preserves_variant_and_metadata() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake succeeds");

    assert!(!handshake.local_protocol_was_capped());
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
    assert!(!handshake.local_protocol_was_capped());

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

    assert!(!handshake.local_protocol_was_capped());
    let mut handshake = handshake
        .try_map_stream_inner(
            |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                Ok(InstrumentedTransport::new(inner))
            },
        )
        .expect("mapping succeeds");

    assert!(!handshake.local_protocol_was_capped());
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

    assert!(!handshake.local_protocol_was_capped());
    let err = handshake
        .try_map_stream_inner(
            |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                Err((io::Error::other("boom"), inner))
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
    assert!(!handshake.local_protocol_was_capped());
    assert_eq!(
        handshake.remote_advertisement(),
        RemoteProtocolAdvertisement::from_raw(future_version, ProtocolVersion::NEWEST)
    );

    let parts = handshake.into_stream_parts();
    assert_eq!(parts.decision(), NegotiationPrologue::Binary);
    assert_eq!(parts.remote_protocol(), ProtocolVersion::NEWEST);
    assert_eq!(parts.remote_advertised_protocol(), future_version);
    assert_eq!(parts.local_advertised_protocol(), ProtocolVersion::NEWEST);
    assert!(parts.remote_protocol_was_clamped());
    assert!(!parts.local_protocol_was_capped());
    assert_eq!(
        parts.remote_advertisement(),
        RemoteProtocolAdvertisement::from_raw(future_version, ProtocolVersion::NEWEST)
    );
}

#[test]
fn session_handshake_parts_round_trip_binary_handshake() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake succeeds");

    assert!(!handshake.local_protocol_was_capped());
    let parts = handshake.into_stream_parts();
    assert_eq!(parts.decision(), NegotiationPrologue::Binary);
    assert_eq!(parts.negotiated_protocol(), remote_version);
    assert_eq!(parts.remote_protocol(), remote_version);
    assert_eq!(
        parts.remote_advertised_protocol(),
        u32::from(remote_version.as_u8())
    );
    assert_eq!(parts.local_advertised_protocol(), ProtocolVersion::NEWEST);
    assert!(!parts.remote_protocol_was_clamped());
    assert!(!parts.local_protocol_was_capped());
    assert!(parts.server_greeting().is_none());
    assert_eq!(parts.stream().decision(), NegotiationPrologue::Binary);

    let binary_parts = parts
        .into_binary_parts()
        .expect("binary parts available")
        .map_stream_inner(InstrumentedTransport::new);
    let parts = SessionHandshakeParts::Binary(binary_parts);

    let handshake = SessionHandshake::from_stream_parts(parts);
    let mut binary = handshake
        .into_binary()
        .expect("parts reconstruct binary handshake");

    assert!(!binary.local_protocol_was_capped());
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

    assert!(!handshake.local_protocol_was_capped());
    let parts = handshake.into_stream_parts();
    assert!(!parts.local_protocol_was_capped());
    let parts =
        SessionHandshakeParts::Binary(parts.into_binary_parts().expect("binary parts available"));

    let parts = parts
        .try_map_stream_inner(
            |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                Ok(InstrumentedTransport::new(inner))
            },
        )
        .expect("mapping succeeds");

    let handshake = SessionHandshake::from_stream_parts(parts);
    let mut binary = handshake.into_binary().expect("variant remains binary");

    assert!(!binary.local_protocol_was_capped());
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

    assert!(!handshake.local_protocol_was_capped());
    let parts = handshake.into_stream_parts();
    assert!(!parts.local_protocol_was_capped());
    let parts =
        SessionHandshakeParts::Binary(parts.into_binary_parts().expect("binary parts available"));

    let err = parts
        .try_map_stream_inner(
            |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                Err((io::Error::other("boom"), inner))
            },
        )
        .expect_err("mapping fails");

    assert_eq!(err.error().kind(), io::ErrorKind::Other);
    let original = err.into_original();
    assert!(!original.local_protocol_was_capped());
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

    assert!(!handshake.local_protocol_was_capped());
    let parts = handshake.into_stream_parts();
    assert_eq!(parts.decision(), NegotiationPrologue::LegacyAscii);
    let negotiated = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    assert_eq!(parts.negotiated_protocol(), negotiated);
    assert_eq!(parts.remote_protocol(), negotiated);
    assert_eq!(parts.remote_advertised_protocol(), 31);
    assert!(!parts.remote_protocol_was_clamped());
    assert!(!parts.local_protocol_was_capped());
    let server = parts.server_greeting().expect("server greeting retained");
    assert_eq!(server.advertised_protocol(), 31);
    assert_eq!(parts.stream().decision(), NegotiationPrologue::LegacyAscii);

    let legacy_parts = parts
        .into_legacy_parts()
        .expect("legacy parts available")
        .map_stream_inner(InstrumentedTransport::new);
    let parts = SessionHandshakeParts::Legacy(legacy_parts);

    let handshake = SessionHandshake::from_stream_parts(parts);
    let mut legacy = handshake
        .into_legacy()
        .expect("parts reconstruct legacy handshake");

    assert!(!legacy.local_protocol_was_capped());
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
fn session_handshake_converts_via_from_impls() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let parts: SessionHandshakeParts<_> = negotiate_session(transport, ProtocolVersion::NEWEST)
        .expect("binary handshake")
        .into();

    assert_eq!(parts.decision(), NegotiationPrologue::Binary);
    assert_eq!(parts.remote_protocol(), remote_version);

    let handshake: SessionHandshake<_> = SessionHandshake::from(parts);
    assert!(matches!(handshake.decision(), NegotiationPrologue::Binary));
    assert_eq!(handshake.remote_protocol(), remote_version);

    let legacy_transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let legacy_parts: SessionHandshakeParts<_> =
        negotiate_session(legacy_transport, ProtocolVersion::NEWEST)
            .expect("legacy handshake")
            .into();

    assert_eq!(legacy_parts.decision(), NegotiationPrologue::LegacyAscii);
    assert_eq!(
        legacy_parts.remote_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );

    let legacy_handshake: SessionHandshake<_> = SessionHandshake::from(legacy_parts);
    assert!(matches!(
        legacy_handshake.decision(),
        NegotiationPrologue::LegacyAscii
    ));
    assert_eq!(
        legacy_handshake.remote_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );
}

#[test]
fn session_handshake_from_variant_impls_promote_wrappers() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let binary = BinaryHandshake::try_from(
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake"),
    )
    .expect("conversion succeeds");

    let session: SessionHandshake<_> = SessionHandshake::from(binary);
    assert!(matches!(session.decision(), NegotiationPrologue::Binary));
    assert_eq!(session.remote_protocol(), remote_version);

    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let legacy = LegacyDaemonHandshake::try_from(
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("legacy handshake"),
    )
    .expect("conversion succeeds");

    let session: SessionHandshake<_> = SessionHandshake::from(legacy);
    assert!(matches!(
        session.decision(),
        NegotiationPrologue::LegacyAscii
    ));
    assert_eq!(
        session.remote_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );
}

#[test]
fn session_handshake_try_from_variant_impls_recover_wrappers() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let binary = BinaryHandshake::try_from(
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake"),
    )
    .expect("binary conversion succeeds");
    assert_eq!(binary.remote_protocol(), remote_version);

    let legacy_transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let legacy = LegacyDaemonHandshake::try_from(
        negotiate_session(legacy_transport, ProtocolVersion::NEWEST).expect("legacy handshake"),
    )
    .expect("legacy conversion succeeds");
    assert_eq!(legacy.server_greeting().advertised_protocol(), 31);

    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    match BinaryHandshake::try_from(
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("legacy handshake"),
    ) {
        Err(SessionHandshake::Legacy(handshake)) => {
            assert_eq!(handshake.server_greeting().advertised_protocol(), 31);
        }
        _ => panic!("legacy session must return original handshake"),
    }

    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));
    match LegacyDaemonHandshake::try_from(
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake"),
    ) {
        Err(SessionHandshake::Binary(handshake)) => {
            assert_eq!(handshake.remote_protocol(), remote_version);
        }
        _ => panic!("binary session must return original handshake"),
    }
}

#[test]
fn negotiate_session_from_stream_errors_when_undetermined() {
    let transport = MemoryTransport::new(b"");
    let undecided = NegotiatedStream::from_raw_parts(
        transport,
        NegotiationPrologue::NeedMoreData,
        0,
        0,
        Vec::new(),
    );

    let err = negotiate_session_from_stream(undecided, ProtocolVersion::NEWEST)
        .expect_err("undetermined prologue must error");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(err.to_string(), NEGOTIATION_PROLOGUE_UNDETERMINED_MSG);
}

#[test]
fn session_handshake_parts_from_variant_impls_promote_wrappers() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let binary = BinaryHandshake::try_from(
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake"),
    )
    .expect("binary conversion succeeds");
    let parts: SessionHandshakeParts<_> = SessionHandshakeParts::from(binary);
    assert!(matches!(parts.decision(), NegotiationPrologue::Binary));
    assert_eq!(parts.remote_protocol(), remote_version);

    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let legacy = LegacyDaemonHandshake::try_from(
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("legacy handshake"),
    )
    .expect("legacy conversion succeeds");
    let parts: SessionHandshakeParts<_> = SessionHandshakeParts::from(legacy);
    assert!(matches!(parts.decision(), NegotiationPrologue::LegacyAscii));
    assert_eq!(parts.remote_advertised_protocol(), 31);
}

#[test]
fn session_handshake_parts_try_from_variants_recover_wrappers() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let binary_parts =
        negotiate_session_parts(transport, ProtocolVersion::NEWEST).expect("binary parts");
    let binary =
        BinaryHandshake::try_from(binary_parts.clone()).expect("binary conversion succeeds");
    assert_eq!(binary.remote_protocol(), remote_version);

    match LegacyDaemonHandshake::try_from(binary_parts) {
        Err(SessionHandshakeParts::Binary(_)) => {}
        _ => panic!("binary parts must return original value"),
    }

    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let legacy_parts =
        negotiate_session_parts(transport, ProtocolVersion::NEWEST).expect("legacy parts");
    let legacy =
        LegacyDaemonHandshake::try_from(legacy_parts.clone()).expect("legacy conversion succeeds");
    assert_eq!(legacy.server_greeting().advertised_protocol(), 31);

    match BinaryHandshake::try_from(legacy_parts) {
        Err(SessionHandshakeParts::Legacy(_)) => {}
        _ => panic!("legacy parts must return original value"),
    }
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
    assert!(!handshake.local_protocol_was_capped());

    let parts = handshake.into_stream_parts();
    assert_eq!(parts.decision(), NegotiationPrologue::LegacyAscii);
    assert_eq!(parts.remote_protocol(), ProtocolVersion::NEWEST);
    assert_eq!(parts.remote_advertised_protocol(), 40);
    assert!(parts.remote_protocol_was_clamped());
    assert!(!parts.local_protocol_was_capped());
}

#[test]
fn session_handshake_parts_preserve_remote_protocol_for_legacy_caps() {
    let desired = ProtocolVersion::from_supported(30).expect("protocol 30 supported");
    let transport = MemoryTransport::new(b"@RSYNCD: 32.0\n");

    let handshake = negotiate_session(transport, desired)
        .expect("legacy handshake succeeds with future advertisement");

    assert!(handshake.local_protocol_was_capped());
    let parts = handshake.into_stream_parts();
    let remote = ProtocolVersion::from_supported(32).expect("protocol 32 supported");
    assert_eq!(parts.negotiated_protocol(), desired);
    assert_eq!(parts.remote_protocol(), remote);
    assert_eq!(parts.remote_advertised_protocol(), 32);
    assert!(!parts.remote_protocol_was_clamped());
    assert!(parts.local_protocol_was_capped());
    let server = parts.server_greeting().expect("server greeting retained");
    assert_eq!(server.protocol(), remote);
    assert_eq!(server.advertised_protocol(), 32);
}

#[test]
fn session_handshake_parts_clone_preserves_binary_stream_state() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake succeeds");

    assert!(!handshake.local_protocol_was_capped());
    let parts = handshake.into_stream_parts();
    let clone = parts.clone();

    let handshake = SessionHandshake::from_stream_parts(parts);
    assert_eq!(handshake.decision(), NegotiationPrologue::Binary);
    assert_eq!(handshake.negotiated_protocol(), remote_version);
    let mut binary = handshake
        .into_binary()
        .expect("original parts reconstruct binary handshake");

    assert!(!binary.local_protocol_was_capped());

    let clone_handshake = SessionHandshake::from_stream_parts(clone);
    assert_eq!(clone_handshake.decision(), NegotiationPrologue::Binary);
    assert_eq!(clone_handshake.negotiated_protocol(), remote_version);
    let mut cloned = clone_handshake
        .into_binary()
        .expect("cloned parts reconstruct binary handshake");

    assert!(!cloned.local_protocol_was_capped());

    binary
        .stream_mut()
        .write_all(b"original")
        .expect("write succeeds");
    cloned
        .stream_mut()
        .write_all(b"clone")
        .expect("write succeeds");

    let original_transport = binary.into_stream().into_inner();
    let cloned_transport = cloned.into_stream().into_inner();

    let mut expected_original = binary_handshake_bytes(ProtocolVersion::NEWEST).to_vec();
    expected_original.extend_from_slice(b"original");
    assert_eq!(original_transport.writes(), expected_original.as_slice());
    assert_eq!(original_transport.flushes(), 1);

    let mut expected_clone = binary_handshake_bytes(ProtocolVersion::NEWEST).to_vec();
    expected_clone.extend_from_slice(b"clone");
    assert_eq!(cloned_transport.writes(), expected_clone.as_slice());
    assert_eq!(cloned_transport.flushes(), 1);
}

#[test]
fn session_handshake_into_conversion_matches_method_for_binary() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake succeeds");

    let via_method = handshake.clone().into_stream_parts();
    let via_into: SessionHandshakeParts<_> = handshake.into();

    assert_eq!(via_into.decision(), NegotiationPrologue::Binary);
    assert_eq!(via_into.negotiated_protocol(), remote_version);
    assert_eq!(via_into.remote_protocol(), remote_version);
    assert_eq!(
        via_into.remote_advertised_protocol(),
        u32::from(remote_version.as_u8())
    );
    assert!(via_into.server_greeting().is_none());

    assert_eq!(via_into.decision(), via_method.decision());
    assert_eq!(
        via_into.negotiated_protocol(),
        via_method.negotiated_protocol()
    );
    assert_eq!(via_into.remote_protocol(), via_method.remote_protocol());
    assert_eq!(
        via_into.remote_advertised_protocol(),
        via_method.remote_advertised_protocol()
    );

    let reconstructed: SessionHandshake<_> = via_into.clone().into();
    assert_eq!(reconstructed.decision(), NegotiationPrologue::Binary);
    assert_eq!(reconstructed.negotiated_protocol(), remote_version);
    assert_eq!(reconstructed.remote_protocol(), remote_version);
    assert_eq!(
        reconstructed.remote_advertised_protocol(),
        u32::from(remote_version.as_u8())
    );
}

#[test]
fn session_handshake_parts_clone_preserves_legacy_stream_state() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("legacy handshake succeeds");

    assert!(!handshake.local_protocol_was_capped());
    let parts = handshake.into_stream_parts();
    let clone = parts.clone();

    let handshake = SessionHandshake::from_stream_parts(parts);
    assert_eq!(handshake.decision(), NegotiationPrologue::LegacyAscii);
    let negotiated = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    assert_eq!(handshake.negotiated_protocol(), negotiated);
    let mut legacy = handshake
        .into_legacy()
        .expect("original parts reconstruct legacy handshake");

    assert!(!legacy.local_protocol_was_capped());

    let clone_handshake = SessionHandshake::from_stream_parts(clone);
    assert_eq!(clone_handshake.decision(), NegotiationPrologue::LegacyAscii);
    assert_eq!(clone_handshake.negotiated_protocol(), negotiated);
    let mut cloned = clone_handshake
        .into_legacy()
        .expect("cloned parts reconstruct legacy handshake");

    assert!(!cloned.local_protocol_was_capped());

    legacy
        .stream_mut()
        .write_all(b"module\n")
        .expect("write succeeds");
    cloned
        .stream_mut()
        .write_all(b"other\n")
        .expect("write succeeds");

    let original_transport = legacy.into_stream().into_inner();
    let cloned_transport = cloned.into_stream().into_inner();

    let mut expected_original = format_legacy_daemon_greeting(negotiated).into_bytes();
    expected_original.extend_from_slice(b"module\n");
    assert_eq!(original_transport.writes(), expected_original.as_slice());
    assert_eq!(original_transport.flushes(), 1);

    let mut expected_clone = format_legacy_daemon_greeting(negotiated).into_bytes();
    expected_clone.extend_from_slice(b"other\n");
    assert_eq!(cloned_transport.writes(), expected_clone.as_slice());
    assert_eq!(cloned_transport.flushes(), 1);
}

#[test]
fn session_handshake_into_conversion_matches_method_for_legacy() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("legacy handshake succeeds");

    let via_method = handshake.clone().into_stream_parts();
    let via_into: SessionHandshakeParts<_> = handshake.into();

    assert_eq!(via_into.decision(), NegotiationPrologue::LegacyAscii);
    let negotiated = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    assert_eq!(via_into.negotiated_protocol(), negotiated);
    assert_eq!(via_into.remote_protocol(), negotiated);
    assert_eq!(
        via_into.remote_advertised_protocol(),
        u32::from(negotiated.as_u8())
    );
    assert!(via_into.server_greeting().is_some());

    assert_eq!(via_into.decision(), via_method.decision());
    assert_eq!(
        via_into.negotiated_protocol(),
        via_method.negotiated_protocol()
    );
    assert_eq!(via_into.remote_protocol(), via_method.remote_protocol());
    assert_eq!(
        via_into.remote_advertised_protocol(),
        via_method.remote_advertised_protocol()
    );

    let reconstructed: SessionHandshake<_> = via_into.clone().into();
    assert_eq!(reconstructed.decision(), NegotiationPrologue::LegacyAscii);
    assert_eq!(reconstructed.negotiated_protocol(), negotiated);
    assert!(reconstructed.server_greeting().is_some());
}

#[test]
fn binary_handshake_clone_preserves_stream_state() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let mut handshake = negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("binary handshake succeeds");

    let _ = handshake
        .stream_mut()
        .read(&mut [0u8; 1])
        .expect("reading from replay buffer never fails");
    let consumed = handshake.stream().buffered_consumed();
    let remaining = handshake.stream().buffered_remaining();

    let cloned = handshake.clone();
    assert_eq!(
        cloned.negotiated_protocol(),
        handshake.negotiated_protocol()
    );
    assert_eq!(cloned.remote_protocol(), handshake.remote_protocol());
    assert_eq!(cloned.stream().buffered_consumed(), consumed);
    assert_eq!(cloned.stream().buffered_remaining(), remaining);

    let mut clone_stream = cloned.into_stream();
    let mut clone_replay = Vec::new();
    clone_stream
        .read_to_end(&mut clone_replay)
        .expect("cloned stream replays remaining bytes");
    let clone_transport = clone_stream.into_inner();

    let mut original_stream = handshake.into_stream();
    let mut original_replay = Vec::new();
    original_stream
        .read_to_end(&mut original_replay)
        .expect("original stream replays remaining bytes");
    let original_transport = original_stream.into_inner();

    assert_eq!(clone_replay, original_replay);
    assert_eq!(clone_transport.writes(), original_transport.writes());
    assert_eq!(clone_transport.flushes(), original_transport.flushes());
}

#[test]
fn legacy_handshake_clone_preserves_stream_state() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

    let mut handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
        .expect("legacy handshake succeeds");

    let _ = handshake
        .stream_mut()
        .read(&mut [0u8; 1])
        .expect("reading from replay buffer never fails");
    let consumed = handshake.stream().buffered_consumed();
    let remaining = handshake.stream().buffered_remaining();

    let cloned = handshake.clone();
    assert_eq!(
        cloned.negotiated_protocol(),
        handshake.negotiated_protocol()
    );
    assert_eq!(cloned.server_protocol(), handshake.server_protocol());
    assert_eq!(cloned.stream().buffered_consumed(), consumed);
    assert_eq!(cloned.stream().buffered_remaining(), remaining);

    let mut clone_stream = cloned.into_stream();
    let mut clone_replay = Vec::new();
    clone_stream
        .read_to_end(&mut clone_replay)
        .expect("cloned stream replays remaining bytes");
    let clone_transport = clone_stream.into_inner();

    let mut original_stream = handshake.into_stream();
    let mut original_replay = Vec::new();
    original_stream
        .read_to_end(&mut original_replay)
        .expect("original stream replays remaining bytes");
    let original_transport = original_stream.into_inner();

    assert_eq!(clone_replay, original_replay);
    assert_eq!(clone_transport.writes(), original_transport.writes());
    assert_eq!(clone_transport.flushes(), original_transport.flushes());
}

#[test]
fn session_handshake_clone_preserves_stream_state() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let mut handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake succeeds");

    let _ = handshake
        .stream_mut()
        .read(&mut [0u8; 1])
        .expect("reading from replay buffer never fails");
    let consumed = handshake.stream().buffered_consumed();
    let remaining = handshake.stream().buffered_remaining();

    let cloned = handshake.clone();
    assert_eq!(cloned.decision(), handshake.decision());
    assert_eq!(
        cloned.negotiated_protocol(),
        handshake.negotiated_protocol()
    );
    assert_eq!(cloned.stream().buffered_consumed(), consumed);
    assert_eq!(cloned.stream().buffered_remaining(), remaining);

    let mut clone_stream = cloned.into_stream();
    let mut clone_replay = Vec::new();
    clone_stream
        .read_to_end(&mut clone_replay)
        .expect("cloned stream replays remaining bytes");
    let clone_transport = clone_stream.into_inner();

    let mut original_stream = handshake.into_stream();
    let mut original_replay = Vec::new();
    original_stream
        .read_to_end(&mut original_replay)
        .expect("original stream replays remaining bytes");
    let original_transport = original_stream.into_inner();

    assert_eq!(clone_replay, original_replay);
    assert_eq!(clone_transport.writes(), original_transport.writes());
    assert_eq!(clone_transport.flushes(), original_transport.flushes());
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
