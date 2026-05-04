#[test]
fn runtime_options_rejects_duplicate_global_chmod() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "incoming chmod = Duog\nincoming chmod = Other\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("duplicate incoming chmod should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate 'incoming chmod' directive")
    );
}
