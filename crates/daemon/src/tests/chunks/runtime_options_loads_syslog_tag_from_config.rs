#[test]
fn runtime_options_loads_syslog_tag_from_config() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    writeln!(
        File::create(&config_path).expect("create config"),
        "syslog tag = mybackup\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with syslog tag");

    assert_eq!(options.syslog_tag(), "mybackup");
}
