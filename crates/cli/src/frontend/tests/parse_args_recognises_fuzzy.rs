use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_fuzzy_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--fuzzy"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.fuzzy, Some(1));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-fuzzy"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.fuzzy, Some(0));
}

#[test]
fn parse_args_recognises_double_fuzzy() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-yy"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.fuzzy, Some(2));
}
