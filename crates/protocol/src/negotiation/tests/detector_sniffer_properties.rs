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

        if detector.requires_more_data() && expected == NegotiationPrologue::LegacyAscii {
            prop_assert!(matches!(
                last,
                NegotiationPrologue::LegacyAscii | NegotiationPrologue::NeedMoreData
            ));
        } else {
            prop_assert_eq!(last, expected);
        }

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

        if let Some(NegotiationPrologue::LegacyAscii) = detector.decision() {
            if let Some(remaining) = detector.legacy_prefix_remaining() {
                prop_assert!(remaining > 0);
                prop_assert!(!detector.legacy_prefix_complete());
            } else {
                prop_assert!(detector.legacy_prefix_complete());
            }
        } else {
            prop_assert_eq!(detector.legacy_prefix_remaining(), None);
            prop_assert!(!detector.legacy_prefix_complete());
            prop_assert!(buffered.is_empty());
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
