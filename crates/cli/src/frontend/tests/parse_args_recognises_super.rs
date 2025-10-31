use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_super_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--super"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.super_mode, Some(true));
}
