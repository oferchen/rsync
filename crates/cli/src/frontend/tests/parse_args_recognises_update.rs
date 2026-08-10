use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_update_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--update"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.update);
}
