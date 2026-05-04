#![allow(unused_must_use, clippy::uninlined_format_args)]

use crate::legacy::LEGACY_DAEMON_PREFIX_LEN;
use crate::negotiation::NegotiationPrologue;

use super::NegotiationPrologueDetector;

#[test]
fn new_detector_has_no_decision() {
    let detector = NegotiationPrologueDetector::new();
    assert!(detector.decision().is_none());
    assert!(!detector.is_decided());
}

#[test]
fn default_equals_new() {
    let default = NegotiationPrologueDetector::default();
    let new = NegotiationPrologueDetector::new();
    assert_eq!(default.buffered_len(), new.buffered_len());
    assert_eq!(default.decision(), new.decision());
}

#[test]
fn binary_detected_on_non_at_byte() {
    let mut detector = NegotiationPrologueDetector::new();
    let result = detector.observe_byte(0x00);
    assert_eq!(result, NegotiationPrologue::Binary);
    assert!(detector.is_binary());
    assert!(!detector.is_legacy());
}

#[test]
fn legacy_detected_on_at_byte() {
    let mut detector = NegotiationPrologueDetector::new();
    let result = detector.observe_byte(b'@');
    assert!(detector.is_legacy());
    assert!(!detector.is_binary());
    assert!(!detector.legacy_prefix_complete());
    assert!(
        result == NegotiationPrologue::LegacyAscii || result == NegotiationPrologue::NeedMoreData
    );
}

#[test]
fn decision_is_sticky() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe_byte(0x00);
    assert!(detector.is_binary());

    detector.observe(b"@RSYNCD:");
    assert!(detector.is_binary());
}

#[test]
fn buffered_prefix_empty_for_binary() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe_byte(0x00);
    assert!(detector.buffered_prefix().is_empty());
    assert_eq!(detector.buffered_len(), 0);
}

#[test]
fn buffered_prefix_grows_for_legacy() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe(b"@RSY");
    assert!(detector.is_legacy());
    assert_eq!(detector.buffered_len(), 4);
    assert_eq!(detector.buffered_prefix(), b"@RSY");
}

#[test]
fn legacy_prefix_remaining_tracks_bytes() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe_byte(b'@');
    let remaining = detector.legacy_prefix_remaining();
    assert!(remaining.is_some());
    assert!(remaining.unwrap() < LEGACY_DAEMON_PREFIX_LEN);
}

#[test]
fn legacy_prefix_complete_after_full_prefix() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe(b"@RSYNCD:");
    assert!(detector.is_legacy());
    assert!(detector.legacy_prefix_complete());
}

#[test]
fn requires_more_data_when_empty() {
    let detector = NegotiationPrologueDetector::new();
    assert!(detector.requires_more_data());
}

#[test]
fn requires_more_data_false_for_binary() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe_byte(0x00);
    assert!(!detector.requires_more_data());
}

#[test]
fn copy_buffered_prefix_into_success() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe(b"@RSY");

    let mut buffer = [0u8; 10];
    let copied = detector.copy_buffered_prefix_into(&mut buffer).unwrap();
    assert_eq!(copied, 4);
    assert_eq!(&buffer[..4], b"@RSY");
}

#[test]
fn copy_buffered_prefix_into_too_small() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe(b"@RSYNCD:");

    let mut buffer = [0u8; 2];
    let result = detector.copy_buffered_prefix_into(&mut buffer);
    assert!(result.is_err());
}

#[test]
fn copy_buffered_prefix_into_array_success() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe(b"@RSY");

    let mut buffer = [0u8; LEGACY_DAEMON_PREFIX_LEN];
    let copied = detector
        .copy_buffered_prefix_into_array(&mut buffer)
        .unwrap();
    assert_eq!(copied, 4);
}

#[test]
fn reset_clears_state() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe(b"@RSYNCD:");
    assert!(detector.is_legacy());
    assert!(detector.legacy_prefix_complete());

    detector.reset();
    assert!(detector.decision().is_none());
    assert!(!detector.is_decided());
    assert_eq!(detector.buffered_len(), 0);
}

#[test]
fn observe_empty_chunk_returns_need_more() {
    let mut detector = NegotiationPrologueDetector::new();
    let result = detector.observe(&[]);
    assert_eq!(result, NegotiationPrologue::NeedMoreData);
}

