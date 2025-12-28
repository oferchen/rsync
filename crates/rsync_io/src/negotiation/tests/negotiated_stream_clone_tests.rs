#[test]
fn negotiated_stream_try_clone_with_propagates_error_without_side_effects() {
    let legacy = b"@RSYNCD: 31.0\nrest";
    let mut stream = sniff_bytes(legacy).expect("sniff succeeds");
    let expected_len = stream.buffered_len();

    let err = stream
        .try_clone_with(|_| Err::<Cursor<Vec<u8>>, io::Error>(io::Error::other("boom")))
        .expect_err("clone should propagate errors");
    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert_eq!(stream.buffered_len(), expected_len);

    let mut replay = Vec::new();
    stream
        .read_to_end(&mut replay)
        .expect("original stream remains readable after failed clone");
    assert_eq!(replay, legacy);
}

#[test]
fn try_map_inner_on_parts_transforms_transport() {
    let legacy = b"@RSYNCD: 31.0\nrest";
    let parts = sniff_bytes(legacy).expect("sniff succeeds").into_parts();

    let mapped = parts
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Ok(RecordingTransport::from_cursor(cursor))
            },
        )
        .expect("mapping succeeds");

    let mut replay = Vec::new();
    mapped
        .into_stream()
        .read_to_end(&mut replay)
        .expect("stream reconstruction works");
    assert_eq!(replay, legacy);
}

#[test]
fn try_map_inner_on_parts_preserves_original_on_error() {
    let legacy = b"@RSYNCD: 31.0\n";
    let parts = sniff_bytes(legacy).expect("sniff succeeds").into_parts();

    let err = parts
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("boom"), cursor))
            },
        )
        .expect_err("mapping fails");

    assert_eq!(err.error().kind(), io::ErrorKind::Other);
    let mut original = err.into_original().into_stream();
    let mut replay = Vec::new();
    original
        .read_to_end(&mut replay)
        .expect("original stream still readable");
    assert_eq!(replay, legacy);
}

#[test]
fn negotiated_stream_parts_try_clone_with_clones_inner_reader() {
    let legacy = b"@RSYNCD: 30.0\nlisting";
    let parts = sniff_bytes(legacy).expect("sniff succeeds").into_parts();
    let expected_buffer = parts.buffered().to_vec();

    let cloned = parts
        .try_clone_with(|cursor| Ok::<Cursor<Vec<u8>>, io::Error>(cursor.clone()))
        .expect("cursor clone succeeds");

    assert_eq!(cloned.decision(), parts.decision());
    assert_eq!(cloned.buffered(), expected_buffer.as_slice());

    let mut cloned_stream = cloned.into_stream();
    let mut cloned_replay = Vec::new();
    cloned_stream
        .read_to_end(&mut cloned_replay)
        .expect("cloned parts replay buffered bytes");
    assert_eq!(cloned_replay, legacy);

    let mut original_stream = parts.into_stream();
    let mut original_replay = Vec::new();
    original_stream
        .read_to_end(&mut original_replay)
        .expect("original parts remain usable after clone");
    assert_eq!(original_replay, legacy);
}

#[test]
fn negotiated_stream_parts_try_clone_with_propagates_error_without_side_effects() {
    let legacy = b"@RSYNCD: 30.0\nlisting";
    let parts = sniff_bytes(legacy).expect("sniff succeeds").into_parts();
    let expected_len = parts.buffered_len();

    let err = parts
        .try_clone_with(|_| Err::<Cursor<Vec<u8>>, io::Error>(io::Error::other("boom")))
        .expect_err("clone should propagate errors");
    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert_eq!(parts.buffered_len(), expected_len);

    let mut original_stream = parts.into_stream();
    let mut replay = Vec::new();
    original_stream
        .read_to_end(&mut replay)
        .expect("parts remain convertible to stream after failed clone");
    assert_eq!(replay, legacy);
}

#[test]
fn try_map_inner_error_can_transform_error_without_losing_original() {
    let legacy = b"@RSYNCD: 31.0\n";
    let parts = sniff_bytes(legacy).expect("sniff succeeds").into_parts();

    let err = parts
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("boom"), cursor))
            },
        )
        .expect_err("mapping fails");

    let mapped = err.map_error(|error| error.kind());
    assert_eq!(*mapped.error(), io::ErrorKind::Other);

    let mut original = mapped.into_original().into_stream();
    let mut replay = Vec::new();
    original
        .read_to_end(&mut replay)
        .expect("mapped error preserves original stream");
    assert_eq!(replay, legacy);
}

#[test]
fn try_map_inner_error_map_original_transforms_preserved_value() {
    let legacy = b"@RSYNCD: 31.0\nmotd\n";
    let stream = sniff_bytes(legacy).expect("sniff succeeds");

    let err = stream
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("boom"), cursor))
            },
        )
        .expect_err("mapping fails");

    let mapped = err.map_original(NegotiatedStream::into_parts);
    assert_eq!(mapped.error().kind(), io::ErrorKind::Other);

    let parts = mapped.into_original();
    assert_eq!(parts.sniffed_prefix_len(), LEGACY_DAEMON_PREFIX_LEN);

    let mut rebuilt = parts.into_stream();
    let mut replay = Vec::new();
    rebuilt
        .read_to_end(&mut replay)
        .expect("mapped original preserves replay bytes");
    assert_eq!(replay, legacy);
}

