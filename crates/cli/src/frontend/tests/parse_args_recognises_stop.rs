use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_stop_after_option() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--stop-after=5"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.stop_after, Some(OsString::from("5")));
    assert!(parsed.stop_at.is_none());
}

#[test]
fn parse_args_recognises_time_limit_alias() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--time-limit=15"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.stop_after, Some(OsString::from("15")));
    assert!(parsed.stop_at.is_none());
}

#[test]
fn parse_args_recognises_stop_at_option() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--stop-at=2099-12-31T23:59"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.stop_at, Some(OsString::from("2099-12-31T23:59")));
    assert!(parsed.stop_after.is_none());
}
