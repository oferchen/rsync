#[test]
fn runtime_options_rejects_empty_refuse_options_directive() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nrefuse options =   \n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("empty refuse options should fail");

    let rendered = error.message().to_string();
    assert!(rendered.contains("must specify at least one option"));
}

