#[test]
fn runtime_options_require_secrets_file_with_auth_users() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[secure]\npath = /srv/secure\nauth users = alice\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("missing secrets file should error");

    assert!(
        error
            .message()
            .to_string()
            .contains("missing the required 'secrets file' directive")
    );
}

