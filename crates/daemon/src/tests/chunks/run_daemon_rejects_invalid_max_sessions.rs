#[test]
fn run_daemon_rejects_invalid_max_sessions() {
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([OsString::from("--max-sessions"), OsString::from("0")])
        .build();

    let error = run_daemon(config).expect_err("invalid max sessions should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("--max-sessions must be greater than zero")
    );
}

