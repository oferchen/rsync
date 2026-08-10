use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_whole_file_flags() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-W"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.whole_file, Some(true));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-whole-file"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.whole_file, Some(false));
}