#[test]
fn is_decided_true_after_classification() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe_byte(0x00);
    assert!(detector.is_decided());
}

#[test]
fn binary_detected_for_all_non_at_bytes() {
    for byte in 0u8..=255 {
        if byte == b'@' {
            continue;
        }
        let mut detector = NegotiationPrologueDetector::new();
        let result = detector.observe_byte(byte);
        assert_eq!(
            result,
            NegotiationPrologue::Binary,
            "byte {:#04X} should be binary",
            byte
        );
        assert!(detector.is_binary());
        assert!(!detector.is_legacy());
    }
}

#[test]
fn binary_detection_immediate() {
    let mut detector = NegotiationPrologueDetector::new();
    let result = detector.observe_byte(0x1F);
    assert_eq!(result, NegotiationPrologue::Binary);
    assert!(detector.is_decided());
    assert!(!detector.requires_more_data());
}

#[test]
fn legacy_full_prefix_match() {
    let mut detector = NegotiationPrologueDetector::new();
    let result = detector.observe(b"@RSYNCD:");

    assert!(detector.is_legacy());
    assert!(detector.legacy_prefix_complete());
    assert!(!detector.requires_more_data());
    assert_eq!(detector.buffered_len(), LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(detector.buffered_prefix(), b"@RSYNCD:");
    assert!(result == NegotiationPrologue::LegacyAscii);
}

#[test]
fn legacy_incremental_byte_by_byte() {
    let prefix = b"@RSYNCD:";
    let mut detector = NegotiationPrologueDetector::new();

    for (i, &byte) in prefix.iter().enumerate() {
        let _result = detector.observe_byte(byte);
        assert!(detector.is_legacy(), "should be legacy at byte {i}");

        if i < prefix.len() - 1 {
            assert!(!detector.legacy_prefix_complete());
            assert!(detector.requires_more_data());
        } else {
            assert!(detector.legacy_prefix_complete());
            assert!(!detector.requires_more_data());
        }

        let remaining = detector.legacy_prefix_remaining();
        if i < prefix.len() - 1 {
            assert_eq!(remaining, Some(prefix.len() - 1 - i));
        } else {
            assert!(remaining.is_none() || remaining == Some(0));
        }
    }
}

#[test]
fn legacy_incremental_chunks() {
    let mut detector = NegotiationPrologueDetector::new();

    detector.observe(b"@RSY");
    assert!(detector.is_legacy());
    assert!(!detector.legacy_prefix_complete());
    assert_eq!(detector.buffered_len(), 4);

    detector.observe(b"NCD:");
    assert!(detector.is_legacy());
    assert!(detector.legacy_prefix_complete());
    assert_eq!(detector.buffered_len(), 8);
}

#[test]
fn legacy_mismatch_early() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe(b"@WRONG:");

    assert!(detector.is_legacy());
    assert!(detector.legacy_prefix_complete());
    assert!(!detector.requires_more_data());
}

#[test]
fn legacy_mismatch_at_second_byte() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe(b"@X");

    assert!(detector.is_legacy());
    assert!(detector.legacy_prefix_complete());
}

#[test]
fn decision_sticky_after_binary() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe_byte(0x00);
    assert!(detector.is_binary());

    detector.observe(b"@RSYNCD:");
    assert!(detector.is_binary());
    assert!(!detector.is_legacy());
    assert_eq!(detector.buffered_len(), 0);
}

#[test]
fn decision_sticky_after_legacy() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe(b"@RSYNCD:");
    assert!(detector.is_legacy());

    detector.observe(&[0x00, 0x01, 0x02]);
    assert!(detector.is_legacy());
    assert!(!detector.is_binary());
}

#[test]
fn reset_after_binary() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe_byte(0x00);
    assert!(detector.is_binary());

    detector.reset();
    assert!(!detector.is_decided());
    assert!(detector.decision().is_none());
    assert_eq!(detector.buffered_len(), 0);
    assert!(detector.requires_more_data());
}

#[test]
fn reset_after_legacy() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe(b"@RSYNCD:");
    assert!(detector.is_legacy());
    assert_eq!(detector.buffered_len(), 8);

    detector.reset();
    assert!(!detector.is_decided());
    assert!(detector.decision().is_none());
    assert_eq!(detector.buffered_len(), 0);
    assert!(detector.requires_more_data());
}

