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

/// A merge name with a slash is reported cleaned and cwd-relative, never joined
/// onto an absolute directory: the operator sees the name they typed.
///
/// upstream: exclude.c:737-747 parse_merge_name() joins the name onto the still
/// empty `dirbuf`, so 3.5.2 prints `failed to open exclude file missing.txt`.
#[test]
fn transfer_request_reports_a_missing_merge_file_by_its_cleaned_name() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from(". sub/../missing.txt"),
        OsString::from("src"),
        OsString::from("dst"),
    ]);
    assert_eq!(code, 11);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic utf8");
    assert!(
        rendered.contains("failed to open exclude file missing.txt: No such file or directory (2)"),
        "unexpected diagnostic: {rendered}"
    );
    assert_contains_client_trailer(&rendered);
}

/// upstream: exclude.c:1714-1719 - a missing operator-named merge file is
/// rsyserr()'s `rsync: [client] ...` line and then log_exit()'s trailer, two
/// lines where oc printed a single `rsync error: ... (code 11)` line.
#[test]
fn transfer_request_reports_a_missing_merge_file_on_two_lines() {
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from(". missing.txt"),
        OsString::from("src"),
        OsString::from("dst"),
    ]);
    assert_eq!(code, 11);
    let rendered = String::from_utf8(stderr).expect("diagnostic utf8");
    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(lines.len(), 2, "{rendered}");
    assert_eq!(
        lines[0],
        "rsync: [client] failed to open exclude file missing.txt: No such file or directory (2)"
    );
    assert!(
        lines[1].starts_with("rsync error: error in file IO (code 11) at "),
        "{rendered}"
    );
    assert_contains_client_trailer(&rendered);
}

/// upstream: exclude.c:132-136 filter_rule_err() is a bare rprintf(FERROR)
/// line, so the rule error itself names no role; only the trailer does.
#[test]
fn transfer_request_reports_a_bad_filter_rule_as_a_bare_line() {
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-f"),
        OsString::from("x y"),
        OsString::from("src"),
        OsString::from("dst"),
    ]);
    assert_eq!(code, 1);
    let rendered = String::from_utf8(stderr).expect("diagnostic utf8");
    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(lines.len(), 2, "{rendered}");
    assert_eq!(lines[0], "Unknown filter rule: x y");
    assert!(
        lines[1].starts_with("rsync error: syntax or usage error (code 1) at "),
        "{rendered}"
    );
}
