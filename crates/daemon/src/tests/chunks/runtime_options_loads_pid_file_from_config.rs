/// A relative `pid file` reaches the runtime verbatim.
///
/// upstream: clientserver.c:1584 `create_pid_file()` opens the string
/// `lp_pid_file()` returns - loadparm.c stores parameter values as written -
/// so a relative value names a path under the daemon's working directory.
/// This test previously asserted the config file's directory as the prefix,
/// which is where oc used to put it; that is the behaviour being removed, not
/// a contract to preserve.
#[test]
fn runtime_options_loads_pid_file_from_config() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    writeln!(
        File::create(&config_path).expect("create config"),
        "pid file = daemon.pid\n[docs]\npath = /srv/docs\nuse chroot = no\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with pid file");

    assert_eq!(options.pid_file(), Some(Path::new("daemon.pid")));
}
