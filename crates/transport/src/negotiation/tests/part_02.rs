#[test]
fn negotiation_buffer_copy_all_vectored_avoids_touching_extra_buffers() {
    let transcript = b"@RSYNCD:".to_vec();
    let buffer = NegotiationBuffer::new(LEGACY_DAEMON_PREFIX_LEN, 0, transcript.clone());

    let mut first = vec![0u8; 4];
    let mut second = vec![0u8; transcript.len() - first.len()];
    let mut extra = vec![0xAAu8; 6];

    let copied = {
        let mut bufs = [
            IoSliceMut::new(first.as_mut_slice()),
            IoSliceMut::new(second.as_mut_slice()),
            IoSliceMut::new(extra.as_mut_slice()),
        ];
        buffer
            .copy_all_into_vectored(&mut bufs)
            .expect("copy should succeed with ample capacity")
    };

    assert_eq!(copied, transcript.len());
    assert_eq!(first.as_slice(), &transcript[..first.len()]);
    assert_eq!(second.as_slice(), &transcript[first.len()..]);
    assert!(extra.iter().all(|byte| *byte == 0xAA));
}

#[test]
fn negotiation_buffer_copy_remaining_vectored_avoids_touching_extra_buffers() {
    let transcript = b"@RSYNCD: handshake".to_vec();
    let consumed = 3usize;
    let buffer = NegotiationBuffer::new(LEGACY_DAEMON_PREFIX_LEN, consumed, transcript.clone());
    let remaining_len = transcript.len() - consumed;

    let first_len = remaining_len / 2 + 1;
    let mut first = vec![0u8; first_len];
    let mut second = vec![0u8; remaining_len - first_len];
    let mut extra = vec![0xCCu8; 5];

    let copied = {
        let mut bufs = [
            IoSliceMut::new(first.as_mut_slice()),
            IoSliceMut::new(second.as_mut_slice()),
            IoSliceMut::new(extra.as_mut_slice()),
        ];
        buffer
            .copy_remaining_into_vectored(&mut bufs)
            .expect("copy should succeed with ample capacity")
    };

    assert_eq!(copied, remaining_len);
    assert_eq!(
        first.as_slice(),
        &transcript[consumed..consumed + first.len()]
    );
    assert_eq!(
        second.as_slice(),
        &transcript[consumed + first.len()..consumed + first.len() + second.len()]
    );
    assert!(extra.iter().all(|byte| *byte == 0xCC));
}

#[test]
fn negotiation_buffered_slices_into_iter_over_reference_yields_segments() {
    let prefix = b"@RSYNCD:";
    let remainder = b" reply";
    let slices = NegotiationBufferedSlices::new(prefix, remainder);

    let collected: Vec<u8> = (&slices)
        .into_iter()
        .flat_map(|slice| slice.as_ref().iter().copied())
        .collect();

    let mut expected = Vec::new();
    expected.extend_from_slice(prefix);
    expected.extend_from_slice(remainder);
    assert_eq!(collected, expected);
}

#[test]
fn negotiation_buffered_slices_into_iter_consumes_segments() {
    let prefix = b"@RSYNCD:";
    let remainder = b" banner";
    let slices = NegotiationBufferedSlices::new(prefix, remainder);

    let lengths: Vec<usize> = slices
        .into_iter()
        .map(|slice| slice.as_ref().len())
        .collect();

    assert_eq!(lengths, vec![prefix.len(), remainder.len()]);
}

#[test]
fn sniff_negotiation_detects_binary_prefix() {
    let mut stream = sniff_bytes(&[0x00, 0x12, 0x34]).expect("sniff succeeds");
    assert_eq!(stream.decision(), NegotiationPrologue::Binary);
    assert_eq!(stream.sniffed_prefix(), &[0x00]);
    assert_eq!(stream.sniffed_prefix_len(), 1);
    assert_eq!(stream.sniffed_prefix_remaining(), 1);
    assert!(!stream.legacy_prefix_complete());
    assert_eq!(stream.buffered_len(), 1);
    assert!(stream.buffered_remainder().is_empty());
    let (prefix, remainder) = stream.buffered_split();
    assert_eq!(prefix, &[0x00]);
    assert!(remainder.is_empty());

    let mut buf = [0u8; 3];
    stream
        .read_exact(&mut buf)
        .expect("read_exact drains buffered prefix and remainder");
    assert_eq!(&buf, &[0x00, 0x12, 0x34]);
    assert_eq!(stream.sniffed_prefix_remaining(), 0);
    assert!(!stream.legacy_prefix_complete());

    let mut tail = [0u8; 2];
    let read = stream
        .read(&mut tail)
        .expect("read after buffer consumes inner");
    assert_eq!(read, 0);
    assert!(tail.iter().all(|byte| *byte == 0));

    let parts = stream.into_parts();
    assert!(!parts.legacy_prefix_complete());
}

