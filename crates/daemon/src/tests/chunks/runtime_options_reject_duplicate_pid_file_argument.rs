#[test]
fn runtime_options_reject_duplicate_pid_file_argument() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--pid-file"),
        OsString::from("/var/run/one.pid"),
        OsString::from("--pid-file"),
        OsString::from("/var/run/two.pid"),
    ])
    .expect_err("duplicate pid file should fail");

    assert!(error.message().to_string().contains("--pid-file"));
}

