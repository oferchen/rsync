use super::*;

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
