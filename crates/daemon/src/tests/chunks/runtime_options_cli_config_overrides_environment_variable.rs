#[test]
fn runtime_options_cli_config_overrides_environment_variable() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let dir = tempdir().expect("config dir");
    let env_module_dir = dir.path().join("env-module");
    let cli_module_dir = dir.path().join("cli-module");
    fs::create_dir_all(&env_module_dir).expect("env module dir");
    fs::create_dir_all(&cli_module_dir).expect("cli module dir");

    let env_config = dir.path().join("env.conf");
    fs::write(
        &env_config,
        format!("[env]\npath = {}\n", env_module_dir.display()),
    )
    .expect("write env config");

    let cli_config = dir.path().join("cli.conf");
    fs::write(
        &cli_config,
        format!("[cli]\npath = {}\n", cli_module_dir.display()),
    )
    .expect("write cli config");

    let _env = EnvGuard::set(LEGACY_CONFIG_ENV, env_config.as_os_str());
    let args = [
        OsString::from("--config"),
        cli_config.clone().into_os_string(),
    ];
    let options = RuntimeOptions::parse(&args).expect("parse cli config");

    assert_eq!(options.modules().len(), 1);
    let module = &options.modules()[0];
    assert_eq!(module.name, "cli");
    assert_eq!(module.path, cli_module_dir);
    assert_eq!(
        options.delegate_arguments,
        vec![
            OsString::from("--config"),
            cli_config.into_os_string(),
        ]
    );
}

