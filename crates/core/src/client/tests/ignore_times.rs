#[test]
fn sequential_runs_respect_ignore_times() {
    use filetime::{set_file_times, FileTime};

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("destination.txt");
    fs::write(&source, b"newdata").expect("write source");
    fs::write(&destination, b"olddata").expect("write destination");

    let timestamp = std::time::UNIX_EPOCH + Duration::from_secs(1_700_200_000);
    let filetime = FileTime::from_system_time(timestamp);
    set_file_times(&source, filetime, filetime).expect("source times");
    set_file_times(&destination, filetime, filetime).expect("dest times");

    let operands = vec![source.clone().into_os_string(), destination.clone().into_os_string()];

    let baseline_config = ClientConfig::builder().transfer_args(operands.clone()).build();
    run_client(baseline_config).expect("baseline run");
    assert_eq!(fs::read(&destination).expect("read dest"), b"olddata");

    let ignore_config = ClientConfig::builder()
        .transfer_args(operands)
        .ignore_times(true)
        .build();
    run_client(ignore_config).expect("ignore run");
    assert_eq!(fs::read(&destination).expect("read dest"), b"newdata");
}
