#[test]
fn runtime_options_rejects_invalid_bwlimit_in_config() {
    let mut file = NamedTempFile::new().expect("config file");
    // upstream: the daemon `bwlimit` config directive is a global-section
    // parameter; a bad value is rejected at parse time.
    writeln!(file, "bwlimit = nope\n[docs]\npath = /srv/docs\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("invalid bwlimit should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("invalid 'bwlimit' value 'nope'")
    );
}

