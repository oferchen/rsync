#[test]
fn runtime_options_loads_unlimited_max_connections_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nuse chroot = no\nmax connections = 0\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("config parses");

    // upstream: connection.c:27 - `max connections = 0` returns success
    // before the lock file is opened, so the module is unlimited.
    assert_eq!(
        options.modules[0].max_connections(),
        MaxConnections::Unlimited
    );
}
