use super::sniffer::map_reserve_error_for_io;
use super::*;
use crate::NegotiationError;
use crate::legacy::{LEGACY_DAEMON_PREFIX, LEGACY_DAEMON_PREFIX_BYTES, LEGACY_DAEMON_PREFIX_LEN};
use proptest::prelude::*;
use std::{
    collections::TryReserveError,
    error::Error as _,
    io::{self, Cursor, Read, Write},
    ptr, slice,
    str::FromStr,
};

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

#[test]
fn prologue_detector_caches_decision() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.observe(b"@X"), NegotiationPrologue::LegacyAscii);
    assert_eq!(
        detector.observe(b"anything"),
        NegotiationPrologue::LegacyAscii
    );
    assert_eq!(detector.legacy_prefix_remaining(), None);
}

#[test]
fn prologue_detector_exposes_decision_state() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.decision(), None);
    assert_eq!(detector.observe(b""), NegotiationPrologue::NeedMoreData);
    assert_eq!(detector.decision(), None);

    assert_eq!(detector.observe(b"x"), NegotiationPrologue::Binary);
    assert_eq!(detector.decision(), Some(NegotiationPrologue::Binary));
}

#[test]
fn prologue_detector_exposes_legacy_decision_state() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.decision(), None);

    assert_eq!(detector.observe(b"@"), NegotiationPrologue::LegacyAscii);
    assert_eq!(detector.decision(), Some(NegotiationPrologue::LegacyAscii));

    // Additional observations keep reporting the cached decision, matching
    // upstream's handling once the legacy path has been chosen.
    assert_eq!(detector.observe(b"RSYN"), NegotiationPrologue::LegacyAscii);
    assert_eq!(detector.decision(), Some(NegotiationPrologue::LegacyAscii));
}

#[test]
fn legacy_prefix_completion_reports_state_before_decision() {
    let detector = NegotiationPrologueDetector::new();
    assert!(!detector.legacy_prefix_complete());
}

#[test]
fn legacy_prefix_completion_tracks_partial_prefix() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.observe(b"@"), NegotiationPrologue::LegacyAscii);
    assert!(!detector.legacy_prefix_complete());

    assert_eq!(detector.observe(b"RSYN"), NegotiationPrologue::LegacyAscii);
    assert!(!detector.legacy_prefix_complete());

    assert_eq!(detector.observe(b"CD:"), NegotiationPrologue::LegacyAscii);
    assert!(detector.legacy_prefix_complete());
}

#[test]
fn legacy_prefix_completion_handles_mismatch() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.observe(b"@X"), NegotiationPrologue::LegacyAscii);
    assert!(detector.legacy_prefix_complete());
}

#[test]
fn legacy_prefix_completion_stays_false_for_binary_detection() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.observe(&[0x00]), NegotiationPrologue::Binary);
    assert!(!detector.legacy_prefix_complete());
}

#[test]
fn legacy_prefix_completion_resets_with_detector() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(
        detector.observe(b"@RSYNCD:"),
        NegotiationPrologue::LegacyAscii
    );
    assert!(detector.legacy_prefix_complete());

    detector.reset();
    assert!(!detector.legacy_prefix_complete());
}

#[test]
fn observe_byte_after_reset_restarts_detection() {
    let mut detector = NegotiationPrologueDetector::new();

    for &byte in LEGACY_DAEMON_PREFIX.as_bytes() {
        assert_eq!(
            detector.observe_byte(byte),
            NegotiationPrologue::LegacyAscii
        );
    }
    assert!(detector.legacy_prefix_complete());

    detector.reset();

    assert_eq!(
        detector.observe_byte(b'@'),
        NegotiationPrologue::LegacyAscii
    );
    assert_eq!(detector.buffered_prefix(), b"@");
    assert_eq!(detector.buffered_len(), 1);
    assert_eq!(
        detector.legacy_prefix_remaining(),
        Some(LEGACY_DAEMON_PREFIX_LEN - 1)
    );
}

#[test]
fn legacy_prefix_remaining_reports_none_before_decision() {
    let detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.legacy_prefix_remaining(), None);
}

#[test]
fn legacy_prefix_remaining_tracks_mismatch_completion() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.observe(b"@RS"), NegotiationPrologue::LegacyAscii);
    assert_eq!(
        detector.legacy_prefix_remaining(),
        Some(LEGACY_DAEMON_PREFIX_LEN - 3)
    );

    // Diverging from the canonical marker completes the prefix handling
    // immediately, mirroring upstream's behavior. The helper should report
    // that no additional bytes are required once the mismatch has been
    // observed.
    assert_eq!(detector.observe(b"YNXD"), NegotiationPrologue::LegacyAscii);
    assert_eq!(detector.legacy_prefix_remaining(), None);
}

#[test]
fn legacy_prefix_remaining_counts_down_through_canonical_prefix() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.legacy_prefix_remaining(), None);

    for (idx, &byte) in LEGACY_DAEMON_PREFIX.as_bytes().iter().enumerate() {
        let observed = detector.observe_byte(byte);
        assert_eq!(observed, NegotiationPrologue::LegacyAscii);

        let expected_remaining = if idx + 1 < LEGACY_DAEMON_PREFIX_LEN {
            Some(LEGACY_DAEMON_PREFIX_LEN - idx - 1)
        } else {
            None
        };

        assert_eq!(detector.legacy_prefix_remaining(), expected_remaining);
        assert_eq!(detector.buffered_len(), idx + 1);
        assert_eq!(
            detector.buffered_prefix(),
            &LEGACY_DAEMON_PREFIX.as_bytes()[..idx + 1]
        );
    }

    assert!(detector.legacy_prefix_complete());
    assert_eq!(detector.buffered_prefix(), LEGACY_DAEMON_PREFIX.as_bytes());
}

#[test]
fn buffered_prefix_tracks_bytes_consumed_for_legacy_detection() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.buffered_prefix(), b"");

    assert_eq!(detector.observe(b"@RS"), NegotiationPrologue::LegacyAscii);
    assert_eq!(detector.buffered_prefix(), b"@RS");

    // Additional observations extend the buffered prefix until the full
    // legacy marker is buffered.
    assert_eq!(detector.observe(b"YNCD"), NegotiationPrologue::LegacyAscii);
    assert_eq!(detector.buffered_prefix(), b"@RSYNCD");

    // Feeding an empty chunk after the decision reports that additional input
    // is required while keeping the cached legacy classification available via
    // the detector's helpers.
    assert_eq!(detector.observe(b""), NegotiationPrologue::NeedMoreData);
    assert!(detector.is_legacy());
    assert_eq!(detector.buffered_prefix(), b"@RSYNCD");
}

#[test]
fn prologue_detector_copy_buffered_prefix_into_copies_bytes() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.observe(b"@RS"), NegotiationPrologue::LegacyAscii);

    let mut scratch = [0xAA; LEGACY_DAEMON_PREFIX_LEN];
    let copied = detector
        .copy_buffered_prefix_into(&mut scratch)
        .expect("scratch slice should capture buffered prefix");

    assert_eq!(copied, 3);
    assert_eq!(&scratch[..copied], b"@RS");
    assert!(scratch[copied..].iter().all(|&byte| byte == 0xAA));

    let mut binary = NegotiationPrologueDetector::new();
    assert_eq!(binary.observe(&[0x00]), NegotiationPrologue::Binary);

    let mut untouched = [0xCC; LEGACY_DAEMON_PREFIX_LEN];
    let copied = binary
        .copy_buffered_prefix_into(&mut untouched)
        .expect("binary detection should copy zero bytes");

    assert_eq!(copied, 0);
    assert!(untouched.iter().all(|&byte| byte == 0xCC));
}

#[test]
fn prologue_detector_copy_buffered_prefix_into_reports_small_buffer() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.observe(b"@RSYN"), NegotiationPrologue::LegacyAscii);

    let mut scratch = vec![0u8; detector.buffered_len() - 1];
    let err = detector
        .copy_buffered_prefix_into(scratch.as_mut_slice())
        .expect_err("insufficient slice should error");

    assert_eq!(err.required(), 5);
    assert_eq!(err.available(), scratch.len());
    assert_eq!(detector.buffered_prefix(), b"@RSYN");
}

#[test]
fn prologue_detector_copy_buffered_prefix_into_array_copies_bytes() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.observe(b"@RSYN"), NegotiationPrologue::LegacyAscii);

    let mut scratch = [0u8; LEGACY_DAEMON_PREFIX_LEN];
    let copied = detector
        .copy_buffered_prefix_into_array(&mut scratch)
        .expect("array should capture buffered prefix");

    assert_eq!(copied, 5);
    assert_eq!(&scratch[..copied], b"@RSYN");
    assert!(scratch[copied..].iter().all(|&byte| byte == 0));

    let mut binary = NegotiationPrologueDetector::new();
    assert_eq!(binary.observe(&[0x00]), NegotiationPrologue::Binary);

    let mut untouched = [0xCC; LEGACY_DAEMON_PREFIX_LEN];
    let copied = binary
        .copy_buffered_prefix_into_array(&mut untouched)
        .expect("binary detection should copy zero bytes");

    assert_eq!(copied, 0);
    assert!(untouched.iter().all(|&byte| byte == 0xCC));
}

