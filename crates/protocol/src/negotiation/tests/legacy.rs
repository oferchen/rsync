
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

#[test]
fn sniffer_rehydrate_from_parts_binary_preserves_state() {
    let data = [0x34, 0x12, 0x00, 0x01];
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(&data[..]);
    let decision = sniffer
        .read_from(&mut reader)
        .expect("binary negotiation detection succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);
    let prefix_len = sniffer.sniffed_prefix_len();
    let snapshot = sniffer.buffered().to_vec();

    let mut restored = NegotiationPrologueSniffer::new();
    restored
        .rehydrate_from_parts(decision, prefix_len, &snapshot)
        .expect("rehydration succeeds");

    assert_eq!(restored.buffered(), snapshot.as_slice());
    assert_eq!(restored.sniffed_prefix_len(), prefix_len);
    assert_eq!(restored.decision(), Some(decision));
}

#[test]
fn sniffer_rehydrate_from_parts_partial_legacy_preserves_pending_state() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (_, consumed) = sniffer.observe(b"@RS").expect("observation succeeds");
    assert_eq!(consumed, 3);
    assert!(sniffer.requires_more_data());
    let decision = sniffer.decision().expect("cached decision available");
    let prefix_len = sniffer.sniffed_prefix_len();
    let remaining = sniffer.legacy_prefix_remaining();
    let snapshot = sniffer.buffered().to_vec();

    let mut restored = NegotiationPrologueSniffer::new();
    restored
        .rehydrate_from_parts(decision, prefix_len, &snapshot)
        .expect("rehydration succeeds");

    assert_eq!(restored.buffered(), snapshot.as_slice());
    assert_eq!(restored.sniffed_prefix_len(), prefix_len);
    assert_eq!(restored.decision(), Some(decision));
    assert_eq!(restored.legacy_prefix_remaining(), remaining);
    assert!(restored.requires_more_data());
}

#[test]
fn sniffer_into_parts_produces_rehydratable_snapshot() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX_BYTES)
        .expect("observation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.legacy_prefix_complete());

    let remainder = b" 31.0\n";
    sniffer.buffered_storage_mut().extend_from_slice(remainder);

    let expected_prefix_len = sniffer.sniffed_prefix_len();
    let expected_buffer = {
        let mut transcript = LEGACY_DAEMON_PREFIX_BYTES.to_vec();
        transcript.extend_from_slice(remainder);
        transcript
    };

    let (snapshot_decision, prefix_len, transcript) = sniffer.into_parts();
    assert_eq!(snapshot_decision, decision);
    assert_eq!(prefix_len, expected_prefix_len);
    assert_eq!(transcript, expected_buffer);

    let mut restored = NegotiationPrologueSniffer::new();
    restored
        .rehydrate_from_parts(snapshot_decision, prefix_len, &transcript)
        .expect("rehydration succeeds");

    assert_eq!(restored.buffered(), transcript.as_slice());
    assert_eq!(restored.sniffed_prefix_len(), prefix_len);
    assert_eq!(restored.decision(), Some(snapshot_decision));
    assert!(!restored.requires_more_data());
}

#[test]
fn sniffer_into_parts_preserves_pending_state() {
    let sniffer = NegotiationPrologueSniffer::new();
    let (decision, prefix_len, transcript) = sniffer.into_parts();

    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert_eq!(prefix_len, 0);
    assert!(transcript.is_empty());

    let mut restored = NegotiationPrologueSniffer::new();
    restored
        .rehydrate_from_parts(decision, prefix_len, &transcript)
        .expect("rehydration succeeds");

    assert!(restored.decision().is_none());
    assert_eq!(restored.sniffed_prefix_len(), 0);
    assert!(restored.requires_more_data());
    assert!(restored.buffered().is_empty());
}
