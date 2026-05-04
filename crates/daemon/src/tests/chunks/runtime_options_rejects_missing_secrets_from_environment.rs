#[test]
fn runtime_options_rejects_missing_secrets_from_environment() {
    let missing = OsString::from("/nonexistent/secrets.txt");
    let options = with_test_secrets_env(
        Some(TestSecretsEnvOverride {
            branded: Some(missing),
            legacy: None,
        }),
        || RuntimeOptions::parse(&[]),
    )
    .expect("missing secrets should be ignored");
    assert!(
        options.global_secrets_file().is_none(),
        "no secrets override should be loaded when the environment path is missing"
    );
}

