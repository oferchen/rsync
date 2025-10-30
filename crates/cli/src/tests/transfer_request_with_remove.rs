use super::common::*;
use super::*;

#[test]
fn transfer_request_with_remove_source_files_deletes_source() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("destination.txt");
    std::fs::write(&source, b"move me").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--remove-source-files"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert!(!source.exists(), "source should be removed");
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"move me"
    );
}

#[test]
fn transfer_request_with_remove_sent_files_alias_deletes_source() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("destination.txt");
    std::fs::write(&source, b"alias move").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--remove-sent-files"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert!(!source.exists(), "source should be removed");
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"alias move"
    );
}
