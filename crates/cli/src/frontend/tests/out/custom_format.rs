//! Tests for --out-format custom output formatting.
//!
//! These tests verify:
//! 1. Format specifiers (%n, %f, %l, etc.) work correctly
//! 2. Escape sequences are handled
//! 3. Format matches upstream rsync behavior
//! 4. Invalid formats are handled appropriately

use super::*;

// ============================================================================
// Format Specifier Tests - Basic Placeholders
// ============================================================================

#[test]
fn out_format_filename_placeholder_renders_basename() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let source = src_dir.join("test_file.txt");
    std::fs::write(&source, b"content").expect("write source");
    let destination = dst_dir.join("test_file.txt");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data copy event");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%n"))
        .expect("parse %n")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %n");

    assert_eq!(String::from_utf8(output).expect("utf8"), "test_file.txt\n");
}

#[test]
fn out_format_full_path_placeholder_renders_complete_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let source = src_dir.join("file.dat");
    std::fs::write(&source, b"data").expect("write source");
    let destination = dst_dir.join("file.dat");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data copy event");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%f"))
        .expect("parse %f")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %f");

    let rendered = String::from_utf8(output).expect("utf8");
    assert!(rendered.contains("file.dat"));
}

#[test]
fn out_format_file_length_placeholder_shows_size() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let contents = b"12345678";
    let source = src_dir.join("sized.bin");
    std::fs::write(&source, contents).expect("write source");
    let destination = dst_dir.join("sized.bin");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data copy event");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%l"))
        .expect("parse %l")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %l");

    assert_eq!(String::from_utf8(output).expect("utf8"), "8\n");
}

#[test]
fn out_format_bytes_transferred_placeholder_shows_transferred() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let contents = vec![0xAA; 1024];
    let source = src_dir.join("transfer.bin");
    std::fs::write(&source, &contents).expect("write source");
    let destination = dst_dir.join("transfer.bin");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data copy event");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%b"))
        .expect("parse %b")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %b");

    assert_eq!(String::from_utf8(output).expect("utf8"), "1024\n");
}

#[test]
fn out_format_operation_placeholder_describes_event() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let source = src_dir.join("op.txt");
    std::fs::write(&source, b"test").expect("write source");
    let destination = dst_dir.join("op.txt");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data copy event");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%o"))
        .expect("parse %o")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %o");

    let rendered = String::from_utf8(output).expect("utf8");
    assert!(!rendered.trim().is_empty());
}

#[test]
fn out_format_process_id_placeholder_shows_pid() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let source = src_dir.join("pid_test.txt");
    std::fs::write(&source, b"pid").expect("write source");
    let destination = dst_dir.join("pid_test.txt");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data copy event");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%p"))
        .expect("parse %p")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %p");

    let rendered = String::from_utf8(output).expect("utf8");
    let pid: u32 = rendered.trim().parse().expect("parse pid");
    assert!(pid > 0);
}

// ============================================================================
// Escape Sequences Tests
// ============================================================================

#[test]
fn out_format_escaped_percent_renders_literal_percent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let source = src_dir.join("escape.txt");
    std::fs::write(&source, b"escape").expect("write source");
    let destination = dst_dir.join("escape.txt");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data copy event");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("100%% complete"))
        .expect("parse %%")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %%");

    assert_eq!(String::from_utf8(output).expect("utf8"), "100% complete\n");
}

#[test]
fn out_format_multiple_escaped_percent_signs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let source = src_dir.join("multi_escape.txt");
    std::fs::write(&source, b"content").expect("write source");
    let destination = dst_dir.join("multi_escape.txt");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data copy event");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%% %% %%"))
        .expect("parse multiple %%")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render multiple %%");

    assert_eq!(String::from_utf8(output).expect("utf8"), "% % %\n");
}

// ============================================================================
// Combined Format Tests
// ============================================================================

#[test]
fn out_format_combines_multiple_placeholders() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let contents = b"combined test";
    let source = src_dir.join("combined.txt");
    std::fs::write(&source, contents).expect("write source");
    let destination = dst_dir.join("combined.txt");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data copy event");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("[%n] size=%l bytes=%b"))
        .expect("parse combined format")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render combined format");

    let rendered = String::from_utf8(output).expect("utf8");
    assert!(rendered.contains("[combined.txt]"));
    assert!(rendered.contains("size=13"));
    assert!(rendered.contains("bytes=13"));
}

#[test]
fn out_format_mixes_literals_and_placeholders() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let source = src_dir.join("mixed.log");
    std::fs::write(&source, b"log entry").expect("write source");
    let destination = dst_dir.join("mixed.log");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data copy event");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("Transferred: %n (%l bytes)"))
        .expect("parse mixed format")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render mixed format");

    let rendered = String::from_utf8(output).expect("utf8");
    assert!(rendered.starts_with("Transferred: mixed.log"));
    assert!(rendered.contains("bytes)"));
}

// ============================================================================
// Width and Alignment Tests
// ============================================================================

#[test]
fn out_format_respects_width_specifier() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let source = src_dir.join("width.txt");
    std::fs::write(&source, b"width test").expect("write source");
    let destination = dst_dir.join("width.txt");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data copy event");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%20n"))
        .expect("parse width format")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render width format");

    let rendered = String::from_utf8(output).expect("utf8");
    let trimmed = rendered.trim_end();
    // Should be padded to 20 characters (right-aligned by default)
    assert_eq!(trimmed.len(), 20);
    assert!(trimmed.ends_with("width.txt"));
}

