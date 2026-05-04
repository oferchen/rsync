#[test]
fn with_segments_invokes_closure_with_rendered_bytes() {
    let message = Message::error(35, "timeout in data send")
        .with_role(Role::Receiver)
        .with_source(message_source!());

    let expected = message.to_bytes().unwrap();
    let mut collected = Vec::new();

    let value = message.with_segments(false, |segments| {
        for slice in segments {
            collected.extend_from_slice(slice.as_ref());
        }

        0xdead_beefu64
    });

    assert_eq!(value, 0xdead_beefu64);
    assert_eq!(collected, expected);
}

#[test]
fn with_segments_supports_newline_variants() {
    let message = Message::warning("vanished files detected").with_code(24);

    let mut collected = Vec::new();
    message.with_segments(true, |segments| {
        for slice in segments {
            collected.extend_from_slice(slice.as_ref());
        }
    });

    assert_eq!(collected, message.to_line_bytes().unwrap());
}

#[test]
fn with_segments_supports_reentrant_rendering() {
    let message = Message::warning("vanished files detected").with_code(24);
    let expected = message.to_bytes().expect("rendering into Vec never fails");

    message.with_segments(false, |segments| {
        let nested = message
            .to_bytes()
            .expect("rendering inside closure should not panic");
        assert_eq!(nested, expected);

        let flattened = segments.to_vec().expect("collecting segments never fails");
        assert_eq!(flattened, expected);
    });
}

#[test]
fn render_to_writer_with_scratch_matches_fresh_scratch() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut reused = Vec::new();
    message
        .render_to_writer_with_scratch(&mut scratch, &mut reused)
        .expect("writing into a vector never fails");

    let mut baseline = Vec::new();
    message
        .render_to_writer(&mut baseline)
        .expect("writing into a vector never fails");

    assert_eq!(reused, baseline);
}

#[test]
fn scratch_supports_sequential_messages() {
    let mut scratch = MessageScratch::new();
    let mut output = Vec::new();

    rsync_error!(23, "delta-transfer failure")
        .render_line_to_writer_with_scratch(&mut scratch, &mut output)
        .expect("writing into a vector never fails");

    rsync_warning!("some files vanished")
        .with_code(24)
        .render_line_to_writer_with_scratch(&mut scratch, &mut output)
        .expect("writing into a vector never fails");

    let rendered = String::from_utf8(output).expect("messages are UTF-8");
    assert!(rendered.lines().any(|line| line.contains("(code 23)")));
    assert!(rendered.lines().any(|line| line.contains("(code 24)")));
}

#[test]
fn message_segments_iterator_covers_all_bytes() {
    let message = Message::error(23, "delta-transfer failure")
        .with_role(Role::Receiver)
        .with_source(message_source!());
    let mut scratch = MessageScratch::new();

    let collected: Vec<u8> = {
        let segments = message.as_segments(&mut scratch, true);
        segments
            .iter()
            .flat_map(|slice| {
                let bytes: &[u8] = slice.as_ref();
                bytes.iter().copied()
            })
            .collect()
    };

    assert_eq!(collected, message.to_line_bytes().unwrap());
}

#[test]
fn message_segments_iter_bytes_matches_iter() {
    let message = Message::error(23, "delta-transfer failure")
        .with_role(Role::Sender)
        .with_source(message_source!());
    let mut scratch = MessageScratch::new();

    let segments = message.as_segments(&mut scratch, false);
    let via_iter: Vec<&[u8]> = segments.iter().map(|slice| slice.as_ref()).collect();
    let via_bytes: Vec<&[u8]> = segments.iter_bytes().collect();

    assert_eq!(via_bytes, via_iter);
}

#[test]
fn message_segments_iter_bytes_supports_double_ended_iteration() {
    let message = Message::warning("vanished").with_code(24);
    let mut scratch = MessageScratch::new();

    let segments = message.as_segments(&mut scratch, true);
    let forward: Vec<&[u8]> = segments.iter_bytes().collect();
    let reverse: Vec<&[u8]> = segments.iter_bytes().rev().collect();
    let expected_forward: Vec<&[u8]> = segments.iter().map(|slice| slice.as_ref()).collect();
    let expected_reverse: Vec<&[u8]> = expected_forward.iter().rev().copied().collect();

    assert_eq!(forward, expected_forward);
    assert_eq!(reverse, expected_reverse);
}

