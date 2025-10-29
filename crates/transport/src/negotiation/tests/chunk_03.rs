#[test]
fn negotiated_stream_parts_buffered_vectored_handles_manual_components() {
    let cursor = Cursor::new(Vec::<u8>::new());
    let stream = NegotiatedStream::from_raw_components(
        cursor,
        NegotiationPrologue::Binary,
        1,
        0,
        vec![0x11, b'm', b'n'],
    );

    let parts = stream.into_parts();
    let vectored = parts.buffered_vectored();
    assert_eq!(vectored.segment_count(), 2);
    let slices: Vec<&[u8]> = vectored.iter().map(|slice| slice.as_ref()).collect();
    assert_eq!(slices, vec![&[0x11][..], &b"mn"[..]]);
}

#[test]
fn negotiated_stream_parts_buffered_remaining_vectored_handles_consumed_prefix() {
    let cursor = Cursor::new(Vec::<u8>::new());
    let mut stream = NegotiatedStream::from_raw_components(
        cursor,
        NegotiationPrologue::Binary,
        2,
        0,
        vec![0x22, 0x33, b'p', b'q'],
    );

    let mut buf = [0u8; 1];
    stream
        .read_exact(&mut buf)
        .expect("reading consumes part of the prefix");
    assert_eq!(&buf, &[0x22]);

    let parts = stream.into_parts();
    let remaining = parts.buffered_remaining_vectored();
    assert_eq!(remaining.segment_count(), 2);
    let slices: Vec<&[u8]> = remaining.iter().map(|slice| slice.as_ref()).collect();
    assert_eq!(slices, vec![&[0x33][..], &b"pq"[..]]);
}

#[test]
fn negotiated_stream_parts_copy_helpers_accept_empty_buffers() {
    let stream = NegotiatedStream::from_raw_parts(
        Cursor::new(Vec::<u8>::new()),
        NegotiationPrologue::Binary,
        0,
        0,
        Vec::new(),
    );
    let parts = stream.into_parts();

    assert_eq!(parts.buffered_len(), 0);
    assert_eq!(parts.sniffed_prefix_len(), 0);
    assert!(parts.buffered().is_empty());

    let mut cleared = vec![0xAA];
    let copied_vec = parts
        .copy_buffered_into_vec(&mut cleared)
        .expect("copying into vec succeeds");
    assert_eq!(copied_vec, 0);
    assert!(cleared.is_empty());

    let mut replaced = vec![0xBB];
    let copied = parts
        .copy_buffered_into(&mut replaced)
        .expect("copying into vec succeeds");
    assert_eq!(copied, 0);
    assert!(replaced.is_empty());

    let mut appended = b"log".to_vec();
    let extended = parts
        .extend_buffered_into_vec(&mut appended)
        .expect("extending into vec succeeds");
    assert_eq!(extended, 0);
    assert_eq!(appended, b"log");

    let mut slice = [0xCC; 4];
    let copied_slice = parts
        .copy_buffered_into_slice(&mut slice)
        .expect("copying into slice succeeds");
    assert_eq!(copied_slice, 0);
    assert_eq!(slice, [0xCC; 4]);

    let mut array = [0xDD; 2];
    let copied_array = parts
        .copy_buffered_into_array(&mut array)
        .expect("copying into array succeeds");
    assert_eq!(copied_array, 0);
    assert_eq!(array, [0xDD; 2]);

    let mut vectored = [IoSliceMut::new(&mut slice[..])];
    let copied_vectored = parts
        .copy_buffered_into_vectored(&mut vectored)
        .expect("empty buffer copies successfully");
    assert_eq!(copied_vectored, 0);
    assert_eq!(slice, [0xCC; 4]);

    let mut output = Vec::new();
    let written = parts
        .copy_buffered_into_writer(&mut output)
        .expect("writing into vec succeeds");
    assert_eq!(written, 0);
    assert!(output.is_empty());
}

