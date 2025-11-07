#[test]
fn runtime_options_config_pid_file_respects_cli_override() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    writeln!(
        File::create(&config_path).expect("create config"),
        "pid file = config.pid\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let cli_pid = PathBuf::from("/var/run/override.pid");
    let options = RuntimeOptions::parse(&[
        OsString::from("--pid-file"),
        cli_pid.as_os_str().to_os_string(),
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with cli override");

    assert_eq!(options.pid_file(), Some(cli_pid.as_path()));
}

