use super::common::*;
use super::*;

#[test]
fn transfer_request_reports_missing_operands() {
    let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC)]);

    assert_eq!(code, 1);
    let stdout_rendered = String::from_utf8(stdout).expect("usage banner utf8");
    let expected_usage = format!("{}\n", clap_command(RSYNC).render_usage());
    assert_eq!(stdout_rendered, expected_usage);

    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("missing source operands"));
    assert_contains_client_trailer(&rendered);
}

#[test]
fn transfer_request_reports_filter_file_errors() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--exclude-from"),
        OsString::from("missing.txt"),
        OsString::from("src"),
        OsString::from("dst"),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic utf8");
    assert!(rendered.contains("failed to read filter file 'missing.txt'"));
    assert_contains_client_trailer(&rendered);
}
