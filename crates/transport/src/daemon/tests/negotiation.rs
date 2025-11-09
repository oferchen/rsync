use super::super::*;
use super::common::MemoryTransport;
use crate::RemoteProtocolAdvertisement;
use protocol::{
    LEGACY_DAEMON_PREFIX_LEN, NegotiationError, NegotiationPrologue,
    NegotiationPrologueSniffer, ProtocolVersion, format_legacy_daemon_greeting,
};
use std::io::{self, Read, Write};

#[test]
fn negotiate_legacy_daemon_session_exchanges_banners() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake should succeed");

    let parts = handshake.clone().into_parts();
    let protocol_31 = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    assert_eq!(
        parts.remote_advertisement(),
        RemoteProtocolAdvertisement::Supported(protocol_31)
    );
    assert_eq!(handshake.negotiated_protocol(), protocol_31,);
    assert_eq!(handshake.server_protocol(), protocol_31,);
    assert_eq!(handshake.server_greeting().advertised_protocol(), 31);
    assert_eq!(handshake.remote_advertised_protocol(), 31);
    assert_eq!(
        handshake.remote_advertisement(),
        RemoteProtocolAdvertisement::Supported(protocol_31)
    );
    assert!(!handshake.remote_protocol_was_clamped());
    assert!(!handshake.local_protocol_was_capped());

    let transport = handshake.into_stream().into_inner();
    assert_eq!(transport.written(), b"@RSYNCD: 31.0\n");
    assert_eq!(transport.flushes(), 1);
}

#[test]
fn negotiate_respects_requested_protocol_cap() {
    let transport = MemoryTransport::new(b"@RSYNCD: 32.0\n");
    let desired = ProtocolVersion::from_supported(30).expect("protocol 30 supported");
    let handshake =
        negotiate_legacy_daemon_session(transport, desired).expect("handshake should succeed");

    assert_eq!(handshake.negotiated_protocol(), desired);
    assert_eq!(
        handshake.server_protocol(),
        ProtocolVersion::from_supported(32).expect("protocol 32 supported"),
    );
    assert_eq!(handshake.remote_advertised_protocol(), 32);
    assert_eq!(
        handshake.remote_advertisement(),
        RemoteProtocolAdvertisement::Supported(
            ProtocolVersion::from_supported(32).expect("protocol 32 supported"),
        )
    );
    assert!(!handshake.remote_protocol_was_clamped());
    assert!(handshake.local_protocol_was_capped());

    let parts = handshake.into_parts();
    assert!(!parts.remote_protocol_was_clamped());
    assert!(parts.local_protocol_was_capped());
    assert_eq!(
        parts.remote_advertisement(),
        RemoteProtocolAdvertisement::Supported(
            ProtocolVersion::from_supported(32).expect("protocol 32 supported"),
        )
    );

    let transport = parts.into_handshake().into_stream().into_inner();
    assert_eq!(transport.written(), b"@RSYNCD: 30.0\n");
}

#[test]
fn negotiate_clamps_future_advertisement() {
    let transport = MemoryTransport::new(b"@RSYNCD: 40.0\n");
    let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake should succeed");

    assert_eq!(handshake.server_greeting().advertised_protocol(), 40);
    assert_eq!(handshake.remote_advertised_protocol(), 40);
    assert_eq!(handshake.server_protocol(), ProtocolVersion::NEWEST);
    assert_eq!(handshake.negotiated_protocol(), ProtocolVersion::NEWEST);
    assert!(handshake.remote_protocol_was_clamped());
    assert!(!handshake.local_protocol_was_capped());
    assert_eq!(
        handshake.remote_advertisement(),
        RemoteProtocolAdvertisement::from_raw(40, ProtocolVersion::NEWEST)
    );

    let parts = handshake.into_parts();
    assert!(parts.remote_protocol_was_clamped());
    assert!(!parts.local_protocol_was_capped());
    assert_eq!(
        parts.remote_advertisement(),
        RemoteProtocolAdvertisement::from_raw(40, ProtocolVersion::NEWEST)
    );

    let transport = parts.into_handshake().into_stream().into_inner();
    assert_eq!(transport.written(), b"@RSYNCD: 32.0\n");
}

#[test]
fn negotiate_clamps_large_future_advertisement() {
    let transport = MemoryTransport::new(b"@RSYNCD: 999.0\n");
    let err = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
        .expect_err("advertisements beyond the upstream cap must be rejected");

    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    let negotiation = err
        .into_inner()
        .and_then(|inner| inner.downcast::<NegotiationError>().ok())
        .expect("negotiation error available");
    assert_eq!(*negotiation, NegotiationError::UnsupportedVersion(999));
}

