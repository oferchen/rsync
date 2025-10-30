#[test]
fn runtime_options_loads_pid_file_from_config() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    writeln!(
        File::create(&config_path).expect("create config"),
        "pid file = daemon.pid\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with pid file");

    let expected = dir.path().join("daemon.pid");
    assert_eq!(options.pid_file(), Some(expected.as_path()));
}

