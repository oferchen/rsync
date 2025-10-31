use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_implied_dirs_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--implied-dirs"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.implied_dirs, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-implied-dirs"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.implied_dirs, Some(false));
}
