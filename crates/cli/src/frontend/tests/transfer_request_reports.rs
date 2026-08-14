use super::common::*;
use super::*;
use crate::frontend::execution::render_missing_operands_stdout;

#[test]
fn transfer_request_reports_missing_operands() {
    let (code, stdout, stderr) = run_with_args([OsString::from(RSYNC)]);

    assert_eq!(code, 1);
    let stdout_rendered = String::from_utf8(stdout).expect("usage banner utf8");
    let expected_usage = render_missing_operands_stdout(ProgramName::Rsync);
    assert_eq!(stdout_rendered, expected_usage);

    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains("syntax or usage error"));
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

    // upstream: exclude.c:1712-1719 - RERR_FILEIO (11), not the generic 1 this
    // asserted before, and upstream's own wording.
    assert_eq!(code, 11);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic utf8");
    assert!(
        rendered.contains("failed to open exclude file missing.txt"),
        "unexpected diagnostic: {rendered}"
    );
    assert_contains_client_trailer(&rendered);
}
