#[test]
fn runtime_options_loads_lock_file_from_config() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    writeln!(
        File::create(&config_path).expect("create config"),
        "lock file = daemon.lock\n[docs]\npath = /srv/docs\nuse chroot = no\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with lock file");

    // upstream: connection.c claim_connection() opens the value as given, so
    // a relative lock file resolves against the daemon's cwd, not the config
    // file's directory.
    assert_eq!(options.lock_file(), Some(Path::new("daemon.lock")));
}
