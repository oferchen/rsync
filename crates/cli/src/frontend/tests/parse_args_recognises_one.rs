use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_one_file_system_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--one-file-system"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.one_file_system, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-one-file-system"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.one_file_system, Some(false));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-x"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.one_file_system, Some(true));
}
