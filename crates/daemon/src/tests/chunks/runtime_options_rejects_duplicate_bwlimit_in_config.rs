#[test]
fn runtime_options_rejects_duplicate_bwlimit_in_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nbwlimit = 1M\nbwlimit = 2M\n"
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("duplicate bwlimit should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate 'bwlimit' directive")
    );
}

