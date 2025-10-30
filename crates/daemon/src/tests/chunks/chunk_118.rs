#[test]
fn runtime_options_branded_config_env_overrides_legacy_env() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let dir = tempdir().expect("config dir");
    let branded_dir = dir.path().join("branded");
    let legacy_dir = dir.path().join("legacy");
    fs::create_dir_all(&branded_dir).expect("branded module dir");
    fs::create_dir_all(&legacy_dir).expect("legacy module dir");

    let branded_config = dir.path().join("oc.conf");
    fs::write(
        &branded_config,
        format!("[branded]\npath = {}\n", branded_dir.display()),
    )
    .expect("write branded config");

    let legacy_config = dir.path().join("legacy.conf");
    fs::write(
        &legacy_config,
        format!("[legacy]\npath = {}\n", legacy_dir.display()),
    )
    .expect("write legacy config");

    let _legacy = EnvGuard::set(LEGACY_CONFIG_ENV, legacy_config.as_os_str());
    let _branded = EnvGuard::set(BRANDED_CONFIG_ENV, branded_config.as_os_str());
    let options = RuntimeOptions::parse(&[]).expect("parse env config");

    assert_eq!(options.modules().len(), 1);
    let module = &options.modules()[0];
    assert_eq!(module.name, "branded");
    assert_eq!(module.path, branded_dir);
    assert_eq!(
        &options.delegate_arguments,
        &[
            OsString::from("--config"),
            branded_config.clone().into_os_string(),
        ]
    );
}

