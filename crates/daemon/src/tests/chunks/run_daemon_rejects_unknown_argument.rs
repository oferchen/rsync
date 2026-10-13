#[test]
fn run_daemon_rejects_unknown_argument() {
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([OsString::from("--unknown")])
        .build();

    let error = run_daemon(config).expect_err("unknown argument should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    // The cause line is a preamble printed before the trailer (options.c:1576).
    let rendered = error.to_string();
    assert!(
        rendered.contains("unknown option"),
        "diagnostic should classify unknown daemon flags"
    );
    assert!(
        rendered.contains("--help"),
        "diagnostic should point operators to the help output"
    );
}
