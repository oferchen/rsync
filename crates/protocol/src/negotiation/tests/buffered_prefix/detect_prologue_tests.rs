#[test]
fn detect_negotiation_prologue_requires_data() {
    assert_eq!(
        detect_negotiation_prologue(b""),
        NegotiationPrologue::NeedMoreData
    );
}

#[test]
fn detect_negotiation_prologue_delegates_to_initial_byte_helper() {
    for byte in 0u8..=u8::MAX {
        assert_eq!(
            detect_negotiation_prologue(&[byte]),
            NegotiationPrologue::from_initial_byte(byte)
        );
    }
}

#[test]
fn detect_negotiation_prologue_classifies_partial_prefix_as_legacy() {
    assert_eq!(
        detect_negotiation_prologue(b"@RS"),
        NegotiationPrologue::LegacyAscii
    );
}

#[test]
fn detect_negotiation_prologue_flags_legacy_ascii() {
    assert_eq!(
        detect_negotiation_prologue(b"@RSYNCD: 31.0\n"),
        NegotiationPrologue::LegacyAscii
    );
}

#[test]
fn detect_negotiation_prologue_flags_malformed_ascii_as_legacy() {
    assert_eq!(
        detect_negotiation_prologue(b"@RSYNCX"),
        NegotiationPrologue::LegacyAscii
    );
}

#[test]
fn detect_negotiation_prologue_detects_binary() {
    assert_eq!(
        detect_negotiation_prologue(&[0x00, 0x20, 0x00, 0x00]),
        NegotiationPrologue::Binary
    );
}
