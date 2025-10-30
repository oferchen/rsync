use super::common::*;
use super::*;

#[test]
fn checksum_with_no_times_preserves_existing_destination() {
    use filetime::{FileTime, set_file_mtime};
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source-checksum.txt");
    let destination = tmp.path().join("dest-checksum.txt");
    std::fs::write(&source, b"payload").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--no-times"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let preserved = FileTime::from_unix_time(1_700_200_000, 0);
    set_file_mtime(&destination, preserved).expect("set destination mtime");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--checksum"),
        OsString::from("--no-times"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let metadata = std::fs::metadata(&destination).expect("dest metadata");
    let final_mtime = FileTime::from_last_modification_time(&metadata);
    assert_eq!(final_mtime, preserved);
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"payload"
    );
}

#[test]
fn checksum_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--checksum"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert!(parsed.checksum);
}
