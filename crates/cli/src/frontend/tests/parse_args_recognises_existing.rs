use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_existing_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--existing"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.existing);
}
