#[test]
fn rsync_exit_code_macro_returns_message_for_known_code() {
    let message = rsync_exit_code!(23).expect("exit code 23 is defined");

    assert_eq!(message.severity(), Severity::Error);
    assert_eq!(message.code(), Some(23));
    assert!(
        message
            .text()
            .contains("some files/attrs were not transferred")
    );
    assert!(message.source().is_some());
}

#[test]
fn rsync_exit_code_macro_returns_none_for_unknown_code() {
    assert!(rsync_exit_code!(7).is_none());
}

#[test]
fn rsync_exit_code_macro_honors_track_caller() {
    let expected_line = line!() + 1;
    let message = tracked_rsync_exit_code_macro();
    let source = message.source().expect("macro records source location");

    assert_eq!(source.line(), expected_line);
    assert!(
        source.path().starts_with(TESTS_DIR),
        "expected {} to start with {}",
        source.path(),
        TESTS_DIR
    );
}

#[test]
fn append_normalized_os_str_rewrites_backslashes() {
    let mut rendered = String::from("prefix/");
    append_normalized_os_str(&mut rendered, OsStr::new(r"dir\file.txt"));

    assert_eq!(rendered, "prefix/dir/file.txt");
}

#[test]
fn append_normalized_os_str_preserves_existing_forward_slashes() {
    let mut rendered = String::new();
    append_normalized_os_str(&mut rendered, OsStr::new("dir/sub"));

    assert_eq!(rendered, "dir/sub");
}

#[test]
fn append_normalized_os_str_handles_unc_prefixes() {
    let mut rendered = String::new();
    append_normalized_os_str(&mut rendered, OsStr::new(r"\\server\share\path"));

    assert_eq!(rendered, "//server/share/path");
}

#[test]
fn append_normalized_os_str_preserves_trailing_backslash() {
    let mut rendered = String::new();
    append_normalized_os_str(&mut rendered, OsStr::new(r#"C:\path\to\dir\"#));

    assert_eq!(rendered, "C:/path/to/dir/");
}

#[derive(Default)]
struct TrackingWriter {
    written: Vec<u8>,
    vectored_calls: usize,
    unsupported_once: bool,
    always_unsupported: bool,
    vectored_limit: Option<usize>,
}

impl TrackingWriter {
    fn with_unsupported_once() -> Self {
        Self {
            unsupported_once: true,
            ..Self::default()
        }
    }

    fn with_always_unsupported() -> Self {
        Self {
            always_unsupported: true,
            ..Self::default()
        }
    }

    fn with_vectored_limit(limit: usize) -> Self {
        Self {
            vectored_limit: Some(limit),
            ..Self::default()
        }
    }
}

impl io::Write for TrackingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.vectored_calls += 1;

        if self.unsupported_once {
            self.unsupported_once = false;
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no vectored support",
            ));
        }

        if self.always_unsupported {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no vectored support",
            ));
        }

        let mut limit = self.vectored_limit.unwrap_or(usize::MAX);
        let mut total = 0usize;
        for buf in bufs {
            if limit == 0 {
                break;
            }

            let slice: &[u8] = buf.as_ref();
            let take = slice.len().min(limit);
            self.written.extend_from_slice(&slice[..take]);
            total += take;
            limit -= take;

            if take < slice.len() {
                break;
            }
        }

        Ok(total)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct PartialThenUnsupportedWriter {
    written: Vec<u8>,
    vectored_calls: usize,
    fallback_writes: usize,
    limit: usize,
}

impl PartialThenUnsupportedWriter {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }
}

impl io::Write for PartialThenUnsupportedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.fallback_writes += 1;
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.vectored_calls += 1;

        if self.vectored_calls == 1 {
            let mut limit = self.limit;
            let mut total = 0usize;

            for buf in bufs {
                if limit == 0 {
                    break;
                }

                let slice: &[u8] = buf.as_ref();
                let take = slice.len().min(limit);
                self.written.extend_from_slice(&slice[..take]);
                total += take;
                limit -= take;

                if take < slice.len() {
                    break;
                }
            }

            return Ok(total);
        }

        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "vectored disabled after first call",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct OverreportingWriter {
    buffer: Vec<u8>,
}

impl io::Write for OverreportingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let mut total = 0usize;

        for buf in bufs {
            let slice: &[u8] = buf.as_ref();
            self.buffer.extend_from_slice(slice);
            total += slice.len();
        }

        Ok(total.saturating_add(1))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct ZeroProgressWriter {
    write_calls: usize,
}

impl io::Write for ZeroProgressWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_calls += 1;
        Ok(buf.len())
    }

    fn write_vectored(&mut self, _bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        Ok(0)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct LeadingEmptyAwareWriter {
    buffer: Vec<u8>,
    vectored_calls: usize,
    write_calls: usize,
}

impl io::Write for LeadingEmptyAwareWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_calls += 1;
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.vectored_calls += 1;

        if bufs.first().is_some_and(|slice| {
            let bytes: &[u8] = slice.as_ref();
            bytes.is_empty()
        }) {
            return Ok(0);
        }

        let mut total = 0;
        for slice in bufs {
            let bytes: &[u8] = slice.as_ref();
            self.buffer.extend_from_slice(bytes);
            total += bytes.len();
        }
        Ok(total)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn segments_write_to_prefers_vectored_io() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut writer = TrackingWriter::default();

    {
        let segments = message.as_segments(&mut scratch, true);
        segments
            .write_to(&mut writer)
            .expect("writing into a vector never fails");
    }

    assert_eq!(writer.written, message.to_line_bytes().unwrap());
    assert!(writer.vectored_calls >= 1);
}

