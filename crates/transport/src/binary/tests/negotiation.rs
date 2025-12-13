use super::ADVERTISED_COMPATIBILITY_FLAGS;
use super::helpers::{CountingTransport, MemoryTransport, handshake_bytes, handshake_payload};
use crate::RemoteProtocolAdvertisement;
use protocol::{CompatibilityFlags, NegotiationPrologueSniffer, ProtocolVersion};
use std::io;

fn sample_flags() -> CompatibilityFlags {
    CompatibilityFlags::INC_RECURSE | CompatibilityFlags::VARINT_FLIST_FLAGS
}

#[test]
fn negotiate_binary_session_exchanges_versions() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let transport = MemoryTransport::new(&handshake_payload(remote_version, sample_flags()));

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
    assert_eq!(handshake.remote_compatibility_flags(), sample_flags(),);
    assert!(!handshake.remote_protocol_was_clamped());
    assert!(!handshake.local_protocol_was_capped());

    let transport = handshake.into_stream().into_inner();
    assert_eq!(
        transport.written(),
        &handshake_payload(ProtocolVersion::NEWEST, ADVERTISED_COMPATIBILITY_FLAGS)
    );
}

#[test]
fn negotiate_binary_session_clamps_future_protocols() {
    let future_version = 40u32;
    let mut payload = future_version.to_be_bytes().to_vec();
    sample_flags()
        .encode_to_vec(&mut payload)
        .expect("compatibility encoding succeeds");
    let transport = MemoryTransport::new(&payload);

    let desired = ProtocolVersion::from_supported(31).expect("31 supported");
    let handshake =
        super::negotiate_binary_session(transport, desired).expect("future versions clamp");

    assert_eq!(handshake.remote_protocol(), ProtocolVersion::NEWEST);
    assert_eq!(handshake.negotiated_protocol(), desired);
    assert_eq!(handshake.remote_advertised_protocol(), future_version);
    assert_eq!(
        handshake.remote_advertisement(),
        RemoteProtocolAdvertisement::from_raw(future_version, ProtocolVersion::NEWEST)
    );
    assert_eq!(handshake.remote_compatibility_flags(), sample_flags());

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
    assert_eq!(parts.remote_compatibility_flags(), sample_flags());

    let transport = parts.into_handshake().into_stream().into_inner();
    assert_eq!(
        transport.written(),
        &handshake_payload(desired, ADVERTISED_COMPATIBILITY_FLAGS)
    );
}

#[test]
fn negotiate_binary_session_clamps_protocols_beyond_u8_range() {
    let future_version = 0x0001_0200u32;
    let mut payload = future_version.to_be_bytes().to_vec();
    sample_flags()
        .encode_to_vec(&mut payload)
        .expect("compatibility encoding succeeds");
    let transport = MemoryTransport::new(&payload);

    let err = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect_err("future advertisements beyond upstream cap must error");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn negotiate_binary_session_clamps_u32_max_advertisement() {
    let mut payload = u32::MAX.to_be_bytes().to_vec();
    sample_flags()
        .encode_to_vec(&mut payload)
        .expect("compatibility encoding succeeds");
    let transport = MemoryTransport::new(&payload);

    let err = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect_err("maximum u32 advertisement must exceed upstream cap");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn negotiate_binary_session_applies_cap() {
    let remote_version = ProtocolVersion::NEWEST;
    let desired = ProtocolVersion::from_supported(30).expect("30 supported");
    let transport = MemoryTransport::new(&handshake_payload(remote_version, sample_flags()));

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
    assert_eq!(handshake.remote_compatibility_flags(), sample_flags());
    assert!(!handshake.remote_protocol_was_clamped());
    assert!(handshake.local_protocol_was_capped());

    let parts = handshake.into_parts();
    assert!(!parts.remote_protocol_was_clamped());
    assert!(parts.local_protocol_was_capped());
    assert_eq!(parts.remote_compatibility_flags(), sample_flags());
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
    let transport =
        CountingTransport::new(&handshake_payload(ProtocolVersion::NEWEST, sample_flags()));

    let handshake = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds");

    assert!(!handshake.local_protocol_was_capped());
    assert_eq!(handshake.remote_compatibility_flags(), sample_flags());
    let transport = handshake.into_stream().into_inner();
    assert_eq!(transport.flushes(), 1);
    assert_eq!(
        transport.written(),
        &handshake_payload(ProtocolVersion::NEWEST, ADVERTISED_COMPATIBILITY_FLAGS)
    );
}

#[test]
fn negotiate_binary_session_handles_absent_compatibility_flags() {
    let remote_version = ProtocolVersion::NEWEST;
    let bytes = handshake_bytes(remote_version);
    let transport = MemoryTransport::new(&bytes);

    let handshake = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds without compatibility payload");

    assert_eq!(handshake.remote_protocol(), remote_version);
    assert_eq!(
        handshake.remote_compatibility_flags(),
        CompatibilityFlags::EMPTY
    );
}

#[test]
fn negotiate_binary_session_errors_on_truncated_compatibility_flags() {
    let remote_version = ProtocolVersion::NEWEST;
    let mut payload = handshake_bytes(remote_version).to_vec();
    payload.push(0x80); // Indicates an additional byte that never arrives.
    let transport = MemoryTransport::new(&payload);

    let err = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect_err("truncated compatibility flags must fail");

    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn negotiate_binary_session_with_sniffer_reuses_instance() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let transport1 = MemoryTransport::new(&handshake_payload(remote_version, sample_flags()));
    let transport2 = MemoryTransport::new(&handshake_payload(remote_version, sample_flags()));

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
    assert_eq!(handshake1.remote_compatibility_flags(), sample_flags());

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
    assert_eq!(handshake2.remote_compatibility_flags(), sample_flags());
}
