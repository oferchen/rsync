#[test]
fn runtime_options_rejects_invalid_timeout() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\ntimeout = never\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("invalid timeout should fail");

    assert!(error.message().to_string().contains("invalid timeout"));
}

