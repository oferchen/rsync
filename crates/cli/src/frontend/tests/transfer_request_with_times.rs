use super::common::*;
use super::*;

#[test]
fn transfer_request_with_times_preserves_timestamp() {
    use filetime::{FileTime, set_file_times};
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source-times.txt");
    let destination = tmp.path().join("dest-times.txt");
    std::fs::write(&source, b"data").expect("write source");
    let mtime = FileTime::from_unix_time(1_700_090_000, 500_000_000);
    set_file_times(&source, mtime, mtime).expect("set times");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--times"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let metadata = std::fs::metadata(&destination).expect("dest metadata");
    let dest_mtime = FileTime::from_last_modification_time(&metadata);
    assert_eq!(dest_mtime, mtime);
}
