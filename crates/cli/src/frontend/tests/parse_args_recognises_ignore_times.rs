use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_ignore_times_flag() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--ignore-times"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.ignore_times);
}
