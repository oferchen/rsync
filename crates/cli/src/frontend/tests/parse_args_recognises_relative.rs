use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_relative_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--relative"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.relative, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-relative"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.relative, Some(false));
}
