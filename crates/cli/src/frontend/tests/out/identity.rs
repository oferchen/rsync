use super::*;
use core::client::run_client;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use tempfile::tempdir;

#[test]
fn out_format_renders_permission_and_identity_placeholders() {
    let temp = tempdir().expect("tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    fs::create_dir(&src_dir).expect("create src");
    fs::create_dir(&dst_dir).expect("create dst");
    let source = src_dir.join("script.sh");
    fs::write(&source, b"echo ok\n").expect("write source");

    let expected_uid = fs::metadata(&source).expect("source metadata").uid();
    let expected_gid = fs::metadata(&source).expect("source metadata").gid();

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

    let summary = run_client(config).expect("run client");
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
    // %u is the daemon auth user; off-daemon (client) it renders literally.
    parse_out_format(OsStr::new("%u"))
        .expect("parse %u")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %u");
    assert_eq!(output, b"%u\n");

    output.clear();
    // upstream has no %g code; it renders literally.
    parse_out_format(OsStr::new("%g"))
        .expect("parse %g")
        .render(event, &OutFormatContext::default(), &mut output)
        .expect("render %g");
    assert_eq!(output, b"%g\n");
}