#[test]
fn segments_write_to_skips_vectored_for_single_segment() {
    let message = Message::info("");
    let mut scratch = MessageScratch::new();
    let segments = message.as_segments(&mut scratch, false);

    assert_eq!(segments.segment_count(), 1);

    let mut writer = RecordingWriter::new();
    segments
        .write_to(&mut writer)
        .expect("single-segment writes succeed");

    assert_eq!(writer.vectored_calls, 0, "vectored path should be skipped");
    assert_eq!(writer.write_calls, 1, "single write_all call expected");
    assert_eq!(writer.buffer, message.to_bytes().unwrap());
}

#[test]
fn segments_write_to_falls_back_after_unsupported_vectored_call() {
    let message = Message::error(30, "timeout in data send/receive")
        .with_role(Role::Receiver)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut writer = TrackingWriter::with_unsupported_once();

    {
        let segments = message.as_segments(&mut scratch, false);
        segments
            .write_to(&mut writer)
            .expect("sequential fallback should succeed");
    }

    assert_eq!(writer.written, message.to_bytes().unwrap());
    assert_eq!(writer.vectored_calls, 1);
}

#[test]
fn segments_write_to_skips_leading_empty_slices_before_vectored_write() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut segments = message.as_segments(&mut scratch, false);

    let original_count = segments.count;
    assert!(original_count < MAX_MESSAGE_SEGMENTS);

    for index in (0..original_count).rev() {
        segments.segments[index + 1] = segments.segments[index];
    }
    segments.segments[0] = IoSlice::new(&[]);
    segments.count = original_count + 1;
    // The total length remains unchanged because the new segment is empty.

    let mut writer = LeadingEmptyAwareWriter::default();
    segments
        .write_to(&mut writer)
        .expect("leading empty slices should not trigger write_zero errors");

    assert_eq!(writer.buffer, message.to_bytes().unwrap());
    assert_eq!(
        writer.vectored_calls, 1,
        "vectored path should succeed once"
    );
    assert_eq!(writer.write_calls, 0, "no sequential fallback expected");
}

#[test]
fn segments_write_to_handles_persistent_unsupported_vectored_calls() {
    let message = Message::error(124, "remote shell failed")
        .with_role(Role::Client)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut writer = TrackingWriter::with_always_unsupported();

    {
        let segments = message.as_segments(&mut scratch, false);
        segments
            .write_to(&mut writer)
            .expect("sequential fallback should succeed");
    }

    assert_eq!(writer.written, message.to_bytes().unwrap());
    assert_eq!(writer.vectored_calls, 1);
}

#[test]
fn segments_write_to_errors_when_total_len_underreports_written_bytes() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut segments = message.as_segments(&mut scratch, false);
    assert!(
        segments.segment_count() > 1,
        "test requires multiple segments"
    );
    assert!(!segments.is_empty(), "message must contain bytes");

    segments.total_len = segments
        .total_len
        .checked_sub(1)
        .expect("total length should exceed one byte");

    let mut writer = TrackingWriter::with_always_unsupported();

    let err = segments
        .write_to(&mut writer)
        .expect_err("length mismatch must produce an error");

    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn segments_write_to_retries_after_partial_vectored_write() {
    let message = Message::error(35, "protocol generator aborted")
        .with_role(Role::Generator)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut writer = TrackingWriter::with_vectored_limit(8);

    {
        let segments = message.as_segments(&mut scratch, true);
        segments
            .write_to(&mut writer)
            .expect("partial vectored writes should succeed");
    }

    assert_eq!(writer.written, message.to_line_bytes().unwrap());
    assert!(writer.vectored_calls >= 2);
}

#[test]
fn segments_write_to_handles_partial_then_unsupported_vectored_call() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut writer = PartialThenUnsupportedWriter::new(8);

    {
        let segments = message.as_segments(&mut scratch, false);
        segments
            .write_to(&mut writer)
            .expect("sequential fallback should succeed after partial vectored writes");
    }

    assert_eq!(writer.written, message.to_bytes().unwrap());
    assert_eq!(writer.vectored_calls, 2);
    assert!(writer.fallback_writes >= 1);
}

#[test]
fn segments_write_to_handles_cross_slice_progress_before_unsupported_vectored_call() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut writer = PartialThenUnsupportedWriter::new(18);

    {
        let segments = message.as_segments(&mut scratch, false);
        segments
            .write_to(&mut writer)
            .expect("sequential fallback should succeed after cross-slice progress");
    }

    assert_eq!(writer.written, message.to_bytes().unwrap());
    assert_eq!(writer.vectored_calls, 2);
    assert!(writer.fallback_writes >= 1);
}

#[test]
fn segments_write_to_errors_when_vectored_makes_no_progress() {
    let message = Message::error(11, "error in file IO")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut writer = ZeroProgressWriter::default();

    let err = {
        let segments = message.as_segments(&mut scratch, false);
        segments
            .write_to(&mut writer)
            .expect_err("zero-length vectored write must error")
    };

    assert_eq!(err.kind(), io::ErrorKind::WriteZero);
    assert_eq!(writer.write_calls, 0, "sequential writes should not run");
}

#[test]
fn segments_write_to_errors_when_writer_overreports_progress() {
    let message = Message::error(23, "delta-transfer failure")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut scratch = MessageScratch::new();
    let mut writer = OverreportingWriter::default();

    let err = {
        let segments = message.as_segments(&mut scratch, false);
        segments
            .write_to(&mut writer)
            .expect_err("overreporting writer must trigger an error")
    };

    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert_eq!(writer.buffer, message.to_bytes().unwrap());
}
