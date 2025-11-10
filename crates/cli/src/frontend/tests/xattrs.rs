use super::common::*;
use super::*;
use std::ffi::OsString;

#[cfg(not(feature = "xattr"))]
#[test]
fn xattrs_option_reports_unsupported_when_feature_disabled() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    std::fs::write(&source, b"data").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--xattrs"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("UTF-8 error");
    assert!(rendered.contains("extended attributes are not supported on this client"));
    assert_contains_client_trailer(&rendered);
}

#[cfg(all(unix, feature = "xattr"))]
#[test]
fn xattrs_option_preserves_attributes() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    std::fs::write(&source, b"attr data").expect("write source");

    xattr::set(&source, "user.test", b"value").expect("set xattr");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--xattrs"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let copied = xattr::get(&destination, "user.test")
        .expect("read dest xattr")
        .expect("xattr present");
    assert_eq!(copied, b"value");
}
