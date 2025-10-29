#[test]
fn runtime_options_parse_lock_file_argument() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--lock-file"),
        OsString::from("/var/run/rsyncd.lock"),
    ])
    .expect("parse lock file argument");

    assert_eq!(options.lock_file(), Some(Path::new("/var/run/rsyncd.lock")));
}

#[test]
fn runtime_options_reject_duplicate_lock_file_argument() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--lock-file"),
        OsString::from("/tmp/one.lock"),
        OsString::from("--lock-file"),
        OsString::from("/tmp/two.lock"),
    ])
    .expect_err("duplicate lock file should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate daemon argument '--lock-file'")
    );
}

#[test]
fn runtime_options_parse_motd_sources() {
    let dir = tempdir().expect("motd dir");
    let motd_path = dir.path().join("motd.txt");
    fs::write(&motd_path, "Welcome to rsyncd\nSecond line\n").expect("write motd");

    let options = RuntimeOptions::parse(&[
        OsString::from("--motd-file"),
        motd_path.as_os_str().to_os_string(),
        OsString::from("--motd-line"),
        OsString::from("Trailing notice"),
    ])
    .expect("parse motd options");

    let expected = vec![
        String::from("Welcome to rsyncd"),
        String::from("Second line"),
        String::from("Trailing notice"),
    ];

    assert_eq!(options.motd_lines(), expected.as_slice());
}

#[test]
fn runtime_options_loads_motd_from_config_directives() {
    let dir = tempdir().expect("motd dir");
    let config_path = dir.path().join("rsyncd.conf");
    let motd_path = dir.path().join("motd.txt");
    fs::write(&motd_path, "First line\nSecond line\r\n").expect("write motd file");

    fs::write(
        &config_path,
        "motd file = motd.txt\nmotd = Inline note\n[docs]\npath = /srv/docs\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with motd directives");

    let expected = vec![
        String::from("First line"),
        String::from("Second line"),
        String::from("Inline note"),
    ];

    assert_eq!(options.motd_lines(), expected.as_slice());
}

#[test]
fn runtime_options_default_enables_reverse_lookup() {
    let options = RuntimeOptions::parse(&[]).expect("parse defaults");
    assert!(options.reverse_lookup());
}

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

#[test]
fn runtime_options_loads_config_from_legacy_environment_variable() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!("[legacy]\npath = {}\n", module_dir.display()),
    )
    .expect("write config");

    let _env = EnvGuard::set(LEGACY_CONFIG_ENV, config_path.as_os_str());
    let options = RuntimeOptions::parse(&[]).expect("parse env config");

    assert_eq!(options.modules().len(), 1);
    let module = &options.modules()[0];
    assert_eq!(module.name, "legacy");
    assert_eq!(module.path, module_dir);
    assert_eq!(
        &options.delegate_arguments,
        &[
            OsString::from("--config"),
            config_path.clone().into_os_string(),
        ]
    );
}

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

