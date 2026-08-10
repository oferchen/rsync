use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_force_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--force"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.force, Some(true));
}

#[test]
fn parse_args_recognises_no_force_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-force"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.force, Some(false));
}
