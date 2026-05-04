#[test]
fn runtime_options_loads_lock_file_from_config() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    writeln!(
        File::create(&config_path).expect("create config"),
        "lock file = daemon.lock\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with lock file");

    let expected = dir.path().join("daemon.lock");
    assert_eq!(options.lock_file(), Some(expected.as_path()));
}

