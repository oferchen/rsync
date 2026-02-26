#[test]
fn runtime_options_no_detach_disables_detach() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _config = EnvGuard::remove(BRANDED_CONFIG_ENV);
    let _config_legacy = EnvGuard::remove(LEGACY_CONFIG_ENV);

    let options = RuntimeOptions::parse(&[OsString::from("--no-detach")])
        .expect("parse --no-detach");

    assert!(
        !options.detach(),
        "--no-detach should disable detach regardless of platform"
    );
}
