use super::*;

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
fn session_handshake_into_parts_aliases_method_for_binary() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote_version));

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("binary handshake succeeds");

    let via_alias = handshake.clone().into_parts();
    let via_method = handshake.into_stream_parts();

    assert_eq!(via_alias.decision(), NegotiationPrologue::Binary);
    assert_eq!(via_alias.decision(), via_method.decision());
    assert_eq!(
        via_alias.negotiated_protocol(),
        via_method.negotiated_protocol()
    );
    assert_eq!(via_alias.remote_protocol(), via_method.remote_protocol());
    assert_eq!(
        via_alias.remote_advertised_protocol(),
        via_method.remote_advertised_protocol()
    );
    assert_eq!(
        via_alias.local_advertised_protocol(),
        via_method.local_advertised_protocol()
    );
    assert_eq!(via_alias.server_greeting(), via_method.server_greeting());
    assert_eq!(
        via_alias.stream().buffered(),
        via_method.stream().buffered()
    );
    assert_eq!(
        via_alias.stream().sniffed_prefix_len(),
        via_method.stream().sniffed_prefix_len()
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
fn session_handshake_into_parts_aliases_method_for_legacy() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

    let handshake =
        negotiate_session(transport, ProtocolVersion::NEWEST).expect("legacy handshake succeeds");

    let via_alias = handshake.clone().into_parts();
    let via_method = handshake.into_stream_parts();

    assert_eq!(via_alias.decision(), NegotiationPrologue::LegacyAscii);
    assert_eq!(via_alias.decision(), via_method.decision());
    assert_eq!(
        via_alias.negotiated_protocol(),
        via_method.negotiated_protocol()
    );
    assert_eq!(via_alias.remote_protocol(), via_method.remote_protocol());
    assert_eq!(
        via_alias.remote_advertised_protocol(),
        via_method.remote_advertised_protocol()
    );
    assert_eq!(
        via_alias.local_advertised_protocol(),
        via_method.local_advertised_protocol()
    );
    assert_eq!(via_alias.server_greeting(), via_method.server_greeting());
    assert_eq!(
        via_alias.stream().buffered(),
        via_method.stream().buffered()
    );
    assert_eq!(
        via_alias.stream().sniffed_prefix_len(),
        via_method.stream().sniffed_prefix_len()
    );
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

#[test]
fn session_handshake_parts_from_binary_components_round_trips() {
    let remote = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&binary_handshake_bytes(remote));

    let parts = negotiate_session_parts(transport, ProtocolVersion::NEWEST)
        .expect("binary negotiation succeeds");
    let (remote_advertised, remote_protocol, local_advertised, negotiated, stream_parts) = parts
        .clone()
        .into_binary()
        .expect("binary components available");

    let rebuilt = SessionHandshakeParts::from_binary_components(
        remote_advertised,
        remote_protocol,
        local_advertised,
        negotiated,
        stream_parts,
    );

    assert!(rebuilt.is_binary());
    assert_eq!(rebuilt.remote_protocol(), remote_protocol);
    assert_eq!(rebuilt.local_advertised_protocol(), local_advertised);
    assert_eq!(rebuilt.negotiated_protocol(), negotiated);

    let transport = rebuilt
        .clone()
        .into_handshake()
        .into_binary()
        .expect("binary handshake reconstructed")
        .into_stream()
        .into_inner();

    assert_eq!(
        transport.writes(),
        &binary_handshake_bytes(local_advertised)
    );
    assert_eq!(transport.flushes(), 1);
}

#[test]
fn session_handshake_parts_from_legacy_components_round_trips() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

    let parts = negotiate_session_parts(transport, ProtocolVersion::NEWEST)
        .expect("legacy negotiation succeeds");
    let (greeting, negotiated, stream_parts) = parts
        .clone()
        .into_legacy()
        .expect("legacy components available");
    let expected_protocol = negotiated;
    let expected_advertised = greeting.advertised_protocol();

    let rebuilt = SessionHandshakeParts::from_legacy_components(greeting, negotiated, stream_parts);

    assert!(rebuilt.is_legacy());
    assert_eq!(rebuilt.negotiated_protocol(), expected_protocol);
    let rebuilt_greeting = rebuilt
        .server_greeting()
        .expect("legacy parts expose greeting");
    assert_eq!(rebuilt_greeting.advertised_protocol(), expected_advertised);

    let transport = rebuilt
        .clone()
        .into_handshake()
        .into_legacy()
        .expect("legacy handshake reconstructed")
        .into_stream()
        .into_inner();

    assert_eq!(transport.writes(), b"@RSYNCD: 31.0\n");
    assert_eq!(transport.flushes(), 1);
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
