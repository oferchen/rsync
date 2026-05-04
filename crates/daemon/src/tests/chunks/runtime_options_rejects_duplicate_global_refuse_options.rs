#[test]
fn runtime_options_rejects_duplicate_global_refuse_options() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "refuse options = compress\nrefuse options = delete\n[docs]\npath = /srv/docs\n",
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("duplicate global refuse options should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate 'refuse options' directive")
    );
}

