#[test]
fn runtime_options_last_detach_flag_wins_detach_after_no_detach() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _config = EnvGuard::remove(BRANDED_CONFIG_ENV);
    let _config_legacy = EnvGuard::remove(LEGACY_CONFIG_ENV);

    let options = RuntimeOptions::parse(&[
        OsString::from("--no-detach"),
        OsString::from("--detach"),
    ])
    .expect("parse --no-detach --detach");

    assert!(
        options.detach(),
        "last --detach flag should win"
    );
}

#[test]
fn runtime_options_last_detach_flag_wins_no_detach_after_detach() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _config = EnvGuard::remove(BRANDED_CONFIG_ENV);
    let _config_legacy = EnvGuard::remove(LEGACY_CONFIG_ENV);

    let options = RuntimeOptions::parse(&[
        OsString::from("--detach"),
        OsString::from("--no-detach"),
    ])
    .expect("parse --detach --no-detach");

    assert!(
        !options.detach(),
        "last --no-detach flag should win"
    );
}
