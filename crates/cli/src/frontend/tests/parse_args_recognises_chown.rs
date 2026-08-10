use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_chown_option() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--chown=user:group"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.chown, Some(OsString::from("user:group")));
}
