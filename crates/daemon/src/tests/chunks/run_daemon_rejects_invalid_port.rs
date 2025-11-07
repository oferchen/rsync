#[test]
fn run_daemon_rejects_invalid_port() {
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([OsString::from("--port"), OsString::from("not-a-number")])
        .build();

    let error = run_daemon(config).expect_err("invalid port should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("invalid value for --port")
    );
}