#[test]
fn prologue_detector_copy_buffered_prefix_into_array_reports_small_buffer() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(
        detector.observe(b"@RSYNCD"),
        NegotiationPrologue::LegacyAscii
    );

    let mut scratch = [0u8; LEGACY_DAEMON_PREFIX_LEN - 2];
    let err = detector
        .copy_buffered_prefix_into_array(&mut scratch)
        .expect_err("array that is too small must error");

    assert_eq!(err.required(), detector.buffered_len());
    assert_eq!(err.available(), scratch.len());
    assert_eq!(detector.buffered_prefix(), b"@RSYNCD");
}

#[test]
fn buffered_prefix_captures_full_marker_when_present_in_single_chunk() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(
        detector.observe(b"@RSYNCD: 31.0\n"),
        NegotiationPrologue::LegacyAscii
    );
    assert_eq!(detector.buffered_prefix(), b"@RSYNCD:");
}

#[test]
fn buffered_prefix_is_empty_for_binary_detection() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.observe(&[0x00]), NegotiationPrologue::Binary);
    assert_eq!(detector.buffered_prefix(), b"");
}

#[test]
fn buffered_prefix_stops_growing_after_mismatch_with_long_chunk() {
    let mut detector = NegotiationPrologueDetector::new();

    // Feed a chunk that starts with the legacy marker but diverges on the
    // second byte. The detector should record the observed prefix up to
    // the mismatch and ignore the remainder of the chunk, mirroring
    // upstream's behavior of replaying the legacy decision without
    // extending the buffered slice past the canonical marker length.
    let mut chunk = Vec::new();
    chunk.push(b'@');
    chunk.extend_from_slice(&[b'X'; 32]);

    assert_eq!(detector.observe(&chunk), NegotiationPrologue::LegacyAscii,);
    assert_eq!(detector.buffered_prefix(), b"@X");
    assert_eq!(detector.buffered_prefix().len(), 2);

    // Additional bytes keep replaying the cached decision without mutating
    // the buffered prefix that was captured before the mismatch.
    assert_eq!(detector.observe(b"more"), NegotiationPrologue::LegacyAscii);
    assert_eq!(detector.buffered_prefix(), b"@X");
}

#[test]
fn prologue_detector_can_be_reset_for_reuse() {
    let mut detector = NegotiationPrologueDetector::new();
    assert_eq!(detector.observe(b"@RS"), NegotiationPrologue::LegacyAscii);
    assert_eq!(detector.buffered_prefix(), b"@RS");
    assert_eq!(detector.decision(), Some(NegotiationPrologue::LegacyAscii));

    detector.reset();
    assert_eq!(detector.decision(), None);
    assert_eq!(detector.buffered_prefix(), b"");
    assert_eq!(detector.buffered_len(), 0);
    assert_eq!(detector.legacy_prefix_remaining(), None);

    assert_eq!(detector.observe(&[0x00]), NegotiationPrologue::Binary);
    assert_eq!(detector.decision(), Some(NegotiationPrologue::Binary));
    assert_eq!(detector.legacy_prefix_remaining(), None);
}

fn assert_detector_matches_across_partitions(data: &[u8]) {
    let expected = detect_negotiation_prologue(data);

    for first_end in 0..=data.len() {
        for second_end in first_end..=data.len() {
            let mut detector = NegotiationPrologueDetector::new();
            let _ = detector.observe(&data[..first_end]);
            let _ = detector.observe(&data[first_end..second_end]);
            let result = detector.observe(&data[second_end..]);

            if expected == NegotiationPrologue::LegacyAscii
                && result == NegotiationPrologue::NeedMoreData
            {
                assert!(
                    detector.is_legacy(),
                    "detector should cache legacy decision for {:?}",
                    data
                );
                assert!(detector.requires_more_data());
            } else {
                assert_eq!(
                    result, expected,
                    "segmented detection mismatch for {:?} with splits ({}, {})",
                    data, first_end, second_end
                );
            }

            match expected {
                NegotiationPrologue::NeedMoreData => {
                    assert_eq!(detector.decision(), None);
                }
                NegotiationPrologue::LegacyAscii => {
                    assert!(detector.is_legacy());
                }
                decision => {
                    assert_eq!(detector.decision(), Some(decision));
                }
            }
        }
    }
}

#[test]
fn prologue_detector_matches_stateless_detection_across_partitions() {
    assert_detector_matches_across_partitions(b"");
    assert_detector_matches_across_partitions(b"@");
    assert_detector_matches_across_partitions(b"@RS");
    assert_detector_matches_across_partitions(b"@RSYNCD: 31.0\n");
    assert_detector_matches_across_partitions(b"@RSYNCX");
    assert_detector_matches_across_partitions(&[0x00, 0x20, 0x00, 0x00]);
    assert_detector_matches_across_partitions(b"modern");
}

#[test]
fn prologue_detector_observe_byte_matches_slice_behavior() {
    fn run_case(data: &[u8]) {
        let mut slice_detector = NegotiationPrologueDetector::new();
        let slice_result = slice_detector.observe(data);

        let mut byte_detector = NegotiationPrologueDetector::new();
        let byte_result = if data.is_empty() {
            byte_detector.observe(data)
        } else {
            let mut last = NegotiationPrologue::NeedMoreData;
            for &byte in data {
                last = byte_detector.observe_byte(byte);
            }
            last
        };

        assert_eq!(
            byte_result, slice_result,
            "decision mismatch for {:?}",
            data
        );
        assert_eq!(
            byte_detector.decision(),
            slice_detector.decision(),
            "cached decision mismatch for {:?}",
            data
        );
        assert_eq!(
            byte_detector.legacy_prefix_complete(),
            slice_detector.legacy_prefix_complete(),
            "prefix completion mismatch for {:?}",
            data
        );
        assert_eq!(
            byte_detector.buffered_prefix(),
            slice_detector.buffered_prefix(),
            "buffered prefix mismatch for {:?}",
            data
        );
    }

    run_case(b"");
    run_case(b"@");
    run_case(b"@RS");
    run_case(b"@RSYNCD:");
    run_case(b"@RSYNCD: 31.0\n");
    run_case(b"@RSYNCX");
    run_case(b"modern");
    run_case(&[0x00, 0x20, 0x00, 0x00]);
}

proptest! {
    #[test]
    fn prologue_detector_matches_stateless_detection_for_random_chunks(
        chunks in prop::collection::vec(
            prop::collection::vec(any::<u8>(), 0..=LEGACY_DAEMON_PREFIX_LEN + 2),
            0..=4
        )
    ) {
        let concatenated: Vec<u8> = chunks.iter().flatten().copied().collect();
        let expected = detect_negotiation_prologue(&concatenated);

        let mut detector = NegotiationPrologueDetector::new();
        let mut last = NegotiationPrologue::NeedMoreData;

        for chunk in &chunks {
            last = detector.observe(chunk);
        }

        prop_assert_eq!(last, expected);

        match expected {
            NegotiationPrologue::NeedMoreData => {
                prop_assert_eq!(detector.decision(), None);
            }
            decision => {
                prop_assert_eq!(detector.decision(), Some(decision));
            }
        }

        let buffered = detector.buffered_prefix();
        prop_assert_eq!(buffered.len(), detector.buffered_len());

        match detector.decision() {
            Some(NegotiationPrologue::LegacyAscii) => {
                if let Some(remaining) = detector.legacy_prefix_remaining() {
                    prop_assert!(remaining > 0);
                    prop_assert!(!detector.legacy_prefix_complete());
                } else {
                    prop_assert!(detector.legacy_prefix_complete());
                }
            }
            _ => {
                prop_assert_eq!(detector.legacy_prefix_remaining(), None);
                prop_assert!(!detector.legacy_prefix_complete());
                prop_assert!(buffered.is_empty());
            }
        }
    }
}

