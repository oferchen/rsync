#[test]
fn negotiated_stream_copy_buffered_remaining_into_slice_copies_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nreplay").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut prefix = [0u8; 4];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let mut scratch = [0u8; 64];
    let copied = stream
        .copy_buffered_remaining_into_slice(&mut scratch)
        .expect("copying remaining bytes succeeds");

    assert_eq!(copied, expected.len() - consumed);
    assert_eq!(&scratch[..copied], &expected[consumed..]);
    assert_eq!(stream.buffered_consumed(), consumed);
}

#[test]
fn negotiated_stream_copy_buffered_into_vec_copies_bytes() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\nreplay").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut target = Vec::with_capacity(expected.len() + 8);
    target.extend_from_slice(b"junk data");
    let initial_capacity = target.capacity();
    let initial_ptr = target.as_ptr();

    let copied = stream
        .copy_buffered_into_vec(&mut target)
        .expect("copying into vec succeeds");

    assert_eq!(copied, expected.len());
    assert_eq!(target, expected);
    assert_eq!(target.capacity(), initial_capacity);
    assert_eq!(target.as_ptr(), initial_ptr);
}

#[test]
fn negotiated_stream_copy_buffered_remaining_into_vec_copies_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nreplay").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut prefix = [0u8; 4];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let mut target = Vec::new();
    let copied = stream
        .copy_buffered_remaining_into_vec(&mut target)
        .expect("copying remaining bytes succeeds");

    assert_eq!(copied, expected.len() - consumed);
    assert_eq!(target, &expected[consumed..]);
    assert_eq!(stream.buffered_consumed(), consumed);
}

#[test]
fn negotiated_stream_copy_helpers_accept_empty_buffers() {
    let stream = NegotiatedStream::from_raw_parts(
        Cursor::new(Vec::<u8>::new()),
        NegotiationPrologue::Binary,
        0,
        0,
        Vec::new(),
    );

    let mut cleared = vec![0xAA];
    let copied_vec = stream
        .copy_buffered_into_vec(&mut cleared)
        .expect("copying into vec succeeds");
    assert_eq!(copied_vec, 0);
    assert!(cleared.is_empty());

    let mut replaced = vec![0xBB];
    let copied = stream
        .copy_buffered_into(&mut replaced)
        .expect("copying into vec succeeds");
    assert_eq!(copied, 0);
    assert!(replaced.is_empty());

    let mut appended = b"log".to_vec();
    let extended = stream
        .extend_buffered_into_vec(&mut appended)
        .expect("extending into vec succeeds");
    assert_eq!(extended, 0);
    assert_eq!(appended, b"log");

    let mut slice = [0xCC; 4];
    let copied_slice = stream
        .copy_buffered_into_slice(&mut slice)
        .expect("copying into slice succeeds");
    assert_eq!(copied_slice, 0);
    assert_eq!(slice, [0xCC; 4]);

    let mut array = [0xDD; 2];
    let copied_array = stream
        .copy_buffered_into_array(&mut array)
        .expect("copying into array succeeds");
    assert_eq!(copied_array, 0);
    assert_eq!(array, [0xDD; 2]);

    let mut vectored = [IoSliceMut::new(&mut slice[..])];
    let copied_vectored = stream
        .copy_buffered_into_vectored(&mut vectored)
        .expect("empty buffer copies successfully");
    assert_eq!(copied_vectored, 0);
    assert_eq!(slice, [0xCC; 4]);

    let mut output = Vec::new();
    let written = stream
        .copy_buffered_into_writer(&mut output)
        .expect("writing into vec succeeds");
    assert_eq!(written, 0);
    assert!(output.is_empty());
}

#[test]
fn negotiated_stream_extend_buffered_into_vec_appends_bytes() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\nextend").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();
    let buffered_remaining = stream.buffered_remaining();

    let mut target = b"prefix: ".to_vec();
    let initial_capacity = target.capacity();

    let appended = stream
        .extend_buffered_into_vec(&mut target)
        .expect("vector can reserve space for replay bytes");

    assert_eq!(appended, expected.len());
    assert!(target.capacity() >= initial_capacity);
    let mut expected_target = b"prefix: ".to_vec();
    expected_target.extend_from_slice(&expected);
    assert_eq!(target, expected_target);
    assert_eq!(stream.buffered_remaining(), buffered_remaining);
}