#[test]
fn try_map_inner_error_map_parts_transforms_error_and_original() {
    let legacy = b"@RSYNCD: 31.0\nlisting";
    let stream = sniff_bytes(legacy).expect("sniff succeeds");

    let err = stream
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("boom"), cursor))
            },
        )
        .expect_err("mapping fails");

    let mapped = err.map_parts(|error, stream| (error.kind(), stream.into_parts()));
    assert_eq!(mapped.error(), &io::ErrorKind::Other);

    let parts = mapped.into_original();
    assert_eq!(parts.sniffed_prefix_len(), LEGACY_DAEMON_PREFIX_LEN);

    let mut rebuilt = parts.into_stream();
    let mut replay = Vec::new();
    rebuilt
        .read_to_end(&mut replay)
        .expect("mapped parts preserve replay bytes");
    assert_eq!(replay, legacy);
}

#[test]
fn try_map_inner_error_clone_preserves_components() {
    let parts = sniff_bytes(b"@RSYNCD: 31.0\nreply")
        .expect("sniff succeeds")
        .into_parts();
    let err = TryMapInnerError::new(String::from("clone"), parts.clone());

    let cloned = err.clone();
    assert_eq!(cloned.error(), "clone");
    assert_eq!(cloned.original().buffered(), parts.buffered());
    assert_eq!(err.original().buffered(), parts.buffered());
}

#[test]
fn try_map_inner_error_from_tuple_matches_constructor() {
    let parts = sniff_bytes(b"@RSYNCD: 31.0\nreply")
        .expect("sniff succeeds")
        .into_parts();
    let tuple_err: TryMapInnerError<_, _> = (io::Error::other("tuple"), parts.clone()).into();

    assert_eq!(tuple_err.error().kind(), io::ErrorKind::Other);
    assert_eq!(tuple_err.original().buffered(), parts.buffered());
}

#[test]
fn try_map_inner_error_into_tuple_recovers_parts() {
    let parts = sniff_bytes(b"@RSYNCD: 31.0\nreply")
        .expect("sniff succeeds")
        .into_parts();
    let err = TryMapInnerError::new(io::Error::other("into"), parts.clone());

    let (error, original): (io::Error, _) =
        TryMapInnerError::from((io::Error::other("shadow"), parts.clone())).into();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(original.buffered(), parts.buffered());

    let (error, original): (io::Error, _) = err.into();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(original.buffered(), parts.buffered());
}

#[test]
fn try_map_inner_error_display_mentions_original_type() {
    let legacy = b"@RSYNCD: 31.0\nlisting";
    let stream = sniff_bytes(legacy).expect("sniff succeeds");

    let err = stream
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("wrap failed"), cursor))
            },
        )
        .expect_err("mapping fails");

    let display = format!("{err}");
    assert!(display.contains("wrap failed"));
    assert!(display.contains("Cursor"));
    assert!(display.contains("original type"));

    let alternate = format!("{err:#}");
    assert!(alternate.contains("recover via into_original"));
    assert!(alternate.contains("Cursor"));
}

#[test]
fn try_map_inner_error_debug_reports_recovery_hint() {
    let legacy = b"@RSYNCD: 31.0\nlisting";
    let stream = sniff_bytes(legacy).expect("sniff succeeds");

    let err = stream
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("wrap failed"), cursor))
            },
        )
        .expect_err("mapping fails");

    let debug = format!("{err:?}");
    assert!(debug.contains("TryMapInnerError"));
    assert!(debug.contains("original_type"));
    assert!(debug.contains("Cursor"));

    let debug_alternate = format!("{err:#?}");
    assert!(debug_alternate.contains("recover"));
    assert!(debug_alternate.contains("Cursor"));
}

#[test]
fn try_map_inner_error_mut_accessors_preserve_state() {
    let legacy = b"@RSYNCD: 31.0\n";
    let stream = sniff_bytes(legacy).expect("sniff succeeds");

    let mut err = stream
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("boom"), cursor))
            },
        )
        .expect_err("mapping fails");

    *err.error_mut() = io::Error::new(io::ErrorKind::TimedOut, "timeout");
    assert_eq!(err.error().kind(), io::ErrorKind::TimedOut);

    {
        let original = err.original_mut();
        let mut first = [0u8; 1];
        original
            .read_exact(&mut first)
            .expect("reading from preserved stream succeeds");
        assert_eq!(&first, b"@");
    }

    let mut restored = err.into_original();
    let mut replay = Vec::new();
    restored
        .read_to_end(&mut replay)
        .expect("mutations persist when recovering the original stream");
    assert_eq!(replay, &legacy[1..]);
}