proptest! {
    #[test]
    fn prologue_sniffer_stays_in_lockstep_with_detector(
        chunks in prop::collection::vec(
            prop::collection::vec(any::<u8>(), 0..=LEGACY_DAEMON_PREFIX_LEN + 2),
            0..=6
        )
    ) {
        let mut detector = NegotiationPrologueDetector::new();
        let mut sniffer = NegotiationPrologueSniffer::new();

        for chunk in &chunks {
            let (sniffer_decision, consumed) = sniffer
                .observe(chunk)
                .expect("buffer reservation succeeds");
            prop_assert!(consumed <= chunk.len());

            let detector_decision = if consumed != 0 {
                detector.observe(&chunk[..consumed])
            } else {
                detector
                    .decision()
                    .unwrap_or(NegotiationPrologue::NeedMoreData)
            };

            if sniffer_decision == NegotiationPrologue::NeedMoreData
                && detector_decision == NegotiationPrologue::LegacyAscii
                && !detector.legacy_prefix_complete()
            {
                // The sniffer intentionally reports `NeedMoreData` while it finishes buffering
                // the canonical legacy prefix even though the detector has already classified
                // the exchange as legacy ASCII. Higher layers rely on
                // `legacy_prefix_remaining`/`legacy_prefix_complete` to decide when to replay
                // the buffered bytes, so the differing intermediate decision is expected.
            } else {
                prop_assert_eq!(sniffer_decision, detector_decision);
            }

            let detector_buffer = detector.buffered_prefix();
            prop_assert!(sniffer.buffered().starts_with(detector_buffer));
            prop_assert!(sniffer.buffered_len() >= detector.buffered_len());

            match sniffer_decision {
                NegotiationPrologue::LegacyAscii => {
                    if detector.legacy_prefix_complete() {
                        prop_assert!(sniffer.legacy_prefix_complete());
                        prop_assert_eq!(sniffer.legacy_prefix_remaining(), None);
                        prop_assert_eq!(detector.legacy_prefix_remaining(), None);
                    } else {
                        prop_assert!(!sniffer.legacy_prefix_complete());
                        prop_assert_eq!(
                            sniffer.legacy_prefix_remaining(),
                            detector.legacy_prefix_remaining()
                        );
                    }
                }
                NegotiationPrologue::Binary => {
                    prop_assert!(!sniffer.legacy_prefix_complete());
                    prop_assert!(!detector.legacy_prefix_complete());
                    prop_assert_eq!(sniffer.legacy_prefix_remaining(), None);
                    prop_assert_eq!(detector.legacy_prefix_remaining(), None);
                }
                NegotiationPrologue::NeedMoreData => {
                    prop_assert_eq!(
                        sniffer.legacy_prefix_complete(),
                        detector.legacy_prefix_complete()
                    );
                    prop_assert_eq!(
                        sniffer.legacy_prefix_remaining(),
                        detector.legacy_prefix_remaining()
                    );
                }
            }
        }

        prop_assert_eq!(sniffer.decision(), detector.decision());
    }
}

#[test]
fn prologue_sniffer_reports_binary_negotiation() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut cursor = Cursor::new(vec![0x00, 0x20, 0x00]);

    let decision = sniffer
        .read_from(&mut cursor)
        .expect("binary negotiation should succeed");
    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(sniffer.buffered(), &[0x00]);

    // Subsequent calls reuse the cached decision and avoid additional I/O.
    let decision = sniffer
        .read_from(&mut cursor)
        .expect("cached decision should be returned");
    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(cursor.position(), 1);
}

#[test]
fn prologue_sniffer_reports_binary_and_legacy_flags() {
    let undecided = NegotiationPrologueSniffer::new();
    assert!(!undecided.is_binary());
    assert!(!undecided.is_legacy());
    assert!(!undecided.is_decided());
    assert!(undecided.requires_more_data());

    let mut binary = NegotiationPrologueSniffer::new();
    let (decision, consumed) = binary
        .observe(&[0x00, 0x10, 0x20])
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(consumed, 1);
    assert!(binary.is_binary());
    assert!(!binary.is_legacy());
    assert!(binary.is_decided());
    assert!(!binary.requires_more_data());

    let mut partial_legacy = NegotiationPrologueSniffer::new();
    let (decision, consumed) = partial_legacy
        .observe(b"@R")
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert!(partial_legacy.is_legacy());
    assert!(!partial_legacy.is_binary());
    assert!(partial_legacy.is_decided());
    assert!(partial_legacy.requires_more_data());
    assert_eq!(consumed, 2);

    let mut legacy = NegotiationPrologueSniffer::new();
    let (decision, consumed) = legacy
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert!(legacy.is_legacy());
    assert!(!legacy.is_binary());
    assert!(legacy.is_decided());
    assert!(!legacy.requires_more_data());
}

#[test]
fn prologue_sniffer_read_from_handles_single_byte_chunks() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = ChunkedReader::new(b"@RSYNCD: 31.0\nrest".to_vec(), 1);

    let decision = sniffer
        .read_from(&mut reader)
        .expect("sniffer should tolerate single-byte reads");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert!(sniffer.legacy_prefix_complete());

    let mut greeting = Vec::new();
    read_legacy_daemon_line(&mut sniffer, &mut reader, &mut greeting)
        .expect("complete legacy greeting should be collected");
    assert_eq!(greeting, b"@RSYNCD: 31.0\n");

    let mut cursor = reader.into_inner();
    let mut remainder = Vec::new();
    cursor.read_to_end(&mut remainder).expect("read remainder");
    assert_eq!(remainder, b"rest");
}

#[test]
fn prologue_sniffer_sniffed_prefix_len_tracks_binary_reads() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut cursor = Cursor::new(vec![0x42, 0x10, 0x20]);

    let decision = sniffer
        .read_from(&mut cursor)
        .expect("binary negotiation should succeed");

    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(sniffer.sniffed_prefix_len(), 1);
    assert_eq!(sniffer.buffered_len(), 1);
    assert_eq!(cursor.position(), 1);

    let mut remainder = Vec::new();
    cursor
        .read_to_end(&mut remainder)
        .expect("remaining payload should stay unread");
    assert_eq!(remainder, vec![0x10, 0x20]);
}

#[test]
fn prologue_sniffer_sniffed_prefix_returns_canonical_slice() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX_BYTES)
        .expect("buffer reservation succeeds");

    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(sniffer.sniffed_prefix_len(), LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(sniffer.sniffed_prefix(), LEGACY_DAEMON_PREFIX_BYTES);

    sniffer
        .buffered_storage_mut()
        .extend_from_slice(b" trailing payload");

    assert_eq!(sniffer.sniffed_prefix(), LEGACY_DAEMON_PREFIX_BYTES);
    assert_eq!(sniffer.sniffed_prefix_len(), LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(sniffer.buffered_remainder(), b" trailing payload");
}

#[test]
fn prologue_sniffer_sniffed_prefix_handles_binary_negotiation() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let payload = [0x7Fu8, 0x10, 0x20];
    let (decision, consumed) = sniffer
        .observe(payload.as_slice())
        .expect("buffer reservation succeeds");

    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(consumed, 1);
    assert_eq!(sniffer.sniffed_prefix(), &payload[..1]);
    assert_eq!(sniffer.sniffed_prefix_len(), 1);
    assert!(sniffer.buffered_remainder().is_empty());

    sniffer
        .buffered_storage_mut()
        .extend_from_slice(&payload[1..]);

    assert_eq!(sniffer.sniffed_prefix(), &payload[..1]);
    assert_eq!(sniffer.sniffed_prefix_len(), 1);
    assert_eq!(sniffer.buffered_remainder(), &payload[1..]);
}

#[test]
fn prologue_sniffer_buffered_split_exposes_pending_prefix() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(b"@RS")
        .expect("buffer reservation succeeds");

    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert_eq!(consumed, 3);

    let (prefix, remainder) = sniffer.buffered_split();
    assert_eq!(prefix, b"@RS");
    assert!(remainder.is_empty());
}

#[test]
fn prologue_sniffer_buffered_split_returns_prefix_and_remainder_for_legacy() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(b"@RSYNCD:legacy tail")
        .expect("buffer reservation succeeds");

    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());

    let trailing = b"legacy tail";
    sniffer.buffered_storage_mut().extend_from_slice(trailing);

    let (prefix, remainder) = sniffer.buffered_split();
    assert_eq!(prefix, LEGACY_DAEMON_PREFIX.as_bytes());
    assert_eq!(remainder, trailing);
}

#[test]
fn prologue_sniffer_buffered_split_returns_prefix_and_remainder_for_binary() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let payload = [0x00, 0x51, 0x72, 0x81];
    let (decision, consumed) = sniffer
        .observe(payload.as_slice())
        .expect("buffer reservation succeeds");

    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(consumed, 1);

    sniffer
        .buffered_storage_mut()
        .extend_from_slice(&payload[consumed..]);

    let (prefix, remainder) = sniffer.buffered_split();
    assert_eq!(prefix, &payload[..1]);
    assert_eq!(remainder, &payload[1..]);
}

#[test]
fn prologue_sniffer_sniffed_prefix_exposes_partial_legacy_bytes() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer.observe(b"@R").expect("buffer reservation succeeds");

    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert_eq!(consumed, 2);
    assert_eq!(sniffer.sniffed_prefix(), b"@R");
    assert_eq!(sniffer.sniffed_prefix_len(), 2);
}

#[test]
fn prologue_sniffer_buffered_remainder_survives_draining() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX_BYTES)
        .expect("buffer reservation succeeds");

    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);

    let mut drained = Vec::new();
    let drained_len = sniffer
        .take_buffered_into(&mut drained)
        .expect("draining buffered prefix succeeds");

    assert_eq!(drained_len, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(drained, LEGACY_DAEMON_PREFIX_BYTES);
    assert_eq!(sniffer.sniffed_prefix_len(), 0);
    assert!(sniffer.sniffed_prefix().is_empty());
    assert!(sniffer.buffered_remainder().is_empty());

    sniffer
        .buffered_storage_mut()
        .extend_from_slice(b"residual payload");

    assert_eq!(sniffer.sniffed_prefix_len(), 0);
    assert!(sniffer.sniffed_prefix().is_empty());
    assert_eq!(sniffer.buffered_remainder(), b"residual payload");
}

