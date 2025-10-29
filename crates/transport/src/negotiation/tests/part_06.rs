#[test]
fn sniff_negotiation_buffered_vectored_matches_buffered_bytes() {
    let cursor = Cursor::new(Vec::<u8>::new());
    let stream = NegotiatedStream::from_raw_components(
        cursor,
        NegotiationPrologue::Binary,
        1,
        0,
        vec![0x00, b'a', b'b'],
    );

    let vectored = stream.buffered_vectored();
    assert_eq!(vectored.segment_count(), 2);
    assert_eq!(vectored.len(), stream.buffered().len());

    let collected: Vec<&[u8]> = vectored.iter().map(|slice| slice.as_ref()).collect();
    assert_eq!(collected, vec![&[0x00][..], &b"ab"[..]]);

    let flattened: Vec<u8> = vectored
        .iter()
        .flat_map(|slice| slice.as_ref().iter().copied())
        .collect();
    assert_eq!(flattened, stream.buffered());
}

#[test]
fn sniff_negotiation_buffered_remaining_vectored_tracks_consumption() {
    let cursor = Cursor::new(Vec::<u8>::new());
    let mut stream = NegotiatedStream::from_raw_components(
        cursor,
        NegotiationPrologue::Binary,
        2,
        0,
        vec![0xAA, 0xBB, b'x', b'y'],
    );

    let mut buf = [0u8; 1];
    stream
        .read_exact(&mut buf)
        .expect("read_exact consumes part of the prefix");
    assert_eq!(&buf, &[0xAA]);

    let remaining = stream.buffered_remaining_vectored();
    assert_eq!(remaining.segment_count(), 2);
    assert_eq!(remaining.len(), stream.buffered_remaining());

    let slices: Vec<&[u8]> = remaining.iter().map(|slice| slice.as_ref()).collect();
    assert_eq!(slices, vec![&[0xBB][..], &b"xy"[..]]);

    let flattened: Vec<u8> = remaining
        .iter()
        .flat_map(|slice| slice.as_ref().iter().copied())
        .collect();
    assert_eq!(flattened, stream.buffered_remaining_slice());
}

