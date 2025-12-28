use super::common::*;
use super::*;

#[test]
fn transfer_request_with_ignore_existing_leaves_destination_unchanged() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("destination.txt");
    std::fs::write(&source, b"updated").expect("write source");
    std::fs::write(&destination, b"original").expect("write destination");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--ignore-existing"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"original"
    );
}

#[test]
fn transfer_request_with_ignore_missing_args_skips_missing_sources() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let missing = tmp.path().join("missing.txt");
    let destination = tmp.path().join("destination.txt");
    std::fs::write(&destination, b"existing").expect("write destination");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--ignore-missing-args"),
        missing.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"existing"
    );
}
