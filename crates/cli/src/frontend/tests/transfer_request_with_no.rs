use super::common::*;
use super::*;

#[test]
fn transfer_request_with_no_bwlimit_copies_file() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("destination.txt");
    std::fs::write(&source, b"unlimited").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-bwlimit"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"unlimited"
    );
}

#[cfg(unix)]
#[test]
fn transfer_request_with_no_perms_overrides_archive() {
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source-no-perms.txt");
    let destination = tmp.path().join("dest-no-perms.txt");
    std::fs::write(&source, b"data").expect("write source");
    std::fs::set_permissions(&source, PermissionsExt::from_mode(0o600)).expect("set perms");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("--no-perms"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let metadata = std::fs::metadata(&destination).expect("dest metadata");
    assert_ne!(metadata.permissions().mode() & 0o777, 0o600);
}

// Archive mode includes Unix-specific options (preserve permissions, ownership)
#[cfg(unix)]
#[test]
fn transfer_request_with_no_times_overrides_archive() {
    use filetime::{FileTime, set_file_times};
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source-no-times.txt");
    let destination = tmp.path().join("dest-no-times.txt");
    std::fs::write(&source, b"data").expect("write source");
    let mtime = FileTime::from_unix_time(1_700_100_000, 0);
    set_file_times(&source, mtime, mtime).expect("set times");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("--no-times"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let metadata = std::fs::metadata(&destination).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&metadata);
    assert_ne!(dest_mtime, mtime);
}
