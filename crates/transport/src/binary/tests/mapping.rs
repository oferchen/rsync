use super::helpers::{InstrumentedTransport, MemoryTransport, handshake_bytes};
use protocol::ProtocolVersion;
use std::io::{self, Write};

#[test]
fn map_stream_inner_preserves_protocols_and_replays_transport() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let remote_advertisement = handshake_bytes(remote_version);
    let transport = MemoryTransport::new(&remote_advertisement);

    let handshake = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds");

    assert!(!handshake.local_protocol_was_capped());
    let mut handshake = handshake.map_stream_inner(InstrumentedTransport::new);
    handshake
        .stream_mut()
        .write_all(b"payload")
        .expect("write propagates");
    handshake.stream_mut().flush().expect("flush propagates");

    assert_eq!(handshake.remote_protocol(), remote_version);
    assert_eq!(handshake.negotiated_protocol(), remote_version);
    assert_eq!(
        handshake.remote_advertised_protocol(),
        u32::from(remote_version.as_u8())
    );
    assert!(!handshake.remote_protocol_was_clamped());
    assert!(!handshake.local_protocol_was_capped());

    let instrumented = handshake.into_stream().into_inner();
    assert_eq!(instrumented.writes(), b"payload");
    assert_eq!(instrumented.flushes(), 1);

    let inner = instrumented.into_inner();
    let mut expected = handshake_bytes(ProtocolVersion::NEWEST).to_vec();
    expected.extend_from_slice(b"payload");
    assert_eq!(inner.written(), expected.as_slice());
}

#[test]
fn try_map_stream_inner_transforms_transport() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let remote_advertisement = handshake_bytes(remote_version);
    let transport = MemoryTransport::new(&remote_advertisement);

    let handshake = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds");

    assert!(!handshake.local_protocol_was_capped());
    let mut handshake = handshake
        .try_map_stream_inner(
            |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                Ok(InstrumentedTransport::new(inner))
            },
        )
        .expect("mapping succeeds");

    assert!(!handshake.local_protocol_was_capped());
    handshake
        .stream_mut()
        .write_all(b"payload")
        .expect("write propagates");
    handshake.stream_mut().flush().expect("flush propagates");
    assert_eq!(handshake.remote_protocol(), remote_version);

    let instrumented = handshake.into_stream().into_inner();
    assert_eq!(instrumented.writes(), b"payload");
    assert_eq!(instrumented.flushes(), 1);
}

#[test]
fn parts_map_stream_inner_transforms_transport() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let remote_advertisement = handshake_bytes(remote_version);
    let transport = MemoryTransport::new(&remote_advertisement);

    let parts = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds")
        .into_parts();
    assert_eq!(parts.remote_protocol(), remote_version);

    let mapped = parts.map_stream_inner(InstrumentedTransport::new);
    assert_eq!(mapped.remote_protocol(), remote_version);

    let mut handshake = mapped.into_handshake();
    handshake
        .stream_mut()
        .write_all(b"payload")
        .expect("write propagates");
    handshake.stream_mut().flush().expect("flush propagates");

    let instrumented = handshake.into_stream().into_inner();
    assert_eq!(instrumented.writes(), b"payload");
    assert_eq!(instrumented.flushes(), 1);

    let inner = instrumented.into_inner();
    let mut expected = handshake_bytes(ProtocolVersion::NEWEST).to_vec();
    expected.extend_from_slice(b"payload");
    assert_eq!(inner.written(), expected.as_slice());
}

#[test]
fn parts_try_map_stream_inner_transforms_transport() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let remote_advertisement = handshake_bytes(remote_version);
    let transport = MemoryTransport::new(&remote_advertisement);

    let parts = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds")
        .into_parts();

    let mapped = parts
        .try_map_stream_inner(
            |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                Ok(InstrumentedTransport::new(inner))
            },
        )
        .expect("mapping succeeds");
    assert_eq!(mapped.remote_protocol(), remote_version);

    let mut handshake = mapped.into_handshake();
    handshake
        .stream_mut()
        .write_all(b"payload")
        .expect("write propagates");

    let instrumented = handshake.into_stream().into_inner();
    assert_eq!(instrumented.writes(), b"payload");
}

#[test]
fn parts_try_map_stream_inner_preserves_original_on_error() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let remote_advertisement = handshake_bytes(remote_version);
    let transport = MemoryTransport::new(&remote_advertisement);

    let parts = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds")
        .into_parts();

    let err = parts
        .try_map_stream_inner(
            |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                Err((io::Error::other("wrap failed"), inner))
            },
        )
        .expect_err("mapping fails");
    assert_eq!(err.error().kind(), io::ErrorKind::Other);

    let restored = err.into_original();
    assert_eq!(restored.remote_protocol(), remote_version);

    let remapped = restored.map_stream_inner(InstrumentedTransport::new);
    assert_eq!(remapped.remote_protocol(), remote_version);
}

#[test]
fn try_map_stream_inner_preserves_original_on_error() {
    let remote_version = ProtocolVersion::from_supported(31).expect("31 supported");
    let remote_advertisement = handshake_bytes(remote_version);
    let transport = MemoryTransport::new(&remote_advertisement);

    let handshake = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake succeeds");

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
    assert_eq!(original.remote_protocol(), remote_version);
    let transport = original.into_stream().into_inner();
    assert_eq!(
        transport.written(),
        &handshake_bytes(ProtocolVersion::NEWEST)
    );
}
