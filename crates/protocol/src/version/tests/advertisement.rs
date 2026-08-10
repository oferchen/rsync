use crate::error::NegotiationError;

use super::ProtocolVersion;

#[test]
fn clamps_future_versions_in_peer_advertisements_directly() {
    let negotiated = ProtocolVersion::from_peer_advertisement(40).expect("future versions clamp");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
}

#[test]
fn rejects_peer_advertisements_older_than_supported_range() {
    let err = ProtocolVersion::from_peer_advertisement(27).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(27));
}

#[test]
fn rejects_peer_advertisements_beyond_upstream_cap() {
    let err = ProtocolVersion::from_peer_advertisement(41).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(41));
}

#[test]
fn rejects_zero_peer_advertisement() {
    let err = ProtocolVersion::from_peer_advertisement(0).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(0));
}
