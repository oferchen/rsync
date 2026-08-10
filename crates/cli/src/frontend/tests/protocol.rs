use super::common::*;
use super::*;

#[test]
fn protocol_option_requires_daemon_operands() {
    use tempfile::tempdir;
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    std::fs::write(&source, b"data").expect("write source");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--protocol=30"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);
    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
    assert!(rendered.contains("--protocol"));
    assert!(rendered.contains("rsync daemon"));
    assert!(!destination.exists());
}

#[test]
fn protocol_value_with_whitespace_and_plus_is_accepted() {
    let version = parse_protocol_version_arg(OsStr::new(" +31 \n"))
        .expect("whitespace-wrapped value should parse");
    assert_eq!(version.as_u8(), 31);
}

#[test]
fn protocol_value_negative_reports_specific_diagnostic() {
    let message = parse_protocol_version_arg(OsStr::new("-30"))
        .expect_err("negative protocol should be rejected");
    let rendered = message.to_string();
    assert!(rendered.contains("invalid protocol version '-30'"));
    assert!(rendered.contains("cannot be negative"));
}

#[test]
fn protocol_value_empty_reports_specific_diagnostic() {
    let message = parse_protocol_version_arg(OsStr::new(" "))
        .expect_err("empty protocol value should be rejected");
    let rendered = message.to_string();
    assert!(rendered.contains("invalid protocol version ' '"));
    assert!(rendered.contains("must not be empty"));
}

#[test]
fn parse_args_accepts_boundary_protocol_versions() {
    for version in ["28", "30", "32"] {
        let result = parse_args([
            OsString::from(RSYNC),
            OsString::from(format!("--protocol={version}")),
            OsString::from("source"),
            OsString::from("dest"),
        ]);
        assert!(
            result.is_ok(),
            "--protocol={version} should be accepted but was rejected"
        );
    }
}

#[test]
fn parse_args_accepts_minimum_protocol_version() {
    // 28 is the minimum accepted version
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--protocol=28"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("protocol 28 should be accepted");
    assert_eq!(parsed.protocol, Some(OsString::from("28")));
}

#[test]
fn parse_args_accepts_maximum_protocol_version() {
    // 32 is the maximum accepted version
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--protocol=32"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("protocol 32 should be accepted");
    assert_eq!(parsed.protocol, Some(OsString::from("32")));
}