#[test]
fn ensure_decision_accepts_matching_style() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    stream
        .ensure_decision(
            NegotiationPrologue::LegacyAscii,
            "legacy daemon negotiation requires @RSYNCD: prefix",
        )
        .expect("legacy decision matches expectation");
}

#[test]
fn ensure_decision_rejects_mismatched_style() {
    let stream = sniff_bytes(&[0x00, 0x12, 0x34]).expect("sniff succeeds");
    let err = stream
        .ensure_decision(
            NegotiationPrologue::LegacyAscii,
            "legacy daemon negotiation requires @RSYNCD: prefix",
        )
        .expect_err("binary decision must be rejected");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        err.to_string(),
        "legacy daemon negotiation requires @RSYNCD: prefix"
    );
}

#[test]
fn ensure_decision_reports_undetermined_prologue() {
    let stream = NegotiatedStream::from_raw_components(
        Cursor::new(Vec::<u8>::new()),
        NegotiationPrologue::NeedMoreData,
        0,
        0,
        Vec::new(),
    );

    let err = stream
        .ensure_decision(
            NegotiationPrologue::Binary,
            "binary negotiation requires binary prologue",
        )
        .expect_err("undetermined prologue must surface as EOF");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(err.to_string(), NEGOTIATION_PROLOGUE_UNDETERMINED_MSG);
}

#[test]
fn negotiated_stream_reports_handshake_style_helpers() {
    let binary = sniff_bytes(&[0x00, 0x12, 0x34]).expect("sniff succeeds");
    assert!(binary.is_binary());
    assert!(!binary.is_legacy());

    let legacy = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    assert!(legacy.is_legacy());
    assert!(!legacy.is_binary());
}

#[test]
fn negotiated_stream_buffered_to_vec_matches_slice() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    let owned = stream.buffered_to_vec().expect("allocation succeeds");
    assert_eq!(owned.as_slice(), stream.buffered());
}

#[test]
fn negotiated_stream_buffered_remaining_to_vec_matches_slice() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    let owned = stream
        .buffered_remaining_to_vec()
        .expect("allocation succeeds");
    assert_eq!(owned.as_slice(), stream.buffered_remainder());
}

#[test]
fn negotiated_stream_parts_reports_handshake_style_helpers() {
    let binary_parts = sniff_bytes(&[0x00, 0x12, 0x34])
        .expect("sniff succeeds")
        .into_parts();
    assert!(binary_parts.is_binary());
    assert!(!binary_parts.is_legacy());

    let legacy_parts = sniff_bytes(b"@RSYNCD: 31.0\nrest")
        .expect("sniff succeeds")
        .into_parts();
    assert!(legacy_parts.is_legacy());
    assert!(!legacy_parts.is_binary());
}

#[test]
fn negotiated_stream_parts_buffered_to_vec_matches_slice() {
    let parts = sniff_bytes(b"@RSYNCD: 31.0\nrest")
        .expect("sniff succeeds")
        .into_parts();
    let owned = parts.buffered_to_vec().expect("allocation succeeds");
    assert_eq!(owned.as_slice(), parts.buffered());
}

#[test]
fn negotiated_stream_parts_buffered_remaining_to_vec_matches_slice() {
    let parts = sniff_bytes(b"@RSYNCD: 31.0\nrest")
        .expect("sniff succeeds")
        .into_parts();
    let owned = parts
        .buffered_remaining_to_vec()
        .expect("allocation succeeds");
    assert_eq!(owned.as_slice(), parts.buffered_remainder());
}