#[test]
fn prologue_sniffer_discard_sniffed_prefix_preserves_remainder_for_legacy_negotiations() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX_BYTES)
        .expect("buffer reservation succeeds");

    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(sniffer.sniffed_prefix(), LEGACY_DAEMON_PREFIX_BYTES);

    let expected_remainder = b" trailing payload";
    sniffer
        .buffered_storage_mut()
        .extend_from_slice(expected_remainder);

    let dropped = sniffer.discard_sniffed_prefix();

    assert_eq!(dropped, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(sniffer.sniffed_prefix_len(), 0);
    assert!(sniffer.sniffed_prefix().is_empty());
    assert_eq!(sniffer.buffered(), expected_remainder);
    assert_eq!(sniffer.buffered_remainder(), expected_remainder);
    assert_eq!(sniffer.buffered_len(), expected_remainder.len());
}

#[test]
fn prologue_sniffer_discard_sniffed_prefix_releases_binary_prefix_byte() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let payload = [0x00, 0x05, 0x08, 0x09];
    let (decision, consumed) = sniffer
        .observe(payload.as_slice())
        .expect("buffer reservation succeeds");

    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(consumed, 1);
    assert!(sniffer.is_binary());

    let expected_remainder = &payload[consumed..];
    sniffer
        .buffered_storage_mut()
        .extend_from_slice(expected_remainder);

    let dropped = sniffer.discard_sniffed_prefix();

    assert_eq!(dropped, 1);
    assert_eq!(sniffer.sniffed_prefix_len(), 0);
    assert!(sniffer.sniffed_prefix().is_empty());
    assert_eq!(sniffer.buffered(), expected_remainder);
    assert_eq!(sniffer.buffered_remainder(), expected_remainder);
    assert_eq!(sniffer.buffered_len(), expected_remainder.len());
}

#[test]
fn prologue_sniffer_preallocates_legacy_prefix_capacity() {
    let buffered = NegotiationPrologueSniffer::new().into_buffered();
    assert_eq!(buffered.capacity(), LEGACY_DAEMON_PREFIX_LEN);
    assert!(buffered.is_empty());
}

#[test]
fn prologue_sniffer_into_buffered_trims_oversized_capacity() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    *sniffer.buffered_storage_mut() = Vec::with_capacity(256);
    sniffer
        .buffered_storage_mut()
        .extend_from_slice(LEGACY_DAEMON_PREFIX.as_bytes());

    assert!(sniffer.buffered_storage().capacity() > LEGACY_DAEMON_PREFIX_LEN);

    let replay = sniffer.into_buffered();

    assert_eq!(replay, LEGACY_DAEMON_PREFIX.as_bytes());
    assert_eq!(replay.capacity(), LEGACY_DAEMON_PREFIX_LEN);
}

#[test]
fn prologue_sniffer_reports_legacy_negotiation() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut cursor = Cursor::new(b"@RSYNCD: 31.0\n".to_vec());

    let decision = sniffer
        .read_from(&mut cursor)
        .expect("legacy negotiation should succeed");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert_eq!(sniffer.buffered_len(), LEGACY_DAEMON_PREFIX_LEN);

    assert!(sniffer.legacy_prefix_complete());
    assert_eq!(sniffer.legacy_prefix_remaining(), None);

    let mut remaining = Vec::new();
    cursor.read_to_end(&mut remaining).expect("read remainder");
    let mut replay = sniffer.into_buffered();
    replay.extend_from_slice(&remaining);
    assert_eq!(replay, b"@RSYNCD: 31.0\n");
}

#[test]
fn prologue_sniffer_reports_buffered_length() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    assert_eq!(sniffer.buffered_len(), 0);

    let (decision, consumed) = sniffer
        .observe(b"@RS")
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert_eq!(consumed, 3);
    assert_eq!(sniffer.buffered_len(), 3);

    let (decision, consumed) = sniffer.observe(b"YN").expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert_eq!(consumed, 2);
    assert_eq!(sniffer.buffered_len(), 5);

    let buffered = sniffer.take_buffered();
    assert_eq!(buffered, b"@RSYN");
    assert_eq!(sniffer.buffered_len(), 0);
    assert_eq!(sniffer.buffered(), b"");
}

#[test]
fn prologue_sniffer_observe_consumes_only_required_bytes() {
    let mut sniffer = NegotiationPrologueSniffer::new();

    let (decision, consumed) = sniffer
        .observe(b"@RS")
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert_eq!(consumed, 3);
    assert_eq!(sniffer.buffered(), b"@RS");
    assert_eq!(
        sniffer.legacy_prefix_remaining(),
        Some(LEGACY_DAEMON_PREFIX_LEN - 3)
    );

    let (decision, consumed) = sniffer
        .observe(b"YNCD: remainder")
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN - 3);
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert!(sniffer.legacy_prefix_complete());
    assert_eq!(sniffer.legacy_prefix_remaining(), None);

    let (decision, consumed) = sniffer
        .observe(b" trailing data")
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, 0);
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
}

#[test]
fn prologue_sniffer_observe_handles_prefix_mismatches() {
    let mut sniffer = NegotiationPrologueSniffer::new();

    let (decision, consumed) = sniffer
        .observe(b"@X remainder")
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, 2);
    assert_eq!(sniffer.buffered(), b"@X");
    assert!(sniffer.legacy_prefix_complete());
    assert_eq!(sniffer.legacy_prefix_remaining(), None);

    let (decision, consumed) = sniffer
        .observe(b"anything else")
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, 0);
    assert_eq!(sniffer.buffered(), b"@X");

    let replay = sniffer.into_buffered();
    assert_eq!(replay, b"@X");
}

#[test]
fn prologue_sniffer_observe_byte_matches_slice_behavior() {
    let mut slice_sniffer = NegotiationPrologueSniffer::new();
    let mut byte_sniffer = NegotiationPrologueSniffer::new();

    let stream = b"@RSYNCD: 31.0\n";

    for &byte in stream {
        let (expected, consumed) = slice_sniffer
            .observe(slice::from_ref(&byte))
            .expect("buffer reservation succeeds");
        assert!(consumed <= 1);
        let observed = byte_sniffer
            .observe_byte(byte)
            .expect("buffer reservation succeeds");
        assert_eq!(observed, expected);
        assert_eq!(byte_sniffer.buffered(), slice_sniffer.buffered());
        assert_eq!(
            byte_sniffer.legacy_prefix_remaining(),
            slice_sniffer.legacy_prefix_remaining()
        );
    }

    assert_eq!(byte_sniffer.decision(), slice_sniffer.decision());
    assert_eq!(
        byte_sniffer.legacy_prefix_complete(),
        slice_sniffer.legacy_prefix_complete()
    );
}

#[test]
fn prologue_sniffer_observe_returns_need_more_data_for_empty_chunk() {
    let mut sniffer = NegotiationPrologueSniffer::new();

    let (decision, consumed) = sniffer.observe(b"").expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert_eq!(consumed, 0);
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), None);

    let (decision, consumed) = sniffer.observe(b"").expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert_eq!(consumed, 0);
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), None);
}

#[test]
fn prologue_sniffer_observe_empty_chunk_after_partial_legacy_prefix() {
    let mut sniffer = NegotiationPrologueSniffer::new();

    let (decision, consumed) = sniffer.observe(b"@").expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert_eq!(consumed, 1);
    assert!(sniffer.requires_more_data());
    assert_eq!(sniffer.buffered(), b"@");
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));

    let (decision, consumed) = sniffer.observe(b"").expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert_eq!(consumed, 0);
    assert_eq!(sniffer.buffered(), b"@");
    assert!(sniffer.requires_more_data());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
}

#[test]
fn prologue_sniffer_observe_empty_chunk_after_complete_legacy_prefix() {
    let mut sniffer = NegotiationPrologueSniffer::new();

    let (decision, consumed) = sniffer.observe(b"@").expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert_eq!(consumed, 1);

    let (decision, consumed) = sniffer
        .observe(b"RSYNCD:")
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN - 1);
    assert!(sniffer.legacy_prefix_complete());
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX_BYTES);

    let (decision, consumed) = sniffer.observe(b"").expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, 0);
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX_BYTES);
    assert!(sniffer.legacy_prefix_complete());
}

#[test]
fn prologue_sniffer_observe_handles_binary_detection() {
    let mut sniffer = NegotiationPrologueSniffer::new();

    let (decision, consumed) = sniffer
        .observe(&[0x42, 0x99, 0x00])
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(consumed, 1);
    assert_eq!(sniffer.buffered(), &[0x42]);

    let (decision, consumed) = sniffer
        .observe(&[0x00])
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(consumed, 0);
    assert_eq!(sniffer.buffered(), &[0x42]);
}

