#[test]
fn runtime_options_rejects_invalid_max_connections() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nmax connections = nope\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("invalid max connections should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("invalid max connections value")
    );
}

