use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_iconv_specification() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--iconv=utf8,iso88591"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.iconv, Some(OsString::from("utf8,iso88591")));
    assert!(!parsed.no_iconv);
}

#[test]
fn parse_args_recognises_no_iconv_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-iconv"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.iconv.is_none());
    assert!(parsed.no_iconv);
}