#[test]
fn prologue_sniffer_reads_until_canonical_prefix_is_buffered() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut cursor = Cursor::new(b"@RSYNCD: 31.0\n".to_vec());

    let decision = sniffer
        .read_from(&mut cursor)
        .expect("first byte should classify legacy negotiation");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert!(sniffer.legacy_prefix_complete());
    assert_eq!(sniffer.legacy_prefix_remaining(), None);

    let position_after_prefix = cursor.position();

    let decision = sniffer
        .read_from(&mut cursor)
        .expect("cached decision should avoid extra reads once prefix buffered");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(cursor.position(), position_after_prefix);

    let mut remaining = Vec::new();
    cursor.read_to_end(&mut remaining).expect("read remainder");
    assert_eq!(remaining, b" 31.0\n");
}

#[test]
fn prologue_sniffer_limits_legacy_reads_to_required_bytes() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = RecordingReader::new(b"@RSYNCD: 31.0\n".to_vec());

    let decision = sniffer
        .read_from(&mut reader)
        .expect("legacy negotiation should succeed");

    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(
        reader.calls(),
        &[1, LEGACY_DAEMON_PREFIX_LEN - 1],
        "sniffer should request the first byte and then the remaining prefix",
    );
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert!(sniffer.legacy_prefix_complete());
}

#[test]
fn prologue_sniffer_read_from_preserves_bytes_after_malformed_prefix() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let malformed_banner = b"@XFAIL!\n".to_vec();
    let mut cursor = Cursor::new(malformed_banner.clone());

    let decision = sniffer
        .read_from(&mut cursor)
        .expect("malformed legacy negotiation should still classify as legacy");

    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert!(sniffer.legacy_prefix_complete());
    assert_eq!(sniffer.legacy_prefix_remaining(), None);
    assert_eq!(sniffer.buffered(), malformed_banner.as_slice());
    assert_eq!(sniffer.sniffed_prefix_len(), 2);
    assert_eq!(sniffer.buffered_len(), malformed_banner.len());
    assert_eq!(cursor.position(), malformed_banner.len() as u64);

    let mut replay = Vec::new();
    sniffer
        .take_buffered_into(&mut replay)
        .expect("replaying malformed prefix should succeed");
    assert_eq!(replay, malformed_banner);
}

#[test]
fn prologue_sniffer_take_buffered_drains_accumulated_prefix() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.legacy_prefix_complete());

    let buffered = sniffer.take_buffered();
    assert_eq!(buffered, LEGACY_DAEMON_PREFIX.as_bytes());
    assert!(buffered.capacity() <= LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
    assert_eq!(sniffer.legacy_prefix_remaining(), None);

    sniffer.reset();
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), None);
}

#[test]
fn prologue_sniffer_take_buffered_into_drains_accumulated_prefix() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.legacy_prefix_complete());

    let mut reused = b"placeholder".to_vec();
    let drained = sniffer
        .take_buffered_into(&mut reused)
        .expect("should copy buffered prefix");

    assert_eq!(reused, LEGACY_DAEMON_PREFIX.as_bytes());
    assert_eq!(drained, LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
    assert_eq!(sniffer.legacy_prefix_remaining(), None);

    sniffer.reset();
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), None);
}

#[test]
fn prologue_sniffer_take_buffered_into_includes_remainder_bytes() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.legacy_prefix_complete());

    let remainder = b" trailing payload";
    sniffer.buffered_storage_mut().extend_from_slice(remainder);

    let mut reused = Vec::new();
    let drained = sniffer
        .take_buffered_into(&mut reused)
        .expect("buffer transfer should succeed");

    let mut expected = LEGACY_DAEMON_PREFIX.as_bytes().to_vec();
    expected.extend_from_slice(remainder);
    assert_eq!(reused, expected);
    assert_eq!(drained, expected.len());
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
}

#[test]
fn prologue_sniffer_take_buffered_into_slice_copies_prefix() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.legacy_prefix_complete());

    let mut scratch = [0u8; LEGACY_DAEMON_PREFIX_LEN];
    let copied = sniffer
        .take_buffered_into_slice(&mut scratch)
        .expect("slice should fit negotiation prefix");

    assert_eq!(copied, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(&scratch[..copied], LEGACY_DAEMON_PREFIX.as_bytes());
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
    assert_eq!(sniffer.legacy_prefix_remaining(), None);
}

#[test]
fn prologue_sniffer_take_buffered_into_array_copies_prefix() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.legacy_prefix_complete());

    let mut scratch = [0u8; LEGACY_DAEMON_PREFIX_LEN];
    let copied = sniffer
        .take_buffered_into_array(&mut scratch)
        .expect("array should fit negotiation prefix");

    assert_eq!(copied, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(&scratch[..copied], LEGACY_DAEMON_PREFIX.as_bytes());
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
    assert_eq!(sniffer.legacy_prefix_remaining(), None);
}

#[test]
fn prologue_sniffer_take_buffered_into_array_reports_small_buffer() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.legacy_prefix_complete());

    let mut scratch = [0u8; LEGACY_DAEMON_PREFIX_LEN - 1];
    let err = sniffer
        .take_buffered_into_array(&mut scratch)
        .expect_err("array without enough capacity should error");

    assert_eq!(err.required(), LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(err.available(), scratch.len());
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
    assert_eq!(sniffer.legacy_prefix_remaining(), None);
}

#[test]
fn prologue_sniffer_take_buffered_into_writer_copies_prefix() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(b"@RSYNCD: 31.0\n".to_vec());
    let decision = sniffer
        .read_from(&mut reader)
        .expect("legacy negotiation detection succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);

    let mut sink = Vec::new();
    let written = sniffer
        .take_buffered_into_writer(&mut sink)
        .expect("writing buffered prefix succeeds");
    assert_eq!(written, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(sink, LEGACY_DAEMON_PREFIX.as_bytes());
    assert!(sniffer.buffered().is_empty());
}

#[test]
fn prologue_sniffer_take_buffered_into_writer_allows_empty_buffers() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut sink = Vec::new();

    let written = sniffer
        .take_buffered_into_writer(&mut sink)
        .expect("writing empty buffer succeeds");
    assert_eq!(written, 0);
    assert!(sink.is_empty());
}

#[test]
fn prologue_sniffer_take_buffered_into_writer_returns_initial_binary_byte() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(vec![0x42, 0x00, 0x00, 0x00]);
    let decision = sniffer
        .read_from(&mut reader)
        .expect("binary negotiation detection succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);

    let mut sink = Vec::new();
    let written = sniffer
        .take_buffered_into_writer(&mut sink)
        .expect("writing buffered binary byte succeeds");
    assert_eq!(written, 1);
    assert_eq!(sink, [0x42]);
    assert!(sniffer.buffered().is_empty());
}

#[test]
fn prologue_sniffer_take_buffered_into_writer_includes_remainder_bytes() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.legacy_prefix_complete());

    let remainder = b" module list";
    sniffer.buffered_storage_mut().extend_from_slice(remainder);

    let mut sink = Vec::new();
    let written = sniffer
        .take_buffered_into_writer(&mut sink)
        .expect("writer should receive buffered bytes");

    let mut expected = LEGACY_DAEMON_PREFIX.as_bytes().to_vec();
    expected.extend_from_slice(remainder);
    assert_eq!(sink, expected);
    assert_eq!(written, expected.len());
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
}

struct FailingWriter {
    error: io::Error,
}

impl FailingWriter {
    fn new() -> Self {
        Self {
            error: io::Error::other("simulated write failure"),
        }
    }
}

