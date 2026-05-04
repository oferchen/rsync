
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
fn prologue_sniffer_try_reserve_buffered_grows_capacity() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let initial_capacity = sniffer.buffered_storage().capacity();

    sniffer
        .try_reserve_buffered(LEGACY_DAEMON_PREFIX_LEN * 3)
        .expect("reservation should succeed for modest growth");

    assert!(sniffer.buffered().is_empty());
    assert!(sniffer.requires_more_data());
    assert!(
        sniffer.buffered_storage().capacity() >= initial_capacity.max(LEGACY_DAEMON_PREFIX_LEN * 3),
        "buffer capacity must accommodate the requested additional bytes"
    );
}

#[test]
fn prologue_sniffer_try_reserve_buffered_reports_overflow() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let err = sniffer
        .try_reserve_buffered(usize::MAX)
        .expect_err("overflowing reservation must error");

    let rendered = format!("{err:?}");
    assert!(rendered.contains("CapacityOverflow"));
    assert!(sniffer.buffered().is_empty());
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
    assert!(drained.is_empty());
    assert_eq!(sniffer.buffered(), partial_prefix);
    assert!(sniffer.requires_more_data());

    let remaining_prefix = &LEGACY_DAEMON_PREFIX.as_bytes()[partial_len..];
    let (original_decision, original_consumed) = sniffer
        .observe(remaining_prefix)
        .expect("buffer reservation succeeds");
    assert_eq!(original_decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(original_consumed, remaining_prefix.len());
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert!(sniffer.is_legacy());
    assert!(!sniffer.requires_more_data());

    let replay = sniffer.take_buffered();
    assert_eq!(replay, LEGACY_DAEMON_PREFIX.as_bytes());
    assert!(sniffer.buffered().is_empty());
    assert!(!sniffer.requires_more_data());

    assert_eq!(cloned.buffered(), partial_prefix);
    let (clone_decision, clone_consumed) = cloned
        .observe(remaining_prefix)
        .expect("buffer reservation succeeds");
    assert_eq!(clone_decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(clone_consumed, remaining_prefix.len());
    assert_eq!(cloned.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert!(cloned.is_legacy());
    assert!(!cloned.requires_more_data());

    let cloned_replay = cloned.take_buffered();
    assert_eq!(cloned_replay, LEGACY_DAEMON_PREFIX.as_bytes());
    assert!(cloned.buffered().is_empty());
    assert!(!cloned.requires_more_data());
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
