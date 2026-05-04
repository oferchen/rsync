use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_specials_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--specials"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.specials, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-specials"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.specials, Some(false));
}
