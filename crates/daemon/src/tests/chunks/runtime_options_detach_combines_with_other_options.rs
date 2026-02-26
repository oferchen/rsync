#[test]
fn runtime_options_no_detach_combines_with_port_and_once() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _config = EnvGuard::remove(BRANDED_CONFIG_ENV);
    let _config_legacy = EnvGuard::remove(LEGACY_CONFIG_ENV);

    let options = RuntimeOptions::parse(&[
        OsString::from("--no-detach"),
        OsString::from("--port"),
        OsString::from("9999"),
        OsString::from("--once"),
    ])
    .expect("parse combined args");

    assert!(
        !options.detach(),
        "--no-detach should remain in effect when combined with other options"
    );
}

#[test]
fn runtime_options_detach_combines_with_log_file() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _config = EnvGuard::remove(BRANDED_CONFIG_ENV);
    let _config_legacy = EnvGuard::remove(LEGACY_CONFIG_ENV);

    let options = RuntimeOptions::parse(&[
        OsString::from("--detach"),
        OsString::from("--log-file"),
        OsString::from("/var/log/rsyncd.log"),
    ])
    .expect("parse --detach with --log-file");

    assert!(
        options.detach(),
        "--detach should remain in effect when combined with other options"
    );
    assert_eq!(
        options.log_file(),
        Some(&PathBuf::from("/var/log/rsyncd.log"))
    );
}