#[test]
fn buffered_consumed_tracks_reads() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nremaining").expect("sniff succeeds");
    assert_eq!(stream.buffered_consumed(), 0);

    let total = stream.buffered_len();
    assert!(total > 0);

    let mut remaining = total;
    let mut scratch = [0u8; 4];
    while remaining > 0 {
        let chunk = remaining.min(scratch.len());
        let read = stream
            .read(&mut scratch[..chunk])
            .expect("buffered bytes are readable");
        assert!(read > 0);
        remaining -= read;
        assert_eq!(stream.buffered_consumed(), total - remaining);
    }

    assert_eq!(stream.buffered_consumed(), total);
}

#[test]
fn negotiated_stream_buffered_consumed_slice_exposes_replayed_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    let transcript = stream.buffered().to_vec();
    assert!(stream.buffered_consumed_slice().is_empty());

    let mut prefix = vec![0u8; 4];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is readable");
    assert_eq!(stream.buffered_consumed_slice(), &transcript[..4]);

    let remaining = transcript.len().saturating_sub(4);
    if remaining > 0 {
        let mut tail = vec![0u8; remaining];
        stream
            .read_exact(&mut tail)
            .expect("remaining buffered bytes are readable");
    }

    assert_eq!(stream.buffered_consumed_slice(), transcript.as_slice());
}

#[test]
fn parts_buffered_consumed_matches_stream_state() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    let total = stream.buffered_len();
    assert!(total > 1);

    let mut prefix = vec![0u8; total - 1];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is replayed");
    assert_eq!(stream.buffered_consumed(), total - 1);

    let parts = stream.into_parts();
    assert_eq!(parts.buffered_consumed(), total - 1);
    assert_eq!(parts.buffered_remaining(), 1);
}

#[test]
fn negotiated_stream_parts_buffered_consumed_slice_reflects_progress() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    let transcript = stream.buffered().to_vec();

    let mut consumed_prefix = vec![0u8; 5.min(transcript.len())];
    stream
        .read_exact(&mut consumed_prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let parts = stream.clone().into_parts();
    assert_eq!(parts.buffered_consumed_slice(), &transcript[..consumed]);

    let remaining = transcript.len().saturating_sub(consumed);
    if remaining > 0 {
        let mut rest = vec![0u8; remaining];
        stream
            .read_exact(&mut rest)
            .expect("remaining buffered bytes are readable");
    }

    let parts = stream.into_parts();
    assert_eq!(parts.buffered_consumed_slice(), transcript.as_slice());
}

#[test]
fn negotiated_stream_buffered_remaining_slice_tracks_consumption() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");

    let expected = {
        let (prefix, remainder) = stream.buffered_split();
        let mut combined = Vec::new();
        combined.extend_from_slice(prefix);
        combined.extend_from_slice(remainder);
        combined
    };
    assert_eq!(stream.buffered_remaining_slice(), expected.as_slice());

    let mut prefix = [0u8; 5];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is replayed first");
    let expected_after = {
        let (prefix_slice, remainder_slice) = stream.buffered_split();
        let mut combined = Vec::new();
        combined.extend_from_slice(prefix_slice);
        combined.extend_from_slice(remainder_slice);
        combined
    };
    assert_eq!(stream.buffered_remaining_slice(), expected_after.as_slice());

    let mut remainder = Vec::new();
    stream
        .read_to_end(&mut remainder)
        .expect("remaining buffered bytes are replayed");
    let expected_final = {
        let (prefix_slice, remainder_slice) = stream.buffered_split();
        let mut combined = Vec::new();
        combined.extend_from_slice(prefix_slice);
        combined.extend_from_slice(remainder_slice);
        combined
    };
    assert_eq!(stream.buffered_remaining_slice(), expected_final.as_slice());
    assert!(expected_final.is_empty());
}

#[test]
fn parts_buffered_remaining_slice_matches_stream_state() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nreply").expect("sniff succeeds");
    let mut prefix = [0u8; 4];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is replayed");
    let parts = stream.into_parts();
    let expected_remaining = {
        let (prefix_slice, remainder_slice) = parts.buffered_split();
        let mut combined = Vec::new();
        combined.extend_from_slice(prefix_slice);
        combined.extend_from_slice(remainder_slice);
        combined
    };
    assert_eq!(
        parts.buffered_remaining_slice(),
        expected_remaining.as_slice()
    );

    let rebuilt = parts.into_stream();
    assert_eq!(
        rebuilt.buffered_remaining_slice(),
        expected_remaining.as_slice()
    );
}

