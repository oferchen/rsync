#[test]
fn runtime_options_config_lock_file_respects_cli_override() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    writeln!(
        File::create(&config_path).expect("create config"),
        "lock file = config.lock\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let cli_lock = PathBuf::from("/var/run/override.lock");
    let options = RuntimeOptions::parse(&[
        OsString::from("--lock-file"),
        cli_lock.as_os_str().to_os_string(),
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with cli lock override");

    assert_eq!(options.lock_file(), Some(cli_lock.as_path()));
}

