use super::helpers::{MemoryTransport, handshake_payload, local_handshake_payload};
use crate::RemoteProtocolAdvertisement;
use protocol::{CompatibilityFlags, ProtocolVersion};
use std::io::Write;

fn sample_flags() -> CompatibilityFlags {
    CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SAFE_FILE_LIST
}

#[test]
fn parts_stream_parts_mut_exposes_inner_transport() {
    let remote_version = ProtocolVersion::from_supported(31).expect("protocol 31 supported");
    let transport = MemoryTransport::new(&handshake_payload(remote_version, sample_flags()));

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
    assert_eq!(parts.remote_compatibility_flags(), sample_flags());

    let inner = parts.into_handshake().into_stream().into_inner();
    let mut expected = local_handshake_payload(ProtocolVersion::NEWEST);
    expected.extend_from_slice(b"payload");
    assert_eq!(inner.written(), expected.as_slice());
}
