#[test]
fn runtime_options_rejects_duplicate_refuse_options_directives() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nrefuse options = delete\nrefuse options = compress\n"
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("duplicate refuse options should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate 'refuse options' directive")
    );
}

