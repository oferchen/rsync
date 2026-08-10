#[test]
fn segments_into_iter_respects_segment_count() {
    let mut scratch = MessageScratch::new();
    let message = Message::info("protocol negotiation complete");

    let segments = message.as_segments(&mut scratch, false);
    let iter = segments.clone().into_iter();

    assert_eq!(iter.count(), segments.segment_count());
}

struct RecordingWriter {
    buffer: Vec<u8>,
    vectored_calls: usize,
    write_calls: usize,
    vectored_limit: Option<usize>,
    supports_vectored: bool,
}

impl RecordingWriter {
    fn new() -> Self {
        Self {
            buffer: Vec::new(),
            vectored_calls: 0,
            write_calls: 0,
            vectored_limit: None,
            supports_vectored: true,
        }
    }

    fn with_vectored_limit(limit: usize) -> Self {
        let mut writer = Self::new();
        writer.vectored_limit = Some(limit);
        writer
    }

    fn without_vectored() -> Self {
        let mut writer = Self::new();
        writer.supports_vectored = false;
        writer
    }
}

impl IoWrite for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_calls += 1;
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        if !self.supports_vectored {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "vectored writes unsupported",
            ));
        }
        self.vectored_calls += 1;

        let mut to_write: usize = bufs.iter().map(|slice| slice.len()).sum();
        if let Some(limit) = self.vectored_limit {
            let capped = to_write.min(limit);
            self.vectored_limit = Some(limit.saturating_sub(capped));
            to_write = capped;

            if to_write == 0 {
                self.supports_vectored = false;
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "vectored limit reached",
                ));
            }
        }

        let mut remaining = to_write;
        for slice in bufs {
            if remaining == 0 {
                break;
            }

            let data: &[u8] = slice.as_ref();
            let portion = data.len().min(remaining);
            self.buffer.extend_from_slice(&data[..portion]);
            remaining -= portion;
        }

        Ok(to_write)
    }
}

#[test]
fn severity_as_str_matches_expected_labels() {
    assert_eq!(Severity::Info.as_str(), "info");
    assert_eq!(Severity::Warning.as_str(), "warning");
    assert_eq!(Severity::Error.as_str(), "error");
}

#[test]
fn severity_prefix_matches_expected_strings() {
    assert_eq!(Severity::Info.prefix(), "rsync info: ");
    assert_eq!(Severity::Warning.prefix(), "rsync warning: ");
    assert_eq!(Severity::Error.prefix(), "rsync error: ");
}

#[test]
fn severity_display_matches_as_str() {
    assert_eq!(Severity::Info.to_string(), "info");
    assert_eq!(Severity::Warning.to_string(), "warning");
    assert_eq!(Severity::Error.to_string(), "error");
}

#[test]
fn severity_predicates_match_variants() {
    assert!(Severity::Info.is_info());
    assert!(!Severity::Info.is_warning());
    assert!(!Severity::Info.is_error());

    assert!(Severity::Warning.is_warning());
    assert!(!Severity::Warning.is_info());
    assert!(!Severity::Warning.is_error());

    assert!(Severity::Error.is_error());
    assert!(!Severity::Error.is_info());
    assert!(!Severity::Error.is_warning());
}

#[test]
fn severity_from_str_parses_known_labels() {
    assert_eq!(Severity::from_str("info"), Ok(Severity::Info));
    assert_eq!(Severity::from_str("warning"), Ok(Severity::Warning));
    assert_eq!(Severity::from_str("error"), Ok(Severity::Error));
}

#[test]
fn severity_from_str_rejects_unknown_labels() {
    assert!(Severity::from_str("verbose").is_err());
}

#[test]
fn role_as_str_matches_expected_labels() {
    assert_eq!(Role::Sender.as_str(), "sender");
    assert_eq!(Role::Receiver.as_str(), "receiver");
    assert_eq!(Role::Generator.as_str(), "generator");
    assert_eq!(Role::Server.as_str(), "server");
    assert_eq!(Role::Client.as_str(), "client");
    assert_eq!(Role::Daemon.as_str(), "daemon");
}

#[test]
fn role_display_matches_as_str() {
    assert_eq!(Role::Sender.to_string(), "sender");
    assert_eq!(Role::Daemon.to_string(), "daemon");
}

