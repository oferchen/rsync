use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_hard_links_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--hard-links"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.hard_links, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-hard-links"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.hard_links, Some(false));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-H"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.hard_links, Some(true));
}