#[test]
fn negotiated_stream_extend_buffered_remaining_into_vec_appends_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nextend").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut prefix = [0u8; 8];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let mut target = b"prefix: ".to_vec();
    let appended = stream
        .extend_buffered_remaining_into_vec(&mut target)
        .expect("extending remaining bytes succeeds");

    assert_eq!(appended, expected.len() - consumed);
    let mut expected_target = b"prefix: ".to_vec();
    expected_target.extend_from_slice(&expected[consumed..]);
    assert_eq!(target, expected_target);
    assert_eq!(stream.buffered_consumed(), consumed);
}

#[test]
fn negotiated_stream_copy_buffered_into_array_copies_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\narray").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();
    let buffered_remaining = stream.buffered_remaining();

    let mut scratch = [0u8; 64];
    let copied = stream
        .copy_buffered_into_array(&mut scratch)
        .expect("copying into array succeeds");

    assert_eq!(copied, expected.len());
    assert_eq!(&scratch[..copied], expected.as_slice());
    assert_eq!(stream.buffered_remaining(), buffered_remaining);

    let mut replay = vec![0u8; expected.len()];
    stream
        .read_exact(&mut replay)
        .expect("buffered bytes remain available after array copy");
    assert_eq!(replay, expected);
}

#[test]
fn negotiated_stream_copy_buffered_into_vectored_copies_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nvectored payload").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();
    let buffered_remaining = stream.buffered_remaining();

    let mut prefix = [0u8; 12];
    let mut suffix = [0u8; 64];
    let mut bufs = [IoSliceMut::new(&mut prefix), IoSliceMut::new(&mut suffix)];
    let copied = stream
        .copy_buffered_into_vectored(&mut bufs)
        .expect("vectored copy succeeds");

    assert_eq!(copied, expected.len());

    let prefix_len = prefix.len().min(copied);
    let remainder_len = copied - prefix_len;
    let mut assembled = Vec::new();
    assembled.extend_from_slice(&prefix[..prefix_len]);
    if remainder_len > 0 {
        assembled.extend_from_slice(&suffix[..remainder_len]);
    }
    assert_eq!(assembled, expected);
    assert_eq!(stream.buffered_remaining(), buffered_remaining);

    let mut replay = vec![0u8; expected.len()];
    stream
        .read_exact(&mut replay)
        .expect("buffered bytes remain available after vectored copy");
    assert_eq!(replay, expected);
}

#[test]
fn negotiated_stream_copy_buffered_remaining_into_vectored_copies_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\nvectored payload").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut consumed_prefix = [0u8; 9];
    stream
        .read_exact(&mut consumed_prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let mut prefix = [0u8; 12];
    let mut suffix = [0u8; 64];
    let mut bufs = [IoSliceMut::new(&mut prefix), IoSliceMut::new(&mut suffix)];
    let copied = stream
        .copy_buffered_remaining_into_vectored(&mut bufs)
        .expect("vectored copy of remaining bytes succeeds");

    assert_eq!(copied, expected.len() - consumed);

    let prefix_len = prefix.len().min(copied);
    let remainder_len = copied - prefix_len;
    let mut assembled = Vec::new();
    assembled.extend_from_slice(&prefix[..prefix_len]);
    if remainder_len > 0 {
        assembled.extend_from_slice(&suffix[..remainder_len]);
    }
    assert_eq!(assembled, expected[consumed..]);
    assert_eq!(stream.buffered_consumed(), consumed);
}

