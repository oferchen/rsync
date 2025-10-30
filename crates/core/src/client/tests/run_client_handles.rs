use super::prelude::*;


#[test]
fn run_client_handles_delta_transfer_mode_locally() {
    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("source.bin");
    let destination = tmp.path().join("dest.bin");
    fs::write(&source, b"payload").expect("write source");

    let config = ClientConfig::builder()
        .transfer_args([
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ])
        .whole_file(false)
        .build();

    let summary = run_client(config).expect("delta mode executes locally");

    assert_eq!(fs::read(&destination).expect("read dest"), b"payload");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), b"payload".len() as u64);
}

