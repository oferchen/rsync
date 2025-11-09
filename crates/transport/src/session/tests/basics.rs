use super::*;

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
        _remote_flags,
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

    let (
        _remote_advertised,
        _remote_protocol,
        _local_advertised,
        _negotiated,
        _remote_flags,
        stream_parts,
    ) =
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
