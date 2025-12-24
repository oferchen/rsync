use super::*;

#[test]
fn session_reports_clamped_binary_future_version() {
    let future_version = 40u32;
    let transport = MemoryTransport::new(&future_version.to_be_bytes());

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
