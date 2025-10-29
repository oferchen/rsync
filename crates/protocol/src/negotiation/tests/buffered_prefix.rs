#[test]
fn buffered_prefix_too_small_converts_to_io_error_with_context() {
    let err = BufferedPrefixTooSmall::new(LEGACY_DAEMON_PREFIX_LEN, 4);
    let message = err.to_string();
    let required = err.required();
    let available = err.available();
    let missing = err.missing();

    let io_err: io::Error = err.into();

    assert_eq!(io_err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(io_err.to_string(), message);

    let source = io_err
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<BufferedPrefixTooSmall>())
        .expect("io::Error must retain BufferedPrefixTooSmall source");
    assert_eq!(source.required(), required);
    assert_eq!(source.available(), available);
    assert_eq!(source.missing(), missing);
}

#[test]
fn buffered_prefix_too_small_reports_missing_bytes() {
    let err = BufferedPrefixTooSmall::new(LEGACY_DAEMON_PREFIX_LEN, LEGACY_DAEMON_PREFIX_LEN - 3);
    assert_eq!(err.missing(), 3);

    let saturated = BufferedPrefixTooSmall::new(4, 8);
    assert_eq!(saturated.missing(), 0);
}

struct InterruptedOnceReader {
    inner: Cursor<Vec<u8>>,
    interrupted: bool,
}

impl InterruptedOnceReader {
    fn new(data: Vec<u8>) -> Self {
        Self {
            inner: Cursor::new(data),
            interrupted: false,
        }
    }

    fn was_interrupted(&self) -> bool {
        self.interrupted
    }

    fn into_inner(self) -> Cursor<Vec<u8>> {
        self.inner
    }
}

impl Read for InterruptedOnceReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.interrupted {
            self.interrupted = true;
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "simulated EINTR during negotiation sniff",
            ));
        }

        self.inner.read(buf)
    }
}

struct RecordingReader {
    inner: Cursor<Vec<u8>>,
    calls: Vec<usize>,
}

impl RecordingReader {
    fn new(data: Vec<u8>) -> Self {
        Self {
            inner: Cursor::new(data),
            calls: Vec::new(),
        }
    }

    fn calls(&self) -> &[usize] {
        &self.calls
    }
}

impl Read for RecordingReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !buf.is_empty() {
            self.calls.push(buf.len());
        }

        self.inner.read(buf)
    }
}

struct ChunkedReader {
    inner: Cursor<Vec<u8>>,
    chunk: usize,
}

impl ChunkedReader {
    fn new(data: Vec<u8>, chunk: usize) -> Self {
        assert!(chunk > 0, "chunk size must be non-zero to make progress");
        Self {
            inner: Cursor::new(data),
            chunk,
        }
    }

    fn into_inner(self) -> Cursor<Vec<u8>> {
        self.inner
    }
}

impl Read for ChunkedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let limit = buf.len().min(self.chunk);
        self.inner.read(&mut buf[..limit])
    }
}

#[test]
fn detect_negotiation_prologue_requires_data() {
    assert_eq!(
        detect_negotiation_prologue(b""),
        NegotiationPrologue::NeedMoreData
    );
}

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
fn prologue_sniffer_with_buffer_reuses_canonical_allocation() {
    let mut buffer = Vec::with_capacity(LEGACY_DAEMON_PREFIX_LEN);
    buffer.extend_from_slice(b"junk");
    let ptr = buffer.as_ptr();

    let sniffer = NegotiationPrologueSniffer::with_buffer(buffer);

    assert_eq!(sniffer.decision(), None, "fresh sniffer must be undecided");
    assert!(sniffer.buffered().is_empty(), "buffer must start empty");
    assert_eq!(
        sniffer.buffered_storage().capacity(),
        LEGACY_DAEMON_PREFIX_LEN,
        "capacity should match canonical prefix length"
    );
    assert!(
        ptr::eq(sniffer.buffered_storage().as_ptr(), ptr),
        "canonical capacity should be reused without reallocating"
    );
}

#[test]
fn prologue_sniffer_with_buffer_trims_oversized_allocations() {
    let buffer = vec![0u8; LEGACY_DAEMON_PREFIX_LEN * 4];

    let sniffer = NegotiationPrologueSniffer::with_buffer(buffer);

    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), None);
    assert_eq!(
        sniffer.buffered_storage().capacity(),
        LEGACY_DAEMON_PREFIX_LEN,
        "oversized buffers should shrink to canonical prefix length"
    );
}

#[test]
fn prologue_sniffer_with_buffer_grows_small_allocations() {
    let buffer = Vec::with_capacity(1);

    let sniffer = NegotiationPrologueSniffer::with_buffer(buffer);

    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), None);
    assert!(
        sniffer.buffered_storage().capacity() >= LEGACY_DAEMON_PREFIX_LEN,
        "small buffers must grow to hold the canonical prefix"
    );
}

#[test]
fn detect_negotiation_prologue_detects_binary() {
    assert_eq!(
        detect_negotiation_prologue(&[0x00, 0x20, 0x00, 0x00]),
        NegotiationPrologue::Binary
    );
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

    // Feeding an empty chunk while still collecting the canonical legacy
    // prefix signals that additional bytes are required without mutating the
    // buffered prefix. The cached decision remains available via accessors so
    // higher layers can keep treating the exchange as legacy while waiting for
    // more input.
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
