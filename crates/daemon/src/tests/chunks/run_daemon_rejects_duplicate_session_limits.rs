#[test]
fn run_daemon_rejects_duplicate_session_limits() {
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--once"),
            OsString::from("--max-sessions"),
            OsString::from("2"),
        ])
        .build();

    let error = run_daemon(config).expect_err("duplicate session limits should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate daemon argument '--max-sessions'")
    );
}