#[test]
fn reset_allows_reuse() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe_byte(0x00);
    assert!(detector.is_binary());

    detector.reset();

    detector.observe(b"@RSYNCD:");
    assert!(detector.is_legacy());
}

#[test]
fn copy_buffered_prefix_exact_size() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe(b"@RSYNCD:");

    let mut buffer = [0u8; LEGACY_DAEMON_PREFIX_LEN];
    let copied = detector
        .copy_buffered_prefix_into_array(&mut buffer)
        .unwrap();

    assert_eq!(copied, 8);
    assert_eq!(&buffer[..8], b"@RSYNCD:");
}

#[test]
fn copy_buffered_prefix_larger_buffer() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe(b"@RSY");

    let mut buffer = [0u8; 100];
    let copied = detector.copy_buffered_prefix_into(&mut buffer).unwrap();

    assert_eq!(copied, 4);
    assert_eq!(&buffer[..4], b"@RSY");
}

#[test]
fn copy_buffered_prefix_binary_returns_zero() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe_byte(0x00);

    let mut buffer = [0u8; 10];
    let copied = detector.copy_buffered_prefix_into(&mut buffer).unwrap();

    assert_eq!(copied, 0);
}

#[test]
fn observe_empty_before_any_data() {
    let mut detector = NegotiationPrologueDetector::new();
    let result = detector.observe(&[]);

    assert_eq!(result, NegotiationPrologue::NeedMoreData);
    assert!(!detector.is_decided());
}

#[test]
fn observe_empty_after_partial_legacy() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe(b"@RSY");

    let result = detector.observe(&[]);
    assert_eq!(result, NegotiationPrologue::NeedMoreData);
    assert!(detector.is_legacy());
    assert!(!detector.legacy_prefix_complete());
}

#[test]
fn observe_empty_after_complete_legacy() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe(b"@RSYNCD:");

    let result = detector.observe(&[]);
    assert!(result == NegotiationPrologue::LegacyAscii);
}

#[test]
fn observe_empty_after_binary() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe_byte(0x00);

    let result = detector.observe(&[]);
    assert_eq!(result, NegotiationPrologue::Binary);
}

#[test]
fn observe_byte_is_equivalent_to_single_byte_slice() {
    let test_bytes = [0x00, b'@', 0xFF, 0x7F];

    for &byte in &test_bytes {
        let mut detector1 = NegotiationPrologueDetector::new();
        let mut detector2 = NegotiationPrologueDetector::new();

        let result1 = detector1.observe_byte(byte);
        let result2 = detector2.observe(&[byte]);

        assert_eq!(result1, result2, "mismatch for byte {:#04X}", byte);
        assert_eq!(detector1.is_binary(), detector2.is_binary());
        assert_eq!(detector1.is_legacy(), detector2.is_legacy());
    }
}

#[test]
fn clone_preserves_state() {
    let mut original = NegotiationPrologueDetector::new();
    original.observe(b"@RSY");

    let cloned = original.clone();

    assert_eq!(original.buffered_len(), cloned.buffered_len());
    assert_eq!(original.decision(), cloned.decision());
    assert_eq!(original.is_legacy(), cloned.is_legacy());
    assert_eq!(
        original.legacy_prefix_complete(),
        cloned.legacy_prefix_complete()
    );
    assert_eq!(original.buffered_prefix(), cloned.buffered_prefix());
}

#[test]
fn clone_independence() {
    let mut original = NegotiationPrologueDetector::new();
    original.observe(b"@RSY");

    let cloned = original.clone();

    original.observe(b"NCD:");
    assert!(original.legacy_prefix_complete());

    assert!(!cloned.legacy_prefix_complete());
    assert_eq!(cloned.buffered_len(), 4);
}

#[test]
fn debug_format_new() {
    let detector = NegotiationPrologueDetector::new();
    let debug = format!("{:?}", detector);
    assert!(debug.contains("NegotiationPrologueDetector"));
}

#[test]
fn debug_format_after_binary() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe_byte(0x00);
    let debug = format!("{:?}", detector);
    assert!(debug.contains("Binary") || debug.contains("Some"));
}

#[test]
fn debug_format_after_legacy() {
    let mut detector = NegotiationPrologueDetector::new();
    detector.observe(b"@RSYNCD:");
    let debug = format!("{:?}", detector);
    assert!(debug.contains("LegacyAscii") || debug.contains("Some"));
}
