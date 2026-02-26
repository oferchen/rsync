#[test]
fn runtime_options_detach_default_matches_platform() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _config = EnvGuard::remove(BRANDED_CONFIG_ENV);
    let _config_legacy = EnvGuard::remove(LEGACY_CONFIG_ENV);

    let options = RuntimeOptions::default();

    #[cfg(unix)]
    assert!(
        options.detach(),
        "detach should default to true on Unix"
    );
    #[cfg(not(unix))]
    assert!(
        !options.detach(),
        "detach should default to false on non-Unix"
    );
}
