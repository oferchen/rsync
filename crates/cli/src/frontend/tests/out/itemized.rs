use super::*;

#[test]
fn out_format_renders_itemized_placeholder_for_new_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src dir");
    std::fs::create_dir(&dst_dir).expect("create dst dir");

    let source = src_dir.join("file.txt");
    std::fs::write(&source, b"content").expect("write source");
    let destination = dst_dir.join("file.txt");

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
        .find(|event| event.relative_path().to_string_lossy() == "file.txt")
        .expect("event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%i"))
        .expect("parse %i")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %i");

    assert_eq!(output, b">f+++++++++\n");
}

#[test]
fn out_format_itemized_placeholder_reports_deletion() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src dir");
    std::fs::create_dir(&dst_dir).expect("create dst dir");

    let destination_file = dst_dir.join("obsolete.txt");
    std::fs::write(&destination_file, b"old").expect("write obsolete");

    let source_operand = OsString::from(format!("{}/", src_dir.display()));
    let dest_operand = OsString::from(format!("{}/", dst_dir.display()));

    let config = ClientConfig::builder()
        .transfer_args([source_operand, dest_operand])
        .delete(true)
        .recursive(true)
        .force_event_collection(true)
        .build();

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(config, None, None).expect("run client");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let mut events = summary.events().iter();
    let event = events
        .find(|event| event.relative_path().to_string_lossy() == "obsolete.txt")
        .unwrap_or_else(|| {
            let recorded: Vec<_> = summary
                .events()
                .iter()
                .map(|event| event.relative_path().to_string_lossy().into_owned())
                .collect();
            panic!("deletion event missing, recorded events: {recorded:?}");
        });

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%i"))
        .expect("parse %i")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %i");

    assert_eq!(output, b"*deleting\n");
}
