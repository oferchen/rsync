use super::common::*;
use super::*;

#[test]
fn protocol_option_accepted_on_local_copy() {
    // upstream: setup_protocol (compat.c:629-637) runs for local copies too, so
    // `--protocol=N` (20..=32) is accepted on a local transfer (exit 0) and the
    // copy proceeds. This build ignores the value for a local copy.
    use tempfile::tempdir;
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    std::fs::write(&source, b"data").expect("write source");
    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--protocol=30"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(destination.exists());
}

#[test]
fn protocol_value_with_whitespace_and_plus_is_accepted() {
    match parse_protocol_version_arg(OsStr::new(" +31 \n"))
        .expect("whitespace-wrapped value should parse")
    {
        ProtocolArg::Supported(version) => assert_eq!(version.as_u8(), 31),
        ProtocolArg::LegacyLocalOnly(raw) => panic!("31 is wire-supported, got legacy {raw}"),
    }
}

#[test]
fn protocol_below_upstream_floor_is_protocol_error() {
    // upstream: compat.c:630 rejects `< MIN_PROTOCOL_VERSION` (20) with
    // RERR_PROTOCOL (errcode.h:26). errcode.h:26 defines RERR_PROTOCOL = 2.
    let message = parse_protocol_version_arg(OsStr::new("19"))
        .expect_err("protocol 19 is below the upstream floor");
    assert_eq!(message.code(), Some(2));
    assert!(message.to_string().contains("at least 20"));
}

#[test]
fn protocol_above_upstream_ceiling_is_protocol_error() {
    // upstream: compat.c:635 rejects `> PROTOCOL_VERSION` (32) with RERR_PROTOCOL.
    let message = parse_protocol_version_arg(OsStr::new("33"))
        .expect_err("protocol 33 is above the upstream ceiling");
    assert_eq!(message.code(), Some(2));
    assert!(message.to_string().contains("no more than 32"));
}

#[test]
fn protocol_legacy_value_is_local_only() {
    // upstream accepts 20..=27; this build classifies them as local-only
    // (below the 28..=32 wire floor).
    match parse_protocol_version_arg(OsStr::new("26")).expect("protocol 26 is valid upstream") {
        ProtocolArg::LegacyLocalOnly(raw) => assert_eq!(raw, 26),
        ProtocolArg::Supported(v) => panic!("26 is below the wire floor, got supported {v}"),
    }
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
