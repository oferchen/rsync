#[test]
fn clap_parse_error_is_reported_via_message() {
    let command = clap_command(Brand::Upstream.daemon_program_name());
    let _error = command
        .try_get_matches_from(vec!["rsyncd", "--version=extra"])
        .unwrap_err();

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = run(
        [OsString::from(RSYNCD), OsString::from("--version=extra")],
        &mut stdout,
        &mut stderr,
    );

    assert_eq!(status, 1);
    assert!(stdout.is_empty());

    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    // The error message should indicate that the argument is invalid
    // (clap wording may vary, but it should mention the unexpected value)
    assert!(rendered.contains("unexpected value 'extra' for '--version' found")
            || rendered.contains("unexpected argument '--version=extra'"));
}
