use super::common::*;
use super::*;

#[test]
fn transfer_request_with_ignore_times_forces_copy_despite_matching_timestamps() {
    use filetime::{FileTime, set_file_times};
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("destination.txt");
    std::fs::write(&source, b"newdata").expect("write source");
    std::fs::write(&destination, b"olddata").expect("write destination");

    let timestamp = FileTime::from_unix_time(1_700_200_000, 0);
    set_file_times(&source, timestamp, timestamp).expect("source times");
    set_file_times(&destination, timestamp, timestamp).expect("dest times");

    let (baseline_code, baseline_stdout, baseline_stderr) = run_with_args([
        OsString::from(RSYNC),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(baseline_code, 0);
    assert!(baseline_stdout.is_empty());
    assert!(baseline_stderr.is_empty());
    assert_eq!(
        std::fs::read(&destination).expect("read destination"),
        b"olddata"
    );

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--ignore-times"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"newdata"
    );
}