#[test]
fn negotiated_stream_parts_extend_buffered_into_vec_appends_bytes() {
    let parts = sniff_bytes(b"@RSYNCD: 30.0\next parts")
        .expect("sniff succeeds")
        .into_parts();
    let expected = parts.buffered().to_vec();
    let buffered_remaining = parts.buffered_remaining();

    let mut target = b"parts: ".to_vec();
    let initial_capacity = target.capacity();

    let appended = parts
        .extend_buffered_into_vec(&mut target)
        .expect("vector can reserve space for replay bytes");

    assert_eq!(appended, expected.len());
    assert!(target.capacity() >= initial_capacity);
    let mut expected_target = b"parts: ".to_vec();
    expected_target.extend_from_slice(&expected);
    assert_eq!(target, expected_target);
    assert_eq!(parts.buffered_remaining(), buffered_remaining);
}

#[test]
fn negotiated_stream_parts_extend_buffered_remaining_into_vec_appends_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 30.0\next parts").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut prefix = [0u8; 7];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let parts = stream.into_parts();

    let mut target = b"parts: ".to_vec();
    let appended = parts
        .extend_buffered_remaining_into_vec(&mut target)
        .expect("extending remaining bytes succeeds");

    assert_eq!(appended, expected.len() - consumed);
    let mut expected_target = b"parts: ".to_vec();
    expected_target.extend_from_slice(&expected[consumed..]);
    assert_eq!(target, expected_target);
    assert_eq!(parts.buffered_consumed(), consumed);
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_array_copies_bytes() {
    let parts = sniff_bytes(b"@RSYNCD: 30.0\nlisting")
        .expect("sniff succeeds")
        .into_parts();
    let expected = parts.buffered().to_vec();

    let mut scratch = [0u8; 64];
    let copied = parts
        .copy_buffered_into_array(&mut scratch)
        .expect("copying into array succeeds");

    assert_eq!(copied, expected.len());
    assert_eq!(&scratch[..copied], expected.as_slice());

    let mut rebuilt = parts.into_stream();
    let mut replay = vec![0u8; expected.len()];
    rebuilt
        .read_exact(&mut replay)
        .expect("rebuilt stream still replays buffered bytes after array copy");
    assert_eq!(replay, expected);
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_vectored_copies_bytes() {
    let parts = sniff_bytes(b"@RSYNCD: 30.0\nrecord")
        .expect("sniff succeeds")
        .into_parts();
    let expected = parts.buffered().to_vec();

    let mut first = [0u8; 10];
    let mut second = [0u8; 64];
    let mut bufs = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];
    let copied = parts
        .copy_buffered_into_vectored(&mut bufs)
        .expect("vectored copy succeeds");

    assert_eq!(copied, expected.len());

    let prefix_len = first.len().min(copied);
    let remainder_len = copied - prefix_len;
    let mut assembled = Vec::new();
    assembled.extend_from_slice(&first[..prefix_len]);
    if remainder_len > 0 {
        assembled.extend_from_slice(&second[..remainder_len]);
    }
    assert_eq!(assembled, expected);
}

