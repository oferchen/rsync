
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
fn prologue_sniffer_take_buffered_split_into_splits_prefix_and_remainder() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.legacy_prefix_complete());

    let remainder = b" trailing payload";
    sniffer.buffered_storage_mut().extend_from_slice(remainder);

    let mut prefix = b"old-prefix".to_vec();
    let mut tail = b"old-remainder".to_vec();
    let (prefix_len, remainder_len) = sniffer
        .take_buffered_split_into(&mut prefix, &mut tail)
        .expect("split transfer succeeds");

    assert_eq!(prefix, LEGACY_DAEMON_PREFIX.as_bytes());
    assert_eq!(tail, remainder);
    assert_eq!(prefix_len, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(remainder_len, remainder.len());
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
}

#[test]
fn prologue_sniffer_take_buffered_variants_wait_for_complete_prefix() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let partial = &LEGACY_DAEMON_PREFIX.as_bytes()[..LEGACY_DAEMON_PREFIX_LEN - 1];

    let (decision, consumed) = sniffer
        .observe(partial)
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert_eq!(consumed, partial.len());
    assert!(sniffer.requires_more_data());

    let drained_vec = sniffer.take_buffered();
    assert!(drained_vec.is_empty());
    assert_eq!(sniffer.buffered(), partial);
    assert!(sniffer.requires_more_data());

    let mut reused = b"unchanged".to_vec();
    let drained = sniffer
        .take_buffered_into(&mut reused)
        .expect("guarded transfer should succeed");
    assert_eq!(drained, 0);
    assert_eq!(reused, b"unchanged");
    assert_eq!(sniffer.buffered(), partial);
    assert!(sniffer.requires_more_data());

    let mut scratch = [0xAA; LEGACY_DAEMON_PREFIX_LEN];
    let copied = sniffer
        .take_buffered_into_slice(&mut scratch)
        .expect("guarded slice transfer should succeed");
    assert_eq!(copied, 0);
    assert!(scratch.iter().all(|&byte| byte == 0xAA));
    assert_eq!(sniffer.buffered(), partial);
    assert!(sniffer.requires_more_data());

    let mut sink = Vec::new();
    let written = sniffer
        .take_buffered_into_writer(&mut sink)
        .expect("guarded writer transfer should succeed");
    assert_eq!(written, 0);
    assert!(sink.is_empty());
    assert_eq!(sniffer.buffered(), partial);
    assert!(sniffer.requires_more_data());
}

#[test]
fn prologue_sniffer_take_buffered_split_into_waits_for_complete_prefix() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let partial = &LEGACY_DAEMON_PREFIX.as_bytes()[..LEGACY_DAEMON_PREFIX_LEN - 1];

    let (decision, consumed) = sniffer
        .observe(partial)
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::NeedMoreData);
    assert_eq!(consumed, partial.len());
    assert!(sniffer.requires_more_data());

    let mut prefix = b"unchanged-prefix".to_vec();
    let mut tail = b"unchanged-tail".to_vec();
    let (prefix_len, remainder_len) = sniffer
        .take_buffered_split_into(&mut prefix, &mut tail)
        .expect("guarded split transfer succeeds");

    assert_eq!(prefix_len, 0);
    assert_eq!(remainder_len, 0);
    assert_eq!(prefix, b"unchanged-prefix");
    assert_eq!(tail, b"unchanged-tail");
    assert_eq!(sniffer.buffered(), partial);
    assert!(sniffer.requires_more_data());
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
fn prologue_sniffer_take_buffered_into_vectored_copies_bytes() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.legacy_prefix_complete());

    let remainder = b" modules";
    sniffer.buffered_storage_mut().extend_from_slice(remainder);

    let mut first = vec![0u8; 4];
    let mut second = vec![0u8; LEGACY_DAEMON_PREFIX_LEN - first.len()];
    let mut third = vec![0u8; remainder.len()];
    let mut buffers = [
        IoSliceMut::new(first.as_mut_slice()),
        IoSliceMut::new(second.as_mut_slice()),
        IoSliceMut::new(third.as_mut_slice()),
    ];

    let copied = sniffer
        .take_buffered_into_vectored(&mut buffers)
        .expect("vectored transfer should succeed");

    let mut actual = Vec::new();
    let mut remaining = copied;
    for buf in &buffers {
        if remaining == 0 {
            break;
        }
        let slice: &[u8] = buf.as_ref();
        let take = slice.len().min(remaining);
        actual.extend_from_slice(&slice[..take]);
        remaining -= take;
    }

    let mut expected = LEGACY_DAEMON_PREFIX.as_bytes().to_vec();
    expected.extend_from_slice(remainder);

    assert_eq!(copied, expected.len());
    assert_eq!(actual, expected);
    assert!(sniffer.buffered().is_empty());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
}

#[test]
fn prologue_sniffer_take_buffered_into_vectored_reports_small_capacity() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.legacy_prefix_complete());

    let mut small = vec![0u8; LEGACY_DAEMON_PREFIX_LEN - 1];
    let mut buffers = [IoSliceMut::new(small.as_mut_slice())];

    let err = sniffer
        .take_buffered_into_vectored(&mut buffers)
        .expect_err("insufficient vectored capacity should error");

    assert_eq!(err.required(), LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(err.available(), small.len());
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
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

#[test]
fn prologue_sniffer_take_buffered_remainder_returns_trailing_bytes() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let (decision, consumed) = sniffer
        .observe(LEGACY_DAEMON_PREFIX.as_bytes())
        .expect("buffer reservation succeeds");
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(consumed, LEGACY_DAEMON_PREFIX_LEN);
    assert!(sniffer.legacy_prefix_complete());

    let remainder = b" version payload";
    sniffer.buffered_storage_mut().extend_from_slice(remainder);

    let drained = sniffer.take_buffered_remainder();
    assert_eq!(drained, remainder);
    assert_eq!(sniffer.buffered(), LEGACY_DAEMON_PREFIX.as_bytes());
    assert_eq!(sniffer.decision(), Some(NegotiationPrologue::LegacyAscii));
    assert_eq!(sniffer.buffered_remainder(), b"");
}

#[test]
fn prologue_sniffer_take_buffered_remainder_handles_binary_negotiation() {
    let mut sniffer = NegotiationPrologueSniffer::new();
    let mut reader = Cursor::new(vec![0x7F, 0xAA, 0xBB, 0xCC]);

    let decision = sniffer
        .read_from(&mut reader)
        .expect("binary negotiation detection succeeds");
    assert_eq!(decision, NegotiationPrologue::Binary);

    sniffer
        .buffered_storage_mut()
        .extend_from_slice(&[0xAA, 0xBB, 0xCC]);

    let drained = sniffer.take_buffered_remainder();
    assert_eq!(drained, [0xAA, 0xBB, 0xCC]);
    assert_eq!(sniffer.buffered(), [0x7F]);
    assert!(sniffer.is_binary());
}