#[test]
fn message_segments_into_iterator_matches_iter() {
    let message = Message::error(12, "example failure")
        .with_role(Role::Sender)
        .with_source(message_source!());
    let mut scratch = MessageScratch::new();

    let segments = message.as_segments(&mut scratch, true);
    let via_method: Vec<usize> = segments.iter().map(|slice| slice.len()).collect();
    let via_into: Vec<usize> = (&segments).into_iter().map(|slice| slice.len()).collect();

    assert_eq!(via_method, via_into);
}

#[test]
fn message_segments_mut_iterator_covers_all_bytes() {
    let message = Message::error(24, "partial transfer").with_source(message_source!());
    let mut scratch = MessageScratch::new();

    let mut segments = message.as_segments(&mut scratch, false);
    let mut total_len = 0;

    for slice in &mut segments {
        let bytes: &[u8] = slice.as_ref();
        total_len += bytes.len();
    }

    assert_eq!(total_len, message.to_bytes().unwrap().len());
}

#[test]
fn message_segments_extend_vec_appends_bytes() {
    let message = Message::error(12, "example failure")
        .with_role(Role::Server)
        .with_source(message_source!());
    let mut scratch = MessageScratch::new();

    let segments = message.as_segments(&mut scratch, false);
    let mut buffer = b"prefix: ".to_vec();
    let prefix_len = buffer.len();
    let appended = segments
        .extend_vec(&mut buffer)
        .expect("Vec<u8> growth should succeed for small messages");

    assert_eq!(&buffer[..prefix_len], b"prefix: ");
    assert_eq!(
        &buffer[prefix_len..],
        message.to_bytes().unwrap().as_slice()
    );
    assert_eq!(appended, message.to_bytes().unwrap().len());
}

#[test]
fn message_segments_extend_vec_noop_for_empty_segments() {
    let segments = MessageSegments {
        segments: [IoSlice::new(&[]); MAX_MESSAGE_SEGMENTS],
        count: 0,
        total_len: 0,
    };

    let mut buffer = b"static prefix".to_vec();
    let expected = buffer.clone();
    let capacity = buffer.capacity();

    let appended = segments
        .extend_vec(&mut buffer)
        .expect("empty segments should not alter the buffer");

    assert_eq!(appended, 0);
    assert_eq!(buffer, expected);
    assert_eq!(buffer.capacity(), capacity);
}

#[test]
fn message_segments_try_extend_vec_appends_bytes() {
    let message = Message::error(12, "example failure")
        .with_role(Role::Server)
        .with_source(message_source!());
    let mut scratch = MessageScratch::new();

    let segments = message.as_segments(&mut scratch, false);
    let mut buffer = b"prefix: ".to_vec();
    let prefix_len = buffer.len();
    let appended = segments
        .try_extend_vec(&mut buffer)
        .expect("Vec<u8> growth should succeed for small messages");

    assert_eq!(&buffer[..prefix_len], b"prefix: ");
    assert_eq!(
        &buffer[prefix_len..],
        message.to_bytes().unwrap().as_slice()
    );
    assert_eq!(appended, message.to_bytes().unwrap().len());
}

#[test]
fn message_segments_try_extend_vec_noop_for_empty_segments() {
    let segments = MessageSegments {
        segments: [IoSlice::new(&[]); MAX_MESSAGE_SEGMENTS],
        count: 0,
        total_len: 0,
    };

    let mut buffer = b"static prefix".to_vec();
    let expected = buffer.clone();
    let capacity = buffer.capacity();

    let appended = segments
        .try_extend_vec(&mut buffer)
        .expect("empty segments should not alter the buffer");

    assert_eq!(appended, 0);
    assert_eq!(buffer, expected);
    assert_eq!(buffer.capacity(), capacity);
}