#[test]
fn negotiated_stream_parts_copy_buffered_remaining_into_vectored_copies_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 30.0\nrecord").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut prefix_buf = [0u8; 4];
    stream
        .read_exact(&mut prefix_buf)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let parts = stream.into_parts();

    let mut first = [0u8; 10];
    let mut second = [0u8; 64];
    let mut bufs = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];
    let copied = parts
        .copy_buffered_remaining_into_vectored(&mut bufs)
        .expect("copying remaining bytes succeeds");

    assert_eq!(copied, expected.len() - consumed);

    let prefix_len = first.len().min(copied);
    let remainder_len = copied - prefix_len;
    let mut assembled = Vec::new();
    assembled.extend_from_slice(&first[..prefix_len]);
    if remainder_len > 0 {
        assembled.extend_from_slice(&second[..remainder_len]);
    }
    assert_eq!(assembled, expected[consumed..]);
    assert_eq!(parts.buffered_consumed(), consumed);
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_vectored_reports_small_buffers() {
    let parts = sniff_bytes(b"@RSYNCD: 31.0\nlimited")
        .expect("sniff succeeds")
        .into_parts();
    let expected_len = parts.buffered().len();

    let mut first = [0u8; 4];
    let mut second = [0u8; 3];
    let mut bufs = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];
    let err = parts
        .copy_buffered_into_vectored(&mut bufs)
        .expect_err("insufficient capacity must error");

    assert_eq!(err.required(), expected_len);
    assert_eq!(err.provided(), first.len() + second.len());
    assert_eq!(err.missing(), expected_len - (first.len() + second.len()));
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_vectored_preserves_buffers_on_error() {
    let parts = sniff_bytes(b"@RSYNCD: 31.0\nlimited")
        .expect("sniff succeeds")
        .into_parts();
    let buffered_snapshot = parts.buffered().to_vec();
    let consumed_before = parts.buffered_consumed();

    let mut first = [0x11u8; 4];
    let mut second = [0x22u8; 3];
    let original_first = first;
    let original_second = second;
    let mut bufs = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];

    parts
        .copy_buffered_into_vectored(&mut bufs)
        .expect_err("insufficient capacity must error");

    assert_eq!(first, original_first);
    assert_eq!(second, original_second);
    assert_eq!(parts.buffered(), buffered_snapshot.as_slice());
    assert_eq!(parts.buffered_consumed(), consumed_before);
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_slice_reports_small_buffer() {
    let parts = sniff_bytes(b"@RSYNCD: 31.0\nlisting")
        .expect("sniff succeeds")
        .into_parts();
    let expected_len = parts.buffered_len();

    let mut scratch = vec![0u8; expected_len.saturating_sub(1)];
    let err = parts
        .copy_buffered_into_slice(&mut scratch)
        .expect_err("insufficient slice capacity must error");

    assert_eq!(err.required(), expected_len);
    assert_eq!(err.provided(), scratch.len());
    assert_eq!(err.missing(), expected_len - scratch.len());
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_array_reports_small_array() {
    let parts = sniff_bytes(b"@RSYNCD: 31.0\nlisting")
        .expect("sniff succeeds")
        .into_parts();
    let expected_len = parts.buffered_len();

    let mut scratch = [0u8; 4];
    let err = parts
        .copy_buffered_into_array(&mut scratch)
        .expect_err("insufficient array capacity must error");

    assert_eq!(err.required(), expected_len);
    assert_eq!(err.provided(), scratch.len());
    assert_eq!(err.missing(), expected_len - scratch.len());
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_writer_copies_bytes() {
    let parts = sniff_bytes(b"@RSYNCD: 30.0\ntrailing")
        .expect("sniff succeeds")
        .into_parts();
    let expected = parts.buffered().to_vec();

    let mut output = Vec::new();
    let written = parts
        .copy_buffered_into_writer(&mut output)
        .expect("writing buffered bytes succeeds");

    assert_eq!(written, expected.len());
    assert_eq!(output, expected);

    let mut rebuilt = parts.into_stream();
    let mut replay = vec![0u8; expected.len()];
    rebuilt
        .read_exact(&mut replay)
        .expect("rebuilt stream still replays buffered bytes after writer copy");
    assert_eq!(replay, expected);
}

#[test]
fn negotiated_stream_parts_copy_buffered_remaining_into_writer_copies_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 30.0\ntrailing").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut prefix = [0u8; 4];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let parts = stream.into_parts();

    let mut output = Vec::new();
    let written = parts
        .copy_buffered_remaining_into_writer(&mut output)
        .expect("writing remaining bytes succeeds");

    assert_eq!(written, expected.len() - consumed);
    assert_eq!(output, expected[consumed..]);
    assert_eq!(parts.buffered_consumed(), consumed);
}

#[test]
fn legacy_prefix_complete_reports_status_for_legacy_sessions() {
    let mut stream = sniff_bytes(b"@RSYNCD: 30.0\nrest").expect("sniff succeeds");
    assert!(stream.legacy_prefix_complete());

    let mut consumed = [0u8; 4];
    stream
        .read_exact(&mut consumed)
        .expect("read_exact consumes part of the prefix");
    assert!(stream.legacy_prefix_complete());

    let parts = stream.into_parts();
    assert!(parts.legacy_prefix_complete());
}

#[test]
fn legacy_prefix_complete_reports_status_for_binary_sessions() {
    let mut stream = sniff_bytes(&[0x00, 0x42, 0x99]).expect("sniff succeeds");
    assert!(!stream.legacy_prefix_complete());

    let mut consumed = [0u8; 1];
    stream
        .read_exact(&mut consumed)
        .expect("read_exact consumes buffered byte");
    assert!(!stream.legacy_prefix_complete());

    let parts = stream.into_parts();
    assert!(!parts.legacy_prefix_complete());
}

#[test]
fn sniff_negotiation_detects_legacy_prefix_and_preserves_remainder() {
    let legacy = b"@RSYNCD: 31.0\n#list";
    let mut stream = sniff_bytes(legacy).expect("sniff succeeds");
    assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
    assert_eq!(stream.sniffed_prefix(), b"@RSYNCD:");
    assert_eq!(stream.sniffed_prefix_len(), LEGACY_DAEMON_PREFIX_LEN);
    assert_eq!(stream.buffered_len(), LEGACY_DAEMON_PREFIX_LEN);
    assert!(stream.buffered_remainder().is_empty());
    let (prefix, remainder) = stream.buffered_split();
    assert_eq!(prefix, b"@RSYNCD:");
    assert!(remainder.is_empty());

    let mut replay = Vec::new();
    stream
        .read_to_end(&mut replay)
        .expect("read_to_end succeeds");
    assert_eq!(replay, legacy);
}

#[test]
fn try_map_inner_transforms_transport_without_losing_buffer() {
    let legacy = b"@RSYNCD: 31.0\nrest";
    let stream = sniff_bytes(legacy).expect("sniff succeeds");

    let mut mapped = stream
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Ok(RecordingTransport::from_cursor(cursor))
            },
        )
        .expect("mapping succeeds");

    let mut replay = Vec::new();
    mapped
        .read_to_end(&mut replay)
        .expect("replay remains available");
    assert_eq!(replay, legacy);

    mapped.write_all(b"payload").expect("writes propagate");
    mapped.flush().expect("flush propagates");
    assert_eq!(mapped.inner().writes(), b"payload");
    assert_eq!(mapped.inner().flushes(), 1);
}

