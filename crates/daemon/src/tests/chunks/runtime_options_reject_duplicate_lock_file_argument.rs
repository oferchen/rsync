#[test]
fn runtime_options_reject_duplicate_lock_file_argument() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--lock-file"),
        OsString::from("/tmp/one.lock"),
        OsString::from("--lock-file"),
        OsString::from("/tmp/two.lock"),
    ])
    .expect_err("duplicate lock file should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate daemon argument '--lock-file'")
    );
}

