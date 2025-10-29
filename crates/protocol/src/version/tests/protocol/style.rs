use super::super::ProtocolVersion;

#[test]
fn negotiation_style_predicates_match_protocol_boundaries() {
    assert!(ProtocolVersion::V32.uses_binary_negotiation());
    assert!(ProtocolVersion::V31.uses_binary_negotiation());
    assert!(ProtocolVersion::V30.uses_binary_negotiation());

    assert!(ProtocolVersion::V29.uses_legacy_ascii_negotiation());
    assert!(ProtocolVersion::V28.uses_legacy_ascii_negotiation());

    assert!(!ProtocolVersion::V29.uses_binary_negotiation());
    assert!(!ProtocolVersion::V28.uses_binary_negotiation());
}

#[test]
fn binary_negotiation_threshold_matches_protocol_30() {
    let expected = ProtocolVersion::from_supported(30).expect("protocol 30 is supported");
    assert_eq!(ProtocolVersion::BINARY_NEGOTIATION_INTRODUCED, expected);
    assert!(ProtocolVersion::BINARY_NEGOTIATION_INTRODUCED.uses_binary_negotiation());
    assert!(!ProtocolVersion::BINARY_NEGOTIATION_INTRODUCED.uses_legacy_ascii_negotiation());
}

#[test]
fn binary_negotiation_threshold_exceeds_oldest_supported_version() {
    let threshold = ProtocolVersion::BINARY_NEGOTIATION_INTRODUCED.as_u8();
    let oldest = ProtocolVersion::OLDEST.as_u8();

    assert!(
        threshold > oldest,
        "binary negotiation threshold must be newer than the oldest supported protocol",
    );

    let preceding = ProtocolVersion::from_supported(threshold - 1)
        .expect("protocol immediately preceding the binary threshold is supported");
    assert!(preceding.uses_legacy_ascii_negotiation());
}

#[test]
fn negotiation_style_helpers_match_protocol_cutoff() {
    for version in ProtocolVersion::supported_versions() {
        if version.as_u8() >= ProtocolVersion::BINARY_NEGOTIATION_INTRODUCED.as_u8() {
            assert!(
                version.uses_binary_negotiation(),
                "version {} should be binary",
                version
            );
            assert!(
                !version.uses_legacy_ascii_negotiation(),
                "version {} should not be legacy",
                version
            );
        } else {
            assert!(
                !version.uses_binary_negotiation(),
                "version {} should not be binary",
                version
            );
            assert!(
                version.uses_legacy_ascii_negotiation(),
                "version {} should be legacy",
                version
            );
        }
    }
}
