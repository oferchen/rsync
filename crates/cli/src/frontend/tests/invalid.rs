use super::common::*;
use super::*;

#[test]
fn out_of_range_protocol_value_reports_error() {
    // upstream: compat.c:635 rejects a value above PROTOCOL_VERSION (32) with
    // RERR_PROTOCOL (errcode.h:26 -> exit 2). Protocol 27 is NOT rejected: it is
    // within upstream's 20..=32 command-line range and is accepted for a local
    // copy (exit 0). Use an out-of-range value to exercise the reject path.
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--protocol=33"),
        OsString::from("source"),
        OsString::from("dest"),
    ]);

    assert_eq!(code, 2); // Mirror upstream: protocol incompatibility returns exit code 2
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("diagnostic is UTF-8");
    assert!(rendered.contains("invalid protocol version '33'"));
    assert!(rendered.contains("no more than 32"));
}