#[test]
fn sniff_negotiation_errors_on_empty_stream() {
    let err = sniff_bytes(&[]).expect_err("sniff should fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn sniffed_stream_supports_bufread_semantics() {
    let data = b"@RSYNCD: 32.0\nhello";
    let mut stream = sniff_bytes(data).expect("sniff succeeds");

    assert_eq!(stream.fill_buf().expect("fill_buf succeeds"), b"@RSYNCD:");
    stream.consume(LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(
        stream.fill_buf().expect("fill_buf after consume succeeds"),
        b" 32.0\nhello"
    );
    stream.consume(3);
    assert_eq!(
        stream.fill_buf().expect("fill_buf after partial consume"),
        b".0\nhello"
    );
}

#[test]
fn sniffed_stream_supports_writing_via_wrapper() {
    let transport = RecordingTransport::new(b"@RSYNCD: 31.0\nrest");
    let mut stream = sniff_negotiation_stream(transport).expect("sniff succeeds");

    stream
        .write_all(b"CLIENT\n")
        .expect("write forwards to inner transport");

    let vectored = [IoSlice::new(b"V1"), IoSlice::new(b"V2")];
    let written = stream
        .write_vectored(&vectored)
        .expect("vectored write forwards to inner transport");
    assert_eq!(written, 4);

    stream.flush().expect("flush forwards to inner transport");

    let mut line = Vec::new();
    stream
        .read_legacy_daemon_line(&mut line)
        .expect("legacy line remains readable after writes");
    assert_eq!(line, b"@RSYNCD: 31.0\n");

    let inner = stream.into_inner();
    assert_eq!(inner.writes(), b"CLIENT\nV1V2");
    assert_eq!(inner.flushes(), 1);
}

#[test]
fn sniffed_stream_supports_vectored_reads_from_buffer() {
    let data = b"@RSYNCD: 31.0\nrest";
    let mut stream = sniff_bytes(data).expect("sniff succeeds");

    let mut head = [0u8; 4];
    let mut tail = [0u8; 8];
    let mut bufs = [IoSliceMut::new(&mut head), IoSliceMut::new(&mut tail)];

    let read = stream
        .read_vectored(&mut bufs)
        .expect("vectored read drains buffered prefix");
    assert_eq!(read, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(&head, b"@RSY");

    let tail_prefix = read.saturating_sub(head.len());
    assert_eq!(&tail[..tail_prefix], b"NCD:");

    let mut remainder = Vec::new();
    stream
        .read_to_end(&mut remainder)
        .expect("remaining bytes are readable");
    assert_eq!(remainder, &data[read..]);
}

#[test]
fn vectored_reads_delegate_to_inner_after_buffer_is_drained() {
    let data = b"\x00rest";
    let mut stream = sniff_bytes(data).expect("sniff succeeds");

    let mut prefix_buf = [0u8; 1];
    let mut bufs = [IoSliceMut::new(&mut prefix_buf)];
    let read = stream
        .read_vectored(&mut bufs)
        .expect("vectored read captures sniffed prefix");
    assert_eq!(read, 1);
    assert_eq!(prefix_buf, [0x00]);

    let mut first = [0u8; 2];
    let mut second = [0u8; 8];
    let mut remainder_bufs = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];
    let remainder_read = stream
        .read_vectored(&mut remainder_bufs)
        .expect("vectored read forwards to inner reader");

    let mut remainder = Vec::new();
    remainder.extend_from_slice(&first[..first.len().min(remainder_read)]);
    if remainder_read > first.len() {
        let extra = (remainder_read - first.len()).min(second.len());
        remainder.extend_from_slice(&second[..extra]);
    }
    if remainder.len() < b"rest".len() {
        let mut tail = Vec::new();
        stream
            .read_to_end(&mut tail)
            .expect("consume any bytes left by the default vectored implementation");
        remainder.extend_from_slice(&tail);
    }
    assert_eq!(remainder, b"rest");
}

#[test]
fn vectored_reads_delegate_to_inner_even_without_specialized_support() {
    let data = b"\x00rest".to_vec();
    let mut stream =
        sniff_negotiation_stream(NonVectoredCursor::new(data)).expect("sniff succeeds");

    let mut prefix_buf = [0u8; 1];
    let mut prefix_vecs = [IoSliceMut::new(&mut prefix_buf)];
    let read = stream
        .read_vectored(&mut prefix_vecs)
        .expect("vectored read yields buffered prefix");
    assert_eq!(read, 1);
    assert_eq!(prefix_buf, [0x00]);

    let mut first = [0u8; 2];
    let mut second = [0u8; 8];
    let mut remainder_bufs = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];
    let remainder_read = stream
        .read_vectored(&mut remainder_bufs)
        .expect("vectored read falls back to inner read implementation");

    let mut remainder = Vec::new();
    remainder.extend_from_slice(&first[..first.len().min(remainder_read)]);
    if remainder_read > first.len() {
        let extra = (remainder_read - first.len()).min(second.len());
        remainder.extend_from_slice(&second[..extra]);
    }
    let mut tail = Vec::new();
    stream
        .read_to_end(&mut tail)
        .expect("consume any bytes left by the default vectored implementation");
    remainder.extend_from_slice(&tail);

    assert_eq!(remainder, b"rest");
}

#[test]
fn parts_structure_exposes_buffered_state() {
    let data = b"\x00more";
    let stream = sniff_bytes(data).expect("sniff succeeds");
    let parts = stream.into_parts();
    assert_eq!(parts.decision(), NegotiationPrologue::Binary);
    assert_eq!(parts.sniffed_prefix(), b"\x00");
    assert!(parts.buffered_remainder().is_empty());
    assert_eq!(parts.buffered_len(), parts.sniffed_prefix_len());
    assert_eq!(parts.buffered_remaining(), parts.sniffed_prefix_len());
    let (prefix, remainder) = parts.buffered_split();
    assert_eq!(prefix, b"\x00");
    assert!(remainder.is_empty());
}

#[test]
fn negotiated_stream_clone_preserves_buffer_progress() {
    let data = b"@RSYNCD: 30.0\nrest";
    let mut stream = sniff_bytes(data).expect("sniff succeeds");

    let mut prefix = [0u8; 5];
    stream
        .read_exact(&mut prefix)
        .expect("read_exact consumes part of the buffered prefix");
    assert_eq!(&prefix, b"@RSYN");

    let remaining_before_clone = stream.buffered_remaining();
    let consumed_before_clone = stream.buffered_consumed();

    let mut cloned = stream.clone();
    assert_eq!(cloned.decision(), stream.decision());
    assert_eq!(cloned.buffered_remaining(), remaining_before_clone);
    assert_eq!(cloned.buffered_consumed(), consumed_before_clone);
    assert_eq!(
        cloned.sniffed_prefix_remaining(),
        stream.sniffed_prefix_remaining()
    );

    let mut cloned_replay = Vec::new();
    cloned
        .read_to_end(&mut cloned_replay)
        .expect("cloned stream replays remaining bytes");

    assert_eq!(stream.buffered_remaining(), remaining_before_clone);
    assert_eq!(stream.buffered_consumed(), consumed_before_clone);

    let mut original_replay = Vec::new();
    stream
        .read_to_end(&mut original_replay)
        .expect("original stream still replays bytes");

    assert_eq!(cloned_replay, original_replay);
}

#[test]
fn parts_can_be_cloned_without_sharing_state() {
    let data = b"@RSYNCD: 30.0\nrest";
    let mut stream = sniff_bytes(data).expect("sniff succeeds");

    let mut prefix_fragment = [0u8; 3];
    stream
        .read_exact(&mut prefix_fragment)
        .expect("read_exact consumes part of the buffered prefix");
    assert_eq!(&prefix_fragment, b"@RS");

    let parts = stream.into_parts();
    let cloned = parts.clone();

    assert_eq!(cloned.decision(), parts.decision());
    assert_eq!(cloned.sniffed_prefix(), parts.sniffed_prefix());
    assert_eq!(cloned.buffered_remainder(), parts.buffered_remainder());
    assert_eq!(cloned.buffered_remaining(), parts.buffered_remaining());

    let mut original_stream = parts.into_stream();
    let mut original_replay = Vec::new();
    original_stream
        .read_to_end(&mut original_replay)
        .expect("original stream replays buffered bytes");

    let mut cloned_stream = cloned.into_stream();
    let mut cloned_replay = Vec::new();
    cloned_stream
        .read_to_end(&mut cloned_replay)
        .expect("cloned stream replays its buffered bytes");

    assert_eq!(original_replay, cloned_replay);
}

#[test]
fn parts_can_be_rehydrated_without_rewinding_consumed_bytes() {
    let data = b"@RSYNCD: 29.0\nrest";
    let mut stream = sniff_bytes(data).expect("sniff succeeds");

    let mut prefix_chunk = [0u8; 4];
    stream
        .read_exact(&mut prefix_chunk)
        .expect("read_exact consumes part of the buffered prefix");
    assert_eq!(&prefix_chunk, b"@RSY");

    let parts = stream.into_parts();
    assert_eq!(
        parts.buffered_remaining(),
        LEGACY_DAEMON_PREFIX_LEN - prefix_chunk.len()
    );
    assert_eq!(parts.buffered_len(), LEGACY_DAEMON_PREFIX_LEN);

    let mut rehydrated = NegotiatedStream::from_parts(parts);
    assert_eq!(
        rehydrated.buffered_remaining(),
        LEGACY_DAEMON_PREFIX_LEN - prefix_chunk.len()
    );

    let (rehydrated_prefix, rehydrated_remainder) = rehydrated.buffered_split();
    assert_eq!(rehydrated_prefix, b"NCD:");
    assert_eq!(rehydrated_remainder, rehydrated.buffered_remainder());

    let mut remainder = Vec::new();
    rehydrated
        .read_to_end(&mut remainder)
        .expect("reconstructed stream yields the remaining bytes");
    assert_eq!(remainder, b"NCD: 29.0\nrest");
}

#[test]
fn parts_rehydrate_sniffer_restores_binary_snapshot() {
    let data = [0x42, 0x10, 0x11, 0x12];
    let stream = sniff_bytes(&data).expect("sniff succeeds");
    let decision = stream.decision();
    assert_eq!(decision, NegotiationPrologue::Binary);
    let prefix_len = stream.sniffed_prefix_len();
    let mut snapshot = Vec::new();
    stream
        .copy_buffered_into_vec(&mut snapshot)
        .expect("snapshot capture succeeds");

    let parts = stream.into_parts();
    let mut sniffer = NegotiationPrologueSniffer::new();
    parts
        .rehydrate_sniffer(&mut sniffer)
        .expect("rehydration succeeds");

    assert_eq!(sniffer.buffered(), snapshot.as_slice());
    assert_eq!(sniffer.sniffed_prefix_len(), prefix_len);
    assert_eq!(
        sniffer
            .read_from(&mut Cursor::new(Vec::new()))
            .expect("cached decision is returned"),
        decision
    );
}

#[test]
fn parts_rehydrate_sniffer_preserves_partial_legacy_state() {
    let buffered = b"@RS".to_vec();
    let decision = NegotiationPrologue::LegacyAscii;
    let stream = NegotiatedStream::from_raw_components(
        Cursor::new(Vec::<u8>::new()),
        decision,
        buffered.len(),
        0,
        buffered.clone(),
    );
    assert!(stream.sniffed_prefix_remaining() > 0);
    let prefix_len = stream.sniffed_prefix_len();
    let mut snapshot = Vec::new();
    stream
        .copy_buffered_into_vec(&mut snapshot)
        .expect("snapshot capture succeeds");

    let parts = stream.into_parts();
    let expected_remaining = LEGACY_DAEMON_PREFIX_LEN.saturating_sub(prefix_len);
    let mut sniffer = NegotiationPrologueSniffer::new();
    parts
        .rehydrate_sniffer(&mut sniffer)
        .expect("rehydration succeeds");

    assert_eq!(sniffer.buffered(), snapshot.as_slice());
    assert_eq!(sniffer.sniffed_prefix_len(), prefix_len);
    assert_eq!(sniffer.decision(), Some(decision));
    let expected_remaining_opt = (expected_remaining > 0).then_some(expected_remaining);
    assert_eq!(sniffer.legacy_prefix_remaining(), expected_remaining_opt);
    assert_eq!(
        sniffer.requires_more_data(),
        expected_remaining_opt.is_some()
    );
}

#[test]
fn raw_parts_round_trip_binary_state() {
    let data = [0x00, 0x12, 0x34, 0x56];
    let stream = sniff_bytes(&data).expect("sniff succeeds");
    let expected_decision = stream.decision();
    assert_eq!(expected_decision, NegotiationPrologue::Binary);
    assert_eq!(stream.sniffed_prefix(), &[0x00]);

    let (decision, sniffed_prefix_len, buffered_pos, buffered, inner) = stream.into_raw_parts();
    assert_eq!(decision, expected_decision);
    assert_eq!(sniffed_prefix_len, 1);
    assert_eq!(buffered_pos, 0);
    assert_eq!(buffered, vec![0x00]);

    let mut reconstructed = NegotiatedStream::from_raw_parts(
        inner,
        decision,
        sniffed_prefix_len,
        buffered_pos,
        buffered,
    );
    let mut replay = Vec::new();
    reconstructed
        .read_to_end(&mut replay)
        .expect("reconstructed stream replays buffered prefix and remainder");
    assert_eq!(replay, data);
}

#[test]
fn raw_parts_preserve_consumed_progress() {
    let data = b"@RSYNCD: 31.0\nrest";
    let mut stream = sniff_bytes(data).expect("sniff succeeds");
    assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);

    let mut consumed = [0u8; 3];
    stream
        .read_exact(&mut consumed)
        .expect("prefix consumption succeeds");
    assert_eq!(&consumed, b"@RS");

    let (decision, sniffed_prefix_len, buffered_pos, buffered, inner) = stream.into_raw_parts();
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(sniffed_prefix_len, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(buffered_pos, consumed.len());
    assert_eq!(buffered, b"@RSYNCD:".to_vec());

    let mut reconstructed = NegotiatedStream::from_raw_parts(
        inner,
        decision,
        sniffed_prefix_len,
        buffered_pos,
        buffered,
    );
    let mut remainder = Vec::new();
    reconstructed
        .read_to_end(&mut remainder)
        .expect("reconstructed stream resumes after consumed prefix");
    assert_eq!(remainder, b"YNCD: 31.0\nrest");

    let mut combined = Vec::new();
    combined.extend_from_slice(&consumed);
    combined.extend_from_slice(&remainder);
    assert_eq!(combined, data);
}

#[test]
fn raw_parts_round_trip_legacy_state() {
    let data = b"@RSYNCD: 32.0\nrest";
    let stream = sniff_bytes(data).expect("sniff succeeds");
    assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
    assert_eq!(stream.sniffed_prefix(), b"@RSYNCD:");

    let (decision, sniffed_prefix_len, buffered_pos, buffered, inner) = stream.into_raw_parts();
    assert_eq!(decision, NegotiationPrologue::LegacyAscii);
    assert_eq!(sniffed_prefix_len, LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(buffered_pos, 0);
    assert_eq!(buffered, b"@RSYNCD:".to_vec());

    let mut reconstructed = NegotiatedStream::from_raw_parts(
        inner,
        decision,
        sniffed_prefix_len,
        buffered_pos,
        buffered,
    );
    let mut replay = Vec::new();
    reconstructed
        .read_to_end(&mut replay)
        .expect("reconstructed stream replays full buffer");
    assert_eq!(replay, data);
}

