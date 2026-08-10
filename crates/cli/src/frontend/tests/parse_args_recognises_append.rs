use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_append_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--append"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.append, Some(true));
    assert!(!parsed.append_verify);

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-append"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.append, Some(false));
    assert!(!parsed.append_verify);
}

#[test]
fn parse_args_recognises_append_verify_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--append-verify"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.append, Some(true));
    assert!(parsed.append_verify);
}