#[test]
fn out_format_respects_left_alignment() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let source = src_dir.join("left.txt");
    std::fs::write(&source, b"left align").expect("write source");
    let destination = dst_dir.join("left.txt");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data copy event");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%-20n"))
        .expect("parse left-aligned format")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render left-aligned format");

    let rendered = String::from_utf8(output).expect("utf8");
    // Should be left-aligned with trailing spaces to reach width 20
    assert!(rendered.starts_with("left.txt"));
    // The rendered output should be exactly 20 characters (8 for "left.txt" + 12 spaces)
    // trim_end_matches('\n') to remove only the newline, not the padding spaces
    let without_newline = rendered.trim_end_matches('\n');
    assert_eq!(
        without_newline.len(),
        20,
        "Expected 20 characters with padding, got: {without_newline:?}"
    );
}

// ============================================================================
// Humanization Tests
// ============================================================================

#[test]
fn out_format_humanizes_with_separator() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let contents = vec![0u8; 12345];
    let source = src_dir.join("sep.bin");
    std::fs::write(&source, &contents).expect("write source");
    let destination = dst_dir.join("sep.bin");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data copy event");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%'l"))
        .expect("parse separator format")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render separator format");

    assert_eq!(String::from_utf8(output).expect("utf8"), "12,345\n");
}

#[test]
fn out_format_humanizes_with_decimal_units() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let contents = vec![0u8; 5000];
    let source = src_dir.join("decimal.bin");
    std::fs::write(&source, &contents).expect("write source");
    let destination = dst_dir.join("decimal.bin");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data copy event");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%''l"))
        .expect("parse decimal format")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render decimal format");

    let rendered = String::from_utf8(output).expect("utf8");
    assert!(rendered.contains('K'));
    assert!(rendered.trim().ends_with('K'));
}

#[test]
fn out_format_humanizes_with_binary_units() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let contents = vec![0u8; 2048];
    let source = src_dir.join("binary.bin");
    std::fs::write(&source, &contents).expect("write source");
    let destination = dst_dir.join("binary.bin");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data copy event");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%'''l"))
        .expect("parse binary format")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render binary format");

    let rendered = String::from_utf8(output).expect("utf8");
    assert_eq!(rendered, "2.00K\n");
}

// ============================================================================
// Invalid Format Tests
// ============================================================================

#[test]
fn out_format_rejects_empty_format_string() {
    let error = parse_out_format(OsStr::new("")).unwrap_err();
    assert!(error.to_string().contains("must not be empty"));
}

#[test]
fn out_format_rejects_trailing_percent() {
    let error = parse_out_format(OsStr::new("test%")).unwrap_err();
    assert!(error.to_string().contains("may not end with '%'"));
}

#[test]
fn out_format_rejects_unsupported_placeholder() {
    let error = parse_out_format(OsStr::new("%z")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported --out-format placeholder")
    );
}

#[test]
fn out_format_rejects_invalid_placeholders() {
    let invalid_placeholders = vec!["%x", "%y", "%Q", "%@", "%#", "%$"];

    for placeholder in invalid_placeholders {
        let error = parse_out_format(OsStr::new(placeholder)).unwrap_err();
        assert!(
            error.to_string().contains("unsupported --out-format"),
            "Expected error for {placeholder}, got: {error}"
        );
    }
}

// ============================================================================
// Compatibility Tests - Matching Upstream rsync
// ============================================================================

#[test]
fn out_format_all_supported_placeholders_parse_successfully() {
    // Test all placeholders documented in upstream rsync
    let placeholders = vec![
        "%n", "%N", "%f", "%i", "%l", "%b", "%c", "%o", "%M", "%B", "%L", "%t", "%u", "%g", "%U",
        "%G", "%p", "%h", "%a", "%m", "%P", "%C",
    ];

    for placeholder in placeholders {
        let result = parse_out_format(OsStr::new(placeholder));
        assert!(
            result.is_ok(),
            "Placeholder {placeholder} should be supported"
        );
    }
}

#[test]
fn out_format_complex_realistic_format() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let contents = vec![0u8; 4096];
    let source = src_dir.join("realistic.dat");
    std::fs::write(&source, &contents).expect("write source");
    let destination = dst_dir.join("realistic.dat");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data copy event");

    // Realistic format like: "2024-01-01 12:34:56 [PID] transferred realistic.dat (4,096 bytes)"
    let mut output = Vec::new();
    parse_out_format(OsStr::new("%t [%p] transferred %n (%'l bytes)"))
        .expect("parse realistic format")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render realistic format");

    let rendered = String::from_utf8(output).expect("utf8");
    assert!(rendered.contains("transferred realistic.dat"));
    assert!(rendered.contains("4,096 bytes"));
    assert!(rendered.contains('['));
    assert!(rendered.contains(']'));
}

// ============================================================================
// Directory Handling Tests
// ============================================================================

#[test]
fn out_format_directory_names_include_trailing_slash() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let subdir = src_dir.join("subdir");
    std::fs::create_dir(&subdir).expect("create subdir");

    let source_operand = OsString::from(format!("{}/", src_dir.display()));
    let dest_operand = OsString::from(format!("{}/", dst_dir.display()));

    let config = ClientConfig::builder()
        .transfer_args([source_operand, dest_operand])
        .recursive(true)
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| {
            event.relative_path().to_string_lossy() == "subdir"
                && matches!(event.kind(), ClientEventKind::DirectoryCreated)
        })
        .expect("directory creation event");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%n"))
        .expect("parse %n")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %n");

    let rendered = String::from_utf8(output).expect("utf8");
    assert_eq!(rendered, "subdir/\n");
}
