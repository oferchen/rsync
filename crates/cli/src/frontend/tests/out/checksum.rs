use super::*;
use oc_rsync_checksums::strong::Md5;

#[test]
fn out_format_renders_full_checksum_for_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src dir");
    std::fs::create_dir(&dst_dir).expect("create dst dir");

    let source = src_dir.join("file.bin");
    let contents = b"checksum payload";
    std::fs::write(&source, contents).expect("write source");
    let destination = dst_dir.join("file.bin");

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
        .find(|event| event.relative_path().to_string_lossy() == "file.bin")
        .expect("file event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%C"))
        .expect("parse %C")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %C");

    let rendered = String::from_utf8(output).expect("utf8");
    let mut hasher = Md5::new();
    hasher.update(contents);
    let digest = hasher.finalize();
    let expected: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(rendered, format!("{expected}\n"));
}

#[test]
fn out_format_renders_full_checksum_for_non_file_entries_as_spaces() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir_all(src_dir.join("nested")).expect("create source tree");
    std::fs::create_dir(&dst_dir).expect("create destination root");
    std::fs::write(src_dir.join("nested").join("file.txt"), b"contents").expect("write file");

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

    let dir_event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::DirectoryCreated))
        .expect("directory event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%C"))
        .expect("parse %C")
        .render(dir_event, &OutFormatContext::default(), &mut output)
        .expect("render %C");

    let rendered = String::from_utf8(output).expect("utf8");
    assert_eq!(rendered.len(), 33);
    assert!(rendered[..32].chars().all(|ch| ch == ' '));
    assert_eq!(rendered.as_bytes()[32], b'\n');
}

#[test]
fn out_format_renders_checksum_bytes_for_data_copy_events() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let source = src_dir.join("payload.bin");
    let contents = b"checksum-bytes";
    std::fs::write(&source, contents).expect("write source");
    let destination = dst_dir.join("payload.bin");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            destination.as_os_str().to_os_string(),
        ])
        .times(true)
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
    parse_out_format(OsStr::new("%c"))
        .expect("parse %c")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %c");

    let rendered = String::from_utf8(output).expect("utf8");
    assert_eq!(rendered.trim_end(), contents.len().to_string());
}

#[test]
fn out_format_renders_checksum_bytes_as_zero_when_metadata_reused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let source = src_dir.join("unchanged.txt");
    std::fs::write(&source, b"same contents").expect("write source");
    let destination = dst_dir.join("unchanged.txt");

    let build_config = || {
        ClientConfig::builder()
            .transfer_args([
                source.as_os_str().to_os_string(),
                destination.as_os_str().to_os_string(),
            ])
            .times(true)
            .force_event_collection(true)
            .build()
    };

    let outcome = run_client_or_fallback::<io::Sink, io::Sink>(build_config(), None, None)
        .expect("initial copy");
    if let ClientOutcome::Fallback(_) = outcome {
        panic!("unexpected fallback outcome during initial copy");
    }

    let outcome =
        run_client_or_fallback::<io::Sink, io::Sink>(build_config(), None, None).expect("re-run");
    let summary = match outcome {
        ClientOutcome::Local(summary) => *summary,
        ClientOutcome::Fallback(_) => panic!("unexpected fallback outcome"),
    };

    let event = summary
        .events()
        .iter()
        .find(|event| matches!(event.kind(), ClientEventKind::MetadataReused))
        .expect("metadata reuse event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%c"))
        .expect("parse %c")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %c");

    let rendered = String::from_utf8(output).expect("utf8");
    assert_eq!(rendered.trim_end(), "0");
}
