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

