#[test]
fn runtime_options_rejects_invalid_boolean_directive() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nread only = maybe\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("invalid boolean should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("invalid boolean value 'maybe'")
    );
}

