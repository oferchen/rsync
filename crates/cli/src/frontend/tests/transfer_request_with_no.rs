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
    // Use 0o777 so that any non-zero umask will produce a different mode,
    // proving that --no-perms applies umask-based defaults instead of
    // preserving the source permissions.
    std::fs::set_permissions(&source, PermissionsExt::from_mode(0o777)).expect("set perms");

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
    assert_ne!(
        metadata.permissions().mode() & 0o777,
        0o777,
        "without --perms, destination should have umask-applied permissions"
    );
}

// Archive mode includes Unix-specific options (preserve permissions, ownership)
#[cfg(unix)]
#[test]
fn transfer_request_with_no_times_overrides_archive() {
    use filetime::{FileTime, set_file_times};
    use std::time::SystemTime;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source-no-times.txt");
    let destination = tmp.path().join("dest-no-times.txt");
    std::fs::write(&source, b"data").expect("write source");
    // Set source mtime to a date far in the past (year 2014)
    let old_mtime = FileTime::from_unix_time(1_400_000_000, 0);
    set_file_times(&source, old_mtime, old_mtime).expect("set times");

    let before_transfer = SystemTime::now();

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

    // With --no-times, the destination mtime should be recent (file creation
    // time), not the old 2014 timestamp from the source.
    let metadata = std::fs::metadata(&destination).expect("dest metadata");
    let dest_mtime = metadata.modified().expect("modified time");
    assert!(
        dest_mtime >= before_transfer,
        "with --no-times, mtime should be recent, not preserved from source"
    );
}
