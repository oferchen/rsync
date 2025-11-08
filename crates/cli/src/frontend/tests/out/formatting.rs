use super::*;

#[test]
fn out_format_respects_width_alignment_and_humanization_controls() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let source = src_dir.join("payload.bin");
    let contents = vec![0u8; 1536];
    std::fs::write(&source, &contents).expect("write source");
    let destination = dst_dir.join("payload.bin");

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
        .expect("data copy event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%10b"))
        .expect("parse width placeholder")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render width placeholder");
    assert_eq!(
        String::from_utf8(output.clone()).expect("utf8"),
        format!("{:>10}\n", contents.len())
    );

    output.clear();
    parse_out_format(OsStr::new("%-10b"))
        .expect("parse left-aligned placeholder")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render left-aligned placeholder");
    assert_eq!(
        String::from_utf8(output.clone()).expect("utf8"),
        format!("{:<10}\n", contents.len())
    );

    output.clear();
    parse_out_format(OsStr::new("%'b"))
        .expect("parse separator placeholder")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render separator placeholder");
    assert_eq!(String::from_utf8(output.clone()).expect("utf8"), "1,536\n");

    output.clear();
    parse_out_format(OsStr::new("%''b"))
        .expect("parse decimal placeholder")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render decimal placeholder");
    assert_eq!(String::from_utf8(output.clone()).expect("utf8"), "1.54K\n");

    output.clear();
    parse_out_format(OsStr::new("%'''b"))
        .expect("parse binary placeholder")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render binary placeholder");
    assert_eq!(String::from_utf8(output).expect("utf8"), "1.50K\n");
}

#[test]
fn out_format_renders_modify_time_placeholder() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("file.txt");
    std::fs::write(&source, b"data").expect("write source");
    let destination = temp.path().join("dest");

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
        .find(|event| event.relative_path().to_string_lossy().contains("file.txt"))
        .expect("event present");

    let format = parse_out_format(OsStr::new("%M")).expect("parse out-format");
    let mut output = Vec::new();
    format
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render out-format");

    assert!(String::from_utf8_lossy(&output).trim().contains('-'));
}
