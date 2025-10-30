use super::common::*;
use super::*;

#[test]
fn connect_program_requires_remote_operands() {
    use tempfile::tempdir;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    std::fs::write(&source, b"content").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--connect-program=/usr/bin/nc %H %P"),
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let message = String::from_utf8(stderr).expect("stderr utf8");
    assert!(
        message.contains(
            "the --connect-program option may only be used when accessing an rsync daemon"
        )
    );
    assert!(!dest.exists());
}
