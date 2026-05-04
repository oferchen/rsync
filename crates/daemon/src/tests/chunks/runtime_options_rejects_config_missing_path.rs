#[test]
fn runtime_options_rejects_config_missing_path() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\ncomment = sample\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("missing path should error");

    assert!(
        error
            .message()
            .to_string()
            .contains("missing required 'path' directive")
    );
}

