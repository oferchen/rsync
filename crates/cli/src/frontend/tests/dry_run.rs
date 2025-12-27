use super::common::*;
use super::*;

#[test]
fn dry_run_flag_skips_destination_mutation() {
    use std::fs;
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    fs::write(&source, b"contents").expect("write source");
    let destination = tmp.path().join("dest.txt");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--dry-run"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert!(!destination.exists());
}

#[test]
fn short_dry_run_flag_skips_destination_mutation() {
    use std::fs;
    use tempfile::tempdir;

    let _lock = ENV_LOCK.lock().expect("env mutex poisoned");
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    fs::write(&source, b"contents").expect("write source");
    let destination = tmp.path().join("dest.txt");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-n"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert!(!destination.exists());
}
