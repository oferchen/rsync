use super::super::*;
use super::common::{InstrumentedTransport, MemoryTransport};
use protocol::ProtocolVersion;
use std::io::{self, Write};

#[test]
fn map_stream_inner_preserves_state_and_transforms_transport() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake should succeed");

    assert!(!handshake.local_protocol_was_capped());
    let mut handshake = handshake.map_stream_inner(InstrumentedTransport::new);
    handshake
        .stream_mut()
        .write_all(b"@RSYNCD: OK\n")
        .expect("write propagates");
    handshake.stream_mut().flush().expect("flush propagates");

    assert_eq!(
        handshake.negotiated_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );
    assert_eq!(
        handshake.server_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );
    assert!(!handshake.local_protocol_was_capped());

    let instrumented = handshake.into_stream().into_inner();
    assert_eq!(instrumented.writes(), b"@RSYNCD: OK\n");
    assert_eq!(instrumented.flushes(), 1);

    let inner = instrumented.into_inner();
    assert_eq!(inner.flushes(), 2);
    assert_eq!(inner.written(), b"@RSYNCD: 31.0\n@RSYNCD: OK\n");
}

#[test]
fn try_map_stream_inner_transforms_transport() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake should succeed");

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
        .write_all(b"@RSYNCD: OK\n")
        .expect("write propagates");
    handshake.stream_mut().flush().expect("flush propagates");

    let instrumented = handshake.into_stream().into_inner();
    assert_eq!(instrumented.writes(), b"@RSYNCD: OK\n");
    assert_eq!(instrumented.flushes(), 1);
}

#[test]
fn parts_map_stream_inner_transforms_transport() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let parts = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake should succeed")
        .into_parts();

    let mapped = parts.map_stream_inner(InstrumentedTransport::new);
    assert_eq!(
        mapped.server_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );

    let mut handshake = mapped.into_handshake();
    handshake
        .stream_mut()
        .write_all(b"OK\n")
        .expect("write propagates");
    handshake.stream_mut().flush().expect("flush propagates");

    let instrumented = handshake.into_stream().into_inner();
    assert_eq!(instrumented.writes(), b"OK\n");
    assert_eq!(instrumented.flushes(), 1);

    let inner = instrumented.into_inner();
    assert_eq!(inner.written(), b"@RSYNCD: 31.0\nOK\n");
}

#[test]
fn parts_try_map_stream_inner_transforms_transport() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let parts = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake should succeed")
        .into_parts();

    let mapped = parts
        .try_map_stream_inner(
            |inner| -> Result<InstrumentedTransport, (io::Error, MemoryTransport)> {
                Ok(InstrumentedTransport::new(inner))
            },
        )
        .expect("mapping succeeds");
    assert_eq!(
        mapped.server_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );

    let mut handshake = mapped.into_handshake();
    handshake
        .stream_mut()
        .write_all(b"OK\n")
        .expect("write propagates");

    let instrumented = handshake.into_stream().into_inner();
    assert_eq!(instrumented.writes(), b"OK\n");
}

#[test]
fn parts_try_map_stream_inner_preserves_original_on_error() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let parts = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake should succeed")
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
    assert_eq!(
        restored.server_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );

    let remapped = restored.map_stream_inner(InstrumentedTransport::new);
    assert_eq!(
        remapped.server_protocol(),
        ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
    );
}

#[test]
fn try_map_stream_inner_preserves_original_on_error() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");
    let handshake = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
        .expect("handshake should succeed");

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
    let transport = original.into_stream().into_inner();
    assert_eq!(transport.written(), b"@RSYNCD: 31.0\n");
}
