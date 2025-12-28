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
            secrets_path.into_os_string(),
        ]
    );
}

