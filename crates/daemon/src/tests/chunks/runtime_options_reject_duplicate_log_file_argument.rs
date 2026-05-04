#[test]
fn runtime_options_reject_duplicate_log_file_argument() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--log-file"),
        OsString::from("/tmp/one.log"),
        OsString::from("--log-file"),
        OsString::from("/tmp/two.log"),
    ])
    .expect_err("duplicate log file should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate daemon argument '--log-file'")
    );
}