impl Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other(self.error.to_string()))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn prologue_sniffer_take_buffered_into_writer_preserves_buffer_on_error() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(b"@RSYNCD: 29.0\n".to_vec());
    sniffer
        .read_from(&mut reader)
        .expect("legacy negotiation detection succeeds");

    let mut failing = FailingWriter::new();
    let err = sniffer
        .take_buffered_into_writer(&mut failing)
        .expect_err("writer failure should be propagated");
    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_preserves_remainder() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let payload = LEGACY_DAEMON_PREFIX.as_bytes();
    let (decision, consumed) = sniffer
        .observe(payload)
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);

    let remainder = b"module data";
    sniffer.buffered_storage_mut().extend_from_slice(remainder);

    let mut prefix = Vec::new();
    let drained = sniffer
        .take_sniffed_prefix_into(&mut prefix)
        .expect("draining prefix should not allocate");
    assert_eq!(drained, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(prefix, payload);
    assert_eq!(sniffer.sniffed_prefix_len(), 0);
    assert_eq!(sniffer.buffered(), remainder);
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
    assert!(!sniffer.requires_more_data());
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_slice_preserves_remainder() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let payload = LEGACY_DAEMON_PREFIX_BYTES;
    let (decision, consumed) = sniffer
        .observe(payload.as_slice())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);

    let remainder = b"module list";
    sniffer.buffered_storage_mut().extend_from_slice(remainder);

    let mut scratch = [0u8; LEGACY_DAEMON_PREFIX_LEN];
    let copied = sniffer
        .take_sniffed_prefix_into_slice(&mut scratch)
        .expect("copying sniffed prefix into slice succeeds");
    assert_eq!(copied, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(&scratch[..copied], &payload[..]);
    assert_eq!(sniffer.sniffed_prefix_len(), 0);
    assert_eq!(sniffer.buffered(), remainder);
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
    assert!(!sniffer.requires_more_data());
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_writer_preserves_remainder() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(LEGACY_DAEMON_PREFIX_BYTES.to_vec());
    let decision = sniffer
        .read_from(&mut reader)
        .expect("legacy negotiation detection succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);

    let tail = b"payload";
    sniffer.buffered_storage_mut().extend_from_slice(tail);

    let mut sink = Vec::new();
    let written = sniffer
        .take_sniffed_prefix_into_writer(&mut sink)
        .expect("writing sniffed prefix succeeds");
    assert_eq!(written, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(sink.as_slice(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert_eq!(sniffer.sniffed_prefix_len(), 0);
    assert_eq!(sniffer.buffered(), tail);
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_writer_handles_binary_negotiation() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(&[0x26, 0x01, 0x02])
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(consumed, 1);

    sniffer
        .buffered_storage_mut()
        .extend_from_slice(&[0x01, 0x02]);

    let mut sink = Vec::new();
    let written = sniffer
        .take_sniffed_prefix_into_writer(&mut sink)
        .expect("writing sniffed binary prefix succeeds");
    assert_eq!(written, 1);
    assert_eq!(sink, vec![0x26]);
    assert!(sniffer.sniffed_prefix().is_empty());
    assert_eq!(sniffer.buffered(), &[0x01, 0x02]);
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_writer_preserves_buffer_on_error() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(b"@RSYNCD: 30.0\n".to_vec());
    let decision = sniffer
        .read_from(&mut reader)
        .expect("legacy negotiation detection succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);

    let mut failing = FailingWriter::new();
    let err = sniffer
        .take_sniffed_prefix_into_writer(&mut failing)
        .expect_err("writer failure should be surfaced");
    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_handles_binary_negotiation() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(&[0x7F, 0x00, 0x01])
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(consumed, 1);

    sniffer
        .buffered_storage_mut()
        .extend_from_slice(&[0xAA, 0x55]);

    let mut prefix = Vec::new();
    let drained = sniffer
        .take_sniffed_prefix_into(&mut prefix)
        .expect("binary prefix extraction should succeed");
    assert_eq!(drained, 1);
    assert_eq!(prefix, &[0x7F]);
    assert!(sniffer.sniffed_prefix().is_empty());
    assert_eq!(sniffer.buffered(), &[0xAA, 0x55]);
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::Binary));
    assert!(!sniffer.requires_more_data());
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_slice_copies_prefix() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let payload = LEGACY_DAEMON_PREFIX.as_bytes();
    let (decision, consumed) = sniffer
        .observe(payload)
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);

    let remainder = b"module";
    sniffer.buffered_storage_mut().extend_from_slice(remainder);

    let mut scratch = [0xAA; LEGACY_DAEMON_PREFIX_LEN];
    let drained = sniffer
        .take_sniffed_prefix_into_slice(&mut scratch)
        .expect("slice large enough to hold prefix");

    assert_eq!(drained, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(scratch, *LEGACY_DAEMON_PREFIX_BYTES);
    assert_eq!(sniffer.buffered(), remainder);
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_slice_reports_small_buffer() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");

    let mut scratch = [0u8; LEGACY_DAEMON_PREFIX_LEN - 1];
    let err = sniffer
        .take_sniffed_prefix_into_slice(&mut scratch)
        .expect_err("insufficient slice should error");

    assert_eq!(err.required(), LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(err.available(), scratch.len());
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_array_copies_prefix() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(&[0x7F, 0x80])
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(consumed, 1);

    let remainder = [0x90, 0x91];
    sniffer.buffered_storage_mut().extend_from_slice(&remainder);

    let mut scratch = [0xFFu8; LEGACY_DAEMON_PREFIX_LEN];
    let drained = sniffer
        .take_sniffed_prefix_into_array(&mut scratch)
        .expect("array large enough for binary prefix");

    assert_eq!(drained, 1);
    assert_eq!(scratch[0], 0x7F);
    assert!(scratch[1..].iter().all(|&byte| byte == 0xFF));
    assert_eq!(sniffer.buffered(), &remainder);
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::Binary));
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_is_noop_when_prefix_incomplete() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer.observe(b"@R").expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert_eq!(consumed, 2);
    assert!(sniffer.requires_more_data());

    let mut prefix = Vec::new();
    prefix.extend_from_slice(b"previous");
    let drained = sniffer
        .take_sniffed_prefix_into(&mut prefix)
        .expect("draining incomplete prefix should not allocate");
    assert_eq!(drained, 0);
    assert_eq!(prefix, b"previous");
    assert_eq!(sniffer.buffered(), b"@R");
    assert!(sniffer.requires_more_data());
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_preserves_remainder() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(LEGACY_DAEMON_PREFIX_BYTES.to_vec());
    let decision = sniffer
        .read_from(&mut reader)
        .expect("legacy negotiation detection succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);

    let remainder = b"module data";
    sniffer.buffered_storage_mut().extend_from_slice(remainder);
    let remainder_snapshot = sniffer.buffered_remainder().to_vec();

    let prefix = sniffer.take_sniffed_prefix();
    assert_eq!(prefix, LEGACY_DAEMON_PREFIX_BYTES);
    assert_eq!(sniffer.sniffed_prefix_len(), 0);
    assert_eq!(sniffer.buffered(), remainder_snapshot);
    assert_eq!(sniffer.buffered_remainder(), remainder);
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
    assert!(!sniffer.requires_more_data());
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_is_noop_when_prefix_incomplete() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    sniffer.observe(b"@R").expect("buffer reservation succeeds");
    assert!(sniffer.requires_more_data());

    let before = sniffer.buffered().to_vec();
    let prefix = sniffer.take_sniffed_prefix();
    assert!(prefix.is_empty());
    assert_eq!(sniffer.buffered(), before);
    assert!(sniffer.requires_more_data());
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_handles_binary_negotiation() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(vec![0x7F, 0x01, 0x02]);
    let decision = sniffer
        .read_from(&mut reader)
        .expect("binary negotiation detection succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);

    sniffer
        .buffered_storage_mut()
        .extend_from_slice(&[0xAA, 0x55]);

    let prefix = sniffer.take_sniffed_prefix();
    assert_eq!(prefix, vec![0x7F]);
    assert_eq!(sniffer.sniffed_prefix_len(), 0);
    assert_eq!(sniffer.buffered(), &[0xAA, 0x55]);
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::Binary));
    assert!(!sniffer.requires_more_data());
}

#[test]
fn prologue_sniffer_reset_trims_oversized_buffer_capacity() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let oversized = LEGACY_DAEMON_PREFIX_LEN * 4;
    *sniffer.buffered_storage_mut() = Vec::with_capacity(oversized);
    assert!(
        sniffer.buffered_storage().capacity() >= oversized,
        "allocator must provide at least the requested oversize capacity"
    );

    sniffer.reset();

    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), None);
    assert!(
        sniffer.buffered_storage().capacity() <= LEGACY_DAEMON_PREFIX_LEN,
        "reset should shrink oversize buffers back to the canonical prefix capacity"
    );
    assert!(sniffer.requires_more_data());
}

#[test]
fn prologue_sniffer_reset_restores_canonical_capacity_after_shrink() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    sniffer.buffered_storage_mut().shrink_to_fit();
    assert_eq!(sniffer.buffered_storage().capacity(), 0);

    sniffer.reset();

    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), None);
    assert!(
        sniffer.buffered_storage().capacity() >= LEGACY_DAEMON_PREFIX_LEN,
        "reset should grow undersized buffers to the canonical prefix capacity"
    );
    assert!(sniffer.requires_more_data());
}

#[test]
fn prologue_sniffer_take_buffered_into_slice_reports_small_buffer() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);

    let mut scratch = [0u8; LEGACY_DAEMON_PREFIX_LEN - 1];
    let err = sniffer
        .take_buffered_into_slice(&mut scratch)
        .expect_err("insufficient slice should error");

    assert_eq!(err.required(), LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(err.available(), scratch.len());
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
    assert_eq!(sniffer.legacy_prefix_remaining(), None);
}

#[test]
fn buffered_prefix_too_small_display_mentions_lengths() {
    let err = BufferedPrefixTooSmall::new(LEGACY_DAEMON_PREFIX_LEN, 4);
    let rendered = err.to_string();

    assert!(rendered.contains(&LEGACY_DAEMON_PREFIX_LEN.to_string()));
    assert!(rendered.contains("4"));
}

#[test]
fn map_reserve_error_for_io_marks_out_of_memory() {
    let mut buffer = Vec::<u8>::new();
    let reserve_err = buffer
        .try_reserve_exact(usize::MAX)
        .expect_err("capacity overflow should error");

    let mapped = map_reserve_error_for_io(reserve_err);
    assert_eq!(mapped.kind(), io::ErrorKind::OutOfMemory);
    assert!(
        mapped
            .to_string()
            .contains("failed to reserve memory for legacy negotiation buffer")
    );

    let source = mapped.source().expect("mapped error must retain source");
    assert!(source.downcast_ref::<TryReserveError>().is_some());
}