#[test]
fn try_map_inner_preserves_original_on_error() {
    let legacy = b"@RSYNCD: 31.0\n";
    let stream = sniff_bytes(legacy).expect("sniff succeeds");

    let err = stream
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("boom"), cursor))
            },
        )
        .expect_err("mapping fails");

    assert_eq!(err.error().kind(), io::ErrorKind::Other);
    let mut original = err.into_original();
    let mut replay = Vec::new();
    original
        .read_to_end(&mut replay)
        .expect("original stream still readable");
    assert_eq!(replay, legacy);
}

#[test]
fn negotiated_stream_try_clone_with_clones_inner_reader() {
    let legacy = b"@RSYNCD: 31.0\nreply";
    let mut stream = sniff_bytes(legacy).expect("sniff succeeds");
    let expected_buffer = stream.buffered().to_vec();

    let mut cloned = stream
        .try_clone_with(|cursor| Ok::<Cursor<Vec<u8>>, io::Error>(cursor.clone()))
        .expect("cursor clone succeeds");

    assert_eq!(cloned.decision(), stream.decision());
    assert_eq!(cloned.buffered(), expected_buffer.as_slice());

    let mut cloned_replay = Vec::new();
    cloned
        .read_to_end(&mut cloned_replay)
        .expect("cloned stream can replay handshake");
    assert_eq!(cloned_replay, legacy);

    let mut original_replay = Vec::new();
    stream
        .read_to_end(&mut original_replay)
        .expect("original stream remains usable after clone");
    assert_eq!(original_replay, legacy);
}