#[test]
fn message_segments_copy_to_slice_copies_exact_bytes() {
    let message = Message::error(12, "example failure")
        .with_role(Role::Server)
        .with_source(message_source!());
    let mut scratch = MessageScratch::new();

    let segments = message.as_segments(&mut scratch, true);
    let mut buffer = vec![0u8; segments.len()];
    let copied = segments
        .copy_to_slice(&mut buffer)
        .expect("buffer is large enough");

    assert_eq!(copied, segments.len());
    assert_eq!(buffer, message.to_line_bytes().unwrap());
}

#[test]
fn message_segments_copy_to_slice_reports_required_length() {
    let message = Message::warning("vanished").with_code(24);
    let mut scratch = MessageScratch::new();
    let segments = message.as_segments(&mut scratch, false);
    let mut buffer = vec![0u8; segments.len().saturating_sub(1)];

    let err = segments
        .copy_to_slice(&mut buffer)
        .expect_err("buffer is intentionally undersized");

    assert_eq!(err.required(), segments.len());
    assert_eq!(err.provided(), buffer.len());
    assert_eq!(err.missing(), segments.len() - buffer.len());
}

#[test]
fn message_segments_copy_to_slice_accepts_empty_inputs() {
    let segments = MessageSegments {
        segments: [IoSlice::new(&[]); MAX_MESSAGE_SEGMENTS],
        count: 0,
        total_len: 0,
    };

    let mut buffer = [0u8; 0];
    let copied = segments
        .copy_to_slice(&mut buffer)
        .expect("empty segments succeed for empty buffers");

    assert_eq!(copied, 0);
}

#[test]
fn message_copy_to_slice_error_converts_into_io_error() {
    let message = Message::info("ready");
    let mut scratch = MessageScratch::new();
    let segments = message.as_segments(&mut scratch, false);

    let mut undersized = vec![0u8; segments.len().saturating_sub(1)];
    let err = segments
        .copy_to_slice(&mut undersized)
        .expect_err("buffer is intentionally undersized");
    let io_err: io::Error = err.into();

    assert_eq!(io_err.kind(), io::ErrorKind::InvalidInput);
    let display = io_err.to_string();
    assert_eq!(display, err.to_string());

    let inner = io_err
        .into_inner()
        .expect("conversion retains source error");
    let recovered = inner
        .downcast::<CopyToSliceError>()
        .expect("inner error matches original type");
    assert_eq!(*recovered, err);
}

#[test]
fn message_segments_is_empty_accounts_for_zero_length_segments() {
    let mut scratch = MessageScratch::new();
    let message = Message::info("ready");
    let populated = message.as_segments(&mut scratch, false);
    assert!(!populated.is_empty());

    let empty = MessageSegments {
        segments: [IoSlice::new(&[]); MAX_MESSAGE_SEGMENTS],
        count: 1,
        total_len: 0,
    };

    assert!(empty.is_empty());
}

#[test]
fn message_segments_to_vec_collects_bytes() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Receiver)
        .with_source(message_source!());
    let mut scratch = MessageScratch::new();

    let segments = message.as_segments(&mut scratch, false);
    let collected = segments
        .to_vec()
        .expect("allocating the rendered message succeeds");

    assert_eq!(collected, message.to_bytes().unwrap());
}

#[test]
fn message_segments_to_vec_respects_newline_flag() {
    let message = Message::warning("vanished file").with_code(24);
    let mut scratch = MessageScratch::new();

    let segments = message.as_segments(&mut scratch, true);
    let collected = segments
        .to_vec()
        .expect("allocating the rendered message succeeds");

    assert_eq!(collected, message.to_line_bytes().unwrap());
}

#[test]
fn render_line_to_appends_newline() {
    let message = Message::warning("soft limit reached");

    let mut rendered = String::new();
    message
        .render_line_to(&mut rendered)
        .expect("rendering into a string never fails");

    assert_eq!(rendered, format!("{message}\n"));
}