#[test]
fn negotiated_stream_copy_buffered_into_vectored_reports_small_buffers() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\nshort").expect("sniff succeeds");
    let required = stream.buffered().len();

    let mut prefix = [0u8; 4];
    let mut suffix = [0u8; 3];
    let mut bufs = [IoSliceMut::new(&mut prefix), IoSliceMut::new(&mut suffix)];
    let err = stream
        .copy_buffered_into_vectored(&mut bufs)
        .expect_err("insufficient capacity must error");

    assert_eq!(err.required(), required);
    assert_eq!(err.provided(), prefix.len() + suffix.len());
    assert_eq!(err.missing(), required - (prefix.len() + suffix.len()));
}

#[test]
fn negotiated_stream_copy_buffered_into_vectored_preserves_buffers_on_error() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\nshort").expect("sniff succeeds");
    let buffered_remaining = stream.buffered_remaining();

    let mut prefix = [0x11u8; 4];
    let mut suffix = [0x22u8; 3];
    let original_prefix = prefix;
    let original_suffix = suffix;
    let mut bufs = [IoSliceMut::new(&mut prefix), IoSliceMut::new(&mut suffix)];

    stream
        .copy_buffered_into_vectored(&mut bufs)
        .expect_err("insufficient capacity must error");

    assert_eq!(prefix, original_prefix);
    assert_eq!(suffix, original_suffix);
    assert_eq!(stream.buffered_remaining(), buffered_remaining);
}

#[test]
fn negotiated_stream_copy_buffered_into_slice_reports_small_buffer() {
    let stream = sniff_bytes(b"@RSYNCD: 30.0\nrest").expect("sniff succeeds");
    let expected_len = stream.buffered_len();
    let buffered_remaining = stream.buffered_remaining();

    let mut scratch = vec![0u8; expected_len.saturating_sub(1)];
    let err = stream
        .copy_buffered_into_slice(&mut scratch)
        .expect_err("insufficient slice capacity must error");

    assert_eq!(err.required(), expected_len);
    assert_eq!(err.provided(), scratch.len());
    assert_eq!(err.missing(), expected_len - scratch.len());
    assert_eq!(stream.buffered_remaining(), buffered_remaining);
}

#[test]
fn negotiated_stream_copy_buffered_into_array_reports_small_array() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\nrest").expect("sniff succeeds");
    let expected_len = stream.buffered_len();
    let buffered_remaining = stream.buffered_remaining();

    let mut scratch = [0u8; 4];
    let err = stream
        .copy_buffered_into_array(&mut scratch)
        .expect_err("insufficient array capacity must error");

    assert_eq!(err.required(), expected_len);
    assert_eq!(err.provided(), scratch.len());
    assert_eq!(err.missing(), expected_len - scratch.len());
    assert_eq!(stream.buffered_remaining(), buffered_remaining);
}

#[test]
fn negotiated_stream_copy_buffered_into_writer_copies_bytes() {
    let stream = sniff_bytes(b"@RSYNCD: 31.0\npayload").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();
    let buffered_remaining = stream.buffered_remaining();

    let mut output = Vec::new();
    let written = stream
        .copy_buffered_into_writer(&mut output)
        .expect("writing buffered bytes succeeds");

    assert_eq!(written, expected.len());
    assert_eq!(output, expected);
    assert_eq!(stream.buffered_remaining(), buffered_remaining);
}

