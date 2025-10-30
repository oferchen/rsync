#[test]
fn runtime_options_rejects_duplicate_global_bwlimit() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "bwlimit = 1M\nbwlimit = 2M\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("duplicate global bwlimit should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate 'bwlimit' directive in global section")
    );
}