#[test]
fn negotiated_stream_into_parts_trait_matches_method() {
    let mut via_method = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    let mut via_trait = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");

    let mut prefix = [0u8; 3];
    via_method
        .read_exact(&mut prefix)
        .expect("buffered prefix is replayed");
    via_trait
        .read_exact(&mut prefix)
        .expect("buffered prefix is replayed");

    let parts_method = via_method.into_parts();
    let parts_trait: NegotiatedStreamParts<_> = via_trait.into();

    assert_eq!(parts_method.decision(), parts_trait.decision());
    assert_eq!(parts_method.buffered(), parts_trait.buffered());
    assert_eq!(
        parts_method.buffered_consumed(),
        parts_trait.buffered_consumed()
    );
}

#[test]
fn negotiated_stream_parts_into_stream_trait_matches_method() {
    let data = b"@RSYNCD: 31.0\npayload";

    let mut expected_stream = sniff_bytes(data).expect("sniff succeeds");
    let mut expected_prefix = [0u8; 5];
    expected_stream
        .read_exact(&mut expected_prefix)
        .expect("buffered prefix is replayed");
    let mut expected_output = Vec::new();
    expected_stream
        .read_to_end(&mut expected_output)
        .expect("remainder is readable");

    let mut original = sniff_bytes(data).expect("sniff succeeds");
    let mut prefix = [0u8; 5];
    original
        .read_exact(&mut prefix)
        .expect("buffered prefix is replayed");
    let parts = original.into_parts();
    let clone = parts.clone();

    let mut via_method = clone.into_stream();
    let mut method_output = Vec::new();
    via_method
        .read_to_end(&mut method_output)
        .expect("method conversion retains bytes");

    let mut via_trait: NegotiatedStream<_> = parts.into();
    let mut trait_output = Vec::new();
    via_trait
        .read_to_end(&mut trait_output)
        .expect("trait conversion retains bytes");

    assert_eq!(method_output, expected_output);
    assert_eq!(trait_output, expected_output);
}

#[test]
fn sniffed_prefix_remaining_tracks_consumed_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 29.0\nrest").expect("sniff succeeds");
    assert_eq!(stream.sniffed_prefix_remaining(), LEGACY_DAEMON_PREFIX_LEN);
    assert!(stream.legacy_prefix_complete());

    let mut prefix_fragment = [0u8; 3];
    stream
        .read_exact(&mut prefix_fragment)
        .expect("prefix fragment is replayed first");
    assert_eq!(
        stream.sniffed_prefix_remaining(),
        LEGACY_DAEMON_PREFIX_LEN - prefix_fragment.len()
    );
    assert!(stream.legacy_prefix_complete());

    let remaining_len = LEGACY_DAEMON_PREFIX_LEN - prefix_fragment.len();
    let mut rest_of_prefix = vec![0u8; remaining_len];
    stream
        .read_exact(&mut rest_of_prefix)
        .expect("remaining prefix bytes are replayed");
    assert_eq!(stream.sniffed_prefix_remaining(), 0);
    assert_eq!(rest_of_prefix, b"YNCD:");
    assert!(stream.legacy_prefix_complete());
}

#[test]
fn sniffed_prefix_remaining_visible_on_parts() {
    let initial_parts = sniff_bytes(b"@RSYNCD: 31.0\n")
        .expect("sniff succeeds")
        .into_parts();
    assert_eq!(
        initial_parts.sniffed_prefix_remaining(),
        LEGACY_DAEMON_PREFIX_LEN
    );
    assert!(initial_parts.legacy_prefix_complete());

    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    let mut prefix_fragment = [0u8; 5];
    stream
        .read_exact(&mut prefix_fragment)
        .expect("prefix fragment is replayed");
    let parts = stream.into_parts();
    assert_eq!(
        parts.sniffed_prefix_remaining(),
        LEGACY_DAEMON_PREFIX_LEN - prefix_fragment.len()
    );
    assert!(parts.legacy_prefix_complete());
}

