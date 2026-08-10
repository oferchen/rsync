#[test]
fn negotiation_prologue_as_str_matches_display() {
    let legacy = NegotiationPrologue::LegacyAscii;
    let binary = NegotiationPrologue::Binary;
    let undecided = NegotiationPrologue::NeedMoreData;

    assert_eq!(legacy.as_str(), "legacy-ascii");
    assert_eq!(binary.as_str(), "binary");
    assert_eq!(undecided.as_str(), "need-more-data");

    assert_eq!(legacy.to_string(), "legacy-ascii");
    assert_eq!(binary.to_string(), "binary");
    assert_eq!(undecided.to_string(), "need-more-data");
}

#[test]
fn negotiation_prologue_converts_into_static_str() {
    let legacy: &'static str = NegotiationPrologue::LegacyAscii.into();
    let binary: &'static str = NegotiationPrologue::Binary.into();
    let undecided: &'static str = NegotiationPrologue::NeedMoreData.into();

    assert_eq!(legacy, "legacy-ascii");
    assert_eq!(binary, "binary");
    assert_eq!(undecided, "need-more-data");

    let mut set = HashSet::new();
    set.insert(NegotiationPrologue::Binary);
    set.insert(NegotiationPrologue::LegacyAscii);
    set.insert(NegotiationPrologue::NeedMoreData);

    assert_eq!(set.len(), 3);
    assert!(set.contains(&NegotiationPrologue::Binary));
}

#[test]
fn negotiation_prologue_default_represents_undecided_state() {
    let default = NegotiationPrologue::default();

    assert_eq!(default, NegotiationPrologue::NeedMoreData);
    assert!(default.requires_more_data());
    assert!(!default.is_decided());
}

#[test]
fn negotiation_prologue_from_initial_byte_matches_binary_split() {
    assert_eq!(
        NegotiationPrologue::from_initial_byte(b'@'),
        NegotiationPrologue::LegacyAscii
    );

    for &byte in &[0x00u8, b'R', b'\n', 0xFF] {
        assert_eq!(
            NegotiationPrologue::from_initial_byte(byte),
            NegotiationPrologue::Binary
        );
    }
}

#[test]
fn negotiation_prologue_from_str_parses_known_identifiers() {
    assert_eq!(
        NegotiationPrologue::from_str("need-more-data").expect("known identifier"),
        NegotiationPrologue::NeedMoreData
    );
    assert_eq!(
        NegotiationPrologue::from_str("legacy-ascii").expect("known identifier"),
        NegotiationPrologue::LegacyAscii
    );
    assert_eq!(
        NegotiationPrologue::from_str("binary").expect("known identifier"),
        NegotiationPrologue::Binary
    );
}

#[test]
fn negotiation_prologue_from_str_trims_ascii_whitespace() {
    let parsed = NegotiationPrologue::from_str("  legacy-ascii  ").expect("whitespace tolerated");
    assert!(parsed.is_legacy());
}

#[test]
fn negotiation_prologue_from_str_rejects_unknown_identifiers() {
    let err = NegotiationPrologue::from_str("legacy").expect_err("unknown value must fail");
    assert_eq!(err.kind(), ParseNegotiationPrologueErrorKind::Invalid);
    assert_eq!(
        err.to_string(),
        "unrecognized negotiation prologue identifier (expected need-more-data, legacy-ascii, or binary)"
    );
}

#[test]
fn negotiation_prologue_from_str_rejects_empty_inputs() {
    let err = NegotiationPrologue::from_str("   ").expect_err("empty value must fail");
    assert_eq!(err.kind(), ParseNegotiationPrologueErrorKind::Empty);
    assert_eq!(err.to_string(), "negotiation prologue identifier is empty");
}

#[test]
fn negotiation_prologue_helpers_report_decision_state() {
    assert!(NegotiationPrologue::NeedMoreData.requires_more_data());
    assert!(!NegotiationPrologue::NeedMoreData.is_decided());

    assert!(NegotiationPrologue::LegacyAscii.is_decided());
    assert!(!NegotiationPrologue::LegacyAscii.requires_more_data());

    assert!(NegotiationPrologue::Binary.is_decided());
    assert!(!NegotiationPrologue::Binary.requires_more_data());
}

#[test]
fn negotiation_prologue_helpers_classify_modes() {
    assert!(NegotiationPrologue::LegacyAscii.is_legacy());
    assert!(!NegotiationPrologue::LegacyAscii.is_binary());

    assert!(NegotiationPrologue::Binary.is_binary());
    assert!(!NegotiationPrologue::Binary.is_legacy());

    assert!(!NegotiationPrologue::NeedMoreData.is_binary());
    assert!(!NegotiationPrologue::NeedMoreData.is_legacy());
}