#[test]
fn negotiated_stream_copy_buffered_remaining_into_writer_copies_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 31.0\npayload").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut consumed_prefix = [0u8; 6];
    stream
        .read_exact(&mut consumed_prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let mut output = Vec::new();
    let written = stream
        .copy_buffered_remaining_into_writer(&mut output)
        .expect("writing remaining bytes succeeds");

    assert_eq!(written, expected.len() - consumed);
    assert_eq!(output, expected[consumed..]);
    assert_eq!(stream.buffered_consumed(), consumed);
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_preserves_replay_state() {
    let parts = sniff_bytes(b"@RSYNCD: 30.0\nleftovers")
        .expect("sniff succeeds")
        .into_parts();
    let expected = parts.buffered().to_vec();

    let mut scratch = Vec::with_capacity(1);
    scratch.extend_from_slice(b"junk");
    let copied = parts
        .copy_buffered_into(&mut scratch)
        .expect("copying buffered bytes succeeds");

    assert_eq!(copied, expected.len());
    assert_eq!(scratch, expected);

    let mut rebuilt = parts.into_stream();
    let mut replay = vec![0u8; expected.len()];
    rebuilt
        .read_exact(&mut replay)
        .expect("rebuilt stream still replays buffered bytes");
    assert_eq!(replay, expected);
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_slice_copies_bytes() {
    let parts = sniff_bytes(b"@RSYNCD: 30.0\nlisting")
        .expect("sniff succeeds")
        .into_parts();
    let expected = parts.buffered().to_vec();

    let mut scratch = vec![0u8; expected.len()];
    let copied = parts
        .copy_buffered_into_slice(&mut scratch)
        .expect("copying into slice succeeds");

    assert_eq!(copied, expected.len());
    assert_eq!(scratch, expected);

    let mut rebuilt = parts.into_stream();
    let mut replay = vec![0u8; expected.len()];
    rebuilt
        .read_exact(&mut replay)
        .expect("rebuilt stream still replays buffered bytes after slice copy");
    assert_eq!(replay, expected);
}

#[test]
fn negotiated_stream_parts_copy_buffered_remaining_into_slice_copies_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 30.0\nlisting").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut prefix = [0u8; 5];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let parts = stream.into_parts();

    let mut scratch = vec![0u8; expected.len()];
    let copied = parts
        .copy_buffered_remaining_into_slice(&mut scratch)
        .expect("copying remaining bytes succeeds");

    assert_eq!(copied, expected.len() - consumed);
    assert_eq!(&scratch[..copied], &expected[consumed..]);
    assert_eq!(parts.buffered_consumed(), consumed);
}

#[test]
fn negotiated_stream_parts_copy_buffered_into_vec_copies_bytes() {
    let parts = sniff_bytes(b"@RSYNCD: 30.0\nlisting")
        .expect("sniff succeeds")
        .into_parts();
    let expected = parts.buffered().to_vec();

    let vectored = parts.buffered_vectored();
    assert_eq!(vectored.len(), expected.len());

    let flattened: Vec<u8> = vectored
        .iter()
        .flat_map(|slice| slice.as_ref().iter().copied())
        .collect();
    assert_eq!(flattened, expected);

    let mut target = Vec::with_capacity(expected.len() + 8);
    target.extend_from_slice(b"junk data");
    let initial_capacity = target.capacity();
    let initial_ptr = target.as_ptr();

    let copied = parts
        .copy_buffered_into_vec(&mut target)
        .expect("copying into vec succeeds");

    assert_eq!(copied, expected.len());
    assert_eq!(target, expected);
    assert_eq!(target.capacity(), initial_capacity);
    assert_eq!(target.as_ptr(), initial_ptr);
}

#[test]
fn negotiated_stream_parts_copy_buffered_remaining_into_vec_copies_unread_bytes() {
    let mut stream = sniff_bytes(b"@RSYNCD: 30.0\nlisting").expect("sniff succeeds");
    let expected = stream.buffered().to_vec();

    let mut prefix = [0u8; 5];
    stream
        .read_exact(&mut prefix)
        .expect("buffered prefix is readable");
    let consumed = stream.buffered_consumed();

    let parts = stream.into_parts();

    let remaining_vectored = parts.buffered_remaining_vectored();
    assert_eq!(remaining_vectored.len(), expected.len() - consumed);

    let flattened: Vec<u8> = remaining_vectored
        .iter()
        .flat_map(|slice| slice.as_ref().iter().copied())
        .collect();
    assert_eq!(flattened, expected[consumed..]);

    let mut target = Vec::new();
    let copied = parts
        .copy_buffered_remaining_into_vec(&mut target)
        .expect("copying remaining bytes succeeds");

    assert_eq!(copied, expected.len() - consumed);
    assert_eq!(target, expected[consumed..]);
    assert_eq!(parts.buffered_consumed(), consumed);
}

