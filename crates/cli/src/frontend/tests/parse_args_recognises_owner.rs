use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_owner_overrides() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--owner"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.owner, Some(true));
    assert_eq!(parsed.group, None);

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("--no-owner"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.owner, Some(false));
    assert!(parsed.archive);
}