#[test]
fn prologue_sniffer_take_buffered_returns_initial_binary_byte() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(&[0x80, 0x81, 0x82])
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(consumed, 1);

    let buffered = sniffer.take_buffered();
    assert_eq!(buffered, [0x80]);
    assert!(buffered.capacity() <= LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::Binary));
}

#[test]
fn prologue_sniffer_take_buffered_into_returns_initial_binary_byte() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(&[0x80, 0x81, 0x82])
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(consumed, 1);

    let mut reused = Vec::with_capacity(16);
    let drained = sniffer
        .take_buffered_into(&mut reused)
        .expect("should copy buffered byte");

    assert_eq!(reused, [0x80]);
    assert_eq!(drained, 1);
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::Binary));
}

#[test]
fn prologue_sniffer_take_buffered_into_slice_returns_initial_binary_byte() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(&[0x80, 0x81, 0x82])
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(consumed, 1);

    let mut scratch = [0xAA; LEGACY_DAEMON_PREFIX_LEN];
    let copied = sniffer
        .take_buffered_into_slice(&mut scratch)
        .expect("scratch slice fits binary prefix");

    assert_eq!(copied, 1);
    assert_eq!(scratch[0], 0x80);
    assert!(scratch[1..].iter().all(|&byte| byte == 0xAA));
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::Binary));
}

#[test]
fn read_legacy_daemon_line_collects_complete_greeting() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.legacy_prefix_complete());

    let mut remainder = Cursor::new(b" 31.0\n".to_vec());
    let mut line = Vec::new();
    read_legacy_daemon_line(&mut sniffer, &mut remainder, &mut line)
        .expect("complete greeting should be collected");

    assert_eq!(line, b"@RSYNCD: 31.0\n");
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
}

#[test]
fn read_legacy_daemon_line_handles_interrupted_reads() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);

    let mut reader = InterruptedOnceReader::new(b" 32.0\n".to_vec());
    let mut line = Vec::new();
    read_legacy_daemon_line(&mut sniffer, &mut reader, &mut line)
        .expect("interrupted read should be retried");

    assert!(reader.was_interrupted());
    assert_eq!(line, b"@RSYNCD: 32.0\n");
}

#[test]
fn read_legacy_daemon_line_preserves_bytes_consumed_during_detection() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(b"@RZYNCD: 31.0\n".to_vec());

    let decision = sniffer
        .read_from(&mut reader)
        .expect("negotiation sniffing should succeed");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(sniffer.buffered(), b"@RZYNCD:");

    let mut line = Vec::new();
    read_legacy_daemon_line(&mut sniffer, &mut reader, &mut line)
        .expect("legacy greeting should include the bytes read during detection");

    assert_eq!(line, b"@RZYNCD: 31.0\n");
}

#[test]
fn read_legacy_daemon_line_uses_buffered_newline_without_additional_io() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.legacy_prefix_complete());

    sniffer
        .buffered_storage_mut()
        .extend_from_slice(b" 28.0\nNEXT");

    let mut reader = Cursor::new(b"unused".to_vec());
    let mut line = Vec::new();

    read_legacy_daemon_line(&mut sniffer, &mut reader, &mut line)
        .expect("buffered newline should avoid extra reads");

    assert_eq!(line, b"@RSYNCD: 28.0\n");
    assert_eq!(reader.position(), 0);
    assert_eq!(sniffer.buffered(), b"NEXT");
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
}

#[test]
fn read_legacy_daemon_line_rejects_incomplete_legacy_prefix() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer.observe(b"@").expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert_eq!(consumed, 1);
    assert_eq!(
        sniffer.decision(),
        Some(NegotiationPrologue::LegacyAscii),
        "detector caches legacy decision once '@' is observed",
    );
    assert!(!sniffer.legacy_prefix_complete());

    let mut reader = Cursor::new(b" remainder\n".to_vec());
    let mut line = b"seed".to_vec();
    let err = read_legacy_daemon_line(&mut sniffer, &mut reader, &mut line)
        .expect_err("incomplete legacy prefix must error");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(line, b"seed");
    assert_eq!(sniffer.buffered(), b"@");
}

#[test]
fn read_legacy_daemon_line_rejects_non_legacy_state() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(&[0x00])
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);
    assert_eq!(consumed, 1);

    let mut reader = Cursor::new(b"anything\n".to_vec());
    let mut line = Vec::new();
    let err = read_legacy_daemon_line(&mut sniffer, &mut reader, &mut line)
        .expect_err("binary negotiation must be rejected");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(line.is_empty());
}

#[test]
fn read_and_parse_legacy_daemon_greeting_succeeds() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(b"@RSYNCD: 31.0\nrest".to_vec());

    let decision = sniffer
        .read_from(&mut reader)
        .expect("legacy negotiation detection succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);

    let mut line = Vec::new();
    let version = read_and_parse_legacy_daemon_greeting(&mut sniffer, &mut reader, &mut line)
        .expect("legacy greeting should parse");

    assert_eq!(version.as_u8(), 31);
    assert_eq!(line, b"@RSYNCD: 31.0\n");

    let mut remainder = Vec::new();
    reader.read_to_end(&mut remainder).expect("read remainder");
    assert_eq!(remainder, b"rest");
}

#[test]
fn read_and_parse_legacy_daemon_greeting_details_exposes_metadata() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(b"@RSYNCD: 31.0 md4 md5\n".to_vec());

    let decision = sniffer
        .read_from(&mut reader)
        .expect("legacy negotiation detection succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);

    let mut line = Vec::new();
    let greeting =
        read_and_parse_legacy_daemon_greeting_details(&mut sniffer, &mut reader, &mut line)
            .expect("legacy greeting should parse");

    assert_eq!(greeting.protocol().as_u8(), 31);
    assert_eq!(greeting.digest_list(), Some("md4 md5"));
    assert!(greeting.has_subprotocol());
    assert_eq!(line, b"@RSYNCD: 31.0 md4 md5\n");
}

#[test]
fn read_and_parse_legacy_daemon_greeting_reports_parse_errors() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(b"@RSYNCD: ???\n".to_vec());

    let decision = sniffer
        .read_from(&mut reader)
        .expect("legacy negotiation detection succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);

    let mut line = Vec::new();
    let err = read_and_parse_legacy_daemon_greeting(&mut sniffer, &mut reader, &mut line)
        .expect_err("malformed greeting should fail");

    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    let source = err
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<NegotiationError>())
        .expect("io::Error must retain NegotiationError source");
    assert_eq!(
        source,
        &NegotiationError::MalformedLegacyGreeting {
            input: "@RSYNCD: ???".to_owned()
        }
    );
    assert_eq!(line, b"@RSYNCD: ???\n");
}

#[test]
fn read_and_parse_legacy_daemon_greeting_rejects_binary_negotiation() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(vec![0x00, 0x42, 0x43]);

    let decision = sniffer
        .read_from(&mut reader)
        .expect("binary negotiation detection succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);

    let mut line = Vec::new();
    let err = read_and_parse_legacy_daemon_greeting(&mut sniffer, &mut reader, &mut line)
        .expect_err("binary negotiation should reject legacy greeting parser");

    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(line.is_empty());
}

#[test]
fn read_and_parse_legacy_daemon_greeting_details_rejects_binary_negotiation() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(vec![0x00, 0x42, 0x43]);

    let decision = sniffer
        .read_from(&mut reader)
        .expect("binary negotiation detection succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);

    let mut line = Vec::new();
    let err = read_and_parse_legacy_daemon_greeting_details(&mut sniffer, &mut reader, &mut line)
        .expect_err("binary negotiation should reject legacy greeting parser");

    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(line.is_empty());
}

#[test]
fn read_legacy_daemon_line_errors_on_unexpected_eof() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);

    let mut reader = Cursor::new(b" incomplete".to_vec());
    let mut line = Vec::new();
    let err = read_legacy_daemon_line(&mut sniffer, &mut reader, &mut line)
        .expect_err("missing newline should error");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    assert!(line.starts_with(LEGACY_DAEMON_PREFIX.as_bytes()));
    assert_eq!(&line[LEGACY_DAEMON_PREFIX_LEN..], b" incomplete");
}

#[test]
fn prologue_sniffer_take_buffered_clamps_replacement_capacity() {
    let mut sniffer = NegotiationPrologueSniffer::new();

    sniffer.buffered_storage_mut().reserve(1024);
    assert!(sniffer.buffered_storage().capacity() > LEGACY_DAEMON_PREFIX_LEN);

    let _ = sniffer.take_buffered();

    assert_eq!(
        sniffer.buffered_storage().capacity(),
        LEGACY_DAEMON_PREFIX_LEN
    );
}

