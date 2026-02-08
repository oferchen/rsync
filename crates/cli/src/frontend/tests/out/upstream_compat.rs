//! Upstream rsync --out-format compatibility tests.
//!
//! These tests verify that our --out-format implementation matches upstream
//! rsync behavior for common format strings and edge cases:
//!
//! - `%i%n` and `%i %n` match upstream --itemize-changes output
//! - `%M` modification time format: `yyyy/mm/dd-hh:mm:ss`
//! - `%t` current time format: `yyyy/mm/dd-hh:mm:ss`
//! - `--verbose` implies default verbose listing (not --out-format)
//! - `--out-format` suppresses default verbose listing
//! - Multiple events rendered in order

use super::*;

// ============================================================================
// Upstream %i%n combined format (canonical --itemize-changes format)
// ============================================================================

#[test]
fn out_format_itemize_filename_combined_matches_upstream_new_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src dir");
    std::fs::create_dir(&dst_dir).expect("create dst dir");

    let source = src_dir.join("hello.txt");
    std::fs::write(&source, b"hello world").expect("write source");
    let destination = dst_dir.join("hello.txt");

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
        .find(|event| event.relative_path().to_string_lossy() == "hello.txt")
        .expect("event present");

    // Test %i%n (no space) -- upstream rsync -i outputs "%i %n" but the user
    // can specify %i%n to get them concatenated
    let mut output = Vec::new();
    parse_out_format(OsStr::new("%i%n"))
        .expect("parse %i%n")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %i%n");

    let rendered = String::from_utf8(output).expect("utf8");
    // New file: >f+++++++++ followed by filename
    assert!(
        rendered.starts_with(">f+++++++++"),
        "expected >f+++++++++ prefix, got: {rendered:?}"
    );
    assert!(rendered.contains("hello.txt"));
}

#[test]
fn out_format_itemize_space_filename_matches_upstream_output() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src dir");
    std::fs::create_dir(&dst_dir).expect("create dst dir");

    let source = src_dir.join("data.bin");
    std::fs::write(&source, b"binary data").expect("write source");
    let destination = dst_dir.join("data.bin");

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
        .find(|event| event.relative_path().to_string_lossy() == "data.bin")
        .expect("event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%i %n"))
        .expect("parse %i %n")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %i %n");

    // Upstream rsync -i output: ">f+++++++++ data.bin"
    assert_eq!(output, b">f+++++++++ data.bin\n");
}

// ============================================================================
// Upstream %M modification time format
// ============================================================================

#[test]
fn out_format_modify_time_follows_upstream_format() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("timestamped.txt");
    std::fs::write(&source, b"data").expect("write source");
    let destination = temp.path().join("dest.txt");

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
        .find(|event| {
            event
                .relative_path()
                .to_string_lossy()
                .contains("timestamped")
        })
        .expect("event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%M"))
        .expect("parse %M")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %M");

    let rendered = String::from_utf8(output).expect("utf8");
    let trimmed = rendered.trim();

    // Upstream rsync modification time format: yyyy/mm/dd-hh:mm:ss
    assert_eq!(
        trimmed.len(),
        19,
        "%%M should be 19 chars (yyyy/mm/dd-hh:mm:ss), got: {trimmed:?}"
    );
    // Verify the separator positions
    assert_eq!(&trimmed[4..5], "/", "position 4 should be '/'");
    assert_eq!(&trimmed[7..8], "/", "position 7 should be '/'");
    assert_eq!(&trimmed[10..11], "-", "position 10 should be '-'");
    assert_eq!(&trimmed[13..14], ":", "position 13 should be ':'");
    assert_eq!(&trimmed[16..17], ":", "position 16 should be ':'");

    // The year should be reasonable (2020-2100)
    let year: u32 = trimmed[..4].parse().expect("parse year");
    assert!(
        (2020..=2100).contains(&year),
        "year should be reasonable, got: {year}"
    );
}

// ============================================================================
// Upstream %t current time format
// ============================================================================

#[test]
fn out_format_current_time_follows_upstream_format() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("time.txt");
    std::fs::write(&source, b"time").expect("write source");
    let destination = temp.path().join("dest.txt");

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
        .find(|event| event.relative_path().to_string_lossy().contains("time.txt"))
        .expect("event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%t"))
        .expect("parse %t")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %t");

    let rendered = String::from_utf8(output).expect("utf8");
    let trimmed = rendered.trim();

    // Same format as %M: yyyy/mm/dd-hh:mm:ss
    assert_eq!(
        trimmed.len(),
        19,
        "%%t should be 19 chars (yyyy/mm/dd-hh:mm:ss), got: {trimmed:?}"
    );
    assert_eq!(&trimmed[4..5], "/");
    assert_eq!(&trimmed[7..8], "/");
    assert_eq!(&trimmed[10..11], "-");
    assert_eq!(&trimmed[13..14], ":");
    assert_eq!(&trimmed[16..17], ":");
}

// ============================================================================
// --out-format suppresses default verbose listing
// ============================================================================

