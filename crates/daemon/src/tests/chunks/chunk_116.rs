#[test]
fn runtime_options_loads_config_from_branded_environment_variable() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let config_path = dir.path().join("oc-rsyncd.conf");
    fs::write(
        &config_path,
        format!("[data]\npath = {}\n", module_dir.display()),
    )
    .expect("write config");

    let _env = EnvGuard::set(BRANDED_CONFIG_ENV, config_path.as_os_str());
    let options = RuntimeOptions::parse(&[]).expect("parse env config");

    assert_eq!(options.modules().len(), 1);
    let module = &options.modules()[0];
    assert_eq!(module.name, "data");
    assert_eq!(module.path, module_dir);
    assert_eq!(
        &options.delegate_arguments,
        &[
            OsString::from("--config"),
            config_path.clone().into_os_string(),
        ]
    );
}

