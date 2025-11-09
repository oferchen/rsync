use super::helpers::{MemoryTransport, handshake_bytes};
use crate::RemoteProtocolAdvertisement;
use protocol::ProtocolVersion;
use std::io::Write;

#[test]
fn parts_stream_parts_mut_exposes_inner_transport() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&handshake_bytes(remote_version));

    let mut parts = super::negotiate_binary_session(transport, ProtocolVersion::NEWEST)
        .expect("binary handshake succeeds")
        .into_parts();

    {
        let stream_parts = parts.stream_parts_mut();
        assert!(stream_parts.decision().is_binary());
        stream_parts
            .inner_mut()
            .write_all(b"payload")
            .expect("write propagates to inner transport");
        stream_parts
            .inner_mut()
            .flush()
            .expect("flush propagates to inner transport");
    }

    assert_eq!(
        parts.remote_advertisement(),
        RemoteProtocolAdvertisement::Supported(remote_version)
    );
    assert_eq!(parts.negotiated_protocol(), remote_version);

    let inner = parts.into_handshake().into_stream().into_inner();
    let mut expected = handshake_bytes(ProtocolVersion::NEWEST).to_vec();
    expected.extend_from_slice(b"payload");
    assert_eq!(inner.written(), expected.as_slice());
}