#[test]
fn negotiate_clamps_max_u32_advertisement() {
    let transport = MemoryTransport::new(b"@RSYNCD: 4294967295.0\n");
    let err = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
        .expect_err("u32::MAX advertisements exceed upstream cap");

    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    let negotiation = err
        .into_inner()
        .and_then(|inner| inner.downcast::<NegotiationError>().ok())
        .expect("negotiation error available");
    assert_eq!(
        *negotiation,
        NegotiationError::UnsupportedVersion(u32::MAX)
    );
}

#[test]
fn negotiate_rejects_binary_prefix() {
    let transport = MemoryTransport::new(&[0x00, 0x20, 0x00, 0x00]);
    match negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST) {
        Ok(_) => panic!("binary negotiation is rejected"),
        Err(err) => assert_eq!(err.kind(), io::ErrorKind::InvalidData),
    }
}

#[test]
fn into_parts_round_trips_legacy_handshake() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake should succeed");

    let parts = handshake.into_parts();
    let expected_protocol = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    assert_eq!(parts.server_protocol(), expected_protocol);
    assert_eq!(parts.negotiated_protocol(), expected_protocol);
    assert_eq!(parts.remote_advertised_protocol(), 31);
    assert!(!parts.remote_protocol_was_clamped());
    assert!(!parts.local_protocol_was_capped());
    assert_eq!(
        parts.stream_parts().decision(),
        NegotiationPrologue::LegacyAscii
    );

    let mut rebuilt = parts.into_handshake();
    assert_eq!(rebuilt.server_protocol(), expected_protocol);
    assert_eq!(rebuilt.negotiated_protocol(), expected_protocol);

    rebuilt
        .stream_mut()
        .write_all(b"@RSYNCD: OK\n")
        .expect("write propagates");
    rebuilt.stream_mut().flush().expect("flush propagates");

    let transport = rebuilt.into_stream().into_inner();
    assert_eq!(transport.flushes(), 2);
    assert_eq!(transport.written(), b"@RSYNCD: 31.0\n@RSYNCD: OK\n");
}

