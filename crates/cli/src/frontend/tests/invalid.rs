use super::common::*;
use super::*;

#[test]
fn invalid_protocol_value_reports_error() {
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--protocol=27"),
        OsString::from("source"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 2); // Mirror upstream: protocol incompatibility returns exit code 2
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
    assert!(rendered.contains("invalid protocol version '27'"));
    assert!(rendered.contains("outside the supported range"));
}
