#[test]
fn negotiation_prologue_detector_default_matches_new() {
    let from_new = NegotiationPrologueDetector::new();
    let from_default = NegotiationPrologueDetector::default();

    assert_eq!(from_default.decision(), from_new.decision());
    assert_eq!(from_default.buffered_len(), from_new.buffered_len());
    assert_eq!(
        from_default.requires_more_data(),
        from_new.requires_more_data()
    );
    assert_eq!(from_default.is_decided(), from_new.is_decided());
    assert_eq!(from_default.is_legacy(), from_new.is_legacy());
    assert_eq!(from_default.is_binary(), from_new.is_binary());
    assert_eq!(
        from_default.legacy_prefix_complete(),
        from_new.legacy_prefix_complete()
    );
}

#[test]
fn prologue_detector_requires_data() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.observe(b""), NegotiationPrologue::NeedMoreData);
    assert_eq!(detector.observe(b"@"), NegotiationPrologue::LegacyAscii);
    assert_eq!(
        detector.observe(b"RSYNCD: 31.0\n"),
        NegotiationPrologue::LegacyAscii
    );
}

#[test]
fn prologue_detector_default_matches_initial_state() {
    let detector = NegotiationPrologueDetector::default();

    assert_eq!(detector.decision(), None);
    assert_eq!(detector.buffered_prefix(), b"");
    assert_eq!(detector.buffered_len(), 0);
    assert!(!detector.legacy_prefix_complete());
    assert_eq!(detector.legacy_prefix_remaining(), None);
}

#[test]
fn prologue_detector_reports_decision_state_helpers() {
    let mut detector = NegotiationPrologueDetector::new();

    assert!(!detector.is_decided());
    assert!(detector.requires_more_data());
    assert!(!detector.is_legacy());
    assert!(!detector.is_binary());

    assert_eq!(detector.observe(b"@"), NegotiationPrologue::LegacyAscii);
    assert!(detector.is_decided());
    assert!(detector.requires_more_data());
    assert!(detector.is_legacy());
    assert!(!detector.is_binary());

    let remainder = &LEGACY_DAEMON_PREFIX.as_bytes()[1..];
    assert_eq!(
        detector.observe(remainder),
        NegotiationPrologue::LegacyAscii
    );
    assert!(detector.is_decided());
    assert!(!detector.requires_more_data());
    assert!(detector.is_legacy());
    assert!(!detector.is_binary());

    detector.reset();
    assert!(!detector.is_decided());
    assert!(detector.requires_more_data());
    assert!(!detector.is_legacy());
    assert!(!detector.is_binary());

    assert_eq!(detector.observe(&[0x00]), NegotiationPrologue::Binary);
    assert!(detector.is_decided());
    assert!(!detector.requires_more_data());
    assert!(!detector.is_legacy());
    assert!(detector.is_binary());
}

#[test]
fn prologue_detector_detects_binary_immediately() {
    let mut detector = NegotiationPrologueDetector::default();
    assert_eq!(detector.observe(b"x"), NegotiationPrologue::Binary);
    assert_eq!(detector.observe(b"@"), NegotiationPrologue::Binary);
}

#[test]
fn prologue_detector_handles_prefix_mismatch() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(
        detector.observe(b"@RSYNCD"),
        NegotiationPrologue::LegacyAscii
    );
    assert_eq!(detector.observe(b"X"), NegotiationPrologue::LegacyAscii);
    assert_eq!(
        detector.observe(b"additional"),
        NegotiationPrologue::LegacyAscii
    );
}

#[test]
fn prologue_detector_handles_mismatch_at_last_prefix_byte() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(
        detector.observe(b"@RSYNCD;"),
        NegotiationPrologue::LegacyAscii
    );
    assert_eq!(detector.buffered_prefix(), b"@RSYNCD;");

    // Subsequent bytes keep replaying the cached decision without extending
    // the buffered prefix because the canonical marker has already been
    // ruled out by the mismatch in the final position.
    assert_eq!(
        detector.observe(b": more"),
        NegotiationPrologue::LegacyAscii
    );
    assert_eq!(detector.buffered_prefix(), b"@RSYNCD;");
}

#[test]
fn prologue_detector_handles_split_prefix_chunks() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.observe(b"@RS"), NegotiationPrologue::LegacyAscii);
    assert_eq!(detector.observe(b"YN"), NegotiationPrologue::LegacyAscii);
    assert_eq!(
        detector.observe(b"CD: 32"),
        NegotiationPrologue::LegacyAscii
    );
}

#[test]
fn prologue_detector_handles_empty_chunk_while_waiting_for_prefix_completion() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.observe(b"@"), NegotiationPrologue::LegacyAscii);
    assert_eq!(detector.buffered_prefix(), b"@");
    assert_eq!(
        detector.legacy_prefix_remaining(),
        Some(LEGACY_DAEMON_PREFIX_LEN - 1)
    );

    assert_eq!(detector.observe(b""), NegotiationPrologue::NeedMoreData);
    assert!(detector.is_legacy());
    assert_eq!(detector.buffered_prefix(), b"@");
    assert_eq!(
        detector.legacy_prefix_remaining(),
        Some(LEGACY_DAEMON_PREFIX_LEN - 1)
    );

    assert_eq!(
        detector.observe(b"RSYNCD:"),
        NegotiationPrologue::LegacyAscii
    );
    assert_eq!(detector.buffered_prefix(), b"@RSYNCD:");
    assert_eq!(detector.legacy_prefix_remaining(), None);
}

#[test]
fn prologue_detector_reports_buffered_length() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.buffered_len(), 0);

    assert_eq!(detector.observe(b"@RS"), NegotiationPrologue::LegacyAscii);
    assert_eq!(detector.buffered_len(), 3);
    assert_eq!(
        detector.legacy_prefix_remaining(),
        Some(LEGACY_DAEMON_PREFIX_LEN - 3)
    );

    assert_eq!(detector.observe(b"YNCD:"), NegotiationPrologue::LegacyAscii);
    assert_eq!(detector.buffered_len(), LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(detector.legacy_prefix_remaining(), None);

    assert_eq!(
        detector.observe(b" 31.0\n"),
        NegotiationPrologue::LegacyAscii
    );
    assert_eq!(detector.buffered_len(), LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(detector.legacy_prefix_remaining(), None);

    let mut binary = NegotiationPrologueDetector::new();
    assert_eq!(binary.observe(b"modern"), NegotiationPrologue::Binary);
    assert_eq!(binary.buffered_len(), 0);
    assert_eq!(binary.legacy_prefix_remaining(), None);
}
