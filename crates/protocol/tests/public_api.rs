#![allow(clippy::needless_pass_by_value)]

use rsync_protocol::{ProtocolVersion, ProtocolVersionAdvertisement, select_highest_mutual};

#[derive(Clone, Copy)]
struct CustomAdvertised(u8);

impl ProtocolVersionAdvertisement for CustomAdvertised {
    #[inline]
    fn into_advertised_version(self) -> u8 {
        self.0
    }
}

#[test]
fn custom_advertised_types_can_participate_in_negotiation() {
    let peers = [CustomAdvertised(31), CustomAdvertised(32)];
    let negotiated = select_highest_mutual(peers).expect("should negotiate successfully");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
}

#[test]
fn supported_protocol_exports_remain_consistent() {
    assert_eq!(
        rsync_protocol::SUPPORTED_PROTOCOL_COUNT,
        rsync_protocol::SUPPORTED_PROTOCOLS.len(),
    );
}

#[test]
fn supported_protocols_match_upstream_order() {
    assert_eq!(rsync_protocol::SUPPORTED_PROTOCOLS, [32, 31, 30, 29, 28]);
}

#[test]
fn supported_protocol_exports_cover_range() {
    assert_eq!(
        ProtocolVersion::supported_protocol_numbers(),
        &rsync_protocol::SUPPORTED_PROTOCOLS,
    );
    assert_eq!(
        ProtocolVersion::supported_protocol_numbers_array(),
        &rsync_protocol::SUPPORTED_PROTOCOLS,
    );
    assert!(
        ProtocolVersion::supported_protocol_numbers_iter().eq(rsync_protocol::SUPPORTED_PROTOCOLS)
    );

    let exported_range = rsync_protocol::SUPPORTED_PROTOCOL_RANGE.clone();
    assert_eq!(ProtocolVersion::supported_range(), exported_range.clone());

    let (oldest, newest) = ProtocolVersion::supported_range_bounds();
    assert_eq!(oldest, *exported_range.start());
    assert_eq!(newest, *exported_range.end());
}
