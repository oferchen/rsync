use super::super::*;
use super::common::MemoryTransport;
use crate::RemoteProtocolAdvertisement;
use rsync_protocol::ProtocolVersion;
use std::io::Write;

#[test]
fn parts_stream_parts_mut_allows_inner_mutation() {
    let transport = MemoryTransport::new(b"@RSYNCD: 31.0\n");

    let mut parts = negotiate_legacy_daemon_session(transport, ProtocolVersion::NEWEST)
        .expect("legacy handshake succeeds")
        .into_parts();

    {
        let stream_parts = parts.stream_parts_mut();
        assert!(stream_parts.decision().is_legacy());
        stream_parts
            .inner_mut()
            .write_all(b"@RSYNCD: OK\n")
            .expect("write propagates to inner transport");
        stream_parts
            .inner_mut()
            .flush()
            .expect("flush propagates to inner transport");
    }

    assert_eq!(
        parts.remote_advertisement(),
        RemoteProtocolAdvertisement::Supported(
            ProtocolVersion::from_supported(31).expect("protocol 31 supported"),
        )
    );
    assert_eq!(
        parts.local_advertised_protocol(),
        parts.negotiated_protocol()
    );

    let inner = parts.into_handshake().into_stream().into_inner();
    assert_eq!(inner.flushes(), 2);
    assert_eq!(inner.written(), b"@RSYNCD: 31.0\n@RSYNCD: OK\n");
}