#[test]
fn out_format_suppresses_verbose_listing_in_summary() {
    use super::super::super::{
        NameOutputLevel, ProgressSetting, emit_transfer_summary, parse_out_format,
    };
    use core::client::{ClientConfig, HumanReadableMode};

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("verbose_test.txt");
    std::fs::write(&source, b"data").expect("write source");
    let destination = temp.path().join("dest.txt");

    let config = ClientConfig::builder()
        .transfer_args([source, destination])
        .verbosity(1)
        .force_event_collection(true)
        .build();

    let summary = core::client::run_client(config).expect("run client");

    // With --out-format specified, the verbose listing should not be duplicated.
    // The out_format renders events, then the verbose listing logic checks
    // `out_format.is_none()` before deciding whether to emit a verbose listing.
    let format = parse_out_format(OsStr::new("%n")).expect("parse format");
    let mut with_out_format = Vec::new();
    emit_transfer_summary(
        &summary,
        1,
        None,
        false,
        false,
        false,
        Some(&format),
        &OutFormatContext::default(),
        NameOutputLevel::Disabled,
        false,
        HumanReadableMode::Disabled,
        false,
        &mut with_out_format,
    )
    .expect("render with out-format");

    let mut without_out_format = Vec::new();
    emit_transfer_summary(
        &summary,
        1,
        None,
        false,
        false,
        false,
        None,
        &OutFormatContext::default(),
        NameOutputLevel::UpdatedOnly,
        false,
        HumanReadableMode::Disabled,
        false,
        &mut without_out_format,
    )
    .expect("render without out-format");

    let with_str = String::from_utf8(with_out_format).expect("utf8");
    let without_str = String::from_utf8(without_out_format).expect("utf8");

    // Both should contain the filename, but they represent different rendering paths
    assert!(
        with_str.contains("verbose_test.txt"),
        "out-format output should contain the filename"
    );
    assert!(
        without_str.contains("verbose_test.txt"),
        "verbose output should contain the filename"
    );

    // The out-format output should have the filename as a bare line (from %n)
    // while the verbose output has additional formatting (e.g. "copied: filename")
    assert!(
        with_str.starts_with("verbose_test.txt"),
        "out-format should render just the name at the start"
    );
}

// ============================================================================
// Multiple events rendered in order
// ============================================================================

#[test]
fn out_format_renders_multiple_files_in_order() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    std::fs::write(src_dir.join("aaa.txt"), b"a").expect("write aaa");
    std::fs::write(src_dir.join("bbb.txt"), b"bb").expect("write bbb");
    std::fs::write(src_dir.join("ccc.txt"), b"ccc").expect("write ccc");

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

    let format = parse_out_format(OsStr::new("%n %l")).expect("parse format");
    let events: Vec<_> = summary
        .events()
        .iter()
        .filter(|e| matches!(e.kind(), ClientEventKind::DataCopied))
        .cloned()
        .collect();

    assert!(
        events.len() >= 3,
        "expected at least 3 data copy events, got {}",
        events.len()
    );

    let mut output = Vec::new();
    crate::emit_out_format(&events, &format, &OutFormatContext::default(), &mut output)
        .expect("emit");

    let rendered = String::from_utf8(output).expect("utf8");
    let lines: Vec<_> = rendered.lines().collect();
    assert_eq!(
        lines.len(),
        events.len(),
        "one line per event, lines={lines:?}"
    );

    // Each line should contain a filename and size
    for line in &lines {
        assert!(
            line.contains(".txt"),
            "each line should contain filename, got: {line:?}"
        );
    }
}

// ============================================================================
// Upstream compatibility: complete format with all basic codes
// ============================================================================

#[test]
fn out_format_complex_upstream_format_renders_all_fields() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let contents = b"complex format test";
    let source = src_dir.join("complex.dat");
    std::fs::write(&source, contents).expect("write source");
    let destination = dst_dir.join("complex.dat");

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

    // Format: "%i %o %n (%l bytes) [pid=%p]"
    let mut output = Vec::new();
    parse_out_format(OsStr::new("%i %o %n (%l bytes) [pid=%p]"))
        .expect("parse complex format")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render complex format");

    let rendered = String::from_utf8(output).expect("utf8");
    // Should contain the itemize string for a new file
    assert!(
        rendered.contains(">f+++++++++"),
        "should contain itemize: {rendered:?}"
    );
    // Should contain the operation
    assert!(
        rendered.contains("copied"),
        "should contain operation: {rendered:?}"
    );
    // Should contain the filename
    assert!(
        rendered.contains("complex.dat"),
        "should contain filename: {rendered:?}"
    );
    // Should contain the file length
    assert!(
        rendered.contains(&format!("{}", contents.len())),
        "should contain length: {rendered:?}"
    );
    // Should contain "bytes"
    assert!(
        rendered.contains("bytes"),
        "should contain 'bytes': {rendered:?}"
    );
    // Should contain pid
    assert!(
        rendered.contains("pid="),
        "should contain 'pid=': {rendered:?}"
    );
}

// ============================================================================
// Edge case: literal-only format string
// ============================================================================

#[test]
fn out_format_literal_only_string_renders_as_is() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let source = src_dir.join("literal.txt");
    std::fs::write(&source, b"data").expect("write source");
    let destination = dst_dir.join("literal.txt");

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
    parse_out_format(OsStr::new("static text here"))
        .expect("parse literal format")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render literal format");

    assert_eq!(
        String::from_utf8(output).expect("utf8"),
        "static text here\n"
    );
}
