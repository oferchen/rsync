use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_msgs2stderr_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--msgs2stderr"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.msgs_to_stderr, Some(true));
}

#[test]
fn parse_args_recognises_no_msgs2stderr_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-msgs2stderr"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.msgs_to_stderr, Some(false));
}
