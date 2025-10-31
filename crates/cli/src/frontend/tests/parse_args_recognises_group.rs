use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_group_overrides() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--group"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.group, Some(true));
    assert_eq!(parsed.owner, None);

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("--no-group"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.group, Some(false));
    assert!(parsed.archive);
}
