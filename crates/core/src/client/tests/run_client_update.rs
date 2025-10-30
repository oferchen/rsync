use super::prelude::*;


#[test]
fn run_client_update_skips_newer_destination() {
    use filetime::{FileTime, set_file_times};

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source-update.txt");
    let destination = tmp.path().join("dest-update.txt");
    fs::write(&source, b"fresh").expect("write source");
    fs::write(&destination, b"existing").expect("write destination");

    let older = FileTime::from_unix_time(1_700_000_000, 0);
    let newer = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_times(&source, older, older).expect("set source times");
    set_file_times(&destination, newer, newer).expect("set dest times");

    let summary = run_client(
        ClientConfig::builder()
            .transfer_args([
                source.clone().into_os_string(),
                destination.clone().into_os_string(),
            ])
            .update(true)
            .build(),
    )
    .expect("run client");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_skipped_newer(), 1);
    assert_eq!(
        fs::read(destination).expect("read destination"),
        b"existing"
    );
}

