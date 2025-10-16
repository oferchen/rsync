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
