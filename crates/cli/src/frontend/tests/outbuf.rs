use super::common::*;
use super::*;

#[test]
fn outbuf_invalid_value_reports_error() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--outbuf=X"),
        OsString::from("src"),
        OsString::from("dst"),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic utf8");
    assert!(rendered.contains("Invalid --outbuf setting"));
    assert_contains_client_trailer(&rendered);
}

#[test]
fn outbuf_valid_value_allows_execution() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--outbuf=L"),
        OsString::from("--version"),
    ]);

    assert_eq!(code, 0);
    assert!(!stdout.is_empty());
    assert!(stderr.is_empty());
}
