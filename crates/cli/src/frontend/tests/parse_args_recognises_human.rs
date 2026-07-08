use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_human_readable_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--human-readable"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Enabled));
}

#[test]
fn parse_args_recognises_double_short_h() {
    // upstream: options.c:1573 - each -h increments; -hh reaches level 3
    // (base-1024 units, HumanReadableMode::Combined).
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-hh"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.human_readable, Some(HumanReadableMode::Combined));
}
