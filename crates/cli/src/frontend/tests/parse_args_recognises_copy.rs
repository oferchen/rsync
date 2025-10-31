use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_copy_links_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--copy-links"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.copy_links, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-copy-links"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.copy_links, Some(false));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-L"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.copy_links, Some(true));
}

#[test]
fn parse_args_recognises_copy_unsafe_links_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--copy-unsafe-links"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.copy_unsafe_links, Some(true));
    assert!(parsed.safe_links);

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-copy-unsafe-links"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.copy_unsafe_links, Some(false));
}

#[test]
fn parse_args_recognises_copy_dirlinks_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--copy-dirlinks"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.copy_dirlinks);

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-k"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.copy_dirlinks);
}
