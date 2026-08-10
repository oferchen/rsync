#[test]
fn runtime_options_detach_enables_detach() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _config = EnvGuard::remove(BRANDED_CONFIG_ENV);
    let _config_legacy = EnvGuard::remove(LEGACY_CONFIG_ENV);

    let options = RuntimeOptions::parse(&[OsString::from("--detach")])
        .expect("parse --detach");

    assert!(
        options.detach(),
        "--detach should explicitly enable detach"
    );
}
