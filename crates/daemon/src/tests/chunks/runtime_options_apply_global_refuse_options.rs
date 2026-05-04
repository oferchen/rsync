#[test]
fn runtime_options_apply_global_refuse_options() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "refuse options = compress, delete\n[docs]\npath = /srv/docs\n[logs]\npath = /srv/logs\nrefuse options = stats\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config with global refuse options");

    assert_eq!(
        options.modules()[0].refused_options(),
        ["compress", "delete"]
    );
    assert_eq!(options.modules()[1].refused_options(), ["stats"]);
}

