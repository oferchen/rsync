use super::common::*;
use super::*;

#[cfg(unix)]
#[test]
fn transfer_request_with_perms_preserves_mode() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source-perms.txt");
    let destination = tmp.path().join("dest-perms.txt");
    std::fs::write(&source, b"data").expect("write source");
    let atime = FileTime::from_unix_time(1_700_070_000, 0);
    let mtime = FileTime::from_unix_time(1_700_080_000, 0);
    set_file_times(&source, atime, mtime).expect("set times");
    std::fs::set_permissions(&source, PermissionsExt::from_mode(0o640)).expect("set perms");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--perms"),
        OsString::from("--times"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let metadata = std::fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
    let dest_mtime = FileTime::from_last_modification_time(&metadata);
    assert_eq!(dest_mtime, mtime);
}