#[test]
fn prologue_sniffer_take_buffered_into_clamps_replacement_capacity() {
    let mut sniffer = NegotiationPrologueSniffer::new();

    sniffer.buffered_storage_mut().reserve(1024);
    assert!(sniffer.buffered_storage().capacity() > LEGACY_DAEMON_PREFIX_LEN);

    let mut reused = Vec::new();
    let drained = sniffer
        .take_buffered_into(&mut reused)
        .expect("should copy buffered prefix");

    assert!(reused.is_empty());
    assert_eq!(drained, 0);
    assert_eq!(
        sniffer.buffered_storage().capacity(),
        LEGACY_DAEMON_PREFIX_LEN
    );
}

#[test]
fn prologue_sniffer_clone_preserves_state_and_buffer_independence() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let partial_len = 3;
    let partial_prefix = &LEGACY_DAEMON_PREFIX.as_bytes()[..partial_len];

    let (decision, consumed) = sniffer
        .observe(partial_prefix)
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert_eq!(consumed, partial_len);
    assert_eq!(sniffer.buffered(), partial_prefix);
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
    assert!(sniffer.requires_more_data());

    let mut cloned = sniffer.clone();
    assert_eq!(cloned.buffered(), partial_prefix);
    assert_eq!(cloned.decision(), sniffer.decision());
    assert_eq!(cloned.requires_more_data(), sniffer.requires_more_data());

    let drained = sniffer.take_buffered();
    assert_eq!(drained, partial_prefix);
    assert!(sniffer.buffered().is_empty());
    assert!(sniffer.requires_more_data());

    let remaining_prefix = &LEGACY_DAEMON_PREFIX.as_bytes()[partial_len..];
    let (clone_decision, clone_consumed) = cloned
        .observe(remaining_prefix)
        .expect("buffer reservation succeeds");
    assert_eq!(clone_decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(clone_consumed, remaining_prefix.len());
    assert_eq!(cloned.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert!(cloned.is_legacy());
    assert!(!cloned.requires_more_data());

    let replay = cloned.take_buffered();
    assert_eq!(replay, LEGACY_DAEMON_PREFIX.as_bytes());
    assert!(cloned.buffered().is_empty());
    assert!(!cloned.requires_more_data());

    assert!(sniffer.buffered().is_empty());
    assert!(sniffer.requires_more_data());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
}

#[test]
fn prologue_sniffer_take_buffered_into_reuses_destination_capacity() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, _) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);

    let mut reused = Vec::with_capacity(64);
    reused.extend_from_slice(b"some prior contents");
    let ptr = reused.as_ptr();
    let capacity_before = reused.capacity();

    let drained = sniffer
        .take_buffered_into(&mut reused)
        .expect("should reuse existing allocation");

    assert_eq!(reused, LEGACY_DAEMON_PREFIX.as_bytes());
    assert_eq!(reused.as_ptr(), ptr);
    assert_eq!(reused.capacity(), capacity_before);
    assert_eq!(drained, LEGACY_DAEMON_PREFIX_LEN);
}

#[test]
fn prologue_sniffer_take_buffered_into_accounts_for_existing_length_when_growing() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);

    // Start with a destination vector that already contains data and whose capacity
    // is smaller than the buffered prefix. The helper must base its reservation on
    // the vector's *length* so the subsequent extend does not trigger another
    // allocation (which would panic on OOM instead of returning `TryReserveError`).
    let mut reused = Vec::with_capacity(LEGACY_DAEMON_PREFIX_LEN - 2);
    reused.resize(LEGACY_DAEMON_PREFIX_LEN - 2, 0xAA);
    let drained = sniffer
        .take_buffered_into(&mut reused)
        .expect("should grow destination to canonical prefix length");

    assert_eq!(drained, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(reused, LEGACY_DAEMON_PREFIX.as_bytes());
    assert_eq!(reused.capacity(), LEGACY_DAEMON_PREFIX_LEN);
}

#[test]
fn prologue_sniffer_reports_binary_prefix_state() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut cursor = Cursor::new(vec![0x00, 0x20, 0x00]);

    let decision = sniffer
        .read_from(&mut cursor)
        .expect("binary negotiation should succeed");
    assert_eq!(decision, NegotiationPrologue::Binary);

    assert!(!sniffer.legacy_prefix_complete());
    assert_eq!(sniffer.legacy_prefix_remaining(), None);
}

#[test]
fn prologue_sniffer_reset_clears_buffer_and_state() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut cursor = Cursor::new(LEGACY_DAEMON_PREFIX.as_bytes().to_vec());
    let _ = sniffer
        .read_from(&mut cursor)
        .expect("legacy negotiation should succeed");

    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));

    sniffer.reset();
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), None);
}

#[test]
fn prologue_sniffer_reset_trims_excess_capacity() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    // Inflate the backing allocation to simulate a previous oversized prefix capture.
    *sniffer.buffered_storage_mut() = Vec::with_capacity(128);
    sniffer
        .buffered_storage_mut()
        .extend_from_slice(LEGACY_DAEMON_PREFIX.as_bytes());
    assert!(sniffer.buffered_storage().capacity() > LEGACY_DAEMON_PREFIX_LEN);

    sniffer.reset();
    assert_eq!(
        sniffer.buffered_storage().capacity(),
        LEGACY_DAEMON_PREFIX_LEN
    );
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), None);
}

#[test]
fn prologue_sniffer_reset_reuses_canonical_allocation() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let ptr_before = sniffer.buffered_storage().as_ptr();
    let capacity_before = sniffer.buffered_storage().capacity();
    assert_eq!(capacity_before, LEGACY_DAEMON_PREFIX_LEN);

    sniffer
        .buffered_storage_mut()
        .extend_from_slice(&LEGACY_DAEMON_PREFIX.as_bytes()[..LEGACY_DAEMON_PREFIX_LEN - 2]);
    sniffer.reset();

    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.buffered_storage().capacity(), capacity_before);
    assert!(ptr::eq(sniffer.buffered_storage().as_ptr(), ptr_before));
    assert_eq!(sniffer.decision(), None);
}

#[test]
fn prologue_sniffer_reset_restores_canonical_capacity() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    // Simulate an external pool swapping in a smaller allocation.
    *sniffer.buffered_storage_mut() = Vec::with_capacity(2);
    sniffer.buffered_storage_mut().extend_from_slice(b"@@");
    assert!(sniffer.buffered_storage().capacity() < LEGACY_DAEMON_PREFIX_LEN);

    sniffer.reset();

    assert_eq!(
        sniffer.buffered_storage().capacity(),
        LEGACY_DAEMON_PREFIX_LEN
    );
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), None);
}

#[test]
fn prologue_sniffer_handles_unexpected_eof() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut cursor = Cursor::new(Vec::<u8>::new());
    let err = sniffer.read_from(&mut cursor).expect_err("EOF should fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn prologue_sniffer_retries_after_interrupted_read() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = InterruptedOnceReader::new(b"@RSYNCD: 31.0\n".to_vec());

    let decision = sniffer
        .read_from(&mut reader)
        .expect("sniffer should retry after EINTR");

    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert!(
        reader.was_interrupted(),
        "the reader must report an interruption"
    );
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert!(sniffer.legacy_prefix_complete());

    let mut cursor = reader.into_inner();
    let mut remainder = Vec::new();
    cursor.read_to_end(&mut remainder).expect("read remainder");
    assert_eq!(remainder, b" 31.0\n");
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_writer_requires_complete_prefix() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let partial = &LEGACY_DAEMON_PREFIX_BYTES[..LEGACY_DAEMON_PREFIX_LEN - 1];
    let (decision, consumed) = sniffer
        .observe(partial)
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert_eq!(consumed, partial.len());
    assert!(sniffer.requires_more_data());

    let mut sink = Vec::new();
    let written = sniffer
        .take_sniffed_prefix_into_writer(&mut sink)
        .expect("writing incomplete prefix should be a no-op");
    assert_eq!(written, 0);
    assert!(sink.is_empty());
    assert_eq!(sniffer.buffered(), partial);
    assert!(sniffer.requires_more_data());
}

#[test]
fn prologue_sniffer_take_sniffed_prefix_into_writer_drains_prefix_and_preserves_remainder() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX_BYTES)
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.legacy_prefix_complete());

    let remainder = b" 31.0\n";
    sniffer.buffered_storage_mut().extend_from_slice(remainder);
    let mut expected = LEGACY_DAEMON_PREFIX_BYTES.to_vec();
    expected.extend_from_slice(remainder);
    assert_eq!(sniffer.buffered(), expected.as_slice());

    let mut sink = Vec::new();
    let written = sniffer
        .take_sniffed_prefix_into_writer(&mut sink)
        .expect("writing sniffed prefix succeeds");
    assert_eq!(written, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(sink, LEGACY_DAEMON_PREFIX_BYTES);
    assert_eq!(sniffer.buffered(), remainder);
    assert_eq!(sniffer.sniffed_prefix_len(), 0);
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));

    let written_again = sniffer
        .take_sniffed_prefix_into_writer(&mut sink)
        .expect("subsequent call must be a no-op");
    assert_eq!(written_again, 0);
    assert_eq!(sniffer.buffered(), remainder);
}
