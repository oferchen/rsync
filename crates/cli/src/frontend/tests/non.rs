use super::common::*;
use super::*;

#[test]
fn non_numeric_protocol_value_reports_error() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--protocol=abc"),
        OsString::from("source"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
    assert!(rendered.contains("invalid protocol version 'abc'"));
    assert!(rendered.contains("unsigned integer"));
}
