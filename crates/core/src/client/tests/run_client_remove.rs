use super::prelude::*;


#[test]
fn run_client_remove_source_files_deletes_source() {
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("dest.txt");
    fs::write(&source, b"move me").expect("write source");

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .remove_source_files(true)
        .build();

    let summary = run_client(config).expect("copy succeeds");

    assert_eq!(summary.sources_removed(), 1);
    assert!(!source.exists(), "source should be removed after transfer");
    assert_eq!(fs::read(&destination).expect("read dest"), b"move me");
}


#[test]
fn run_client_remove_source_files_preserves_matched_source() {
    use filetime::{FileTime, set_file_times};

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("dest.txt");
    let payload = b"stable";
    fs::write(&source, payload).expect("write source");
    fs::write(&destination, payload).expect("write destination");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&source, timestamp, timestamp).expect("set source times");
    set_file_times(&destination, timestamp, timestamp).expect("set dest times");

    let config = ClientConfig::builder()
        .transfer_args([source.clone(), destination.clone()])
        .remove_source_files(true)
        .times(true)
        .build();

    let summary = run_client(config).expect("transfer succeeds");

    assert_eq!(summary.sources_removed(), 0, "unchanged sources remain");
    assert!(source.exists(), "matched source should not be removed");
    assert_eq!(fs::read(&destination).expect("read dest"), payload);
}

