use super::common::*;
use super::*;

#[test]
fn clap_parse_error_is_reported_via_message() {
    let command = clap_command(rsync_core::version::PROGRAM_NAME);
    let error = command
        .try_get_matches_from(vec!["rsync", "--version=extra"])
        .unwrap_err();

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = run(
        [OsString::from(RSYNC), OsString::from("--version=extra")],
        &mut stdout,
        &mut stderr,
    );

    assert_eq!(status, 1);
    assert!(stdout.is_empty());

    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains(error.to_string().trim()));
}
