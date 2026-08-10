use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_preallocate_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--preallocate"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.preallocate);

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.preallocate);
}
