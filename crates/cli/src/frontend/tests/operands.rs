use super::common::*;
use super::*;

#[cfg(windows)]
#[test]
fn operand_detection_ignores_windows_drive_and_device_prefixes() {
    use std::ffi::OsStr;

    assert!(!operand_is_remote(OsStr::new("C:\\temp\\file.txt")));
    assert!(!operand_is_remote(OsStr::new("\\\\?\\C:\\temp\\file.txt")));
    assert!(!operand_is_remote(OsStr::new("\\\\.\\C:\\pipe\\name")));
}

#[test]
fn operands_after_end_of_options_are_preserved() {
    use std::fs;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("-source");
    let destination = tmp.path().join("dest.txt");
    fs::write(&source, b"dash source").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        fs::read(destination).expect("read destination"),
        b"dash source"
    );
}
