use super::super::BinaryHandshake;
use super::helpers::{CountingTransport, MemoryTransport, handshake_bytes};
use crate::RemoteProtocolAdvertisement;
use oc_rsync_protocol::{NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion};
use std::io::{Read, Write};

#[test]
fn binary_handshake_round_trips_from_components() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&handshake_bytes(remote_version));

    let handshake = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds");

    let expected_buffer = handshake.stream().buffered().to_vec();
    let remote_advertised = handshake.remote_advertised_protocol();
    let remote_protocol = handshake.remote_protocol();
    let local_advertised = handshake.local_advertised_protocol();
    let negotiated_protocol = handshake.negotiated_protocol();
    let stream = handshake.into_stream();

    let rebuilt = BinaryHandshake::from_components(
        remote_advertised,
        remote_protocol,
        local_advertised,
        negotiated_protocol,
        stream,
    );

    assert_eq!(rebuilt.remote_advertised_protocol(), remote_advertised);
    assert_eq!(rebuilt.remote_protocol(), remote_protocol);
    assert_eq!(rebuilt.local_advertised_protocol(), local_advertised);
    assert_eq!(rebuilt.negotiated_protocol(), negotiated_protocol);
    assert_eq!(rebuilt.stream().decision(), NegotiationPrologue::Binary);
    assert_eq!(rebuilt.stream().buffered(), expected_buffer.as_slice());
    assert_eq!(
        rebuilt.remote_advertisement(),
        RemoteProtocolAdvertisement::Supported(remote_protocol)
    );
}

#[test]
fn into_stream_parts_exposes_negotiation_state() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let transport = MemoryTransport::new(&handshake_bytes(remote_version));

    let handshake = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds");

    assert!(!handshake.local_protocol_was_capped());
    let (remote_adv, remote, local_advertised, negotiated, parts) = handshake.into_stream_parts();
    assert_eq!(remote_adv, u32::from(remote_version.as_u8()));
    assert_eq!(remote, remote_version);
    assert_eq!(local_advertised, ProtocolVersion::NEWEST);
    assert_eq!(negotiated, remote_version);
    assert_eq!(parts.decision(), NegotiationPrologue::Binary);
    assert_eq!(
        parts.sniffed_prefix(),
        &handshake_bytes(remote_version)[..1]
    );
    assert_eq!(parts.buffered_remaining(), 0);
    assert_eq!(parts.sniffed_prefix_len(), 1);

    let mut stream = parts.into_stream();
    let mut remainder = Vec::new();
    stream
        .read_to_end(&mut remainder)
        .expect("no additional bytes remain after handshake");
    assert!(remainder.is_empty());

    let transport = stream.into_inner();
    assert_eq!(
        transport.written(),
        &handshake_bytes(ProtocolVersion::NEWEST)
    );
}

#[test]
fn binary_handshake_parts_into_components_matches_accessors() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let remote_advertisement = handshake_bytes(remote_version);
    let transport = MemoryTransport::new(&remote_advertisement);

    let parts = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds")
        .into_parts();

    let expected_advertised = parts.remote_advertised_protocol();
    let expected_remote = parts.remote_protocol();
    let expected_local = parts.local_advertised_protocol();
    let expected_negotiated = parts.negotiated_protocol();
    let expected_consumed = parts.stream_parts().buffered_consumed();
    let expected_buffer = parts.stream_parts().buffered().to_vec();

    let (advertised, remote, local, negotiated, stream_parts) = parts.into_components();

    assert_eq!(advertised, expected_advertised);
    assert_eq!(remote, expected_remote);
    assert_eq!(local, expected_local);
    assert_eq!(negotiated, expected_negotiated);
    assert_eq!(stream_parts.decision(), NegotiationPrologue::Binary);
    assert_eq!(
        stream_parts.sniffed_prefix(),
        &handshake_bytes(expected_remote)[..1]
    );
    assert_eq!(stream_parts.buffered_consumed(), expected_consumed);
    assert_eq!(stream_parts.buffered(), expected_buffer.as_slice());
}

#[test]
fn from_stream_parts_rehydrates_binary_handshake() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let transport = CountingTransport::new(&handshake_bytes(remote_version));

    let handshake = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds");

    assert!(!handshake.local_protocol_was_capped());
    let (remote_adv, remote, local_advertised, negotiated, parts) = handshake.into_stream_parts();
    assert_eq!(remote_adv, u32::from(remote_version.as_u8()));
    assert_eq!(remote, remote_version);
    assert_eq!(local_advertised, ProtocolVersion::NEWEST);
    assert_eq!(negotiated, remote_version);
    assert_eq!(parts.decision(), NegotiationPrologue::Binary);

    let mut rehydrated =
        BinaryHandshake::from_stream_parts(remote_adv, remote, local_advertised, negotiated, parts);

    assert!(!rehydrated.local_protocol_was_capped());
    assert_eq!(rehydrated.remote_protocol(), remote_version);
    assert_eq!(rehydrated.negotiated_protocol(), remote_version);
    assert_eq!(rehydrated.stream().decision(), NegotiationPrologue::Binary);

    rehydrated
        .stream_mut()
        .write_all(b"payload")
        .expect("write propagates");
    rehydrated.stream_mut().flush().expect("flush propagates");

    let transport = rehydrated.into_stream().into_inner();
    assert_eq!(transport.flushes(), 2);

    let mut expected = handshake_bytes(ProtocolVersion::NEWEST).to_vec();
    expected.extend_from_slice(b"payload");
    assert_eq!(transport.written(), expected.as_slice());
}

#[test]
fn into_parts_round_trips_binary_handshake() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let transport = CountingTransport::new(&handshake_bytes(remote_version));

    let handshake = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds");

    let parts = handshake.into_parts();
    assert_eq!(
        parts.remote_advertised_protocol(),
        u32::from(remote_version.as_u8())
    );
    assert_eq!(parts.remote_protocol(), remote_version);
    assert_eq!(parts.negotiated_protocol(), remote_version);
    assert_eq!(parts.local_advertised_protocol(), ProtocolVersion::NEWEST);
    assert!(!parts.remote_protocol_was_clamped());
    assert!(!parts.local_protocol_was_capped());
    assert_eq!(parts.stream_parts().decision(), NegotiationPrologue::Binary);

    let mut rebuilt = parts.into_handshake();
    assert_eq!(rebuilt.remote_protocol(), remote_version);
    assert_eq!(rebuilt.negotiated_protocol(), remote_version);

    rebuilt
        .stream_mut()
        .write_all(b"payload")
        .expect("write propagates");
    rebuilt.stream_mut().flush().expect("flush propagates");

    let transport = rebuilt.into_stream().into_inner();
    assert_eq!(transport.flushes(), 2);

    let mut expected = handshake_bytes(ProtocolVersion::NEWEST).to_vec();
    expected.extend_from_slice(b"payload");
    assert_eq!(transport.written(), expected.as_slice());
}

#[test]
fn binary_handshake_rehydrates_sniffer_state() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let mut bytes = handshake_bytes(remote_version).to_vec();
    bytes.extend_from_slice(b"payload");
    let transport = MemoryTransport::new(&bytes);

    let handshake = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds");

    let mut sniffer = NegotiationPrologueSniffer::new();
    handshake
        .rehydrate_sniffer(&mut sniffer)
        .expect("rehydration succeeds");

    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::Binary));
    assert_eq!(sniffer.buffered(), handshake.stream().buffered());
    assert_eq!(
        sniffer.sniffed_prefix_len(),
        handshake.stream().sniffed_prefix_len()
    );
}
