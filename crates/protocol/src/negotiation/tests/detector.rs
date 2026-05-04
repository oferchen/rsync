
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
                    "detector should cache legacy decision for {data:?}"
                );
                assert!(detector.requires_more_data());
            } else {
                assert_eq!(
                    result, expected,
                    "segmented detection mismatch for {data:?} with splits ({first_end}, {second_end})"
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
            "decision mismatch for {data:?}"
        );
        assert_eq!(
            byte_detector.decision(),
            slice_detector.decision(),
            "cached decision mismatch for {data:?}"
        );
        assert_eq!(
            byte_detector.legacy_prefix_complete(),
            slice_detector.legacy_prefix_complete(),
            "prefix completion mismatch for {data:?}"
        );
        assert_eq!(
            byte_detector.buffered_prefix(),
            slice_detector.buffered_prefix(),
            "buffered prefix mismatch for {data:?}"
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

