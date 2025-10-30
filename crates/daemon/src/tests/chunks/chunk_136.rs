#[test]
fn runtime_options_rejects_duplicate_module_across_config_and_cli() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
        OsString::from("--module"),
        OsString::from("docs=/other/path"),
    ])
    .expect_err("duplicate module should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate module definition 'docs'")
    );
}

