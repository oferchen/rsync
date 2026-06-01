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
    // The error diagnostic must reference the problematic flag. Clap's exact
    // wording varies across minor versions so we check for the flag name
    // rather than a full phrase.
    assert!(
        rendered.contains("--version"),
        "expected clap error referencing '--version', got: {rendered}"
    );
}
