#[test]
fn run_daemon_rejects_unknown_argument() {
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([OsString::from("--unknown")])
        .build();

    let error = run_daemon(config).expect_err("unknown argument should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("unsupported daemon argument")
    );
}

