use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_apple_double_skip_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--apple-double-skip"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.apple_double_skip);
}

#[test]
fn parse_args_apple_double_skip_defaults_off() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.apple_double_skip);
}