#[test]
fn role_from_str_parses_known_labels() {
    assert_eq!(Role::from_str("sender"), Ok(Role::Sender));
    assert_eq!(Role::from_str("receiver"), Ok(Role::Receiver));
    assert_eq!(Role::from_str("generator"), Ok(Role::Generator));
    assert_eq!(Role::from_str("server"), Ok(Role::Server));
    assert_eq!(Role::from_str("client"), Ok(Role::Client));
    assert_eq!(Role::from_str("daemon"), Ok(Role::Daemon));
}

#[test]
fn role_from_str_rejects_unknown_labels() {
    assert!(Role::from_str("observer").is_err());
}

#[test]
fn role_all_lists_every_variant_once_in_canonical_order() {
    assert_eq!(
        Role::ALL,
        [
            Role::Sender,
            Role::Receiver,
            Role::Generator,
            Role::Server,
            Role::Client,
            Role::Daemon,
        ]
    );

    for (index, outer) in Role::ALL.iter().enumerate() {
        for inner in Role::ALL.iter().skip(index + 1) {
            assert_ne!(outer, inner, "Role::ALL must not contain duplicates");
        }
    }
}

#[test]
fn encode_unsigned_decimal_formats_expected_values() {
    let mut buf = [0u8; 8];
    assert_eq!(encode_unsigned_decimal(0, &mut buf), "0");
    assert_eq!(encode_unsigned_decimal(42, &mut buf), "42");
    assert_eq!(encode_unsigned_decimal(12_345_678, &mut buf), "12345678");
}

#[test]
fn encode_signed_decimal_handles_positive_and_negative_values() {
    let mut buf = [0u8; 12];
    assert_eq!(encode_signed_decimal(0, &mut buf), "0");
    assert_eq!(encode_signed_decimal(123, &mut buf), "123");
    assert_eq!(encode_signed_decimal(-456, &mut buf), "-456");
}

#[test]
fn encode_signed_decimal_formats_i64_minimum_value() {
    let mut buf = [0u8; 32];
    assert_eq!(
        encode_signed_decimal(i64::MIN, &mut buf),
        "-9223372036854775808"
    );
}

#[test]
fn render_to_writer_formats_minimum_exit_code() {
    let message = Message::error(i32::MIN, "integrity check failure")
        .with_role(Role::Sender)
        .with_source(message_source!());

    let mut buffer = Vec::new();
    message
        .render_to_writer(&mut buffer)
        .expect("rendering into a vector never fails");

    let rendered = String::from_utf8(buffer).expect("message renders as UTF-8");
    assert!(rendered.contains("(code -2147483648)"));
}

#[test]
fn rsync_error_macro_attaches_source_and_code() {
    let message = rsync_error!(23, "delta-transfer failure");

    assert_eq!(message.severity(), Severity::Error);
    assert_eq!(message.code(), Some(23));
    let source = message.source().expect("macro records source location");
    assert!(
        source.path().starts_with(TESTS_DIR),
        "expected {} to start with {}",
        source.path(),
        TESTS_DIR
    );
}

#[test]
fn rsync_error_macro_honors_track_caller() {
    let expected_line = line!() + 1;
    let message = tracked_rsync_error_macro();
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
fn rsync_warning_macro_supports_format_arguments() {
    let message = rsync_warning!("vanished {count} files", count = 2).with_code(24);

    assert_eq!(message.severity(), Severity::Warning);
    assert_eq!(message.code(), Some(24));
    assert_eq!(message.text(), "vanished 2 files");
}

#[test]
fn rsync_warning_macro_honors_track_caller() {
    let expected_line = line!() + 1;
    let message = tracked_rsync_warning_macro();
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
fn rsync_info_macro_attaches_source() {
    let message = rsync_info!("protocol {version} negotiated", version = 32);

    assert_eq!(message.severity(), Severity::Info);
    assert_eq!(message.code(), None);
    assert_eq!(message.text(), "protocol 32 negotiated");
    assert!(message.source().is_some());
}

#[test]
fn rsync_info_macro_honors_track_caller() {
    let expected_line = line!() + 1;
    let message = tracked_rsync_info_macro();
    let source = message.source().expect("macro records source location");

    assert_eq!(source.line(), expected_line);
    assert!(
        source.path().starts_with(TESTS_DIR),
        "expected {} to start with {}",
        source.path(),
        TESTS_DIR
    );
}

