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

    assert_eq!(
        options.global_secrets_file(),
        Some(branded_path.as_path()),
    );
    assert_ne!(
        options.global_secrets_file(),
        Some(legacy_path.as_path()),
        "legacy secrets path should not override branded path"
    );
}

