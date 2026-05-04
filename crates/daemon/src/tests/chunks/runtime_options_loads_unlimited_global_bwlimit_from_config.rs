#[test]
fn runtime_options_loads_unlimited_global_bwlimit_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "bwlimit = 0\n[docs]\npath = /srv/docs\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    assert!(options.bandwidth_limit().is_none());
    assert!(options.bandwidth_burst().is_none());
    assert!(options.bandwidth_limit_configured());
}