#[test]
fn legacy_handshake_rehydrates_sniffer_state() {
    let mut bytes = b"@RSYNCD: 31.0\n".to_vec();
    bytes.extend_from_slice(b"@RSYNCD: OK\n");
    let transport = MemoryTransport::new(&bytes);

    let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake should succeed");

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
fn negotiate_legacy_daemon_session_with_sniffer_can_be_reused() {
    let transport1 = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let transport2 = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let mut sniffer = NegotiationPrologueSniffer::new();

    let handshake1 = negotiate_legacy_daemon_session_with_sniffer(
        transport1,
        ProtocolVersion::NEWEST,
        &mut sniffer,
    )
    .expect("handshake should succeed with supplied sniffer");
    assert_eq!(
        handshake1.negotiated_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );
    assert_eq!(
        handshake1.server_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );

    drop(handshake1);

    let handshake2 = negotiate_legacy_daemon_session_with_sniffer(
        transport2,
        ProtocolVersion::NEWEST,
        &mut sniffer,
    )
    .expect("sniffer can be reused across sessions");
    assert_eq!(
        handshake2.negotiated_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );
    assert_eq!(
        handshake2.server_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );
}

#[test]
fn into_stream_parts_exposes_legacy_state() {
    let desired = ProtocolVersion::from_supported(30).expect("protocol 30 supported");
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

    let handshake =
        negotiate_legacy_daemon_session(transport, desired).expect("handshake should succeed");

    assert!(handshake.local_protocol_was_capped());
    let (greeting, negotiated, parts) = handshake.into_stream_parts();
    assert_eq!(greeting.advertised_protocol(), 31);
    assert_eq!(negotiated, desired);
    assert_eq!(parts.decision(), NegotiationPrologue::LegacyAscii);
    assert_eq!(parts.sniffed_prefix(), b"@RSYNCD:");
    assert_eq!(parts.sniffed_prefix_len(), LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(parts.buffered_remaining(), 0);

    let mut stream = parts.into_stream();
    let mut tail = Vec::new();
    stream
        .read_to_end(&mut tail)
        .expect("legacy handshake drains buffered prefix");
    assert!(tail.is_empty());

    let transport = stream.into_inner();
    assert_eq!(transport.flushes(), 1);
    assert_eq!(
        transport.written(),
        format_legacy_daemon_greeting(negotiated).as_bytes()
    );
}

#[test]
fn legacy_handshake_parts_into_components_matches_accessors() {
    let desired = ProtocolVersion::from_supported(30).expect("protocol 30 supported");
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

    let parts = negotiate_legacy_daemon_session(transport, desired)
        .expect("handshake should succeed")
        .into_parts();

    let expected_greeting = parts.server_greeting().clone();
    let expected_negotiated = parts.negotiated_protocol();
    let expected_consumed = parts.stream_parts().buffered_consumed();
    let expected_buffer = parts.stream_parts().buffered().to_vec();

    let (greeting, negotiated, stream_parts) = parts.into_components();

    assert_eq!(greeting, expected_greeting);
    assert_eq!(negotiated, expected_negotiated);
    assert_eq!(stream_parts.decision(), NegotiationPrologue::LegacyAscii);
    assert_eq!(stream_parts.sniffed_prefix(), b"@RSYNCD:");
    assert_eq!(stream_parts.buffered_consumed(), expected_consumed);
    assert_eq!(stream_parts.buffered(), expected_buffer.as_slice());
}

#[test]
fn from_stream_parts_rehydrates_legacy_handshake() {
    let desired = ProtocolVersion::from_supported(30).expect("protocol 30 supported");
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

    let handshake =
        negotiate_legacy_daemon_session(transport, desired).expect("handshake should succeed");

    assert!(handshake.local_protocol_was_capped());
    let (greeting, negotiated, parts) = handshake.into_stream_parts();
    let greeting_clone = greeting.clone();
    assert_eq!(parts.decision(), NegotiationPrologue::LegacyAscii);

    let mut rehydrated = LegacyDaemonHandshake::from_stream_parts(greeting, negotiated, parts);

    assert!(rehydrated.local_protocol_was_capped());
    assert_eq!(rehydrated.negotiated_protocol(), negotiated);
    assert_eq!(rehydrated.server_greeting(), &greeting_clone);
    assert_eq!(rehydrated.server_protocol(), greeting_clone.protocol());
    assert_eq!(
        rehydrated.stream().decision(),
        NegotiationPrologue::LegacyAscii
    );

    rehydrated
        .stream_mut()
        .write_all(b"@RSYNCD: OK\n")
        .expect("write propagates");
    rehydrated.stream_mut().flush().expect("flush propagates");

    let transport = rehydrated.into_stream().into_inner();
    assert_eq!(transport.flushes(), 2);

    let mut expected = format_legacy_daemon_greeting(negotiated);
    expected.push_str("@RSYNCD: OK\n");
    assert_eq!(transport.written(), expected.as_bytes());
}

#[test]
fn legacy_handshake_round_trips_from_components() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

    let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake should succeed");

    let expected_buffer = handshake.stream().buffered().to_vec();
    let greeting = handshake.server_greeting().clone();
    let negotiated_protocol = handshake.negotiated_protocol();
    let stream = handshake.into_stream();

    let rebuilt =
        LegacyDaemonHandshake::from_components(greeting.clone(), negotiated_protocol, stream);

    assert_eq!(rebuilt.server_greeting(), &greeting);
    assert_eq!(rebuilt.negotiated_protocol(), negotiated_protocol);
    assert_eq!(
        rebuilt.stream().decision(),
        NegotiationPrologue::LegacyAscii
    );
    assert_eq!(rebuilt.stream().buffered(), expected_buffer.as_slice());
}

#[test]
fn legacy_client_greeting_echoes_digest_list() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0 sha512 sha256 sha1 md5 md4\n");

    let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake should succeed");

    let mut stream = handshake.into_stream();
    stream
        .write_all(b"@RSYNCD: OK\n")
        .expect("write propagates");
    stream.flush().expect("flush propagates");

    let inner = stream.into_inner();
    assert_eq!(
        inner.written(),
        b"@RSYNCD: 31.0 sha512 sha256 sha1 md5 md4\n@RSYNCD: OK\n"
    );
}

#[test]
fn legacy_client_greeting_respects_protocol_cap() {
    let desired = ProtocolVersion::from_supported(29).expect("protocol 29 supported");
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0 sha512 sha256\n");

    let handshake =
        negotiate_legacy_daemon_session(transport, desired).expect("handshake should succeed");

    let mut stream = handshake.into_stream();
    stream
        .write_all(b"@RSYNCD: OK\n")
        .expect("write propagates");
    stream.flush().expect("flush propagates");

    let inner = stream.into_inner();
    assert_eq!(
        inner.written(),
        b"@RSYNCD: 29.0 sha512 sha256\n@RSYNCD: OK\n"
    );
}
