use super::common::*;
use super::*;

#[cfg(not(feature = "acl"))]
#[test]
fn acls_option_reports_unsupported_when_feature_disabled() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    std::fs::write(&source, b"data").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--acls"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("UTF-8 error");
    assert!(rendered.contains("POSIX ACLs are not supported on this client"));
    assert_contains_client_trailer(&rendered);
}
