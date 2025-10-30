use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_keep_dirlinks_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--keep-dirlinks"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.keep_dirlinks, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-keep-dirlinks"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.keep_dirlinks, Some(false));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-K"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.keep_dirlinks, Some(true));
}
