use super::helpers::{CountingTransport, MemoryTransport, handshake_bytes};
use crate::RemoteProtocolAdvertisement;
use protocol::{NegotiationPrologueSniffer, ProtocolVersion};
use std::io;

#[test]
fn negotiate_binary_session_exchanges_versions() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let transport = MemoryTransport::new(&handshake_bytes(remote_version));

    let handshake = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds");

    let parts = handshake.clone().into_parts();
    assert_eq!(
        parts.remote_advertisement(),
        RemoteProtocolAdvertisement::Supported(remote_version)
    );
    assert_eq!(handshake.remote_protocol(), remote_version);
    assert_eq!(handshake.negotiated_protocol(), remote_version);
    assert_eq!(
        handshake.remote_advertised_protocol(),
        u32::from(remote_version.as_u8())
    );
    assert_eq!(
        handshake.remote_advertisement(),
        RemoteProtocolAdvertisement::Supported(remote_version)
    );
    assert!(!handshake.remote_protocol_was_clamped());
    assert!(!handshake.local_protocol_was_capped());

    let transport = handshake.into_stream().into_inner();
    assert_eq!(
        transport.written(),
        &handshake_bytes(ProtocolVersion::NEWEST)
    );
}

#[test]
fn negotiate_binary_session_clamps_future_protocols() {
    let future_version = 40u32;
    let transport = MemoryTransport::new(&future_version.to_be_bytes());

    let desired = ProtocolVersion::from_supported(29).expect("29 supported");
    let handshake =
        super::negotiate_binary_session(transport, desired).expect("future versions clamp");

    assert_eq!(handshake.remote_protocol(), ProtocolVersion::NEWEST);
    assert_eq!(handshake.negotiated_protocol(), desired);
    assert_eq!(handshake.remote_advertised_protocol(), future_version);
    assert_eq!(
        handshake.remote_advertisement(),
        RemoteProtocolAdvertisement::from_raw(future_version, ProtocolVersion::NEWEST)
    );

    let parts = handshake.into_parts();
    assert_eq!(parts.remote_protocol(), ProtocolVersion::NEWEST);
    assert_eq!(parts.negotiated_protocol(), desired);
    assert_eq!(parts.remote_advertised_protocol(), future_version);
    assert!(parts.remote_protocol_was_clamped());
    assert!(parts.local_protocol_was_capped());
    assert_eq!(
        parts.remote_advertisement(),
        RemoteProtocolAdvertisement::from_raw(future_version, ProtocolVersion::NEWEST)
    );

    let transport = parts.into_handshake().into_stream().into_inner();
    assert_eq!(transport.written(), &handshake_bytes(desired));
}

#[test]
fn negotiate_binary_session_clamps_protocols_beyond_u8_range() {
    let future_version = 0x0001_0200u32;
    let transport = MemoryTransport::new(&future_version.to_be_bytes());

    let handshake = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("future advertisements beyond u8 clamp to newest");

    assert_eq!(handshake.remote_advertised_protocol(), future_version);
    assert_eq!(handshake.remote_protocol(), ProtocolVersion::NEWEST);
    assert_eq!(handshake.negotiated_protocol(), ProtocolVersion::NEWEST);
    assert!(handshake.remote_protocol_was_clamped());
    assert!(!handshake.local_protocol_was_capped());
    assert_eq!(
        handshake.remote_advertisement(),
        RemoteProtocolAdvertisement::from_raw(future_version, ProtocolVersion::NEWEST)
    );
}

#[test]
fn negotiate_binary_session_clamps_u32_max_advertisement() {
    let transport = MemoryTransport::new(&u32::MAX.to_be_bytes());

    let handshake = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("maximum u32 advertisement clamps to newest");

    assert_eq!(handshake.remote_advertised_protocol(), u32::MAX);
    assert_eq!(handshake.remote_protocol(), ProtocolVersion::NEWEST);
    assert_eq!(handshake.negotiated_protocol(), ProtocolVersion::NEWEST);
    assert!(handshake.remote_protocol_was_clamped());
    assert!(!handshake.local_protocol_was_capped());
    assert_eq!(
        handshake.remote_advertisement(),
        RemoteProtocolAdvertisement::from_raw(u32::MAX, ProtocolVersion::NEWEST)
    );
}

#[test]
fn negotiate_binary_session_applies_cap() {
    let remote_version = ProtocolVersion::NEWEST;
    let desired = ProtocolVersion::from_supported(30).expect("30 supported");
    let transport = MemoryTransport::new(&handshake_bytes(remote_version));

    let handshake =
        super::negotiate_binary_session(transport, desired).expect("handshake succeeds");

    let parts = handshake.clone().into_parts();
    assert_eq!(
        parts.remote_advertisement(),
        RemoteProtocolAdvertisement::Supported(remote_version)
    );
    assert_eq!(handshake.remote_protocol(), remote_version);
    assert_eq!(handshake.negotiated_protocol(), desired);
    assert_eq!(
        handshake.remote_advertised_protocol(),
        u32::from(remote_version.as_u8())
    );
    assert_eq!(
        handshake.remote_advertisement(),
        RemoteProtocolAdvertisement::Supported(remote_version)
    );
    assert!(!handshake.remote_protocol_was_clamped());
    assert!(handshake.local_protocol_was_capped());

    let parts = handshake.into_parts();
    assert!(!parts.remote_protocol_was_clamped());
    assert!(parts.local_protocol_was_capped());
}

#[test]
fn negotiate_binary_session_rejects_legacy_prefix() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let err = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect_err("legacy prefix must fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn negotiate_binary_session_rejects_out_of_range_version() {
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&27u32.to_be_bytes());
    let transport = MemoryTransport::new(&bytes);
    let err = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect_err("unsupported protocol must fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn negotiate_binary_session_flushes_advertisement() {
    let transport = CountingTransport::new(&handshake_bytes(ProtocolVersion::NEWEST));

    let handshake = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds");

    assert!(!handshake.local_protocol_was_capped());
    let transport = handshake.into_stream().into_inner();
    assert_eq!(transport.flushes(), 1);
    assert_eq!(
        transport.written(),
        &handshake_bytes(ProtocolVersion::NEWEST)
    );
}

#[test]
fn negotiate_binary_session_with_sniffer_reuses_instance() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let transport1 = MemoryTransport::new(&handshake_bytes(remote_version));
    let transport2 = MemoryTransport::new(&handshake_bytes(remote_version));

    let mut sniffer = NegotiationPrologueSniffer::new();

    let handshake1 = super::negotiate_binary_session_with_sniffer(
        transport1,
        ProtocolVersion::NEWEST,
        &mut sniffer,
    )
    .expect("handshake succeeds with supplied sniffer");
    assert_eq!(handshake1.remote_protocol(), remote_version);
    assert_eq!(handshake1.negotiated_protocol(), remote_version);
    assert!(!handshake1.local_protocol_was_capped());

    drop(handshake1);

    let handshake2 = super::negotiate_binary_session_with_sniffer(
        transport2,
        ProtocolVersion::NEWEST,
        &mut sniffer,
    )
    .expect("sniffer can be reused for subsequent sessions");
    assert_eq!(handshake2.remote_protocol(), remote_version);
    assert_eq!(handshake2.negotiated_protocol(), remote_version);
    assert!(!handshake2.local_protocol_was_capped());
}
