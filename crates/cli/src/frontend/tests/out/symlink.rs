use super::*;
use std::fs;
use std::os::unix::fs::symlink;
use tempfile::tempdir;

#[test]
fn out_format_renders_symlink_target_placeholder() {
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
        .find(|event| event.relative_path().to_string_lossy().contains("link.txt"))
        .expect("symlink event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%L"))
        .expect("parse %L")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %L");

    assert_eq!(output, b" -> file.txt\n");
}

#[test]
fn out_format_renders_combined_name_and_target_placeholder() {
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
        .find(|event| event.relative_path().to_string_lossy().contains("link.txt"))
        .expect("symlink event present");

    let mut output = Vec::new();
    parse_out_format(OsStr::new("%N"))
        .expect("parse %N")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %N");

    assert_eq!(output, b"src/link.txt -> file.txt\n");
}
