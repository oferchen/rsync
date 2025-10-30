use super::common::*;
use super::*;

#[test]
fn rsync_path_requires_remote_operands() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    std::fs::write(&source, b"content").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--rsync-path=/opt/custom/rsync"),
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let message = String::from_utf8(stderr).expect("stderr utf8");
    assert!(message.contains("the --rsync-path option may only be used with remote connections"));
    assert!(!dest.exists());
}
