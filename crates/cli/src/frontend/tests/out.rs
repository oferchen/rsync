use super::common::*;
use super::*;

#[test]
fn out_format_argument_accepts_supported_placeholders() {
    let format = parse_out_format(OsStr::new(
        "%f %b %c %l %o %M %B %L %N %p %u %g %U %G %t %i %h %a %m %P %C %%",
    ))
    .expect("parse out-format");
    assert!(!format.is_empty());
}

#[test]
fn out_format_argument_rejects_unknown_placeholders() {
    let error = parse_out_format(OsStr::new("%z")).unwrap_err();
    assert!(error.to_string().contains("unsupported --out-format"));
}

#[test]
fn out_format_remote_placeholders_preserve_literals_without_context() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    std::fs::create_dir(&src_dir).expect("create src");
    std::fs::create_dir(&dst_dir).expect("create dst");

    let source = src_dir.join("file.txt");
    std::fs::write(&source, b"payload").expect("write source");
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
        .find(|event| matches!(event.kind(), ClientEventKind::DataCopied))
        .expect("data event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%h %a %m %P"))
        .expect("parse placeholders")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render placeholders");

    let rendered = String::from_utf8(output).expect("utf8");
    assert_eq!(rendered, "%h %a %m %P\n");
}

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

    // First run populates the destination.
    let outcome = run_client_or_fallback::<io::Sink, io::Sink>(build_config(), None, None)
        .expect("initial copy");
    if let ClientOutcome::Fallback(_) = outcome {
        panic!("unexpected fallback outcome during initial copy");
    }

    // Second run should reuse metadata and avoid copying data bytes.
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

#[cfg(unix)]
#[test]
fn out_format_renders_permission_and_identity_placeholders() {
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;
    use users::{get_group_by_gid, get_user_by_uid, gid_t, uid_t};

    let temp = tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    fs::create_dir(&src_dir).expect("create src");
    fs::create_dir(&dst_dir).expect("create dst");
    let source = src_dir.join("script.sh");
    fs::write(&source, b"echo ok\n").expect("write source");

    let expected_uid = fs::metadata(&source).expect("source metadata").uid();
    let expected_gid = fs::metadata(&source).expect("source metadata").gid();
    let expected_user = get_user_by_uid(expected_uid as uid_t)
        .map(|user| user.name().to_string_lossy().into_owned())
        .unwrap_or_else(|| expected_uid.to_string());
    let expected_group = get_group_by_gid(expected_gid as gid_t)
        .map(|group| group.name().to_string_lossy().into_owned())
        .unwrap_or_else(|| expected_gid.to_string());

    let mut permissions = fs::metadata(&source)
        .expect("source metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&source, permissions).expect("set permissions");

    let config = ClientConfig::builder()
        .transfer_args([
            source.as_os_str().to_os_string(),
            dst_dir.as_os_str().to_os_string(),
        ])
        .permissions(true)
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
                .contains("script.sh")
        })
        .expect("event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%B"))
        .expect("parse out-format")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %B");
    assert_eq!(output, b"rwxr-xr-x\n");

    output.clear();
    parse_out_format(OsStr::new("%p"))
        .expect("parse %p")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %p");
    let expected_pid = format!("{}\n", std::process::id());
    assert_eq!(output, expected_pid.as_bytes());

    output.clear();
    parse_out_format(OsStr::new("%U"))
        .expect("parse %U")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %U");
    assert_eq!(output, format!("{expected_uid}\n").as_bytes());

    output.clear();
    parse_out_format(OsStr::new("%G"))
        .expect("parse %G")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %G");
    assert_eq!(output, format!("{expected_gid}\n").as_bytes());

    output.clear();
    parse_out_format(OsStr::new("%u"))
        .expect("parse %u")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %u");
    assert_eq!(output, format!("{expected_user}\n").as_bytes());

    output.clear();
    parse_out_format(OsStr::new("%g"))
        .expect("parse %g")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %g");
    assert_eq!(output, format!("{expected_group}\n").as_bytes());
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

#[cfg(unix)]
#[test]
fn out_format_renders_symlink_target_placeholder() {
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    fs::create_dir(&source_dir).expect("create src dir");
    let file = source_dir.join("file.txt");
    fs::write(&file, b"data").expect("write file");
    let link_path = source_dir.join("link.txt");
    symlink("file.txt", &link_path).expect("create symlink");

    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&dest_dir).expect("create dst dir");

    let config = ClientConfig::builder()
        .transfer_args([
            source_dir.as_os_str().to_os_string(),
            dest_dir.as_os_str().to_os_string(),
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
        .find(|event| event.relative_path().to_string_lossy().contains("link.txt"))
        .expect("symlink event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%L"))
        .expect("parse %L")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %L");

    assert_eq!(output, b" -> file.txt\n");
}

#[cfg(unix)]
#[test]
fn out_format_renders_combined_name_and_target_placeholder() {
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    fs::create_dir(&source_dir).expect("create src dir");
    let file = source_dir.join("file.txt");
    fs::write(&file, b"data").expect("write file");
    let link_path = source_dir.join("link.txt");
    symlink("file.txt", &link_path).expect("create symlink");

    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&dest_dir).expect("create dst dir");

    let config = ClientConfig::builder()
        .transfer_args([
            source_dir.as_os_str().to_os_string(),
            dest_dir.as_os_str().to_os_string(),
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
        .find(|event| event.relative_path().to_string_lossy().contains("link.txt"))
        .expect("symlink event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%N"))
        .expect("parse %N")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %N");

    assert_eq!(output, b"src/link.txt -> file.txt\n");
}
