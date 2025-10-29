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
    assert!(buffered.is_empty());
    assert_eq!(sniffer.buffered_len(), 5);
    assert_eq!(sniffer.buffered(), b"@RSYN");
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