#[test]
fn runtime_options_default_secrets_path_updates_delegate_arguments() {
    let dir = tempdir().expect("config dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }

    let options =
        with_test_secrets_candidates(vec![secrets_path.clone()], || RuntimeOptions::parse(&[]))
            .expect("parse defaults with secrets override");

    assert_eq!(
        options.delegate_arguments,
        [
            OsString::from("--secrets-file"),
            secrets_path.into_os_string(),
        ]
    );
}

#[test]
fn runtime_options_loads_secrets_from_branded_environment_variable() {
    let dir = tempdir().expect("secrets dir");
    let secrets_path = dir.path().join("branded.txt");
    fs::write(&secrets_path, "alice:secret\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }

    let options = with_test_secrets_env(
        Some(TestSecretsEnvOverride {
            branded: Some(secrets_path.clone().into_os_string()),
            legacy: None,
        }),
        || RuntimeOptions::parse(&[]),
    )
    .expect("parse env secrets");

    assert_eq!(
        options.delegate_arguments,
        [
            OsString::from("--secrets-file"),
            secrets_path.clone().into_os_string(),
        ]
    );
}

#[test]
fn runtime_options_loads_secrets_from_legacy_environment_variable() {
    let dir = tempdir().expect("secrets dir");
    let secrets_path = dir.path().join("legacy.txt");
    fs::write(&secrets_path, "bob:secret\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }

    let options = with_test_secrets_env(
        Some(TestSecretsEnvOverride {
            branded: None,
            legacy: Some(secrets_path.clone().into_os_string()),
        }),
        || RuntimeOptions::parse(&[]),
    )
    .expect("parse env secrets");

    assert_eq!(
        options.delegate_arguments,
        [
            OsString::from("--secrets-file"),
            secrets_path.clone().into_os_string(),
        ]
    );
}

#[test]
fn runtime_options_branded_secrets_env_overrides_legacy_env() {
    let dir = tempdir().expect("secrets dir");
    let branded_path = dir.path().join("branded.txt");
    let legacy_path = dir.path().join("legacy.txt");
    fs::write(&branded_path, "carol:secret\n").expect("write branded secrets");
    fs::write(&legacy_path, "dave:secret\n").expect("write legacy secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&branded_path, PermissionsExt::from_mode(0o600))
            .expect("chmod branded secrets");
        fs::set_permissions(&legacy_path, PermissionsExt::from_mode(0o600))
            .expect("chmod legacy secrets");
    }

    let options = with_test_secrets_env(
        Some(TestSecretsEnvOverride {
            branded: Some(branded_path.clone().into_os_string()),
            legacy: Some(legacy_path.clone().into_os_string()),
        }),
        || RuntimeOptions::parse(&[]),
    )
    .expect("parse env secrets");

    let delegate = &options.delegate_arguments;
    let expected_tail = [
        OsString::from("--secrets-file"),
        branded_path.clone().into_os_string(),
    ];
    assert!(delegate.ends_with(&expected_tail));
    assert!(
        !delegate.iter().any(|arg| arg == legacy_path.as_os_str()),
        "legacy secrets path should not be forwarded"
    );
}

#[test]
fn runtime_options_rejects_missing_secrets_from_environment() {
    let missing = OsString::from("/nonexistent/secrets.txt");
    let options = with_test_secrets_env(
        Some(TestSecretsEnvOverride {
            branded: Some(missing.clone()),
            legacy: None,
        }),
        || RuntimeOptions::parse(&[]),
    )
    .expect("missing secrets should be ignored");
    assert!(
        !options
            .delegate_arguments
            .iter()
            .any(|arg| arg == "--secrets-file"),
        "no secrets override should be forwarded when the environment path is missing"
    );
}

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
            cli_config.clone().into_os_string(),
        ]
    );
}

#[test]
fn runtime_options_loads_reverse_lookup_from_config() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        "reverse lookup = no\n[docs]\npath = /srv/docs\n",
    )
    .expect("write config");

    let args = [
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ];
    let options = RuntimeOptions::parse(&args).expect("parse config");
    assert!(!options.reverse_lookup());
}

#[test]
fn runtime_options_rejects_duplicate_reverse_lookup_directive() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        "reverse lookup = yes\nreverse lookup = no\n[docs]\npath = /srv/docs\n",
    )
    .expect("write config");

    let args = [
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ];
    let error = RuntimeOptions::parse(&args).expect_err("duplicate reverse lookup");
    assert!(format!("{error}").contains("reverse lookup"));
}

#[test]
fn runtime_options_parse_hosts_allow_and_deny() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nhosts allow = 127.0.0.1,192.168.0.0/24\nhosts deny = 192.168.0.5\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse hosts directives");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);

    let module = &modules[0];
    assert_eq!(module.hosts_allow.len(), 2);
    assert!(matches!(
        module.hosts_allow[0],
        HostPattern::Ipv4 { prefix: 32, .. }
    ));
    assert!(matches!(
        module.hosts_allow[1],
        HostPattern::Ipv4 { prefix: 24, .. }
    ));
    assert_eq!(module.hosts_deny.len(), 1);
    assert!(matches!(
        module.hosts_deny[0],
        HostPattern::Ipv4 { prefix: 32, .. }
    ));
}

#[test]
fn runtime_options_parse_hostname_patterns() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nhosts allow = trusted.example.com,.example.org\nhosts deny = bad?.example.net\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse hostname hosts directives");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);

    let module = &modules[0];
    assert_eq!(module.hosts_allow.len(), 2);
    assert!(matches!(module.hosts_allow[0], HostPattern::Hostname(_)));
    assert!(matches!(module.hosts_allow[1], HostPattern::Hostname(_)));
    assert_eq!(module.hosts_deny.len(), 1);
    assert!(matches!(module.hosts_deny[0], HostPattern::Hostname(_)));
}

#[test]
fn runtime_options_parse_auth_users_and_secrets_file() {
    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }

    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[secure]\npath = {}\nauth users = alice, bob\nsecrets file = {}\n",
        module_dir.display(),
        secrets_path.display()
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse auth users");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(
        module.auth_users(),
        &[String::from("alice"), String::from("bob")]
    );
    assert_eq!(module.secrets_file(), Some(secrets_path.as_path()));
}

