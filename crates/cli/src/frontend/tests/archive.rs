use super::common::*;
use super::*;

#[test]
fn archive_flag_is_parsed() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse succeeds");

    assert!(parsed.archive);
    assert_eq!(parsed.owner, None);
    assert_eq!(parsed.group, None);
}
