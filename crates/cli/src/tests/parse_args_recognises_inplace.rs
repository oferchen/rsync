use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_inplace_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--inplace"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.inplace, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-inplace"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.inplace, Some(false));
}