#[test]
fn try_map_inner_error_combined_accessors_expose_borrows() {
    let legacy = b"@RSYNCD: 31.0\npayload";
    let stream = sniff_bytes(legacy).expect("sniff succeeds");

    let mut err = stream
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("boom"), cursor))
            },
        )
        .expect_err("mapping fails");

    {
        let (error, original) = err.as_ref();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let (prefix, remainder) = original.buffered_split();
        assert_eq!(prefix, b"@RSYNCD:");
        assert!(remainder.is_empty());
    }

    {
        let (error, original) = err.as_mut();
        *error = io::Error::new(io::ErrorKind::TimedOut, "timeout");
        let mut first = [0u8; 1];
        original
            .read_exact(&mut first)
            .expect("reading from preserved stream succeeds");
        assert_eq!(&first, b"@");
    }

    assert_eq!(err.error().kind(), io::ErrorKind::TimedOut);

    let mut restored = err.into_original();
    let mut replay = Vec::new();
    restored
        .read_to_end(&mut replay)
        .expect("remaining bytes preserved after combined access");
    assert_eq!(replay, &legacy[1..]);
}

#[test]
fn try_map_inner_error_into_parts_returns_error_and_original() {
    let legacy = b"@RSYNCD: 31.0\nremainder";
    let stream = sniff_bytes(legacy).expect("sniff succeeds");

    let err = stream
        .try_map_inner(
            |cursor| -> Result<RecordingTransport, (io::Error, Cursor<Vec<u8>>)> {
                Err((io::Error::other("boom"), cursor))
            },
        )
        .expect_err("mapping fails");

    let (error, mut original) = err.into_parts();
    assert_eq!(error.kind(), io::ErrorKind::Other);

    let mut replay = Vec::new();
    original
        .read_to_end(&mut replay)
        .expect("replay remains available");
    assert_eq!(replay, legacy);
}

#[test]
fn sniff_negotiation_with_supplied_sniffer_reuses_internal_buffer() {
    let mut sniffer = NegotiationPrologueSniffer::new();

    {
        let mut stream = sniff_negotiation_stream_with_sniffer(
            Cursor::new(b"@RSYNCD: 31.0\nrest".to_vec()),
            &mut sniffer,
        )
        .expect("sniff succeeds");

        assert_eq!(stream.decision(), NegotiationPrologue::LegacyAscii);
        assert_eq!(stream.sniffed_prefix(), b"@RSYNCD:");

        let mut replay = Vec::new();
        stream
            .read_to_end(&mut replay)
            .expect("replay reads all bytes");
        assert_eq!(replay, b"@RSYNCD: 31.0\nrest");
    }

    assert_eq!(sniffer.buffered_len(), 0);
    assert_eq!(sniffer.sniffed_prefix_len(), 0);

    {
        let mut stream = sniff_negotiation_stream_with_sniffer(
            Cursor::new(vec![0x00, 0x12, 0x34, 0x56]),
            &mut sniffer,
        )
        .expect("sniff succeeds");

        assert_eq!(stream.decision(), NegotiationPrologue::Binary);
        assert_eq!(stream.sniffed_prefix(), &[0x00]);

        let mut replay = Vec::new();
        stream
            .read_to_end(&mut replay)
            .expect("binary replay drains reader");
        assert_eq!(replay, &[0x00, 0x12, 0x34, 0x56]);
    }

    assert_eq!(sniffer.buffered_len(), 0);
    assert_eq!(sniffer.sniffed_prefix_len(), 0);
}

#[test]
fn sniff_negotiation_buffered_split_exposes_prefix_and_remainder() {
    let cursor = Cursor::new(Vec::<u8>::new());
    let mut stream = NegotiatedStream::from_raw_components(
        cursor,
        NegotiationPrologue::Binary,
        1,
        0,
        vec![0x00, b'a', b'b', b'c'],
    );

    let (prefix, remainder) = stream.buffered_split();
    assert_eq!(prefix, &[0x00]);
    assert_eq!(remainder, b"abc");

    // Partially consume the prefix to ensure the tuple remains stable.
    let mut buf = [0u8; 1];
    stream
        .read_exact(&mut buf)
        .expect("read_exact consumes the buffered prefix");
    assert_eq!(buf, [0x00]);

    let (after_read_prefix, after_read_remainder) = stream.buffered_split();
    assert!(after_read_prefix.is_empty());
    assert_eq!(after_read_remainder, b"abc");

    let mut partial = [0u8; 2];
    stream
        .read_exact(&mut partial)
        .expect("read_exact drains part of the buffered remainder");
    assert_eq!(&partial, b"ab");
    assert_eq!(stream.buffered_remainder(), b"c");

    let mut final_byte = [0u8; 1];
    stream
        .read_exact(&mut final_byte)
        .expect("read_exact consumes the last buffered byte");
    assert_eq!(&final_byte, b"c");
    assert!(stream.buffered_remainder().is_empty());

    let (after_read_prefix, after_read_remainder) = stream.buffered_split();
    assert!(after_read_prefix.is_empty());
    assert!(after_read_remainder.is_empty());
}

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

