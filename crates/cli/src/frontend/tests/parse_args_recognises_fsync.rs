use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_fsync_toggle() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--fsync"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.fsync, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.fsync, None);
}
