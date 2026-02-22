use super::*;
use core::client::run_client;

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

    let summary = run_client(config).expect("run client");

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
